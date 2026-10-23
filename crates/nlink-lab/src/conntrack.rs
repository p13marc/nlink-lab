//! Tracked flows a lab's NAT rules made stale (#303).
//!
//! The kernel translates a flow once, when it begins, and keeps that
//! translation in the flow's conntrack entry until the entry times out. So
//! an `apply` that changes a node's NAT rules changes how new flows are
//! translated, but not the flows already tracked: a UDP stream, an open TCP
//! connection, a ping keep going where the old rules sent them. After an
//! apply commits, [`forget_flows`] deletes the entries of the flows that an
//! added, removed or changed NAT rule matches, and their next packet is
//! translated by the rules now installed.

use std::net::IpAddr;

use nlink::Connection;
use nlink::netlink::Netfilter;
use nlink::netlink::netfilter::{ConntrackEntry, ConntrackFilter, IpProtocol};

use crate::types::{NatAction, NatProto, NatRule};

/// The rules one of `old` and `new` has and the other does not: what an
/// apply added, removed or changed.
pub(crate) fn changed_rules<'a>(old: &'a [NatRule], new: &'a [NatRule]) -> Vec<&'a NatRule> {
    old.iter()
        .filter(|r| !new.contains(r))
        .chain(new.iter().filter(|r| !old.contains(r)))
        .collect()
}

/// Whether `rule` matches a tracked flow, by the flow's original (pre-NAT)
/// tuple: the destination of a dnat or a redirect, the source of a snat or
/// a masquerade, and the protocol and destination port the rule names. An
/// entry does not record interfaces, so a rule's `iif`/`oif` is not
/// checked: it matches the flows on every interface.
pub(crate) fn rule_matches(rule: &NatRule, entry: &ConntrackEntry) -> bool {
    let (cidr, addr) = match rule.action {
        NatAction::Dnat | NatAction::Redirect => (rule.dst.as_deref(), entry.orig.dst_ip),
        NatAction::Snat | NatAction::Masquerade | NatAction::Translate => {
            (rule.src.as_deref(), entry.orig.src_ip)
        }
    };
    if let Some(cidr) = cidr
        && !addr.is_some_and(|a| in_cidr(a, cidr))
    {
        return false;
    }
    if let Some(proto) = rule.proto
        && entry.proto != ip_protocol(proto)
    {
        return false;
    }
    if let Some(port) = rule.dport
        && entry.orig.dst_port != Some(port)
    {
        return false;
    }
    true
}

/// A filter that narrows the dump to what `rule` can match: its protocol,
/// its destination port, and its address when it is one host. The rest
/// (a CIDR) is [`rule_matches`]' to check.
fn coarse_filter(rule: &NatRule) -> ConntrackFilter {
    let mut filter = ConntrackFilter::new();
    if let Some(proto) = rule.proto {
        filter = filter.proto(ip_protocol(proto));
        if let Some(port) = rule.dport {
            filter = filter.orig_dst_port(port);
        }
    }
    let (cidr, dst) = match rule.action {
        NatAction::Dnat | NatAction::Redirect => (rule.dst.as_deref(), true),
        _ => (rule.src.as_deref(), false),
    };
    if let Some((addr, prefix)) = cidr.and_then(|c| crate::helpers::parse_ip_or_cidr(c).ok())
        && prefix == if addr.is_ipv4() { 32 } else { 128 }
    {
        filter = if dst {
            filter.orig_dst(addr)
        } else {
            filter.orig_src(addr)
        };
    }
    filter
}

fn ip_protocol(proto: NatProto) -> IpProtocol {
    match proto {
        NatProto::Tcp => IpProtocol::Tcp,
        NatProto::Udp => IpProtocol::Udp,
    }
}

/// Whether `addr` is in `cidr` (a bare address is a host prefix).
fn in_cidr(addr: IpAddr, cidr: &str) -> bool {
    let Ok((net, prefix)) = crate::helpers::parse_ip_or_cidr(cidr) else {
        return false;
    };
    match (addr, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// Delete the tracked flows that any of `rules` matches, in the namespace
/// `conn` is in; how many went. A flow that ended meanwhile is not an
/// error, and one two rules match is deleted once.
pub(crate) async fn forget_flows(
    conn: &Connection<Netfilter>,
    rules: &[&NatRule],
) -> nlink::Result<usize> {
    let mut gone = 0;
    for rule in rules {
        let entries = conn.get_conntrack_matching(&coarse_filter(rule)).await?;
        for entry in entries.iter().filter(|e| rule_matches(rule, e)) {
            match conn.del_conntrack_entry(entry).await {
                Ok(()) => gone += 1,
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink::netlink::netfilter::ConntrackTuple;

    fn nat(src: &str) -> NatRule {
        let topo = crate::parser::parse(&format!("lab \"n\"\nnode gw {{\n  nat {{ {src} }}\n}}\n"))
            .unwrap();
        topo.nodes["gw"].nat.clone().unwrap().rules.remove(0)
    }

    fn flow(proto: IpProtocol, src: &str, dst: &str, dport: u16) -> ConntrackEntry {
        let mut e = ConntrackEntry::default();
        e.proto = proto;
        e.orig = ConntrackTuple {
            src_ip: Some(src.parse().unwrap()),
            dst_ip: Some(dst.parse().unwrap()),
            src_port: Some(40000),
            dst_port: Some(dport),
            ..Default::default()
        };
        e
    }

    #[test]
    fn a_dnat_matches_flows_to_its_destination_and_port() {
        let dnat = nat("dnat dst 10.0.2.1 tcp dport 80 to 10.0.1.10:8080");
        assert!(rule_matches(
            &dnat,
            &flow(IpProtocol::Tcp, "10.0.2.9", "10.0.2.1", 80)
        ));
        // another port, protocol or destination is another flow
        assert!(!rule_matches(
            &dnat,
            &flow(IpProtocol::Tcp, "10.0.2.9", "10.0.2.1", 81)
        ));
        assert!(!rule_matches(
            &dnat,
            &flow(IpProtocol::Udp, "10.0.2.9", "10.0.2.1", 80)
        ));
        assert!(!rule_matches(
            &dnat,
            &flow(IpProtocol::Tcp, "10.0.2.9", "10.0.2.2", 80)
        ));
        let filter = coarse_filter(&dnat);
        assert!(filter.matches(&flow(IpProtocol::Tcp, "10.0.2.9", "10.0.2.1", 80)));
        assert!(!filter.matches(&flow(IpProtocol::Tcp, "10.0.2.9", "10.0.2.1", 81)));
    }

    #[test]
    fn a_snat_or_masquerade_matches_flows_from_its_source() {
        let masq = nat("masquerade src 10.0.1.0/24");
        assert!(rule_matches(
            &masq,
            &flow(IpProtocol::Udp, "10.0.1.7", "198.51.100.1", 53)
        ));
        assert!(!rule_matches(
            &masq,
            &flow(IpProtocol::Udp, "10.0.3.7", "198.51.100.1", 53)
        ));
        // a CIDR is checked here, not by the kernel
        assert!(coarse_filter(&masq).is_empty());
        let snat = nat("snat src 10.0.1.10 to 203.0.113.1");
        assert!(rule_matches(
            &snat,
            &flow(IpProtocol::Icmp, "10.0.1.10", "8.8.8.8", 0)
        ));
        assert!(!rule_matches(
            &snat,
            &flow(IpProtocol::Icmp, "10.0.1.11", "8.8.8.8", 0)
        ));
        // a family the rule does not have
        assert!(!rule_matches(
            &snat,
            &flow(IpProtocol::Icmp, "fd00::1", "fd00::2", 0)
        ));
    }

    #[test]
    fn the_changed_rules_are_the_ones_only_one_side_has() {
        let (a, b, c) = (
            nat("dnat dst 10.0.2.1 tcp dport 80 to 10.0.1.10:8080"),
            nat("dnat dst 10.0.2.1 tcp dport 80 to 10.0.1.20:8080"),
            nat("masquerade src 10.0.1.0/24"),
        );
        let old = [a.clone(), c.clone()];
        let new = [b.clone(), c.clone()];
        assert_eq!(changed_rules(&old, &new), [&a, &b]);
        assert!(changed_rules(&old, &old).is_empty());
    }
}
