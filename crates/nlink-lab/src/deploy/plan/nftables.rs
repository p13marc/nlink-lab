//! Pure planner: a node's firewall + NAT → `NftablesConfig` (Plan 158a).

use crate::error::{Error, Result};

/// Name of the nftables table that nlink-lab owns on every
/// node carrying firewall / NAT rules. Rules outside this
/// table (or rules in this table without an `nlink-lab/`
/// USERDATA-keyed comment) are treated as foreign and left
/// alone by the reconcile path.
pub(crate) const NLINK_LAB_TABLE: &str = "nlink-lab";

/// Build the declarative [`NftablesConfig`] for a node's
/// firewall + NAT rules. Plan 158a, #164.
///
/// The single resulting config covers the firewall (input and forward
/// chains always, output when declared; filter type) and NAT (prerouting +
/// postrouting chains, nat type) under the shared `nlink-lab` table.
/// `NftablesDiff::apply` then commits every chain/rule/table mutation in one
/// atomic kernel batch.
///
/// Each rule carries a stable `nlink-lab/{fw,nat}/<chain>/<idx>` USERDATA
/// key (the `"nlink:"` prefix is auto-prepended by the library). Stable keys
/// make idempotent re-apply produce zero kernel ops and in-place edits
/// (e.g. `dport 80` → `dport 8080`) replace the rule body without losing its
/// position.
///
/// Every firewall rule is built before the config, so a bad match, verdict
/// or policy surfaces as `Err` before any kernel I/O.
pub(crate) fn topology_to_nftables_config(
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
) -> Result<nlink::netlink::nftables::config::NftablesConfig> {
    use crate::types::NatAction;
    use nlink::netlink::nftables::config::NftablesConfig;
    use nlink::netlink::nftables::types::{ChainType, Family, Hook, Priority};

    let mut cfg = NftablesConfig::new();

    // Decide which chains we actually need to declare. NAT
    // chains are only present when at least one NAT rule
    // demands them; firewall chains follow the same rule for
    // consistency.
    let want_nat = nat.is_some_and(|n| !n.rules.is_empty());
    if fw.is_none() && !want_nat {
        // Caller-side guard normally prevents this, but
        // keeping the cfg empty here means apply has nothing
        // to do — diff returns the empty set and apply is a
        // no-op.
        return Ok(cfg);
    }

    let mut fw_chains = Vec::new();
    let mut fw_rules = Vec::new();
    let mut fw_sets = Vec::new();
    let mut fw_counters = Vec::new();
    if let Some(fw) = fw {
        fw_counters = crate::firewall::counter_names(fw);
        for (name, set) in &fw.sets {
            let lowered = crate::firewall::lower_set(name, set).map_err(Error::deploy_failed)?;
            fw_sets.push((name.clone(), lowered));
        }
        for (chain, policy, rules) in crate::firewall::chains(fw) {
            let policy = crate::firewall::policy(policy).map_err(Error::deploy_failed)?;
            fw_chains.push((chain, policy));
            for (idx, rule) in rules.iter().enumerate() {
                let built =
                    crate::firewall::lower_rule(NLINK_LAB_TABLE, chain, rule).map_err(|e| {
                        Error::deploy_failed(format!("{} rule {idx}: {e}", chain.name()))
                    })?;
                fw_rules.push((chain, format!("nlink-lab/fw/{}/{idx}", chain.name()), built));
            }
        }
    }

    let nat_rules = nat.map(|n| n.rules.as_slice()).unwrap_or(&[]);

    cfg = cfg.table(NLINK_LAB_TABLE, Family::Inet, |mut t| {
        // Sets before the rules that look them up (nlink orders the batch
        // the same way).
        for (name, set) in fw_sets {
            t = t.set(name, move |mut s| {
                s = s.key_type(set.key_type).elements(set.elements);
                if set.interval {
                    s = s.interval();
                }
                if let Some(n) = set.size {
                    s = s.size(n);
                }
                if let Some(d) = set.timeout {
                    s = s.timeout(d);
                }
                if set.dynamic {
                    s = s.dynamic();
                }
                s
            });
        }
        // Named counters, declared by use (#166).
        for name in fw_counters {
            t = t.object(name, nlink::netlink::nftables::ObjectConfig::Counter);
        }
        for (chain, policy) in fw_chains {
            t = t.chain(chain.name(), |c| {
                c.hook(chain.hook())
                    .priority(Priority::Filter)
                    .chain_type(ChainType::Filter)
                    .policy(policy)
            });
        }
        for (chain, key, rule) in fw_rules {
            t = t.rule_keyed(chain.name(), &key, move |_| rule);
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
                            nat_matches(r, &rule_clone).masquerade()
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
                            r = nat_matches(r, &rule_clone);
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
                            r = nat_matches(r, &rule_clone);
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
        if let Some(msg) = nat_match_problem(nat_rule) {
            return Err(Error::deploy_failed(msg));
        }
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

/// What is wrong with a NAT rule's interface or port match, if anything.
pub(crate) fn nat_match_problem(nat: &crate::types::NatRule) -> Option<String> {
    use crate::types::NatAction;
    if nat.dport.is_some() && nat.proto.is_none() {
        return Some("a NAT `dport` needs its protocol (`tcp dport 80`)".to_string());
    }
    if nat.dport.is_some() && nat.action != NatAction::Dnat {
        return Some("only dnat matches a destination port".to_string());
    }
    if nat.iif.is_some() && nat.action != NatAction::Dnat {
        return Some(
            "`iif` applies to dnat (prerouting): masquerade and snat see `oif`".to_string(),
        );
    }
    if nat.oif.is_some() && nat.action == NatAction::Dnat {
        return Some(
            "`oif` applies to masquerade and snat (postrouting): dnat sees `iif`".to_string(),
        );
    }
    for name in [&nat.iif, &nat.oif].into_iter().flatten() {
        if name.len() > 15 {
            return Some(format!(
                "interface name '{name}' is longer than 15 characters"
            ));
        }
    }
    None
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

/// A NAT rule's interface and port matches (#167), after its address:
/// nothing for a rule that has none, so existing rules keep their bytes.
fn nat_matches(
    mut rule: nlink::netlink::nftables::types::Rule,
    nat: &crate::types::NatRule,
) -> nlink::netlink::nftables::types::Rule {
    if let Some(iif) = &nat.iif {
        rule = rule.match_iif(iif);
    }
    if let Some(oif) = &nat.oif {
        rule = rule.match_oif(oif);
    }
    match (nat.proto, nat.dport) {
        (Some(crate::types::NatProto::Tcp), Some(p)) => rule.match_tcp_dport(p),
        (Some(crate::types::NatProto::Udp), Some(p)) => rule.match_udp_dport(p),
        (Some(crate::types::NatProto::Tcp), None) => rule.match_l4proto(6),
        (Some(crate::types::NatProto::Udp), None) => rule.match_l4proto(17),
        (None, _) => rule,
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
/// `"ip saddr 10.0.0.0/8 tcp dport 80"`; see [`crate::firewall`] for the
/// grammar. Kept for the match tests; the planner goes through
/// [`crate::firewall::lower_rule`].
#[cfg(test)]
pub(crate) fn apply_match_expr(
    mut rule: nlink::netlink::nftables::types::Rule,
    expr: &str,
) -> Result<nlink::netlink::nftables::types::Rule> {
    for clause in crate::firewall::parse_match(expr).map_err(Error::deploy_failed)? {
        rule = clause.lower(rule);
    }
    Ok(rule)
}

/// Parse an address match operand: a CIDR (`10.0.1.0/24`, `fd00::/64`)
/// or a bare address (host prefix), either family.
pub(crate) fn parse_addr_match(s: &str) -> Result<(std::net::IpAddr, u8)> {
    crate::helpers::parse_ip_or_cidr(s)
}

#[cfg(test)]
mod golden {
    //! Every example's planned nftables config, pinned. A change here is
    //! a change to what deployed labs run: the next `apply` after an
    //! upgrade replaces each rule whose body moved. Regenerate with
    //! `UPDATE_GOLDEN=1 cargo test -p nlink-lab --lib nftables_golden`
    //! and justify every line of the diff.

    use std::fmt::Write;

    const FIXTURE: &str = "src/deploy/plan/testdata/nftables-golden.txt";

    fn render_all() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in [root.clone(), root.join("cookbook"), root.join("imports")] {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let p = entry.unwrap().path();
                if p.extension().is_some_and(|e| e == "nll") {
                    files.push(p);
                }
            }
        }
        files.sort();
        let mut out = String::new();
        for file in files {
            // parametric modules need --set; they are planned where imported
            let Ok(topo) = crate::parser::parse_file(&file) else {
                continue;
            };
            let name = file.strip_prefix(&root).unwrap().display().to_string();
            for (node_name, node) in &topo.nodes {
                let fw = topo.effective_firewall(node);
                let nat = node.nat.as_ref();
                if fw.is_none() && nat.is_none() {
                    continue;
                }
                let cfg = super::topology_to_nftables_config(fw.as_deref(), nat).unwrap();
                for table in cfg.tables() {
                    for chain in table.chains() {
                        writeln!(
                            out,
                            "{name} {node_name} chain {} policy={:?}",
                            chain.name(),
                            chain.policy()
                        )
                        .unwrap();
                    }
                    for rule in table.rules() {
                        writeln!(
                            out,
                            "{name} {node_name} rule {} {} {:?}",
                            rule.chain(),
                            rule.handle_key().unwrap_or("-"),
                            rule.exprs()
                        )
                        .unwrap();
                    }
                }
            }
        }
        out
    }

    #[test]
    fn nftables_golden() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        let got = render_all();
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        if got != want {
            let diff: Vec<String> = want
                .lines()
                .filter(|l| !got.lines().any(|g| g == *l))
                .map(|l| format!("- {l}"))
                .chain(
                    got.lines()
                        .filter(|l| !want.lines().any(|w| w == *l))
                        .map(|l| format!("+ {l}")),
                )
                .collect();
            panic!(
                "planned nftables config changed for the examples ({FIXTURE}):\n{}",
                diff.join("\n")
            );
        }
    }
}
