//! Pure planner: a node's firewall + NAT → `NftablesConfig` (Plan 158a).

use crate::error::{Error, Result};

/// Name of the nftables table that nlink-lab owns on every
/// node carrying firewall / NAT rules. Rules outside this
/// table (or rules in this table without an `nlink-lab/`
/// USERDATA-keyed comment) are treated as foreign and left
/// alone by the reconcile path.
pub(crate) const NLINK_LAB_TABLE: &str = "nlink-lab";

/// Build the declarative [`NftablesConfig`] for a node's
/// firewall + NAT rules. Plan 158a.
///
/// The single resulting config covers both firewall (input +
/// forward chains, filter type) and NAT (prerouting +
/// postrouting chains, nat type) under the shared
/// `nlink-lab` table. `NftablesDiff::apply` then commits
/// every chain/rule/table mutation in one atomic kernel
/// batch.
///
/// Each rule carries a stable
/// `nlink-lab/{fw,nat}/<chain>/<idx>` USERDATA key (the
/// `"nlink:"` prefix is auto-prepended by the library). Stable
/// keys make idempotent re-apply produce zero kernel ops and
/// in-place edits (e.g. `dport 80` → `dport 8080`) replace the
/// rule body without losing its position.
///
/// `match_expr` strings are validated up-front so the
/// builder closures can safely `.expect()` on lowering; an
/// invalid expression here surfaces as `Err` before any
/// kernel I/O.
pub(crate) fn topology_to_nftables_config(
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
) -> Result<nlink::netlink::nftables::config::NftablesConfig> {
    use crate::types::NatAction;
    use nlink::netlink::nftables::config::NftablesConfig;
    use nlink::netlink::nftables::types::{ChainType, Family, Hook, Policy, Priority, Rule};

    // Pre-validate every firewall rule's match_expr so the
    // closure shape inside .rule_keyed(...) can call
    // apply_match_expr without an error escape hatch.
    if let Some(fw) = fw {
        for fw_rule in &fw.rules {
            let expr = fw_rule.match_expr.as_deref().unwrap_or("");
            if !expr.is_empty() {
                let probe = Rule::new(NLINK_LAB_TABLE, "input").family(Family::Inet);
                let _ = apply_match_expr(probe, expr)?;
            }
        }
    }

    let mut cfg = NftablesConfig::new();

    // Decide which chains we actually need to declare. NAT
    // chains are only present when at least one NAT rule
    // demands them; firewall chains follow the same rule for
    // consistency.
    let want_fw = fw.is_some();
    let want_nat = nat.is_some_and(|n| !n.rules.is_empty());
    if !want_fw && !want_nat {
        // Caller-side guard normally prevents this, but
        // keeping the cfg empty here means apply has nothing
        // to do — diff returns the empty set and apply is a
        // no-op.
        return Ok(cfg);
    }

    let policy = match fw.and_then(|f| f.policy.as_deref()) {
        Some("drop") => Policy::Drop,
        _ => Policy::Accept,
    };

    let fw_rules = fw.map(|f| f.rules.as_slice()).unwrap_or(&[]);
    let nat_rules = nat.map(|n| n.rules.as_slice()).unwrap_or(&[]);

    cfg = cfg.table(NLINK_LAB_TABLE, Family::Inet, |mut t| {
        if want_fw {
            t = t
                .chain("input", |c| {
                    c.hook(Hook::Input)
                        .priority(Priority::Filter)
                        .chain_type(ChainType::Filter)
                        .policy(policy)
                })
                .chain("forward", |c| {
                    c.hook(Hook::Forward)
                        .priority(Priority::Filter)
                        .chain_type(ChainType::Filter)
                        .policy(policy)
                });

            for (idx, fw_rule) in fw_rules.iter().enumerate() {
                let action = fw_rule.action.as_deref().unwrap_or("accept").to_string();
                let match_expr = fw_rule.match_expr.clone().unwrap_or_default();
                let key = format!("nlink-lab/fw/input/{idx}");
                t = t.rule_keyed("input", &key, move |mut r| {
                    if !match_expr.is_empty() {
                        // Pre-validation above guarantees this
                        // can't fail.
                        r = apply_match_expr(r, &match_expr)
                            .expect("validated match_expr must lower");
                    }
                    match action.as_str() {
                        "drop" => r.drop(),
                        _ => r.accept(),
                    }
                });
            }
        }

        if want_nat {
            t = t
                .chain("prerouting", |c| {
                    c.hook(Hook::Prerouting)
                        .priority(Priority::DstNat)
                        .chain_type(ChainType::Nat)
                })
                .chain("postrouting", |c| {
                    c.hook(Hook::Postrouting)
                        .priority(Priority::SrcNat)
                        .chain_type(ChainType::Nat)
                });

            for (idx, nat_rule) in nat_rules.iter().enumerate() {
                let rule_clone = nat_rule.clone();
                match nat_rule.action {
                    NatAction::Masquerade => {
                        let key = format!("nlink-lab/nat/postrouting/{idx}/masq");
                        t = t.rule_keyed("postrouting", &key, move |mut r| {
                            if let Some(src) = &rule_clone.src {
                                let (addr, prefix) =
                                    parse_addr_match(src).expect("validated NAT CIDR must parse");
                                r = match_addr(r, Dir::Saddr, addr, prefix);
                            }
                            r.masquerade()
                        });
                    }
                    NatAction::Snat => {
                        let key = format!("nlink-lab/nat/postrouting/{idx}/snat");
                        t = t.rule_keyed("postrouting", &key, move |mut r| {
                            if let Some(src) = &rule_clone.src {
                                let (addr, prefix) =
                                    parse_addr_match(src).expect("validated NAT CIDR must parse");
                                r = match_addr(r, Dir::Saddr, addr, prefix);
                            }
                            if let Some(target) = &rule_clone.target {
                                let addr: std::net::IpAddr =
                                    target.parse().expect("validated NAT target must parse");
                                r = nat_target(r, NatAction::Snat, addr, None);
                            }
                            r
                        });
                    }
                    NatAction::Dnat => {
                        let key = format!("nlink-lab/nat/prerouting/{idx}/dnat");
                        t = t.rule_keyed("prerouting", &key, move |mut r| {
                            if let Some(dst) = &rule_clone.dst {
                                let (addr, prefix) =
                                    parse_addr_match(dst).expect("validated NAT CIDR must parse");
                                r = match_addr(r, Dir::Daddr, addr, prefix);
                            }
                            if let Some(target) = &rule_clone.target {
                                let addr: std::net::IpAddr =
                                    target.parse().expect("validated NAT target must parse");
                                r = nat_target(r, NatAction::Dnat, addr, rule_clone.target_port);
                            }
                            r
                        });
                    }
                    NatAction::Translate => {
                        unreachable!("translate rules should be expanded during lowering");
                    }
                }
            }
        }

        t
    });

    Ok(cfg)
}

/// Pre-validate every NAT rule's CIDR / target literals so
/// the [`topology_to_nftables_config`] closures can rely on
/// `.expect()`. Surfaces the offending value in the error.
pub(crate) fn validate_nat_rule_literals(nat: &crate::types::NatConfig) -> Result<()> {
    for nat_rule in &nat.rules {
        let mut match_family: Option<(&str, &str, bool)> = None;
        if let Some(src) = &nat_rule.src {
            let (ip, _) = parse_addr_match(src).map_err(|e| {
                Error::deploy_failed(format!("invalid src CIDR '{src}' in NAT rule: {e}"))
            })?;
            match_family = Some(("src", src, ip.is_ipv6()));
        }
        if let Some(dst) = &nat_rule.dst {
            let (ip, _) = parse_addr_match(dst).map_err(|e| {
                Error::deploy_failed(format!("invalid dst CIDR '{dst}' in NAT rule: {e}"))
            })?;
            match_family = Some(("dst", dst, ip.is_ipv6()));
        }
        if let Some(target) = &nat_rule.target {
            let ip = target
                .parse::<std::net::IpAddr>()
                .map_err(|e| Error::deploy_failed(format!("invalid NAT target '{target}': {e}")))?;
            if let Some((field, value, v6)) = match_family
                && v6 != ip.is_ipv6()
            {
                return Err(Error::deploy_failed(format!(
                    "NAT rule {field} '{value}' ({}) and target '{target}' ({}) are different address families",
                    family_name(v6),
                    family_name(ip.is_ipv6())
                )));
            }
        }
    }
    Ok(())
}

fn family_name(v6: bool) -> &'static str {
    if v6 { "IPv6" } else { "IPv4" }
}

/// Which packet address a match applies to.
#[derive(Clone, Copy)]
enum Dir {
    Saddr,
    Daddr,
}

/// Add a source/destination address match of the right family.
fn match_addr(
    rule: nlink::netlink::nftables::types::Rule,
    dir: Dir,
    ip: std::net::IpAddr,
    prefix: u8,
) -> nlink::netlink::nftables::types::Rule {
    match (dir, ip) {
        (Dir::Saddr, std::net::IpAddr::V4(a)) => rule.match_saddr_v4(a, prefix),
        (Dir::Saddr, std::net::IpAddr::V6(a)) => rule.match_saddr_v6(a, prefix),
        (Dir::Daddr, std::net::IpAddr::V4(a)) => rule.match_daddr_v4(a, prefix),
        (Dir::Daddr, std::net::IpAddr::V6(a)) => rule.match_daddr_v6(a, prefix),
    }
}

/// Add the snat/dnat target expression of the right family.
fn nat_target(
    rule: nlink::netlink::nftables::types::Rule,
    action: crate::types::NatAction,
    ip: std::net::IpAddr,
    port: Option<u16>,
) -> nlink::netlink::nftables::types::Rule {
    use crate::types::NatAction;
    match (action, ip) {
        (NatAction::Snat, std::net::IpAddr::V4(a)) => rule.snat(a, port),
        (NatAction::Snat, std::net::IpAddr::V6(a)) => rule.snat_v6(a, port),
        (NatAction::Dnat, std::net::IpAddr::V4(a)) => rule.dnat(a, port),
        (NatAction::Dnat, std::net::IpAddr::V6(a)) => rule.dnat_v6(a, port),
        (NatAction::Masquerade | NatAction::Translate, _) => rule,
    }
}

/// Parse a (possibly compound) match expression and apply it to an nftables rule.
///
/// The expression may contain multiple space-separated clauses such as
/// `"ip saddr 10.0.0.0/8 tcp dport 80"`. Each clause is applied in order.
pub(crate) fn apply_match_expr(
    mut rule: nlink::netlink::nftables::types::Rule,
    expr: &str,
) -> Result<nlink::netlink::nftables::types::Rule> {
    use nlink::netlink::nftables::types::CtState;

    let expr = expr.trim();
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    let mut i = 0;

    while i < tokens.len() {
        match tokens[i] {
            // ip saddr <cidr> / ip daddr <cidr> / ip6 saddr <cidr> / ip6 daddr <cidr>
            keyword @ ("ip" | "ip6")
                if i + 2 < tokens.len()
                    && (tokens[i + 1] == "saddr" || tokens[i + 1] == "daddr") =>
            {
                let cidr = tokens[i + 2];
                let want_v6 = keyword == "ip6";
                let family = if want_v6 { "IPv6" } else { "IPv4" };
                let (addr, prefix) = parse_addr_match(cidr).map_err(|e| {
                    Error::deploy_failed(format!(
                        "invalid {family} CIDR '{cidr}' in firewall rule: {e}"
                    ))
                })?;
                if addr.is_ipv6() != want_v6 {
                    let other = if want_v6 { "ip" } else { "ip6" };
                    return Err(Error::deploy_failed(format!(
                        "'{keyword} {}' needs an {family} address, got '{cidr}' (use '{other} {}')",
                        tokens[i + 1],
                        tokens[i + 1]
                    )));
                }
                let dir = if tokens[i + 1] == "saddr" {
                    Dir::Saddr
                } else {
                    Dir::Daddr
                };
                rule = match_addr(rule, dir, addr, prefix);
                i += 3;
            }
            // tcp dport/sport <port>
            "tcp"
                if i + 2 < tokens.len()
                    && (tokens[i + 1] == "dport" || tokens[i + 1] == "sport") =>
            {
                let port: u16 = tokens[i + 2].parse().map_err(|_| {
                    Error::deploy_failed(format!(
                        "invalid port '{}' in firewall rule",
                        tokens[i + 2]
                    ))
                })?;
                rule = if tokens[i + 1] == "dport" {
                    rule.match_tcp_dport(port)
                } else {
                    rule.match_tcp_sport(port)
                };
                i += 3;
            }
            // udp dport/sport <port>
            "udp"
                if i + 2 < tokens.len()
                    && (tokens[i + 1] == "dport" || tokens[i + 1] == "sport") =>
            {
                let port: u16 = tokens[i + 2].parse().map_err(|_| {
                    Error::deploy_failed(format!(
                        "invalid port '{}' in firewall rule",
                        tokens[i + 2]
                    ))
                })?;
                rule = if tokens[i + 1] == "dport" {
                    rule.match_udp_dport(port)
                } else {
                    rule.match_udp_sport(port)
                };
                i += 3;
            }
            // icmp type <N>
            "icmp" if i + 2 < tokens.len() && tokens[i + 1] == "type" => {
                let icmp_type: u8 = tokens[i + 2].parse().map_err(|_| {
                    Error::deploy_failed(format!(
                        "invalid ICMP type '{}' in firewall rule",
                        tokens[i + 2]
                    ))
                })?;
                rule = rule.match_icmp_type(icmp_type);
                i += 3;
            }
            // icmpv6 type <N>
            "icmpv6" if i + 2 < tokens.len() && tokens[i + 1] == "type" => {
                let icmp_type: u8 = tokens[i + 2].parse().map_err(|_| {
                    Error::deploy_failed(format!(
                        "invalid ICMPv6 type '{}' in firewall rule",
                        tokens[i + 2]
                    ))
                })?;
                rule = rule.match_icmpv6_type(icmp_type);
                i += 3;
            }
            // mark <N>
            "mark" if i + 1 < tokens.len() => {
                let mark: u32 = tokens[i + 1].parse().map_err(|_| {
                    Error::deploy_failed(format!(
                        "invalid mark '{}' in firewall rule",
                        tokens[i + 1]
                    ))
                })?;
                rule = rule.match_mark(mark);
                i += 2;
            }
            // ct state <states>
            "ct" if i + 2 < tokens.len() && tokens[i + 1] == "state" => {
                let states = tokens[i + 2];
                let mut ct = CtState::empty();
                for state in states.split(',') {
                    match state.trim() {
                        "established" => ct |= CtState::ESTABLISHED,
                        "related" => ct |= CtState::RELATED,
                        "new" => ct |= CtState::NEW,
                        "invalid" => ct |= CtState::INVALID,
                        _ => {}
                    }
                }
                rule = rule.match_ct_state(ct);
                i += 3;
            }
            other => {
                return Err(Error::deploy_failed(format!(
                    "unsupported firewall match token '{other}' in expression: '{expr}'. \
                     Supported: 'ip saddr/daddr CIDR', 'ip6 saddr/daddr CIDR', 'ct state ...', \
                     'tcp dport/sport N', 'udp dport/sport N', 'icmp type N', 'icmpv6 type N', \
                     'mark N'"
                )));
            }
        }
    }

    Ok(rule)
}

/// Parse an address match operand: a CIDR (`10.0.1.0/24`, `fd00::/64`)
/// or a bare address (host prefix), either family.
pub(crate) fn parse_addr_match(s: &str) -> Result<(std::net::IpAddr, u8)> {
    crate::helpers::parse_ip_or_cidr(s)
}
