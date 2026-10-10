//! The NLL firewall's match grammar and its lowering to nftables (#164).
//!
//! A rule's `match` is stored as an nft-like string (`ip saddr 10.0.0.0/8
//! tcp dport 22`): it is what the NLL parser produces from `src 10.0.0.0/8
//! tcp dport 22`, and what topology JSON, state files and `apply --check`
//! carry. This module is the one place that reads it — the planner lowers it
//! to an nlink [`Rule`], the validator checks it, `render` turns it back into
//! NLL — where each used to carry its own tokenizer.

use std::net::IpAddr;

use std::collections::BTreeMap;

use nlink::netlink::nftables::{
    CmpOp, CtState, Expr, Family, Hook, MetaKey, PacketField, PayloadBase, Policy, Register, Rule,
    SetElement, SetKeyType,
};

use crate::types::{FirewallConfig, FirewallRule, FirewallSet, RejectWith, SetKey};

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

/// A 32-bit value written in decimal or `0x` hex.
pub(crate) fn parse_u32(s: &str) -> Option<u32> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// `ct state` names nftables knows.
const CT_STATES: [&str; 5] = ["established", "related", "new", "invalid", "untracked"];

/// A packet field a set can be keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    /// `ip|ip6 saddr|daddr`.
    Addr { v6: bool, src: bool },
    /// `tcp|udp dport|sport`.
    Port { tcp: bool, dport: bool },
    /// `mark`.
    Mark,
}

impl Field {
    /// The field at `toks[i]`, and the index after it.
    fn at(toks: &[&str], i: usize) -> Option<(Field, usize)> {
        match (toks.get(i).copied(), toks.get(i + 1).copied()) {
            (Some(kw @ ("ip" | "ip6")), Some(dir @ ("saddr" | "daddr"))) => Some((
                Field::Addr {
                    v6: kw == "ip6",
                    src: dir == "saddr",
                },
                i + 2,
            )),
            (Some(proto @ ("tcp" | "udp")), Some(dir @ ("dport" | "sport"))) => Some((
                Field::Port {
                    tcp: proto == "tcp",
                    dport: dir == "dport",
                },
                i + 2,
            )),
            (Some("mark"), _) => Some((Field::Mark, i + 1)),
            _ => None,
        }
    }

    /// The stored (nft-like) spelling.
    fn text(self) -> &'static str {
        match self {
            Field::Addr {
                v6: false,
                src: true,
            } => "ip saddr",
            Field::Addr {
                v6: false,
                src: false,
            } => "ip daddr",
            Field::Addr {
                v6: true,
                src: true,
            } => "ip6 saddr",
            Field::Addr {
                v6: true,
                src: false,
            } => "ip6 daddr",
            Field::Port {
                tcp: true,
                dport: true,
            } => "tcp dport",
            Field::Port {
                tcp: true,
                dport: false,
            } => "tcp sport",
            Field::Port {
                tcp: false,
                dport: true,
            } => "udp dport",
            Field::Port {
                tcp: false,
                dport: false,
            } => "udp sport",
            Field::Mark => "mark",
        }
    }

    /// The NLL spelling.
    fn nll(self) -> &'static str {
        match self {
            Field::Addr { src: true, .. } => "src",
            Field::Addr { src: false, .. } => "dst",
            other => other.text(),
        }
    }

    /// The set key this field is looked up as.
    pub(crate) fn key(self) -> SetKey {
        match self {
            Field::Addr { v6: false, .. } => SetKey::Ipv4,
            Field::Addr { v6: true, .. } => SetKey::Ipv6,
            Field::Port { .. } => SetKey::Port,
            Field::Mark => SetKey::Mark,
        }
    }

    fn packet_field(self) -> PacketField {
        match self {
            Field::Addr {
                v6: false,
                src: true,
            } => PacketField::Ip4Saddr,
            Field::Addr {
                v6: false,
                src: false,
            } => PacketField::Ip4Daddr,
            Field::Addr {
                v6: true,
                src: true,
            } => PacketField::Ip6Saddr,
            Field::Addr {
                v6: true,
                src: false,
            } => PacketField::Ip6Daddr,
            Field::Port {
                tcp: true,
                dport: true,
            } => PacketField::TcpDport,
            Field::Port {
                tcp: true,
                dport: false,
            } => PacketField::TcpSport,
            Field::Port {
                tcp: false,
                dport: true,
            } => PacketField::UdpDport,
            Field::Port {
                tcp: false,
                dport: false,
            } => PacketField::UdpSport,
            Field::Mark => PacketField::Mark,
        }
    }
}

/// One clause of a match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clause {
    /// `FIELD [!=] @set`.
    InSet {
        field: Field,
        neg: bool,
        set: String,
    },
    /// `FIELD . FIELD … [!=] @set` (a concatenated key).
    Concat {
        fields: Vec<Field>,
        neg: bool,
        set: String,
    },
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
    /// `mark V` or `mark V/M` (decimal or hex, as written in `text`).
    Mark {
        value: u32,
        mask: Option<u32>,
        text: String,
    },
    /// `ct mark V`.
    CtMark { value: u32, text: String },
    /// `iifname|oifname [!=] NAME`.
    Iface { out: bool, neg: bool, name: String },
}

const SUPPORTED: &str = "Supported: 'ip saddr/daddr [!=] CIDR|@set', 'ip6 saddr/daddr [!=] \
     CIDR|@set', 'ct state ...', 'tcp|udp dport/sport [!=] N[-M]|@set', 'icmp type N', \
     'icmpv6 type N', 'mark N|@set', 'iifname|oifname [!=] NAME', 'FIELD . FIELD [!=] @set'";

/// The set name of a `@name` operand.
fn set_ref(op: &str) -> Result<Option<String>, String> {
    let Some(name) = op.strip_prefix('@') else {
        return Ok(None);
    };
    if name.is_empty() {
        return Err("'@' needs a set name in firewall rule".to_string());
    }
    Ok(Some(name.to_string()))
}

/// Parse a stored match expression. An empty expression matches every
/// packet. Errors name the offending token.
pub(crate) fn parse_match(expr: &str) -> Result<Vec<Clause>, String> {
    let toks: Vec<&str> = expr.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Some((field, mut j)) = Field::at(&toks, i) {
            // `FIELD . FIELD … [!=] @set`
            if toks.get(j) == Some(&".") {
                let mut fields = vec![field];
                while toks.get(j) == Some(&".") {
                    let Some((f, k)) = Field::at(&toks, j + 1) else {
                        return Err(format!(
                            "a concatenation joins fields with ` . ` (ip/ip6 saddr/daddr, \
                             tcp/udp dport/sport, mark) in expression: '{expr}'"
                        ));
                    };
                    fields.push(f);
                    j = k;
                }
                let neg = toks.get(j) == Some(&"!=");
                let op = toks.get(j + usize::from(neg)).copied().unwrap_or("");
                let Some(set) = set_ref(op)? else {
                    return Err(format!(
                        "a concatenation can only be matched against a set (`@name`), got \
                         '{op}' in expression: '{expr}'"
                    ));
                };
                out.push(Clause::Concat { fields, neg, set });
                i = j + usize::from(neg) + 1;
                continue;
            }
            // `FIELD [!=] @set`
            let neg = toks.get(j) == Some(&"!=");
            if let Some(op) = toks.get(j + usize::from(neg))
                && let Some(set) = set_ref(op)?
            {
                out.push(Clause::InSet { field, neg, set });
                i = j + usize::from(neg) + 1;
                continue;
            }
        }
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
                let text = operand(i + 1)?;
                let bad = || format!("invalid mark '{text}' in firewall rule");
                let (value, mask) = match text.split_once('/') {
                    Some((v, m)) => (
                        parse_u32(v).ok_or_else(bad)?,
                        Some(parse_u32(m).ok_or_else(bad)?),
                    ),
                    None => (parse_u32(text).ok_or_else(bad)?, None),
                };
                out.push(Clause::Mark {
                    value,
                    mask,
                    text: text.to_string(),
                });
                i += 2;
            }
            "ct" if toks.get(i + 1) == Some(&"mark") => {
                let text = operand(i + 2)?;
                let value = parse_u32(text)
                    .ok_or_else(|| format!("invalid ct mark '{text}' in firewall rule"))?;
                out.push(Clause::CtMark {
                    value,
                    text: text.to_string(),
                });
                i += 3;
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
            Clause::InSet { field, neg, set } => format!("{}{} @{set}", field.nll(), ne(*neg)),
            Clause::Concat { fields, neg, set } => format!(
                "{}{} @{set}",
                fields
                    .iter()
                    .map(|f| f.nll())
                    .collect::<Vec<_>>()
                    .join(" . "),
                ne(*neg)
            ),
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
            Clause::Mark { text, .. } => format!("mark {text}"),
            Clause::CtMark { text, .. } => format!("ct mark {text}"),
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
            Clause::InSet {
                field,
                neg: false,
                set,
            } => rule.match_in_set(field.packet_field(), set),
            Clause::InSet {
                field,
                neg: true,
                set,
            } => rule.match_not_in_set(field.packet_field(), set),
            Clause::Concat { fields, neg, set } => {
                let pf: Vec<PacketField> = fields.iter().map(|f| f.packet_field()).collect();
                if *neg {
                    rule.match_concat_not_in_set(&pf, set)
                } else {
                    rule.match_concat_in_set(&pf, set)
                }
            }
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
            Clause::Mark {
                value, mask: None, ..
            } => rule.match_mark(*value),
            Clause::Mark {
                value,
                mask: Some(mask),
                ..
            } => rule.match_mark_masked(*value, *mask),
            Clause::CtMark { value, .. } => rule.match_ct_mark(*value),
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

impl Clause {
    /// The clause in the stored (nft-like) spelling `parse_match` reads.
    fn to_match(&self) -> String {
        let ne = |neg: bool| if neg { " !=" } else { "" };
        match self {
            Clause::InSet { field, neg, set } => format!("{}{} @{set}", field.text(), ne(*neg)),
            Clause::Concat { fields, neg, set } => format!(
                "{}{} @{set}",
                fields
                    .iter()
                    .map(|f| f.text())
                    .collect::<Vec<_>>()
                    .join(" . "),
                ne(*neg)
            ),
            Clause::Addr {
                v6, src, neg, text, ..
            } => format!(
                "{}{} {text}",
                Field::Addr { v6: *v6, src: *src }.text(),
                ne(*neg)
            ),
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
                    "{}{} {range}",
                    Field::Port {
                        tcp: *tcp,
                        dport: *dport
                    }
                    .text(),
                    ne(*neg)
                )
            }
            Clause::Icmp { v6, ty } => format!("{} type {ty}", if *v6 { "icmpv6" } else { "icmp" }),
            Clause::CtState(states) => format!("ct state {}", states.join(",")),
            Clause::Mark { text, .. } => format!("mark {text}"),
            Clause::CtMark { text, .. } => format!("ct mark {text}"),
            Clause::Iface { out, neg, name } => format!(
                "{}{} {name}",
                if *out { "oifname" } else { "iifname" },
                ne(*neg)
            ),
        }
    }
}

/// Make the `ip`/`ip6` of every address field matched against a set agree
/// with the set's key. NLL's `src @blocked` does not say which family; the
/// set does. Expressions without a set are returned unchanged.
pub(crate) fn normalize_set_families(expr: &str, sets: &BTreeMap<String, FirewallSet>) -> String {
    if !expr.contains('@') {
        return expr.to_string();
    }
    let Ok(mut clauses) = parse_match(expr) else {
        return expr.to_string(); // the validator reports it
    };
    let fix = |field: &mut Field, key: Option<&SetKey>| {
        if let (Field::Addr { v6, .. }, Some(k @ (SetKey::Ipv4 | SetKey::Ipv6))) = (field, key) {
            *v6 = *k == SetKey::Ipv6;
        }
    };
    for c in &mut clauses {
        match c {
            Clause::InSet { field, set, .. } => {
                fix(field, sets.get(set.as_str()).and_then(|s| s.key.first()));
            }
            Clause::Concat { fields, set, .. } => {
                if let Some(s) = sets.get(set.as_str()) {
                    for (f, k) in fields.iter_mut().zip(&s.key) {
                        fix(f, Some(k));
                    }
                }
            }
            _ => {}
        }
    }
    clauses
        .iter()
        .map(Clause::to_match)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What is wrong with a rule's set lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetRefProblem {
    /// The set is not declared.
    Undeclared,
    /// The fields do not match the set's key.
    WrongKey,
}

/// Problems with a rule's set lookups.
pub(crate) fn check_set_refs(
    clauses: &[Clause],
    sets: &BTreeMap<String, FirewallSet>,
) -> Vec<(SetRefProblem, String)> {
    let keys = |k: &[SetKey]| k.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(" . ");
    let mut out = Vec::new();
    for c in clauses {
        let (fields, set) = match c {
            Clause::InSet { field, set, .. } => (vec![*field], set),
            Clause::Concat { fields, set, .. } => (fields.clone(), set),
            _ => continue,
        };
        let Some(declared) = sets.get(set.as_str()) else {
            out.push((
                SetRefProblem::Undeclared,
                format!("set '@{set}' is not declared on this node (`set {set} … [ … ]`)"),
            ));
            continue;
        };
        let want: Vec<SetKey> = fields.iter().map(|f| f.key()).collect();
        if declared.key != want {
            out.push((
                SetRefProblem::WrongKey,
                format!(
                    "`{}` looks up `{}` in set '@{set}', whose key is `{}`",
                    fields
                        .iter()
                        .map(|f| f.nll())
                        .collect::<Vec<_>>()
                        .join(" . "),
                    keys(&want),
                    keys(&declared.key)
                ),
            ));
        }
    }
    out
}

/// A set's key: as declared, or inferred from its first element (addresses
/// and ports; a mark key must be written).
pub(crate) fn set_key(name: &str, set: &FirewallSet) -> Result<Vec<SetKey>, String> {
    if !set.key.is_empty() {
        return Ok(set.key.clone());
    }
    let Some(first) = set.elements.first() else {
        return Err(format!(
            "set '{name}' has no key and no element to infer one from: write it \
             (`set {name} ipv4 [ ]`)"
        ));
    };
    first
        .split(" . ")
        .map(|part| {
            let lo = part.split_once('-').map_or(part, |(a, _)| a);
            let addr = lo.split_once('/').map_or(lo, |(a, _)| a);
            match addr.parse::<IpAddr>() {
                Ok(IpAddr::V4(_)) => Ok(SetKey::Ipv4),
                Ok(IpAddr::V6(_)) => Ok(SetKey::Ipv6),
                Err(_) if addr.parse::<u16>().is_ok() => Ok(SetKey::Port),
                Err(_) => Err(format!(
                    "set '{name}': cannot tell the key of element '{first}': write it \
                     (`set {name} ipv4 . port [ … ]`)"
                )),
            }
        })
        .collect()
}

/// One part of an element, for one key field; `true` when it is a prefix
/// or a range (which needs an interval set).
fn element_part(key: SetKey, part: &str) -> Result<(SetElement, bool), String> {
    let bad = |what: &str| format!("'{part}' is not {what}");
    match key {
        SetKey::Ipv4 | SetKey::Ipv6 => {
            let v6 = key == SetKey::Ipv6;
            let what = if v6 {
                "an IPv6 address, prefix or range"
            } else {
                "an IPv4 address, prefix or range"
            };
            let ip = |s: &str| -> Result<IpAddr, String> {
                match s.parse::<IpAddr>() {
                    Ok(a) if a.is_ipv6() == v6 => Ok(a),
                    _ => Err(bad(what)),
                }
            };
            if let Some((a, b)) = part.split_once('-') {
                let (a, b) = (ip(a)?, ip(b)?);
                if a > b {
                    return Err(format!("range '{part}' starts after it ends"));
                }
                return Ok(match (a, b) {
                    (IpAddr::V4(a), IpAddr::V4(b)) => (SetElement::ipv4_range(a, b), true),
                    (IpAddr::V6(a), IpAddr::V6(b)) => (SetElement::ipv6_range(a, b), true),
                    _ => unreachable!("same family checked"),
                });
            }
            if let Some((a, len)) = part.split_once('/') {
                let len: u8 = len.parse().map_err(|_| bad(what))?;
                let elem = match ip(a)? {
                    IpAddr::V4(a) => SetElement::ipv4_prefix(a, len),
                    IpAddr::V6(a) => SetElement::ipv6_prefix(a, len),
                }
                .map_err(|e| format!("'{part}': {e}"))?;
                return Ok((elem, true));
            }
            Ok(match ip(part)? {
                IpAddr::V4(a) => (SetElement::ipv4(a), false),
                IpAddr::V6(a) => (SetElement::ipv6(a), false),
            })
        }
        SetKey::Port => {
            let port = |s: &str| s.parse::<u16>().map_err(|_| bad("a port or a port range"));
            if let Some((a, b)) = part.split_once('-') {
                let (a, b) = (port(a)?, port(b)?);
                if a > b {
                    return Err(format!("range '{part}' starts after it ends"));
                }
                return Ok((SetElement::port_range(a, b), true));
            }
            Ok((SetElement::port(port(part)?), false))
        }
        SetKey::Mark => {
            if part.contains('-') {
                return Err(format!(
                    "'{part}': a mark set cannot hold ranges (nftables byte-swaps marks, \
                     nlink does not)"
                ));
            }
            let m = part.parse::<u32>().map_err(|_| bad("a mark"))?;
            Ok((SetElement::mark(m), false))
        }
    }
}

/// A set ready for nlink: its key type, elements, and whether it must be an
/// interval set.
pub(crate) struct LoweredSet {
    pub key_type: SetKeyType,
    pub elements: Vec<SetElement>,
    pub interval: bool,
    pub size: Option<u32>,
    pub timeout: Option<std::time::Duration>,
    pub dynamic: bool,
}

/// Check and lower one declared set.
pub(crate) fn lower_set(name: &str, set: &FirewallSet) -> Result<LoweredSet, String> {
    let key = set_key(name, set)?;
    let field_type = |k: SetKey| match k {
        SetKey::Ipv4 => SetKeyType::Ipv4Addr,
        SetKey::Ipv6 => SetKeyType::Ipv6Addr,
        SetKey::Port => SetKeyType::InetService,
        SetKey::Mark => SetKeyType::Mark,
    };
    let key_type = match key.as_slice() {
        [one] => field_type(*one),
        many => SetKeyType::Concat(many.iter().map(|k| field_type(*k)).collect()),
    };
    let mut elements = Vec::new();
    let mut interval = false;
    for elem in &set.elements {
        let parts: Vec<&str> = elem.split(" . ").collect();
        if parts.len() != key.len() {
            return Err(format!(
                "set '{name}': element '{elem}' has {} part(s), the key has {}",
                parts.len(),
                key.len()
            ));
        }
        let mut lowered = Vec::new();
        for (k, part) in key.iter().zip(&parts) {
            let (e, is_interval) =
                element_part(*k, part.trim()).map_err(|e| format!("set '{name}': {e}"))?;
            interval |= is_interval;
            lowered.push(e);
        }
        elements.push(if lowered.len() == 1 {
            lowered.pop().unwrap()
        } else {
            SetElement::concat(lowered)
        });
    }
    if interval && key.len() > 1 && key.contains(&SetKey::Mark) {
        return Err(format!(
            "set '{name}': a concatenated set with a mark cannot hold prefixes or ranges \
             (nlink cannot encode a host-order field in an interval concatenation)"
        ));
    }
    if set.dynamic && interval {
        return Err(format!(
            "set '{name}': a dynamic set cannot hold prefixes or ranges (the kernel has no \
             interval set rules can write to)"
        ));
    }
    let timeout = set
        .timeout
        .as_deref()
        .map(|t| {
            crate::helpers::parse_duration(t).map_err(|e| format!("set '{name}': timeout: {e}"))
        })
        .transpose()?;
    Ok(LoweredSet {
        key_type,
        elements,
        interval,
        size: set.size,
        timeout,
        dynamic: set.dynamic,
    })
}

/// ICMPX codes (`NFT_REJECT_ICMPX_*`), which nlink does not name.
const ICMPX_NO_ROUTE: u8 = 0;
const ICMPX_PORT_UNREACH: u8 = 1;
const ICMPX_HOST_UNREACH: u8 = 2;
const ICMPX_ADMIN_PROHIBITED: u8 = 3;

/// Check a rule's verdict and its `reject with`, given its clauses.
pub(crate) fn check_verdict(rule: &FirewallRule, clauses: &[Clause]) -> Result<(), String> {
    let action = rule.action.as_deref().unwrap_or("accept");
    if !matches!(action, "accept" | "drop" | "reject" | "continue") {
        return Err(format!(
            "unknown firewall action '{action}' (expected accept, drop, reject, or continue for \
             a rule that only counts, logs or limits)"
        ));
    }
    if rule.clamp_mss.is_some() && action != "continue" {
        return Err(format!(
            "`clamp mss` goes on a rule with no verdict: it matches TCP SYNs, so a `{action}` \
             there would apply to SYNs only"
        ));
    }
    if action == "continue" && !rule.has_statement() {
        return Err(
            "a rule with no verdict must count, log or limit (`tcp dport 22 counter`)".to_string(),
        );
    }
    match rule.reject_with {
        Some(_) if action != "reject" => Err(format!(
            "`reject with` on a rule whose action is '{action}'"
        )),
        Some(RejectWith::TcpReset)
            if !clauses.iter().any(|c| match c {
                Clause::Port { tcp: true, .. } => true,
                Clause::InSet { field, .. } => matches!(field, Field::Port { tcp: true, .. }),
                Clause::Concat { fields, .. } => fields
                    .iter()
                    .any(|f| matches!(f, Field::Port { tcp: true, .. })),
                _ => false,
            }) =>
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

/// Check a rule's statements: what the kernel would refuse or misread.
pub(crate) fn check_statements(rule: &FirewallRule) -> Result<(), String> {
    if let Some(l) = &rule.limit {
        if l.rate == 0 {
            return Err("a limit of 0 packets matches nothing (`limit 10/second`)".to_string());
        }
        if l.burst == Some(0) {
            return Err(
                "`burst 0` is the kernel's default of 5: leave it out or give a burst".to_string(),
            );
        }
    }
    if let Some(name) = &rule.counter_name {
        let mut chars = name.chars();
        let ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.len() <= 64
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok && !name.contains("${") {
            return Err(format!(
                "counter name '{name}' must start with a letter and hold letters, digits, `_` and \
                 `-` (at most 64)"
            ));
        }
    }
    if rule.set_mark.is_some_and(|m| m.mask == Some(0)) {
        return Err("`mark set V/0` changes no bit".to_string());
    }
    if let Some(h) = &rule.classify
        && h.parse::<nlink::TcHandle>().is_err()
    {
        return Err(format!(
            "classify '{h}' is not a tc class handle (like 1:10)"
        ));
    }
    if rule.clamp_mss == Some(crate::types::MssClamp::Mss(0)) {
        return Err("`clamp mss 0`: an MSS of 0 is not a size".to_string());
    }
    if let Some(prefix) = rule.log.as_ref().and_then(|l| l.prefix.as_deref()) {
        // NF_LOG_PREFIXLEN is 128 with the NUL
        if prefix.len() > 127 || prefix.contains('\0') {
            return Err(format!(
                "log prefix '{prefix}' is longer than 127 bytes or holds a NUL"
            ));
        }
    }
    Ok(())
}

/// A statement that cannot work in `chain`.
pub(crate) fn check_statement_context(rule: &FirewallRule, chain: Chain) -> Option<String> {
    if chain == Chain::Input && rule.clamp_mss == Some(crate::types::MssClamp::Pmtu) {
        return Some(
            "`clamp mss pmtu` reads the route's MTU, which the kernel only allows in the \
             forward and output chains"
                .to_string(),
        );
    }
    if chain == Chain::Input && rule.classify.is_some() {
        return Some(
            "`classify` in the input chain has no effect: a priority picks a qdisc class on the \
             way out (forward or output)"
                .to_string(),
        );
    }
    None
}

/// Every named counter a firewall's rules count into (they are declared by
/// use), sorted and once each.
pub(crate) fn counter_names(fw: &FirewallConfig) -> Vec<String> {
    let names: std::collections::BTreeSet<String> = fw
        .all_rules()
        .filter_map(|r| r.counter_name.clone())
        .collect();
    names.into_iter().collect()
}

/// Build the nftables rule for one firewall rule in `chain` of the
/// `nlink-lab` table.
pub(crate) fn lower_rule(table: &str, chain: Chain, rule: &FirewallRule) -> Result<Rule, String> {
    let clauses = parse_match(rule.match_expr.as_deref().unwrap_or(""))?;
    check_verdict(rule, &clauses)?;
    check_statements(rule)?;
    let mut r = Rule::new(table, chain.name()).family(Family::Inet);
    for c in &clauses {
        r = c.lower(r);
    }
    if rule.clamp_mss.is_some() {
        // the MSS option rides on SYNs: `tcp flags syn / syn,rst`
        use nlink::netlink::nftables::TcpFlags;
        r = r.match_tcp_flags(TcpFlags::SYN, TcpFlags::SYN | TcpFlags::RST);
    }
    // Statements in a fixed order: the limit first (it is a match too), so
    // the counters and the log see only the packets the rule acts on.
    if let Some(l) = &rule.limit {
        use nlink::netlink::nftables::{LimitExpr, LimitUnit};
        let unit = match l.per {
            crate::types::LimitPer::Second => LimitUnit::Second,
            crate::types::LimitPer::Minute => LimitUnit::Minute,
            crate::types::LimitPer::Hour => LimitUnit::Hour,
            crate::types::LimitPer::Day => LimitUnit::Day,
        };
        let mut e = LimitExpr::packets(l.rate, unit);
        if let Some(b) = l.burst {
            e = e.burst(b);
        }
        if l.over {
            e = e.over();
        }
        r = r.expr(e);
    }
    if rule.counter {
        r = r.counter();
    }
    if let Some(name) = &rule.counter_name {
        r = r.counter_named(name);
    }
    if let Some(l) = &rule.log {
        let mut e = nlink::netlink::nftables::LogExpr::new();
        if let Some(p) = &l.prefix {
            e = e.prefix(p.as_str());
        }
        if let Some(g) = l.group {
            e = e.group(g);
        }
        r = r.expr(e);
    }
    if let Some(m) = rule.set_mark {
        r = match m.mask {
            Some(mask) => r.set_mark_masked(m.value, mask),
            None => r.set_mark(m.value),
        };
    }
    match rule.ct_mark {
        Some(crate::types::CtMarkAction::Set(v)) => r = r.set_ct_mark(v),
        Some(crate::types::CtMarkAction::Save) => r = r.save_mark_to_ct(),
        Some(crate::types::CtMarkAction::Restore) => r = r.restore_mark_from_ct(),
        None => {}
    }
    if let Some(h) = &rule.classify {
        let handle: nlink::TcHandle = h
            .parse()
            .map_err(|_| format!("classify '{h}' is not a tc class handle (like 1:10)"))?;
        r = r.set_priority(handle);
    }
    match rule.clamp_mss {
        Some(crate::types::MssClamp::Pmtu) => r = r.clamp_tcp_mss_to_pmtu(),
        Some(crate::types::MssClamp::Mss(m)) => r = r.clamp_tcp_mss(m),
        None => {}
    }
    use nlink::netlink::nftables::{NFT_REJECT_ICMPX_UNREACH, NFT_REJECT_TCP_RST};
    Ok(match rule.action.as_deref().unwrap_or("accept") {
        "continue" => r,
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
        // a TCP port in a set lookup is a TCP match too
        let mut set = rule("tcp dport @mgmt", "reject");
        set.reject_with = Some(RejectWith::TcpReset);
        assert!(check_verdict(&set, &parse_match("tcp dport @mgmt").unwrap()).is_ok());
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
                Expr::Cmp { op, data, .. } => Some((*op, data.clone())),
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

    fn set(key: &[SetKey], elements: &[&str]) -> FirewallSet {
        FirewallSet {
            key: key.to_vec(),
            elements: elements.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn set_lookups_and_concatenations_parse_and_render() {
        let c =
            parse_match("ip saddr != @blocked tcp dport @web ip daddr . tcp dport @pub").unwrap();
        let nll: Vec<String> = c.iter().map(Clause::to_nll).collect();
        assert_eq!(
            nll,
            ["src != @blocked", "tcp dport @web", "dst . tcp dport @pub"]
        );
        assert!(
            parse_match("ip saddr . tcp dport 22")
                .unwrap_err()
                .contains("only be matched against a set")
        );
        assert!(
            parse_match("ip saddr . frob @x")
                .unwrap_err()
                .contains("joins fields")
        );
        assert!(
            parse_match("ip saddr @")
                .unwrap_err()
                .contains("needs a set name")
        );
    }

    #[test]
    fn set_lookups_take_the_family_of_their_set() {
        let mut sets = BTreeMap::new();
        sets.insert("b6".to_string(), set(&[SetKey::Ipv6], &["fd00::1"]));
        sets.insert(
            "p".to_string(),
            set(&[SetKey::Ipv6, SetKey::Port], &["fd00::1 . 22"]),
        );
        assert_eq!(
            normalize_set_families("ip saddr @b6 tcp dport 22", &sets),
            "ip6 saddr @b6 tcp dport 22"
        );
        assert_eq!(
            normalize_set_families("ip daddr . tcp dport != @p", &sets),
            "ip6 daddr . tcp dport != @p"
        );
        // no set: untouched, byte for byte
        assert_eq!(
            normalize_set_families("ip saddr 10.0.0.1  ", &sets),
            "ip saddr 10.0.0.1  "
        );
    }

    #[test]
    fn set_keys_infer_and_elements_lower() {
        let s = set(&[], &["10.0.0.0/24", "10.0.1.1-10.0.1.9", "10.0.2.2"]);
        assert_eq!(set_key("s", &s).unwrap(), [SetKey::Ipv4]);
        let l = lower_set("s", &s).unwrap();
        assert!(l.interval, "a prefix or a range needs an interval set");
        assert_eq!(l.elements.len(), 3);
        assert!(matches!(l.key_type, SetKeyType::Ipv4Addr));

        let c = set(&[], &["10.0.1.10 . 22", "10.0.1.0/24 . 8000-8999"]);
        assert_eq!(set_key("c", &c).unwrap(), [SetKey::Ipv4, SetKey::Port]);
        let l = lower_set("c", &c).unwrap();
        assert!(l.interval);
        assert!(matches!(l.key_type, SetKeyType::Concat(ref k) if k.len() == 2));

        let p = lower_set("p", &set(&[], &["80", "443"])).unwrap();
        assert!(!p.interval && matches!(p.key_type, SetKeyType::InetService));
    }

    #[test]
    fn bad_sets_are_refused() {
        for (s, want) in [
            (set(&[SetKey::Ipv4], &["fd00::1"]), "not an IPv4"),
            (set(&[SetKey::Port], &["9-1"]), "starts after it ends"),
            (set(&[SetKey::Mark], &["1-2"]), "cannot hold ranges"),
            (
                set(&[SetKey::Ipv4, SetKey::Port], &["10.0.0.1"]),
                "has 1 part(s), the key has 2",
            ),
            (set(&[], &[]), "no key and no element"),
            (
                set(&[SetKey::Ipv4, SetKey::Mark], &["10.0.0.0/24 . 5"]),
                "concatenated set with a mark",
            ),
            (
                FirewallSet {
                    dynamic: true,
                    ..set(&[SetKey::Ipv4], &["10.0.0.0/8"])
                },
                "dynamic set cannot hold prefixes",
            ),
        ] {
            let err = lower_set("s", &s).err().unwrap_or_default();
            assert!(err.contains(want), "{s:?}: {err}");
        }
    }

    #[test]
    fn set_refs_must_exist_and_match_the_key() {
        let mut sets = BTreeMap::new();
        sets.insert("b".to_string(), set(&[SetKey::Ipv4], &["10.0.0.1"]));
        let probs = |expr: &str| {
            check_set_refs(&parse_match(expr).unwrap(), &sets)
                .into_iter()
                .map(|(p, _)| p)
                .collect::<Vec<_>>()
        };
        assert!(probs("ip saddr @b").is_empty());
        assert_eq!(probs("ip saddr @nope"), [SetRefProblem::Undeclared]);
        assert_eq!(probs("tcp dport @b"), [SetRefProblem::WrongKey]);
        assert_eq!(probs("ip saddr . tcp dport @b"), [SetRefProblem::WrongKey]);
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
