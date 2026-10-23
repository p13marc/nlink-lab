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
use nlink::netlink::netfilter::{
    ConntrackEntry, ConntrackFilter, ConntrackStatus, ConntrackTuple, IpProtocol, TcpConntrackState,
};
use nlink_lab_shared::api::{ConntrackFlow, ConntrackQuery, FlowTuple};

use crate::error::{Error, Result};
use crate::running::RunningLab;
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
    // the one address the installed rule matches (`nat_rule_spec`); a
    // `translate` is a 1:1 DNAT, expanded to `dnat` rules by the parser
    let (cidr, addr) = match rule.action {
        NatAction::Dnat | NatAction::Redirect | NatAction::Translate => {
            (rule.dst.as_deref(), entry.orig.dst_ip)
        }
        NatAction::Snat | NatAction::Masquerade => (rule.src.as_deref(), entry.orig.src_ip),
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
        NatAction::Dnat | NatAction::Redirect | NatAction::Translate => (rule.dst.as_deref(), true),
        NatAction::Snat | NatAction::Masquerade => (rule.src.as_deref(), false),
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
    if prefix > if net.is_ipv4() { 32 } else { 128 } {
        return false;
    }
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

/// Conntrack status bits, by nft's names.
const STATUS: [(&str, ConntrackStatus); 7] = [
    ("expected", ConntrackStatus::EXPECTED),
    ("seen-reply", ConntrackStatus::SEEN_REPLY),
    ("assured", ConntrackStatus::ASSURED),
    ("confirmed", ConntrackStatus::CONFIRMED),
    ("snat", ConntrackStatus::SRC_NAT),
    ("dnat", ConntrackStatus::DST_NAT),
    ("dying", ConntrackStatus::DYING),
];

/// A query, ready to run: the filter nlink (and the kernel) applies, and
/// the CIDRs it cannot, checked on what it returns.
struct Selection {
    filter: ConntrackFilter,
    src: Option<String>,
    dst: Option<String>,
}

impl Selection {
    fn of(query: &ConntrackQuery) -> Result<Selection> {
        let mut filter = ConntrackFilter::new();
        let mut l4 = None;
        if let Some(proto) = &query.proto {
            let n = crate::firewall::l4proto_number(proto).ok_or_else(|| {
                Error::invalid_argument(format!(
                    "unknown protocol '{proto}' (a name such as tcp, udp, icmp, icmpv6, sctp, \
                     or a number 0-255)"
                ))
            })?;
            l4 = Some(n);
            filter = filter.proto(match n {
                1 => IpProtocol::Icmp,
                6 => IpProtocol::Tcp,
                17 => IpProtocol::Udp,
                58 => IpProtocol::Icmpv6,
                other => IpProtocol::Other(other),
            });
        }
        if (query.sport.is_some() || query.dport.is_some()) && !matches!(l4, Some(6 | 17)) {
            return Err(Error::invalid_argument(
                "a port needs the protocol it is a port of: tcp or udp",
            ));
        }
        if let Some(port) = query.sport {
            filter = filter.orig_src_port(port);
        }
        if let Some(port) = query.dport {
            filter = filter.orig_dst_port(port);
        }
        let (src_host, src) = host_or_cidr(query.src.as_deref())?;
        let (dst_host, dst) = host_or_cidr(query.dst.as_deref())?;
        if let Some(addr) = src_host {
            filter = filter.orig_src(addr);
        }
        if let Some(addr) = dst_host {
            filter = filter.orig_dst(addr);
        }
        if let Some(mark) = query.mark {
            filter = filter.mark(mark, u32::MAX);
        }
        if let Some(zone) = query.zone {
            filter = filter.zone(zone);
        }
        if !query.status.is_empty() {
            let mut bits = ConntrackStatus::empty();
            for name in &query.status {
                let (_, bit) = STATUS.iter().find(|(n, _)| n == name).ok_or_else(|| {
                    Error::invalid_argument(format!(
                        "unknown status '{name}' (expected {})",
                        STATUS.map(|(n, _)| n).join(", ")
                    ))
                })?;
                bits |= *bit;
            }
            filter = filter.status(bits, bits);
        }
        Ok(Selection { filter, src, dst })
    }

    fn selects(&self, entry: &ConntrackEntry) -> bool {
        let within = |cidr: &Option<String>, addr: Option<IpAddr>| {
            cidr.as_deref()
                .is_none_or(|c| addr.is_some_and(|a| in_cidr(a, c)))
        };
        within(&self.src, entry.orig.src_ip) && within(&self.dst, entry.orig.dst_ip)
    }
}

/// An address operand: one host, which the kernel can match, or a CIDR,
/// which nlink-lab checks.
fn host_or_cidr(text: Option<&str>) -> Result<(Option<IpAddr>, Option<String>)> {
    let Some(text) = text else {
        return Ok((None, None));
    };
    let (addr, prefix) = crate::helpers::parse_ip_or_cidr(text)
        .map_err(|e| Error::invalid_argument(format!("'{text}': {e}")))?;
    if prefix == if addr.is_ipv4() { 32 } else { 128 } {
        Ok((Some(addr), None))
    } else {
        Ok((None, Some(text.to_string())))
    }
}

/// The flows of `conn`'s namespace that `query` selects.
async fn select(
    conn: &Connection<Netfilter>,
    query: &ConntrackQuery,
) -> Result<Vec<ConntrackEntry>> {
    let selection = Selection::of(query)?;
    let entries = conn
        .get_conntrack_matching(&selection.filter)
        .await
        .map_err(|e| Error::nlink("read conntrack", e))?;
    Ok(entries
        .into_iter()
        .filter(|e| selection.selects(e))
        .collect())
}

/// A tracked flow as the API shows it.
fn flow_of(node: &str, entry: &ConntrackEntry) -> ConntrackFlow {
    let tuple = |t: &ConntrackTuple| FlowTuple {
        src: t.src_ip.map(|a| a.to_string()),
        dst: t.dst_ip.map(|a| a.to_string()),
        sport: t.src_port,
        dport: t.dst_port,
    };
    let proto = match entry.proto {
        IpProtocol::Tcp => "tcp".to_string(),
        IpProtocol::Udp => "udp".to_string(),
        IpProtocol::Icmp => "icmp".to_string(),
        IpProtocol::Icmpv6 => "icmpv6".to_string(),
        other => crate::firewall::L4_PROTOS
            .iter()
            .find(|(_, n)| *n == other.number())
            .map_or_else(|| other.number().to_string(), |(name, _)| name.to_string()),
    };
    let status = ConntrackStatus::from_bits(entry.status.unwrap_or(0));
    ConntrackFlow {
        node: node.to_string(),
        family: if entry.orig.src_ip.is_some_and(|a| a.is_ipv6()) {
            "ipv6"
        } else {
            "ipv4"
        }
        .to_string(),
        proto,
        orig: tuple(&entry.orig),
        reply: tuple(&entry.reply),
        state: entry.tcp_state.map(|s| tcp_state_name(s).to_string()),
        status: STATUS
            .iter()
            .filter(|(_, bit)| status.contains(*bit))
            .map(|(name, _)| name.to_string())
            .collect(),
        timeout: entry.timeout,
        mark: entry.mark,
        zone: entry.zone,
    }
}

/// A TCP flow's state by the kernel's name (`TIME_WAIT` as `time_wait`),
/// spelled out so the JSON does not follow nlink's variant names.
fn tcp_state_name(state: TcpConntrackState) -> &'static str {
    match state {
        TcpConntrackState::None => "none",
        TcpConntrackState::SynSent => "syn_sent",
        TcpConntrackState::SynRecv => "syn_recv",
        TcpConntrackState::Established => "established",
        TcpConntrackState::FinWait => "fin_wait",
        TcpConntrackState::CloseWait => "close_wait",
        TcpConntrackState::LastAck => "last_ack",
        TcpConntrackState::TimeWait => "time_wait",
        TcpConntrackState::Close => "close",
        TcpConntrackState::Listen => "listen",
        TcpConntrackState::SynSent2 => "syn_sent2",
        TcpConntrackState::Max => "max",
        TcpConntrackState::Ignore => "ignore",
        TcpConntrackState::Retrans => "retrans",
        TcpConntrackState::Unack => "unack",
        _ => "unknown",
    }
}

impl RunningLab {
    fn netfilter_of(&self, node: &str) -> Result<Connection<Netfilter>> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?;
        Ok(ns.connection()?)
    }

    /// The flows a node's conntrack table holds that `query` selects
    /// (#303): what `nlink-lab conntrack` lists.
    pub async fn conntrack(
        &self,
        node: &str,
        query: &ConntrackQuery,
    ) -> Result<Vec<ConntrackFlow>> {
        let conn = self.netfilter_of(node)?;
        Ok(select(&conn, query)
            .await?
            .iter()
            .map(|e| flow_of(node, e))
            .collect())
    }

    /// [`RunningLab::conntrack`] on `node`, or on every node of the lab.
    pub async fn conntrack_of(
        &self,
        node: Option<&str>,
        query: &ConntrackQuery,
    ) -> Result<Vec<ConntrackFlow>> {
        let mut flows = Vec::new();
        for node in self.conntrack_nodes(node)? {
            flows.extend(self.conntrack(&node, query).await?);
        }
        Ok(flows)
    }

    /// [`RunningLab::flush_conntrack`] on `node`, or on every node: how
    /// many flows went, per node.
    pub async fn flush_conntrack_of(
        &self,
        node: Option<&str>,
        query: &ConntrackQuery,
    ) -> Result<std::collections::BTreeMap<String, usize>> {
        let mut deleted = std::collections::BTreeMap::new();
        for node in self.conntrack_nodes(node)? {
            let n = self.flush_conntrack(&node, query).await?;
            deleted.insert(node, n);
        }
        Ok(deleted)
    }

    /// `node`, checked, or every node of the lab.
    fn conntrack_nodes(&self, node: Option<&str>) -> Result<Vec<String>> {
        match node {
            Some(n) if self.topology().nodes.contains_key(n) => Ok(vec![n.to_string()]),
            Some(n) => Err(Error::NodeNotFound {
                name: n.to_string(),
            }),
            None => Ok(self.topology().nodes.keys().cloned().collect()),
        }
    }

    /// Delete the flows of a node that `query` selects (every flow, with
    /// an empty query), each by its tuple, zone and id; how many went
    /// (#303). Their next packet starts a new flow, through the rules as
    /// they are now.
    pub async fn flush_conntrack(&self, node: &str, query: &ConntrackQuery) -> Result<usize> {
        let conn = self.netfilter_of(node)?;
        let mut gone = 0;
        for entry in select(&conn, query).await? {
            match conn.del_conntrack_entry(&entry).await {
                Ok(()) => gone += 1,
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(Error::nlink(format!("delete a flow on '{node}'"), e)),
            }
        }
        Ok(gone)
    }
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

    /// A `translate` (a 1:1 DNAT the parser expands to `dnat` rules)
    /// matches by destination, as a dnat does.
    #[test]
    fn a_translate_matches_by_destination() {
        let mut t = nat("dnat dst 203.0.113.10 to 10.0.1.10");
        t.action = NatAction::Translate;
        assert!(rule_matches(
            &t,
            &flow(IpProtocol::Tcp, "198.51.100.7", "203.0.113.10", 443)
        ));
        assert!(!rule_matches(
            &t,
            &flow(IpProtocol::Tcp, "203.0.113.10", "198.51.100.7", 443)
        ));
        assert!(!in_cidr("10.0.0.1".parse().unwrap(), "10.0.0.0/33"));
    }

    #[test]
    fn a_query_filters_what_the_kernel_can_and_checks_cidrs_itself() {
        let q = ConntrackQuery {
            proto: Some("udp".into()),
            dst: Some("10.0.2.0/24".into()),
            dport: Some(5000),
            status: vec!["dnat".into()],
            ..Default::default()
        };
        let sel = Selection::of(&q).unwrap();
        let mut e = flow(IpProtocol::Udp, "10.0.1.10", "10.0.2.1", 5000);
        e.status = Some(ConntrackStatus::DST_NAT.bits() | ConntrackStatus::SEEN_REPLY.bits());
        assert!(sel.filter.matches(&e) && sel.selects(&e));
        let mut other = flow(IpProtocol::Udp, "10.0.1.10", "10.0.3.1", 5000);
        other.status = e.status;
        assert!(sel.filter.matches(&other) && !sel.selects(&other));
        let plain = flow(IpProtocol::Udp, "10.0.1.10", "10.0.2.1", 5000);
        assert!(!sel.filter.matches(&plain), "not DNATed");
        // --mark 0 selects the flows nothing marked: the kernel sends no mark
        let unmarked = Selection::of(&ConntrackQuery {
            mark: Some(0),
            ..Default::default()
        })
        .unwrap();
        let mut marked = plain.clone();
        marked.mark = Some(7);
        assert!(unmarked.filter.matches(&plain) && !unmarked.filter.matches(&marked));
        marked.mark = Some(0);
        assert!(unmarked.filter.matches(&marked));
        // what cannot be asked
        for (bad, why) in [
            (
                ConntrackQuery {
                    dport: Some(80),
                    ..Default::default()
                },
                "tcp or udp",
            ),
            (
                ConntrackQuery {
                    proto: Some("quic".into()),
                    ..Default::default()
                },
                "unknown protocol",
            ),
            (
                ConntrackQuery {
                    status: vec!["nat".into()],
                    ..Default::default()
                },
                "unknown status",
            ),
        ] {
            let err = Selection::of(&bad).err().expect(why).to_string();
            assert!(err.contains(why), "{err}");
        }
    }

    #[test]
    fn a_flow_shows_its_directions_state_and_status() {
        let mut e = flow(IpProtocol::Tcp, "10.0.1.10", "10.0.2.1", 80);
        e.reply = ConntrackTuple {
            src_ip: Some("10.0.1.20".parse().unwrap()),
            dst_ip: Some("10.0.1.10".parse().unwrap()),
            src_port: Some(8080),
            dst_port: Some(40000),
            ..Default::default()
        };
        e.tcp_state = Some(TcpConntrackState::TimeWait);
        e.status = Some(ConntrackStatus::DST_NAT.bits() | ConntrackStatus::ASSURED.bits());
        let f = flow_of("gw", &e);
        assert_eq!(
            (f.family.as_str(), f.proto.as_str(), f.state.as_deref()),
            ("ipv4", "tcp", Some("time_wait"))
        );
        assert_eq!(f.status, ["assured", "dnat"]);
        assert_eq!(f.reply.src.as_deref(), Some("10.0.1.20"));
        assert_eq!(f.orig.dport, Some(80));
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
