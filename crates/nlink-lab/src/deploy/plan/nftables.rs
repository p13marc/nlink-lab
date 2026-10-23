//! Pure planner: a node's firewall + NAT → `NftablesConfig` (Plan 158a).

use crate::error::{Error, Result};

/// Name of the nftables table that nlink-lab owns on every
/// node carrying firewall / NAT rules. Rules outside this
/// table, and rules in it that carry no `nlink-lab/` key, are
/// foreign and left alone by the reconcile path, together with
/// the chains, sets and objects they use (nlink #459).
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
/// Each rule carries a stable `nlink-lab/{fw,nat}/<chain>/<idx>` key, which
/// nlink keeps in a userdata TLV of its own (since 0.32, nlink #527; `nft
/// list` no longer shows it as a `comment "nlink:…"`). Stable keys
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
    use nlink::netlink::nftables::types::{BaseChain, Family, NatHook, Priority};

    // The rule closures below `.expect()` these literals.
    if let Some(nat) = nat {
        validate_nat_rule_literals(nat)?;
    }

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
                c.base(BaseChain::filter(chain.hook(), Priority::Filter).policy(policy))
            });
        }
        for (chain, key, rule) in fw_rules {
            t = t.rule_keyed(chain.name(), &key, move |_| rule);
        }

        if want_nat {
            t = t
                .chain("prerouting", |c| {
                    c.base(BaseChain::nat(NatHook::Prerouting, Priority::DstNat))
                })
                .chain("postrouting", |c| {
                    c.base(BaseChain::nat(NatHook::Postrouting, Priority::SrcNat))
                });

            for (idx, nat_rule) in nat_rules.iter().enumerate() {
                let rule_clone = nat_rule.clone();
                // A range, a flag or a redirect (#189) is nlink's
                // `NatStatement`. The other rules keep the expressions
                // they always had, so they keep their bytes.
                if let Some((chain, kind)) = nat_statement_rule(nat_rule) {
                    let key = format!("nlink-lab/nat/{chain}/{idx}/{kind}");
                    t = t.rule_keyed(chain, &key, move |mut r| {
                        let (field, dir) = match rule_clone.action {
                            NatAction::Dnat | NatAction::Redirect => (&rule_clone.dst, Dir::Daddr),
                            _ => (&rule_clone.src, Dir::Saddr),
                        };
                        if let Some(cidr) = field {
                            let (addr, prefix) =
                                parse_addr_match(cidr).expect("validated NAT CIDR must parse");
                            r = match_addr(r, dir, addr, prefix);
                        }
                        nat_matches(r, &rule_clone).nat(nat_statement(&rule_clone))
                    });
                    continue;
                }
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
                                // `snat … to ADDR:PORT`: the port was dropped (#203).
                                r = nat_target(r, NatAction::Snat, addr, rule_clone.target_port);
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
                    NatAction::Redirect => unreachable!("a redirect is a NatStatement rule"),
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
        if let (Some(target), Some(end)) = (&nat_rule.target, &nat_rule.target_end) {
            let (first, last) = (
                target.parse::<std::net::IpAddr>(),
                end.parse::<std::net::IpAddr>(),
            );
            match (first, last) {
                (Ok(a), Ok(b)) if a.is_ipv6() != b.is_ipv6() => {
                    return Err(Error::deploy_failed(format!(
                        "NAT range '{target}-{end}' mixes address families"
                    )));
                }
                (Ok(a), Ok(b)) if b < a => {
                    return Err(Error::deploy_failed(format!(
                        "NAT range '{target}-{end}' runs backwards"
                    )));
                }
                (_, Err(e)) => {
                    return Err(Error::deploy_failed(format!(
                        "invalid NAT range end '{end}': {e}"
                    )));
                }
                _ => {}
            }
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
    let prerouting = matches!(nat.action, NatAction::Dnat | NatAction::Redirect);
    if nat.dport.is_some() && !prerouting {
        return Some("only dnat and redirect match a destination port".to_string());
    }
    if nat.iif.is_some() && !prerouting {
        return Some(
            "`iif` applies to dnat and redirect (prerouting): masquerade and snat see `oif`"
                .to_string(),
        );
    }
    if nat.oif.is_some() && prerouting {
        return Some(
            "`oif` applies to masquerade and snat (postrouting): dnat and redirect see `iif`"
                .to_string(),
        );
    }
    // Ranges and redirect (#189).
    if matches!(nat.action, NatAction::Masquerade | NatAction::Redirect) && nat.target.is_some() {
        return Some("masquerade and redirect take a port (`to :3128`), not an address".into());
    }
    if nat.target_end.is_some() && !matches!(nat.action, NatAction::Snat | NatAction::Dnat) {
        return Some("only snat and dnat take an address range".to_string());
    }
    if nat.target_port_end.is_some() && nat.target_port.is_none() {
        return Some("a port range needs its first port".to_string());
    }
    if let (Some(first), Some(last)) = (nat.target_port, nat.target_port_end)
        && last < first
    {
        return Some(format!("port range {first}-{last} runs backwards"));
    }
    if nat.target_port_end.is_some() && nat.action == NatAction::Redirect {
        return Some("redirect takes one port".to_string());
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

/// The chain and key word of a NAT rule built as an nlink `NatStatement`:
/// one with an address or port range, a `random`/`fully-random`/
/// `persistent` flag, a masquerade port range, or a redirect (#189).
/// `None` for the rest, whose expressions predate it.
fn nat_statement_rule(nat: &crate::types::NatRule) -> Option<(&'static str, &'static str)> {
    use crate::types::NatAction;
    let extended = nat.target_end.is_some()
        || nat.target_port_end.is_some()
        || !nat.flags.is_empty()
        || (nat.action == NatAction::Masquerade && nat.target_port.is_some());
    match nat.action {
        NatAction::Redirect => Some(("prerouting", "redirect")),
        NatAction::Dnat if extended => Some(("prerouting", "dnat")),
        NatAction::Snat if extended => Some(("postrouting", "snat")),
        NatAction::Masquerade if extended => Some(("postrouting", "masq")),
        _ => None,
    }
}

/// The `NatStatement` of a rule [`nat_statement_rule`] picked.
fn nat_statement(nat: &crate::types::NatRule) -> nlink::netlink::nftables::NatStatement {
    use crate::types::{NatAction, NatFlag};
    use nlink::netlink::nftables::NatStatement;
    let addr =
        |s: &String| -> std::net::IpAddr { s.parse().expect("validated NAT target must parse") };
    let mut st = match nat.action {
        NatAction::Masquerade => NatStatement::masquerade(),
        NatAction::Redirect => NatStatement::redirect(),
        NatAction::Snat | NatAction::Dnat => {
            let first = addr(
                nat.target
                    .as_ref()
                    .expect("validated: snat/dnat have a target"),
            );
            let st = if nat.action == NatAction::Snat {
                NatStatement::snat(first)
            } else {
                NatStatement::dnat(first)
            };
            match &nat.target_end {
                Some(last) => st.to(addr(last)),
                None => st,
            }
        }
        NatAction::Translate => unreachable!("translate rules are expanded during lowering"),
    };
    if let Some(first) = nat.target_port {
        st = st.ports(first, nat.target_port_end.unwrap_or(first));
    }
    for flag in &nat.flags {
        st = match flag {
            NatFlag::Random => st.random(),
            NatFlag::FullyRandom => st.fully_random(),
            NatFlag::Persistent => st.persistent(),
        };
    }
    st
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
        (NatAction::Masquerade | NatAction::Translate | NatAction::Redirect, _) => rule,
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
                            rule.exprs().unwrap()
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

#[cfg(test)]
mod nat_ranges {
    use crate::types::{NatAction, NatFlag};

    fn nat_of(src: &str) -> crate::types::NatConfig {
        let topo = crate::parser::parse(src).unwrap();
        assert!(!topo.validate().has_errors(), "{:?}", topo.validate());
        topo.nodes["gw"].nat.clone().unwrap()
    }

    /// `snat … to ADDR:PORT` with no range or flag takes the plain path,
    /// and keeps its port there (#203): it used to be dropped, as `dnat`'s
    /// never was.
    #[test]
    fn a_plain_snat_keeps_its_port() {
        let nat =
            nat_of("lab \"n\"\nnode gw {\n  nat { snat src 10.0.1.0/24 to 203.0.113.1:5000 }\n}\n");
        assert_eq!(nat.rules[0].target_port, Some(5000));
        let cfg = super::topology_to_nftables_config(None, Some(&nat)).unwrap();
        let rule = cfg.tables()[0]
            .rules()
            .iter()
            .find(|r| r.handle_key().is_some_and(|k| k.ends_with("/snat")))
            .map(|r| format!("{:?}", r.exprs()))
            .expect("the snat rule");
        // The port is loaded into a register for the NAT expression, in
        // network byte order: 5000 = 0x1388.
        assert!(rule.contains("Nat") && rule.contains("[19, 136]"), "{rule}");
    }

    /// Ranges, flags and redirect (#189) parse, render back as themselves,
    /// and plan as `NatStatement`s; a plain rule keeps its expressions.
    #[test]
    fn nat_ranges_flags_and_redirect() {
        let src = r#"lab "n"
node gw {
  nat {
    snat src 10.0.1.0/24 oif wan0 to 203.0.113.1-203.0.113.9:40000-40010 fully-random
    masquerade src 10.0.2.0/24 to :1024-2048 random
    redirect iif lan0 tcp dport 80 to :3128
    dnat dst 203.0.113.10 tcp dport 443 to 10.0.1.10:8443 persistent
    masquerade src 10.0.3.0/24
  }
}
"#;
        let nat = nat_of(src);
        let r = &nat.rules;
        assert_eq!(r[0].target_end.as_deref(), Some("203.0.113.9"));
        assert_eq!(
            (r[0].target_port, r[0].target_port_end),
            (Some(40000), Some(40010))
        );
        assert_eq!(r[0].flags, [NatFlag::FullyRandom]);
        assert_eq!(
            (r[1].target_port, r[1].flags.as_slice()),
            (Some(1024), &[NatFlag::Random][..])
        );
        assert_eq!(
            (r[2].action.clone(), r[2].target_port),
            (NatAction::Redirect, Some(3128))
        );
        assert_eq!(r[3].flags, [NatFlag::Persistent]);

        let rendered = crate::render::try_render(&crate::parser::parse(src).unwrap()).unwrap();
        for line in [
            "snat src 10.0.1.0/24 oif wan0 to 203.0.113.1-203.0.113.9:40000-40010 fully-random",
            "masquerade src 10.0.2.0/24 to :1024-2048 random",
            "redirect iif lan0 tcp dport 80 to :3128",
            "dnat dst 203.0.113.10 tcp dport 443 to 10.0.1.10:8443 persistent",
        ] {
            assert!(rendered.contains(line), "{line}\n{rendered}");
        }
        assert_eq!(
            crate::parser::parse(&rendered).unwrap().nodes["gw"].nat,
            Some(nat.clone())
        );

        let cfg = super::topology_to_nftables_config(None, Some(&nat)).unwrap();
        let rules: Vec<String> = cfg.tables()[0]
            .rules()
            .iter()
            .map(|r| format!("{} {:?}", r.handle_key().unwrap_or("-"), r.exprs()))
            .collect();
        assert_eq!(rules.len(), 5, "{rules:#?}");
        assert!(
            rules
                .iter()
                .any(|r| r.starts_with("nlink-lab/nat/prerouting/2/redirect"))
        );
        // The plain masquerade is the expressions it always was.
        let plain = rules
            .iter()
            .find(|r| r.starts_with("nlink-lab/nat/postrouting/4/masq"))
            .unwrap();
        assert!(plain.contains("Masq"), "{plain}");
    }

    #[test]
    fn nat_range_mistakes_are_refused() {
        for (bad, why) in [
            ("masquerade to :2048-1024", "runs backwards"),
            ("snat to 10.0.0.9-10.0.0.1", "runs backwards"),
            ("redirect to :80-90", "one port"),
            ("redirect oif wan0 to :80", "does not apply to redirect"),
        ] {
            let src = format!("lab \"n\"\nnode gw {{ nat {{ {bad} }} }}\n");
            let msg = match crate::parser::parse(&src) {
                Err(e) => e.to_string(),
                Ok(t) => {
                    let v = t.validate();
                    let errs: Vec<String> = v.errors().map(|e| e.message.clone()).collect();
                    match super::validate_nat_rule_literals(t.nodes["gw"].nat.as_ref().unwrap()) {
                        Err(e) => format!("{errs:?} {e}"),
                        Ok(()) => format!("{errs:?}"),
                    }
                }
            };
            assert!(msg.contains(why), "{bad}: {msg}");
        }
    }
}

#[cfg(test)]
mod filter_032 {
    /// #189's filter additions parse, validate, render back as themselves
    /// and plan.
    #[test]
    fn rpf_ranges_byte_limits_and_log_levels_round_trip() {
        let src = r#"lab "f"
node gw {
  firewall forward policy accept {
    drop rpf
    drop iif lan0 tcp dport != 8000-8999
    mark 0x10-0x1f counter
    accept limit 1mb/second burst 64kb
    log "spoof: " level info
  }
}
"#;
        let topo = crate::parser::parse(src).unwrap();
        let v = topo.validate();
        assert!(!v.has_errors(), "{:?}", v.errors().collect::<Vec<_>>());
        let rendered = crate::render::try_render(&topo).unwrap();
        for line in [
            "drop rpf",
            "drop iif lan0 tcp dport != 8000-8999",
            "mark 0x10-0x1f counter",
            "accept limit 1mb/second burst 64kb",
            "log \"spoof: \" level info",
        ] {
            assert!(rendered.contains(line), "{line}\n{rendered}");
        }
        let again = crate::parser::parse(&rendered).unwrap();
        assert_eq!(again.nodes["gw"].firewall, topo.nodes["gw"].firewall);
        let fw = topo.effective_firewall(&topo.nodes["gw"]);
        let cfg = super::topology_to_nftables_config(fw.as_deref(), None).unwrap();
        assert_eq!(cfg.tables()[0].rules().len(), 5);
    }

    #[test]
    fn rpf_in_the_output_chain_is_refused() {
        let topo =
            crate::parser::parse("lab \"f\"\nnode a { firewall output { drop rpf } }\n").unwrap();
        assert!(
            topo.validate().errors().any(|e| e.message.contains("rpf")),
            "{:?}",
            topo.validate().errors().collect::<Vec<_>>()
        );
    }
}
