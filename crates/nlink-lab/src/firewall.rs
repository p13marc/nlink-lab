//! The NLL firewall's match grammar and its lowering to nftables (#164).
//!
//! A rule's `match` is stored as an nft-like string (`ip saddr 10.0.0.0/8
//! tcp dport 22`): it is what the NLL parser produces from `src 10.0.0.0/8
//! tcp dport 22`, and what topology JSON, state files and `apply --check`
//! carry. This module is the one place that reads it — the planner lowers it
//! to an nlink [`Rule`], the validator checks it, `render` turns it back into
//! NLL — where each used to carry its own tokenizer.

use std::net::IpAddr;

use nlink::netlink::nftables::{
    CmpOp, CtState, Expr, Family, Hook, MetaKey, PayloadBase, Policy, Register, Rule,
};

use crate::types::{FirewallConfig, FirewallRule, RejectWith};

/// The base chains a node's firewall can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Chain {
    /// Traffic addressed to the node itself.
    Input,
    /// Traffic routed through the node.
    Forward,
    /// Traffic the node sends.
    Output,
}

impl Chain {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Chain::Input => "input",
            Chain::Forward => "forward",
            Chain::Output => "output",
        }
    }

    pub(crate) fn hook(self) -> Hook {
        match self {
            Chain::Input => Hook::Input,
            Chain::Forward => Hook::Forward,
            Chain::Output => Hook::Output,
        }
    }
}

/// The chains to install for `fw`, each with its policy and rules: input
/// and forward always (a forward chain left out accepts), output only when
/// declared.
pub(crate) fn chains(fw: &FirewallConfig) -> Vec<(Chain, Option<&str>, &[FirewallRule])> {
    let mut out = vec![(Chain::Input, fw.policy.as_deref(), fw.rules.as_slice())];
    match &fw.forward {
        Some(c) => out.push((Chain::Forward, c.policy.as_deref(), c.rules.as_slice())),
        None => out.push((Chain::Forward, None, &[])),
    }
    if let Some(c) = &fw.output {
        out.push((Chain::Output, c.policy.as_deref(), c.rules.as_slice()));
    }
    out
}

/// A chain policy. A base chain takes only accept or drop: there is no
/// `reject` policy (end the chain with a bare `reject` rule instead).
pub(crate) fn policy(p: Option<&str>) -> Result<Policy, String> {
    match p {
        None | Some("accept") => Ok(Policy::Accept),
        Some("drop") => Ok(Policy::Drop),
        Some(other) => Err(format!(
            "firewall policy must be accept or drop, got '{other}' (a chain cannot have a \
             reject policy: end it with a bare `reject` rule)"
        )),
    }
}

/// `ct state` names nftables knows.
const CT_STATES: [&str; 5] = ["established", "related", "new", "invalid", "untracked"];

/// One clause of a match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clause {
    /// `ip|ip6 saddr|daddr [!=] CIDR`. `text` is the operand as written.
    Addr {
        v6: bool,
        src: bool,
        neg: bool,
        addr: IpAddr,
        prefix: u8,
        text: String,
    },
    /// `tcp|udp dport|sport [!=] N` or `… LO-HI`.
    Port {
        tcp: bool,
        dport: bool,
        neg: bool,
        lo: u16,
        hi: u16,
    },
    /// `icmp|icmpv6 type N`.
    Icmp { v6: bool, ty: u8 },
    /// `ct state a,b`.
    CtState(Vec<String>),
    /// `mark N`.
    Mark(u32),
    /// `iifname|oifname [!=] NAME`.
    Iface { out: bool, neg: bool, name: String },
}

const SUPPORTED: &str = "Supported: 'ip saddr/daddr [!=] CIDR', 'ip6 saddr/daddr [!=] CIDR', \
     'ct state ...', 'tcp|udp dport/sport [!=] N[-M]', 'icmp type N', 'icmpv6 type N', \
     'mark N', 'iifname|oifname [!=] NAME'";

/// Parse a stored match expression. An empty expression matches every
/// packet. Errors name the offending token.
pub(crate) fn parse_match(expr: &str) -> Result<Vec<Clause>, String> {
    let toks: Vec<&str> = expr.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let neg = toks.get(i + 2) == Some(&"!=");
        // the operand follows the keyword pair, after an optional `!=`
        let operand = |at: usize| -> Result<&str, String> {
            toks.get(at)
                .copied()
                .ok_or_else(|| format!("'{}' needs a value in expression: '{expr}'", toks[i]))
        };
        match toks[i] {
            kw @ ("ip" | "ip6") if matches!(toks.get(i + 1), Some(&("saddr" | "daddr"))) => {
                let dir = toks[i + 1];
                let at = i + 2 + usize::from(neg);
                let cidr = operand(at)?;
                let want_v6 = kw == "ip6";
                let family = if want_v6 { "IPv6" } else { "IPv4" };
                let (addr, prefix) = crate::helpers::parse_ip_or_cidr(cidr)
                    .map_err(|e| format!("invalid {family} CIDR '{cidr}' in firewall rule: {e}"))?;
                if addr.is_ipv6() != want_v6 {
                    let other = if want_v6 { "ip" } else { "ip6" };
                    return Err(format!(
                        "'{kw} {dir}' needs an {family} address, got '{cidr}' (use '{other} {dir}')"
                    ));
                }
                out.push(Clause::Addr {
                    v6: want_v6,
                    src: dir == "saddr",
                    neg,
                    addr,
                    prefix,
                    text: cidr.to_string(),
                });
                i = at + 1;
            }
            proto @ ("tcp" | "udp") if matches!(toks.get(i + 1), Some(&("dport" | "sport"))) => {
                let at = i + 2 + usize::from(neg);
                let value = operand(at)?;
                let port = |s: &str| {
                    s.parse::<u16>()
                        .map_err(|_| format!("invalid port '{s}' in firewall rule"))
                };
                let (lo, hi) = match value.split_once('-') {
                    Some((a, b)) => (port(a)?, port(b)?),
                    None => {
                        let p = port(value)?;
                        (p, p)
                    }
                };
                if lo > hi {
                    return Err(format!(
                        "port range '{value}' in firewall rule starts after it ends"
                    ));
                }
                if neg && lo != hi {
                    return Err(format!(
                        "a negated port range ('!= {value}') is not supported in a firewall rule; \
                         match the range and put an accept/drop rule for it first"
                    ));
                }
                out.push(Clause::Port {
                    tcp: proto == "tcp",
                    dport: toks[i + 1] == "dport",
                    neg,
                    lo,
                    hi,
                });
                i = at + 1;
            }
            proto @ ("icmp" | "icmpv6") if toks.get(i + 1) == Some(&"type") => {
                let value = operand(i + 2)?;
                let what = if proto == "icmp" { "ICMP" } else { "ICMPv6" };
                let ty = value
                    .parse::<u8>()
                    .map_err(|_| format!("invalid {what} type '{value}' in firewall rule"))?;
                out.push(Clause::Icmp {
                    v6: proto == "icmpv6",
                    ty,
                });
                i += 3;
            }
            "ct" if toks.get(i + 1) == Some(&"state") => {
                let value = operand(i + 2)?;
                let mut states = Vec::new();
                for st in value.split(',') {
                    let st = st.trim();
                    if !CT_STATES.contains(&st) {
                        return Err(format!(
                            "unknown ct state '{st}' in firewall rule (expected {})",
                            CT_STATES.join(", ")
                        ));
                    }
                    states.push(st.to_string());
                }
                out.push(Clause::CtState(states));
                i += 3;
            }
            "mark" => {
                let value = operand(i + 1)?;
                let mark = value
                    .parse::<u32>()
                    .map_err(|_| format!("invalid mark '{value}' in firewall rule"))?;
                out.push(Clause::Mark(mark));
                i += 2;
            }
            kw @ ("iifname" | "oifname") => {
                let neg = toks.get(i + 1) == Some(&"!=");
                let at = i + 1 + usize::from(neg);
                let name = operand(at)?;
                if name.len() > 15 {
                    return Err(format!(
                        "interface name '{name}' in firewall rule is longer than 15 characters"
                    ));
                }
                if name.contains('*') {
                    return Err(format!(
                        "interface name '{name}' in firewall rule: wildcards are not supported, \
                         name the interface"
                    ));
                }
                out.push(Clause::Iface {
                    out: kw == "oifname",
                    neg,
                    name: name.to_string(),
                });
                i = at + 1;
            }
            other => {
                return Err(format!(
                    "unsupported firewall match token '{other}' in expression: '{expr}'. {SUPPORTED}"
                ));
            }
        }
    }
    Ok(out)
}

impl Clause {
    /// The clause in NLL syntax (`src 10.0.0.0/8`, `iif != eth0`, …).
    pub(crate) fn to_nll(&self) -> String {
        let ne = |neg: bool| if neg { " !=" } else { "" };
        match self {
            Clause::Addr { src, neg, text, .. } => {
                format!("{}{} {text}", if *src { "src" } else { "dst" }, ne(*neg))
            }
            Clause::Port {
                tcp,
                dport,
                neg,
                lo,
                hi,
            } => {
                let range = if lo == hi {
                    lo.to_string()
                } else {
                    format!("{lo}-{hi}")
                };
                format!(
                    "{} {}{} {range}",
                    if *tcp { "tcp" } else { "udp" },
                    if *dport { "dport" } else { "sport" },
                    ne(*neg)
                )
            }
            Clause::Icmp { v6, ty } => format!("{} {ty}", if *v6 { "icmpv6" } else { "icmp" }),
            Clause::CtState(states) => format!("ct {}", states.join(",")),
            Clause::Mark(m) => format!("mark {m}"),
            Clause::Iface { out, neg, name } => {
                format!("{}{} {name}", if *out { "oif" } else { "iif" }, ne(*neg))
            }
        }
    }

    /// Append the clause's expressions to `rule`. The plain forms use the
    /// same nlink helpers they always did, so rules written before these
    /// forms existed keep the exact bytes deployed labs carry.
    pub(crate) fn lower(&self, rule: Rule) -> Rule {
        match self {
            Clause::Addr {
                src,
                neg,
                addr,
                prefix,
                ..
            } => match (*src, *neg, *addr) {
                (true, false, IpAddr::V4(a)) => rule.match_saddr_v4(a, *prefix),
                (true, false, IpAddr::V6(a)) => rule.match_saddr_v6(a, *prefix),
                (false, false, IpAddr::V4(a)) => rule.match_daddr_v4(a, *prefix),
                (false, false, IpAddr::V6(a)) => rule.match_daddr_v6(a, *prefix),
                (true, true, IpAddr::V4(a)) => rule.match_saddr_v4_not(a, *prefix),
                (true, true, IpAddr::V6(a)) => rule.match_saddr_v6_not(a, *prefix),
                (false, true, IpAddr::V4(a)) => rule.match_daddr_v4_not(a, *prefix),
                (false, true, IpAddr::V6(a)) => rule.match_daddr_v6_not(a, *prefix),
            },
            Clause::Port {
                tcp,
                dport,
                neg,
                lo,
                hi,
            } => match (*tcp, *dport, *neg, lo == hi) {
                (true, true, false, true) => rule.match_tcp_dport(*lo),
                (true, false, false, true) => rule.match_tcp_sport(*lo),
                (false, true, false, true) => rule.match_udp_dport(*lo),
                (false, false, false, true) => rule.match_udp_sport(*lo),
                (true, true, true, true) => rule.match_tcp_dport_not(*lo),
                (false, true, true, true) => rule.match_udp_dport_not(*lo),
                // `sport !=` and ranges: the payload load nft emits, then
                // one compare (`!=`) or two (`>= lo`, `<= hi`). Ports are
                // big-endian, so the kernel's bytewise compare orders them.
                _ => {
                    let proto = if *tcp { 6 } else { 17 };
                    let mut rule = rule.match_l4proto(proto).expr(Expr::Payload {
                        dreg: Register::R0,
                        base: PayloadBase::Transport,
                        offset: if *dport { 2 } else { 0 },
                        len: 2,
                    });
                    let cmp = |op, port: u16| Expr::Cmp {
                        sreg: Register::R0,
                        op,
                        data: port.to_be_bytes().to_vec(),
                    };
                    if *neg {
                        rule = rule.expr(cmp(CmpOp::Neq, *lo));
                    } else {
                        rule = rule.expr(cmp(CmpOp::Gte, *lo)).expr(cmp(CmpOp::Lte, *hi));
                    }
                    rule
                }
            },
            Clause::Icmp { v6: false, ty } => rule.match_icmp_type(*ty),
            Clause::Icmp { v6: true, ty } => rule.match_icmpv6_type(*ty),
            Clause::CtState(states) => {
                let mut ct = CtState::empty();
                for st in states {
                    ct |= match st.as_str() {
                        "established" => CtState::ESTABLISHED,
                        "related" => CtState::RELATED,
                        "new" => CtState::NEW,
                        "invalid" => CtState::INVALID,
                        _ => CtState::UNTRACKED,
                    };
                }
                rule.match_ct_state(ct)
            }
            Clause::Mark(m) => rule.match_mark(*m),
            Clause::Iface {
                out,
                neg: false,
                name,
            } => {
                if *out {
                    rule.match_oif(name)
                } else {
                    rule.match_iif(name)
                }
            }
            Clause::Iface {
                out,
                neg: true,
                name,
            } => {
                let mut data = name.as_bytes().to_vec();
                data.push(0);
                data.resize(16, 0);
                rule.expr(Expr::Meta {
                    dreg: Register::R0,
                    key: if *out {
                        MetaKey::OifName
                    } else {
                        MetaKey::IifName
                    },
                })
                .expr(Expr::Cmp {
                    sreg: Register::R0,
                    op: CmpOp::Neq,
                    data,
                })
            }
        }
    }
}

/// ICMPX codes (`NFT_REJECT_ICMPX_*`), which nlink does not name.
const ICMPX_NO_ROUTE: u8 = 0;
const ICMPX_PORT_UNREACH: u8 = 1;
const ICMPX_HOST_UNREACH: u8 = 2;
const ICMPX_ADMIN_PROHIBITED: u8 = 3;

/// Check a rule's verdict and its `reject with`, given its clauses.
pub(crate) fn check_verdict(rule: &FirewallRule, clauses: &[Clause]) -> Result<(), String> {
    let action = rule.action.as_deref().unwrap_or("accept");
    if !matches!(action, "accept" | "drop" | "reject") {
        return Err(format!(
            "unknown firewall action '{action}' (expected accept, drop or reject)"
        ));
    }
    match rule.reject_with {
        Some(_) if action != "reject" => Err(format!(
            "`reject with` on a rule whose action is '{action}'"
        )),
        Some(RejectWith::TcpReset)
            if !clauses
                .iter()
                .any(|c| matches!(c, Clause::Port { tcp: true, .. })) =>
        {
            Err(
                "`reject with tcp-reset` needs a `tcp dport`/`tcp sport` match: a reset \
                 answers TCP only"
                    .to_string(),
            )
        }
        _ => Ok(()),
    }
}

/// Build the nftables rule for one firewall rule in `chain` of the
/// `nlink-lab` table.
pub(crate) fn lower_rule(table: &str, chain: Chain, rule: &FirewallRule) -> Result<Rule, String> {
    let clauses = parse_match(rule.match_expr.as_deref().unwrap_or(""))?;
    check_verdict(rule, &clauses)?;
    let mut r = Rule::new(table, chain.name()).family(Family::Inet);
    for c in &clauses {
        r = c.lower(r);
    }
    use nlink::netlink::nftables::{NFT_REJECT_ICMPX_UNREACH, NFT_REJECT_TCP_RST};
    Ok(match rule.action.as_deref().unwrap_or("accept") {
        "drop" => r.drop(),
        "reject" => match rule.reject_with {
            None | Some(RejectWith::PortUnreachable) => {
                r.reject_with(NFT_REJECT_ICMPX_UNREACH, ICMPX_PORT_UNREACH)
            }
            Some(RejectWith::TcpReset) => r.reject_with(NFT_REJECT_TCP_RST, 0),
            Some(RejectWith::HostUnreachable) => {
                r.reject_with(NFT_REJECT_ICMPX_UNREACH, ICMPX_HOST_UNREACH)
            }
            Some(RejectWith::NoRoute) => r.reject_with(NFT_REJECT_ICMPX_UNREACH, ICMPX_NO_ROUTE),
            Some(RejectWith::AdminProhibited) => {
                r.reject_with(NFT_REJECT_ICMPX_UNREACH, ICMPX_ADMIN_PROHIBITED)
            }
        },
        _ => r.accept(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A built rule's expressions (nlink exposes them on a declared rule).
    fn exprs_of(rule: Rule) -> Vec<Expr> {
        let cfg =
            nlink::netlink::nftables::config::NftablesConfig::new()
                .table("t", Family::Inet, |t| t.rule_keyed("input", "k", |_| rule));
        cfg.tables()[0].rules()[0].exprs().to_vec()
    }

    fn rule(m: &str, action: &str) -> FirewallRule {
        FirewallRule {
            match_expr: Some(m.into()),
            action: Some(action.into()),
            ..Default::default()
        }
    }

    #[test]
    fn parses_every_clause_and_renders_it_as_nll() {
        let expr = "ip saddr != 10.0.0.0/8 ip6 daddr fd00::1 tcp dport 8000-8999 \
                    udp sport != 53 icmp type 8 ct state established,related mark 7 \
                    iifname lan0 oifname != wan0";
        let clauses = parse_match(expr).unwrap();
        let nll: Vec<String> = clauses.iter().map(Clause::to_nll).collect();
        assert_eq!(
            nll,
            [
                "src != 10.0.0.0/8",
                "dst fd00::1",
                "tcp dport 8000-8999",
                "udp sport != 53",
                "icmp 8",
                "ct established,related",
                "mark 7",
                "iif lan0",
                "oif != wan0",
            ]
        );
        assert!(parse_match("").unwrap().is_empty());
    }

    #[test]
    fn rejects_what_would_install_wrong() {
        for (expr, want) in [
            ("ct state establised", "unknown ct state 'establised'"),
            ("tcp dport 9000-8000", "starts after it ends"),
            ("tcp dport != 1-2", "negated port range"),
            ("ip saddr fd00::/64", "use 'ip6 saddr'"),
            ("iifname averyveryverylongname0", "longer than 15"),
            ("tcp dport", "needs a value"),
            ("frob 1", "unsupported firewall match token 'frob'"),
        ] {
            let err = parse_match(expr).unwrap_err();
            assert!(err.contains(want), "{expr}: {err}");
        }
    }

    #[test]
    fn reject_lowers_to_reject_not_accept() {
        let r = exprs_of(lower_rule("t", Chain::Input, &rule("tcp dport 22", "reject")).unwrap());
        assert!(matches!(
            r.last(),
            Some(Expr::Reject {
                reject_type: 2,
                icmp_code: 1
            })
        ));
        let mut tcp = rule("tcp dport 22", "reject");
        tcp.reject_with = Some(RejectWith::TcpReset);
        let r = exprs_of(lower_rule("t", Chain::Input, &tcp).unwrap());
        assert!(matches!(
            r.last(),
            Some(Expr::Reject { reject_type: 1, .. })
        ));
        // a reset answers TCP only
        let mut udp = rule("udp dport 53", "reject");
        udp.reject_with = Some(RejectWith::TcpReset);
        assert!(lower_rule("t", Chain::Input, &udp).is_err());
        // `reject with` on an accept, an unknown action
        let mut acc = rule("tcp dport 22", "accept");
        acc.reject_with = Some(RejectWith::NoRoute);
        assert!(lower_rule("t", Chain::Input, &acc).is_err());
        assert!(lower_rule("t", Chain::Input, &rule("", "dorp")).is_err());
        // a bare verdict matches everything
        let r = exprs_of(lower_rule("t", Chain::Forward, &rule("", "drop")).unwrap());
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn port_range_and_negations_lower_to_the_compares_nft_emits() {
        let r =
            exprs_of(lower_rule("t", Chain::Input, &rule("tcp dport 8000-8999", "drop")).unwrap());
        let cmps: Vec<(CmpOp, Vec<u8>)> = r
            .iter()
            .filter_map(|e| match e {
                Expr::Cmp { op, data, .. } => Some((op.clone(), data.clone())),
                _ => None,
            })
            .collect();
        // l4proto == tcp, then >= 8000 and <= 8999, big-endian
        assert_eq!(
            cmps[1..],
            [
                (CmpOp::Gte, 8000u16.to_be_bytes().to_vec()),
                (CmpOp::Lte, 8999u16.to_be_bytes().to_vec())
            ]
        );
        let r =
            exprs_of(lower_rule("t", Chain::Forward, &rule("iifname != wan0", "drop")).unwrap());
        assert!(matches!(
            &r[..2],
            [
                Expr::Meta {
                    key: MetaKey::IifName,
                    ..
                },
                Expr::Cmp { op: CmpOp::Neq, .. }
            ]
        ));
    }

    #[test]
    fn policies_are_accept_or_drop() {
        assert!(matches!(policy(None), Ok(Policy::Accept)));
        assert!(matches!(policy(Some("drop")), Ok(Policy::Drop)));
        assert!(
            policy(Some("reject"))
                .unwrap_err()
                .contains("reject policy")
        );
        assert!(policy(Some("dorp")).is_err());
    }
}
