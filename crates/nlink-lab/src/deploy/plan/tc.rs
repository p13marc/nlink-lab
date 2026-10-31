//! Pure planner: a node's traffic control as declarations in its
//! `NetworkConfig` (#108): `impair` netem, `qdisc` blocks, rate limits
//! (`RateLimiter::declare`), the per-pair matrices of the networks it is a
//! source in (`PerPeerImpairer::declare`), and partitions.
//!
//! Declared, they are planned and applied with the node's links,
//! addresses and routes in its `Stack`: one that is unchanged costs no
//! kernel call (its handle and statistics survive), one the topology
//! drops is removed (`PlanOptions::previous`), one removed by hand is
//! drift that `apply --check` reports and `apply` puts back, and one on a
//! link nlink recreates comes back with it.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use nlink::netlink::action::{ActionList, GactAction};
use nlink::netlink::block::BLOCK_PRIORITY;
use nlink::netlink::config::NetworkConfig;
use nlink::netlink::filter::MatchallFilter;
use nlink::netlink::impair::{PeerImpairment, PerPeerImpairer};
use nlink::netlink::ratelimit::RateLimiter;
use nlink::netlink::tc_spec::QdiscSpec;
use nlink::util::{Bytes, Rate};
use nlink::{EthProto, TcHandle};

use super::qdisc::{build_netem, build_qdisc};
use crate::error::{Error, Result};
use crate::helpers::{parse_rate_bps, parse_size};
use crate::types::{EndpointRef, RateLimit, Topology};

/// Declare `node_name`'s traffic control into `cfg`.
///
/// An interface has one root qdisc. Which, in order:
/// - the per-pair tree of a network whose `impair` rules it is the source
///   of; its own `impair` becomes the tree's default impairment, for
///   traffic to peers no rule names (#169);
/// - its `impair` netem;
/// - its `qdisc` block;
/// - its rate limit's egress HTB, with the ingress half on its IFB. A rate
///   limit on an impaired interface is skipped, since `impair … rate`
///   covers it.
///
/// The validator refuses the other combinations (`qdisc-conflicts`,
/// `network-impair-conflicts`); a runtime `impair` (`partitions` and the
/// impairments overlaid on `topology`) takes the root over from a `qdisc`
/// block or a rate limit until it is cleared.
///
/// A partitioned interface also gets a clsact qdisc with a matchall on
/// both of its hooks, ahead of any other filter there: see
/// [`partition_filters`]. Its root and the tree under it stay as they
/// are, so healing finds them intact (#168).
pub(crate) fn declare_node_tc(
    mut cfg: NetworkConfig,
    node_name: &str,
    topology: &Topology,
    partitions: &BTreeSet<String>,
) -> Result<NetworkConfig> {
    let mut per_pair = per_pair_sources(node_name, topology)?;
    let on_node = |ep: &String| {
        EndpointRef::parse(ep)
            .filter(|e| e.node == node_name)
            .map(|e| e.iface)
    };
    let ifaces: BTreeSet<String> = topology
        .impairments
        .keys()
        .chain(topology.qdiscs.keys())
        .chain(topology.rate_limits.keys())
        .chain(partitions.iter())
        .filter_map(on_node)
        .chain(per_pair.keys().cloned())
        .collect();

    for iface in ifaces {
        let ep = format!("{node_name}:{iface}");
        let impairment = topology.impairments.get(&ep);
        let partitioned = partitions.contains(&ep);
        if partitioned {
            // Before a rate limiter's redirect, which then hangs off the
            // clsact's ingress hook rather than an `ingress` qdisc.
            cfg = cfg.qdisc(&iface, |q| q.spec(QdiscSpec::Clsact));
        }

        let seed_key = crate::seed::endpoint_key(node_name, &iface);
        if let Some(mut impairer) = per_pair.remove(&iface) {
            if let Some(imp) = impairment {
                let seed = crate::seed::netem_seed(topology, imp, &seed_key)?;
                impairer =
                    impairer.default_impairment(PeerImpairment::new(build_netem(imp, seed)?));
            }
            cfg = impairer
                .declare(cfg)
                .map_err(|e| Error::nlink(format!("per-pair impairments on '{ep}'"), e))?;
        } else if let Some(imp) = impairment {
            let seed = crate::seed::netem_seed(topology, imp, &seed_key)?;
            let netem = build_netem(imp, seed)?;
            cfg = cfg.qdisc(&iface, |q| q.spec(netem));
        } else if let Some(qdisc) = topology.qdiscs.get(&ep) {
            let spec = build_qdisc(qdisc)?;
            cfg = cfg.qdisc(&iface, |q| q.spec(spec));
        } else if let Some(limit) = topology.rate_limits.get(&ep) {
            cfg = rate_limiter(&ep, &iface, limit)?.declare(cfg);
        }
        if impairment.is_some() && topology.rate_limits.contains_key(&ep) {
            tracing::warn!(
                "rate limit on '{ep}' skipped: netem impairment already configured (use impairment.rate instead)"
            );
        }

        if partitioned {
            let [ingress, egress] = partition_filters();
            cfg = cfg.tc(&iface, |t| {
                t.filter(
                    TcHandle::CLSACT_INGRESS,
                    BLOCK_PRIORITY,
                    EthProto::ALL,
                    ingress,
                )
                .filter(
                    TcHandle::CLSACT_EGRESS,
                    BLOCK_PRIORITY,
                    EthProto::ALL,
                    egress,
                )
            });
        }
    }
    Ok(cfg)
}

/// A partition's two filters, `[ingress, egress]`: a black hole, which
/// nothing on either side can tell from a dead network (#347).
///
/// The ingress hook drops what arrives (TC_ACT_SHOT); the far sender
/// cannot see that, and the drop counters show it. The egress hook must
/// not drop the same way: a TC_ACT_SHOT there returns `NET_XMIT_DROP` to
/// the endpoint's own stack, its TCP takes that for local congestion,
/// retries every 500 ms without backing off and gives up with ETIMEDOUT
/// about 8 s after the cut. It *steals* the packet instead
/// (TC_ACT_STOLEN): freed, and reported to the sender as sent, as netem's
/// `loss 100%` would, so its connections back off and wait as they do
/// when a WAN goes dark.
pub(crate) fn partition_filters() -> [MatchallFilter; 2] {
    let with = |action: GactAction| MatchallFilter::new().actions(ActionList::new().with(action));
    [with(GactAction::drop()), with(GactAction::stolen())]
}

/// The `RateLimiter` for an endpoint's `rate` declaration.
fn rate_limiter(ep: &str, iface: &str, limit: &RateLimit) -> Result<RateLimiter> {
    let rate = |dir: &str, v: &str| {
        parse_rate_bps(v)
            .map(Rate::bits_per_sec)
            .map_err(|e| Error::deploy_failed(format!("bad {dir} rate on '{ep}': {e}")))
    };
    let mut limiter = RateLimiter::new(iface);
    if let Some(egress) = &limit.egress {
        limiter = limiter.egress(rate("egress", egress)?);
    }
    if let Some(ingress) = &limit.ingress {
        limiter = limiter.ingress(rate("ingress", ingress)?);
    }
    // `burst` is a byte size (issue #71); it applies to whichever
    // directions are limited.
    if let Some(burst) = &limit.burst {
        let bytes = parse_size(burst)
            .map_err(|e| Error::deploy_failed(format!("bad burst on '{ep}': {e}")))?;
        limiter = limiter.burst_size(Bytes::new(bytes));
    }
    Ok(limiter)
}

/// The per-pair impairers whose source is `node_name`, by interface: for
/// each network with `impair A -- B` rules from it, one HTB+netem+flower
/// tree on its member interface there, a rule per destination.
fn per_pair_sources(
    node_name: &str,
    topology: &Topology,
) -> Result<BTreeMap<String, PerPeerImpairer>> {
    let mut out = BTreeMap::new();
    for (net_name, network) in &topology.networks {
        let rules: Vec<_> = network
            .impairments
            .iter()
            .filter(|r| r.src == node_name)
            .collect();
        if rules.is_empty() {
            continue;
        }
        // Each node's interface in this network (its first member entry)
        // and address (the first address of that member's port).
        let mut node_ifaces: BTreeMap<String, String> = BTreeMap::new();
        let mut node_ips: BTreeMap<String, IpAddr> = BTreeMap::new();
        for member in &network.members {
            let Some(ep) = EndpointRef::parse(member) else {
                continue;
            };
            node_ifaces
                .entry(ep.node.clone())
                .or_insert_with(|| ep.iface.clone());
            if let Some(port) = network.ports.get(member)
                && let Some(addr) = port.addresses.first()
                && let Some((ip, _)) = addr.split_once('/')
                && let Ok(ip) = ip.parse::<IpAddr>()
            {
                node_ips.entry(ep.node.clone()).or_insert(ip);
            }
        }
        let Some(iface) = node_ifaces.get(node_name) else {
            return Err(Error::deploy_failed(format!(
                "network '{net_name}': src node '{node_name}' has no interface in this network"
            )));
        };
        let mut impairer = PerPeerImpairer::new(iface.as_str());
        for rule in rules {
            let Some(dst_ip) = node_ips.get(&rule.dst) else {
                return Err(Error::deploy_failed(format!(
                    "network '{net_name}' impair {} -- {}: cannot resolve IP for dst node \
                     '{}' (network needs a subnet, or the dst must have an explicit address)",
                    rule.src, rule.dst, rule.dst
                )));
            };
            let key = crate::seed::pair_key(net_name, &rule.src, &rule.dst, node_name, iface);
            let seed = crate::seed::netem_seed(topology, &rule.impairment, &key)?;
            let mut peer = PeerImpairment::new(build_netem(&rule.impairment, seed)?);
            if let Some(rc) = &rule.rate_cap {
                let bits = parse_rate_bps(rc).map_err(|e| {
                    Error::deploy_failed(format!(
                        "network '{net_name}' impair {} -- {}: bad rate-cap '{rc}': {e}",
                        rule.src, rule.dst
                    ))
                })?;
                peer = peer.rate_cap(Rate::bits_per_sec(bits));
            }
            impairer = impairer.impair_dst_ip(*dst_ip, peer);
        }
        out.insert(iface.clone(), impairer);
    }
    Ok(out)
}
