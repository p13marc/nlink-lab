//! The NLL firewall's match grammar and its lowering to nftables (#164).
//!
//! A rule's `match` is stored as an nft-like string (`ip saddr 10.0.0.0/8
//! tcp dport 22`): it is what the NLL parser produces from `src 10.0.0.0/8
//! tcp dport 22`, and what topology JSON, state files and `apply --check`
//! carry. This module is the one place that reads it — the planner lowers it
//! to an nlink [`RuleSpec`] (nlink 0.33's rules as data: one `Match` per
//! clause, a `Stmt` per statement, a `Verdict`), the validator checks it,
//! `render` turns it back into NLL — where each used to carry its own
//! tokenizer.

use std::net::IpAddr;

use std::collections::BTreeMap;

use nlink::netlink::nftables::{
    CtState, FibFlags, FibResult, Field as NftField, FilterHook, IcmpxCode, Match, MatchOp,
    ObjectType, PacketField, Policy, Reject, RuleSpec, SetElement, SetKeyType, Stmt, Value,
    Verdict,
};

use crate::types::{FirewallConfig, FirewallRule, FirewallSet, RejectWith, SetKey};

/// An address match's value: the address itself for a full-length prefix,
/// as nlink's builder writes it.
pub(crate) fn addr_value(addr: IpAddr, prefix: u8) -> Value {
    let width = if addr.is_ipv4() { 32 } else { 128 };
    if prefix >= width {
        Value::Addr(addr)
    } else {
        Value::Prefix(addr, prefix)
    }
}

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

    pub(crate) fn hook(self) -> FilterHook {
        match self {
            Chain::Input => FilterHook::Input,
            Chain::Forward => FilterHook::Forward,
            Chain::Output => FilterHook::Output,
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
    /// `mark LO-HI` (#189).
    MarkRange { lo: u32, hi: u32, text: String },
    /// `ct mark V`.
    CtMark { value: u32, text: String },
    /// `iifname|oifname [!=] NAME`.
    Iface { out: bool, neg: bool, name: String },
    /// `rpf`, stored as nft's `fib saddr . iif oif missing` (#189): the
    /// packet's source is not routed back out of the interface it came in
    /// on — a spoofed source, on a router.
    Rpf,
}

/// How `rpf` is stored in a match expression: what nft writes for it.
const RPF: [&str; 6] = ["fib", "saddr", ".", "iif", "oif", "missing"];

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
        if toks.get(i..i + RPF.len()) == Some(&RPF[..]) {
            out.push(Clause::Rpf);
            i += RPF.len();
            continue;
        }
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
                if let Some((lo, hi)) = text.split_once('-') {
                    let (lo, hi) = (
                        parse_u32(lo).ok_or_else(bad)?,
                        parse_u32(hi).ok_or_else(bad)?,
                    );
                    if lo > hi {
                        return Err(format!(
                            "mark range '{text}' in firewall rule starts after it ends"
                        ));
                    }
                    out.push(Clause::MarkRange {
                        lo,
                        hi,
                        text: text.to_string(),
                    });
                    i += 2;
                    continue;
                }
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
            Clause::Mark { text, .. } | Clause::MarkRange { text, .. } => format!("mark {text}"),
            Clause::CtMark { text, .. } => format!("ct mark {text}"),
            Clause::Iface { out, neg, name } => {
                format!("{}{} {name}", if *out { "oif" } else { "iif" }, ne(*neg))
            }
            Clause::Rpf => "rpf".to_string(),
        }
    }

    /// The clause as an nlink match. Each is the item nlink's builder
    /// method for the same form appends, so a rule keeps the bytes it
    /// always had — except a port range, which is one `range eq`, as nft
    /// writes it, where nlink-lab 0.14 wrote a `>=`/`<=` pair (#299).
    pub(crate) fn item(&self) -> Match {
        let op = |neg: bool| if neg { MatchOp::Ne } else { MatchOp::Eq };
        match self {
            Clause::InSet { field, neg, set } => {
                Match::new(field.packet_field(), op(*neg), Value::set(set))
            }
            Clause::Concat { fields, neg, set } => Match::new(
                NftField::Concat(fields.iter().map(|f| f.packet_field()).collect()),
                op(*neg),
                Value::set(set),
            ),
            Clause::Addr {
                src,
                neg,
                addr,
                prefix,
                ..
            } => {
                let field = Field::Addr {
                    v6: addr.is_ipv6(),
                    src: *src,
                };
                Match::new(field.packet_field(), op(*neg), addr_value(*addr, *prefix))
            }
            Clause::Port {
                tcp,
                dport,
                neg,
                lo,
                hi,
            } => {
                let field = Field::Port {
                    tcp: *tcp,
                    dport: *dport,
                };
                let value = if lo == hi {
                    Value::from(*lo)
                } else {
                    Value::range(*lo, *hi)
                };
                Match::new(field.packet_field(), op(*neg), value)
            }
            Clause::Icmp { v6: false, ty } => Match::eq(NftField::IcmpType, *ty),
            Clause::Icmp { v6: true, ty } => Match::eq(NftField::Icmp6Type, *ty),
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
                Match::eq(NftField::CtState, ct)
            }
            Clause::Mark {
                value, mask: None, ..
            } => Match::eq(NftField::Mark, *value),
            Clause::Mark {
                value,
                mask: Some(mask),
                ..
            } => Match::eq(
                NftField::Mark,
                Value::Masked {
                    value: (*value).into(),
                    mask: (*mask).into(),
                },
            ),
            Clause::MarkRange { lo, hi, .. } => Match::eq(NftField::Mark, Value::range(*lo, *hi)),
            Clause::CtMark { value, .. } => Match::eq(NftField::CtMark, *value),
            Clause::Iface { out, neg, name } => {
                let field = if *out {
                    NftField::Oifname
                } else {
                    NftField::Iifname
                };
                Match::new(field, op(*neg), name.as_str())
            }
            Clause::Rpf => Match::eq(
                NftField::Fib {
                    flags: FibFlags::SADDR | FibFlags::IIF,
                    result: FibResult::Oif,
                },
                false,
            ),
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
            Clause::Mark { text, .. } | Clause::MarkRange { text, .. } => format!("mark {text}"),
            Clause::CtMark { text, .. } => format!("ct mark {text}"),
            Clause::Iface { out, neg, name } => format!(
                "{}{} {name}",
                if *out { "oifname" } else { "iifname" },
                ne(*neg)
            ),
            Clause::Rpf => RPF.join(" "),
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
/// A log level's nlink value; the names are [`crate::types::LOG_LEVELS`].
fn log_level(name: &str) -> nlink::netlink::nftables::LogLevel {
    use nlink::netlink::nftables::LogLevel;
    match name {
        "emerg" => LogLevel::Emerg,
        "alert" => LogLevel::Alert,
        "crit" => LogLevel::Crit,
        "err" => LogLevel::Err,
        "notice" => LogLevel::Notice,
        "info" => LogLevel::Info,
        "debug" => LogLevel::Debug,
        _ => LogLevel::Warning,
    }
}

pub(crate) fn check_statement_context(rule: &FirewallRule, chain: Chain) -> Option<String> {
    if chain == Chain::Output
        && rule
            .match_expr
            .as_deref()
            .is_some_and(|m| m.contains(&RPF.join(" ")))
    {
        return Some(
            "`rpf` looks the source up with the input interface: input and forward chains only"
                .to_string(),
        );
    }
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

/// The body of one firewall rule: its matches, its statements and its
/// verdict, as an nlink [`RuleSpec`] the planner declares in its chain.
pub(crate) fn lower_rule(rule: &FirewallRule) -> Result<RuleSpec, String> {
    let clauses = parse_match(rule.match_expr.as_deref().unwrap_or(""))?;
    check_verdict(rule, &clauses)?;
    check_statements(rule)?;
    let mut spec = RuleSpec::new();
    for c in &clauses {
        spec.push(c.item());
    }
    if rule.clamp_mss.is_some() {
        // the MSS option rides on SYNs: `tcp flags syn / syn,rst`
        use nlink::netlink::nftables::TcpFlags;
        spec.push(Match::eq(
            NftField::TcpFlags,
            Value::Masked {
                value: TcpFlags::SYN.bits().into(),
                mask: (TcpFlags::SYN | TcpFlags::RST).bits().into(),
            },
        ));
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
        let mut e = if l.bytes {
            LimitExpr::bytes(nlink::Bytes::new(l.rate), unit)
        } else {
            LimitExpr::packets(l.rate, unit)
        };
        if let Some(b) = l.burst {
            e = e.burst(b);
        }
        if l.over {
            e = e.over();
        }
        spec.push(Stmt::Limit(e));
    }
    if rule.counter {
        spec.push(Stmt::Counter);
    }
    if let Some(name) = &rule.counter_name {
        spec.push(Stmt::Objref {
            object_type: ObjectType::Counter,
            name: name.clone(),
        });
    }
    if let Some(l) = &rule.log {
        let mut e = nlink::netlink::nftables::LogExpr::new();
        if let Some(p) = &l.prefix {
            e = e.prefix(p.as_str());
        }
        if let Some(g) = l.group {
            e = e.group(g);
        }
        if let Some(level) = &l.level {
            e = e.level(log_level(level));
        }
        spec.push(Stmt::Log(e));
    }
    if let Some(m) = rule.set_mark {
        spec.push(match m.mask {
            Some(mask) => Stmt::SetMarkMasked {
                value: m.value,
                mask,
            },
            None => Stmt::SetMark(m.value),
        });
    }
    match rule.ct_mark {
        Some(crate::types::CtMarkAction::Set(v)) => spec.push(Stmt::SetCtMark(v)),
        Some(crate::types::CtMarkAction::Save) => spec.push(Stmt::SaveMarkToCt),
        Some(crate::types::CtMarkAction::Restore) => spec.push(Stmt::RestoreMarkFromCt),
        None => {}
    }
    if let Some(h) = &rule.classify {
        let handle: nlink::TcHandle = h
            .parse()
            .map_err(|_| format!("classify '{h}' is not a tc class handle (like 1:10)"))?;
        spec.push(Stmt::SetPriority(handle));
    }
    match rule.clamp_mss {
        Some(crate::types::MssClamp::Pmtu) => spec.push(Stmt::ClampTcpMssToPmtu),
        Some(crate::types::MssClamp::Mss(m)) => spec.push(Stmt::ClampTcpMss(m)),
        None => {}
    }
    match rule.action.as_deref().unwrap_or("accept") {
        "continue" => {}
        "drop" => spec.push(Verdict::Drop),
        "reject" => spec.push(Stmt::Reject(match rule.reject_with {
            None | Some(RejectWith::PortUnreachable) => Reject::Icmpx(IcmpxCode::PortUnreachable),
            Some(RejectWith::TcpReset) => Reject::TcpReset,
            Some(RejectWith::HostUnreachable) => Reject::Icmpx(IcmpxCode::HostUnreachable),
            Some(RejectWith::NoRoute) => Reject::Icmpx(IcmpxCode::NoRoute),
            Some(RejectWith::AdminProhibited) => Reject::Icmpx(IcmpxCode::AdminProhibited),
        })),
        _ => spec.push(Verdict::Accept),
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink::netlink::nftables::{CmpOp, Expr, Family, MetaKey, RangeOp};

    /// What a rule sends: its spec lowered in an inet table, as nlink
    /// declares it.
    fn exprs_of(spec: RuleSpec) -> Vec<Expr> {
        let cfg =
            nlink::netlink::nftables::config::NftablesConfig::new()
                .table("t", Family::Inet, |t| t.push_rule_keyed("input", "k", spec));
        cfg.tables()[0].rules()[0].exprs().expect("the rule lowers")
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
        let r = exprs_of(lower_rule(&rule("tcp dport 22", "reject")).unwrap());
        assert!(matches!(
            r.last(),
            Some(Expr::Reject {
                reject_type: 2,
                icmp_code: 1
            })
        ));
        let mut tcp = rule("tcp dport 22", "reject");
        tcp.reject_with = Some(RejectWith::TcpReset);
        let r = exprs_of(lower_rule(&tcp).unwrap());
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
        assert!(lower_rule(&udp).is_err());
        // `reject with` on an accept, an unknown action
        let mut acc = rule("tcp dport 22", "accept");
        acc.reject_with = Some(RejectWith::NoRoute);
        assert!(lower_rule(&acc).is_err());
        assert!(lower_rule(&rule("", "dorp")).is_err());
        // a bare verdict matches everything
        let r = exprs_of(lower_rule(&rule("", "drop")).unwrap());
        assert_eq!(r.len(), 1);
    }

    /// A port range is one `range eq`, as nft writes it (#299); nlink-lab
    /// 0.14 wrote a `>=`/`<=` pair.
    #[test]
    fn port_range_and_negations_lower_to_the_compares_nft_emits() {
        let spec = lower_rule(&rule("tcp dport 8000-8999", "drop")).unwrap();
        assert_eq!(
            spec.nft_syntax_stateless(Family::Inet).as_deref(),
            Some("tcp dport 8000-8999 drop")
        );
        let r = exprs_of(spec);
        assert!(
            r.iter().any(|e| matches!(
                e,
                Expr::Range { op: RangeOp::Eq, from, to, .. }
                    if from[..] == 8000u16.to_be_bytes() && to[..] == 8999u16.to_be_bytes()
            )),
            "{r:?}"
        );
        assert!(
            !r.iter().any(|e| matches!(
                e,
                Expr::Cmp {
                    op: CmpOp::Gte | CmpOp::Lte,
                    ..
                }
            )),
            "{r:?}"
        );
        let r = exprs_of(lower_rule(&rule("iifname != wan0", "drop")).unwrap());
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

    /// #189: `rpf`, a negated port range, a byte-rate limit and a log
    /// level lower to nlink's expressions; `rpf` is refused in the output
    /// chain, which has no input interface.
    #[test]
    fn rpf_negated_ranges_byte_limits_and_log_levels() {
        assert_eq!(parse_match(&RPF.join(" ")).unwrap(), [Clause::Rpf]);
        assert_eq!(Clause::Rpf.to_nll(), "rpf");
        let r = exprs_of(lower_rule(&rule(&RPF.join(" "), "drop")).unwrap());
        assert!(matches!(r[0], Expr::Fib { .. }), "{r:?}");

        let r = exprs_of(lower_rule(&rule("tcp dport != 8000-8999", "drop")).unwrap());
        assert!(
            r.iter().any(|e| matches!(e, Expr::Range { .. })),
            "a negated range is nlink's range compare: {r:?}"
        );

        let mut limited = rule("", "accept");
        limited.limit = Some(crate::types::FirewallLimit {
            rate: 1024 * 1024,
            per: crate::types::LimitPer::Second,
            burst: Some(64 * 1024),
            over: false,
            bytes: true,
        });
        limited.log = Some(crate::types::FirewallLog {
            level: Some("info".into()),
            ..Default::default()
        });
        let r = exprs_of(lower_rule(&limited).unwrap());
        let shown = format!("{r:?}");
        assert!(
            shown.contains("Bytes") && shown.contains("1048576"),
            "{shown}"
        );
        assert!(shown.contains("Info"), "{shown}");

        let out = rule(&RPF.join(" "), "drop");
        assert!(check_statement_context(&out, Chain::Output).is_some());
        assert!(check_statement_context(&out, Chain::Forward).is_none());
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
