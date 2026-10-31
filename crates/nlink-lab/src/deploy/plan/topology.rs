//! Pure planner for the imperative residue of a deploy: namespaces,
//! containers, hwsim, the mgmt bridge, bridge networks, veths, host-side
//! macvlan/ipvlan, link bring-up, sysctls and DNS overlays.

use std::collections::BTreeMap;

use crate::deploy::op::{Op, PortVlans};
use crate::error::{Error, Result};
use crate::types::{DnsMode, EndpointRef, Topology};

use super::process::build_create_opts;

/// Ops for `Stage::Namespaces` … `Stage::Dns` (everything except the
/// per-node stack, TC and processes, which have their own planners).
pub(crate) fn plan_topology(topology: &Topology, dns_extra_hosts: &[String]) -> Result<Vec<Op>> {
    let mut ops = Vec::new();

    // ── namespaces / containers ──
    // In `depends-on` order: a container starts its `cmd` when it is
    // created, and they were created in name order, `app` before the `db`
    // it depends on (#217). (The `run`s and healthchecks are ordered too,
    // in the Processes stage.)
    for node_name in super::process::topo_sort_nodes(&topology.nodes) {
        let node = &topology.nodes[&node_name];
        let node_name = &node_name;
        if let Some(f) = &node.foreign {
            // another tool's: found, never made (#344)
            ops.push(Op::AttachForeign {
                node: node_name.clone(),
                target: f.target.clone(),
                bridge: matches!(f.target, crate::types::ForeignTarget::Tap(_))
                    .then(|| topology.lab.tap_bridge_name(node_name)),
            });
        } else if let Some(image) = &node.image {
            ops.push(Op::CreateContainer {
                node: node_name.clone(),
                name: format!("{}-{}", topology.lab.prefix(), node_name),
                image: image.clone(),
                pull: node.pull.clone(),
                opts: Box::new(build_create_opts(
                    node_name,
                    node,
                    dns_extra_hosts,
                    topology.source_dir.as_deref().map(std::path::Path::new),
                )),
            });
        } else {
            ops.push(Op::CreateNamespace {
                node: node_name.clone(),
                ns: topology.namespace_name(node_name),
            });
        }
    }

    // ── Wi-Fi radios ──
    // All made in the first Wi-Fi node's namespace: one netgroup for the
    // lab (#176).
    if let Some(maker) = topology
        .nodes
        .iter()
        .find(|(_, n)| !n.wifi.is_empty())
        .map(|(n, _)| n)
    {
        for (node_name, node) in &topology.nodes {
            for wifi in &node.wifi {
                ops.push(Op::CreateRadio {
                    node: node_name.clone(),
                    iface: wifi.name.clone(),
                    radio: crate::wifi::radio_name(&topology.namespace_name(node_name), &wifi.name),
                    mode: wifi.mode.clone(),
                    maker: maker.clone(),
                });
            }
        }
    }

    // ── host-reachable mgmt bridge ──
    if topology.lab.mgmt_host_reachable
        && let Some(mgmt_subnet) = &topology.lab.mgmt_subnet
    {
        // IPv4 or IPv6: bridge = first host address, nodes follow in
        // name order (BTreeMap iterates sorted). Capacity errors here.
        let mgmt = crate::helpers::mgmt_addresses(mgmt_subnet, topology.nodes.len())?;
        let bridge = topology.lab.mgmt_bridge_name();
        ops.push(Op::CreateMgmtBridge {
            name: bridge.clone(),
            ip: mgmt.bridge,
            prefix: mgmt.prefix,
        });
        for (idx, node_name) in topology.nodes.keys().enumerate() {
            if topology.nodes[node_name].is_foreign() {
                continue;
            }
            ops.push(Op::CreateMgmtVeth {
                node: node_name.clone(),
                peer: topology.lab.mgmt_peer_name(idx),
                bridge: bridge.clone(),
                node_ip: mgmt.nodes[idx],
                prefix: mgmt.prefix,
            });
        }
    }

    // ── bridge networks ──
    if !topology.networks.is_empty() {
        let mgmt_ns = format!("{}-mgmt", topology.lab.prefix());
        ops.push(Op::CreateMgmtNamespace {
            ns: mgmt_ns.clone(),
        });
        for (net_name, network) in &topology.networks {
            let bridge = crate::types::network_bridge_name_for(net_name);
            ops.push(Op::CreateBridge {
                ns: mgmt_ns.clone(),
                network: net_name.clone(),
                name: bridge.clone(),
                vlan_filtering: network.vlan_filtering == Some(true),
                mtu: network.mtu,
            });
            for (k, member) in network.members.iter().enumerate() {
                let ep = EndpointRef::parse(member).ok_or_else(|| Error::InvalidEndpoint {
                    endpoint: member.clone(),
                })?;
                if !topology.nodes.contains_key(&ep.node) {
                    return Err(Error::NodeNotFound { name: ep.node });
                }
                let vlans = network
                    .ports
                    .get(member)
                    .or_else(|| network.ports.get(&ep.node))
                    .filter(|p| !p.vlans.is_empty() || p.pvid.is_some())
                    .map(|p| PortVlans {
                        vlans: p.vlans.clone(),
                        pvid: p.pvid,
                        untagged: p.untagged == Some(true),
                    });
                ops.push(Op::CreateNetworkVeth {
                    node: ep.node,
                    iface: ep.iface,
                    peer: crate::types::network_peer_name_for(net_name, k),
                    mgmt_ns: mgmt_ns.clone(),
                    bridge: bridge.clone(),
                    vlans,
                    mtu: network.mtu,
                });
            }
        }
    }

    // ── point-to-point links ──
    let mut tap_links: BTreeMap<String, usize> = BTreeMap::new();
    for link in &topology.links {
        let a = EndpointRef::parse(&link.endpoints[0]).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: link.endpoints[0].clone(),
        })?;
        let b = EndpointRef::parse(&link.endpoints[1]).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: link.endpoints[1].clone(),
        })?;
        for n in [&a.node, &b.node] {
            if !topology.nodes.contains_key(n) {
                return Err(Error::NodeNotFound { name: n.clone() });
            }
        }
        // The lab's end is the one made, and the one an undo deletes by
        // name: never a foreign node's (#344).
        let foreign = |n: &str| topology.nodes[n].foreign.as_ref();
        let (a, b) = if foreign(&a.node).is_some() {
            (b, a)
        } else {
            (a, b)
        };
        if let Some(f) = foreign(&b.node)
            && matches!(f.target, crate::types::ForeignTarget::Tap(_))
        {
            let k = *tap_links.entry(b.node.clone()).or_insert(0usize);
            tap_links.insert(b.node.clone(), k + 1);
            ops.push(Op::CreateTapVeth {
                a,
                port: topology.lab.tap_port_name(&b.node, k),
                bridge: topology.lab.tap_bridge_name(&b.node),
                mtu: link.mtu,
            });
            continue;
        }
        ops.push(Op::CreateVeth {
            a,
            b,
            mtu: link.mtu,
        });
    }

    // ── host-side macvlan / ipvlan ──
    for (node_name, node) in &topology.nodes {
        for mv in &node.macvlans {
            ops.push(Op::CreateMacvlan {
                node: node_name.clone(),
                cfg: mv.clone(),
            });
        }
        for iv in &node.ipvlans {
            ops.push(Op::CreateIpvlan {
                node: node_name.clone(),
                cfg: iv.clone(),
            });
        }
    }

    // ── bring nlink-lab-created interfaces up ──
    for (node_name, node) in &topology.nodes {
        // a foreign node: only the ends nlink-lab made in it; a tap's
        // are in the host's namespace, up when made (#344)
        let foreign = node.foreign.as_ref();
        if foreign.is_some_and(|f| matches!(f.target, crate::types::ForeignTarget::Tap(_))) {
            continue;
        }
        let mut ifaces: Vec<String> = if foreign.is_some() {
            Vec::new()
        } else {
            vec!["lo".to_string()]
        };
        for link in &topology.links {
            for ep_str in &link.endpoints {
                if let Some(ep) = EndpointRef::parse(ep_str)
                    && &ep.node == node_name
                {
                    ifaces.push(ep.iface);
                }
            }
        }
        for network in topology.networks.values() {
            for member in &network.members {
                if let Some(ep) = EndpointRef::parse(member)
                    && &ep.node == node_name
                {
                    ifaces.push(ep.iface);
                }
            }
        }
        if topology.lab.mgmt_host_reachable
            && topology.lab.mgmt_subnet.is_some()
            && foreign.is_none()
        {
            ifaces.push("mgmt0".to_string());
        }
        ifaces.extend(node.macvlans.iter().map(|m| m.name.clone()));
        ifaces.extend(node.ipvlans.iter().map(|i| i.name.clone()));
        ifaces.extend(node.wifi.iter().map(|w| w.name.clone()));
        // Bond members stay down here: the kernel refuses to enslave an
        // up device, and the Stack stage enslaves them, after which the
        // bonding driver opens them itself. nlink 0.30 (#406) would also
        // take an up port down to join, but leaving it down avoids an
        // up→down→up flap.
        let bond_members: std::collections::BTreeSet<&String> = node
            .interfaces
            .values()
            .filter(|i| i.kind == Some(crate::types::InterfaceKind::Bond))
            .flat_map(|i| i.members.iter())
            .collect();
        ifaces.retain(|i| !bond_members.contains(i));
        ifaces.sort();
        ifaces.dedup();
        if ifaces.is_empty() {
            continue;
        }
        ops.push(Op::LinksUp {
            node: node_name.clone(),
            ifaces,
        });
    }

    // ── sysctls ──
    for (node_name, node) in topology.nodes.iter().filter(|(_, n)| !n.is_foreign()) {
        let mut entries: BTreeMap<String, String> = topology.effective_sysctls(node);
        // A declared link's own keys ride on its declaration in `Stack`
        // (#160): the link does not exist yet here.
        entries.retain(|k, v| super::network::link_devconf(node, k, v).is_none());
        if !entries.is_empty() {
            ops.push(Op::Sysctls {
                node: node_name.clone(),
                entries,
            });
        }
    }

    // ── dns ──
    if topology.lab.dns == DnsMode::Hosts
        && !crate::dns::generate_hosts_entries(topology).is_empty()
    {
        ops.push(Op::DnsInject {
            lab: topology.lab.name.clone(),
        });
        for (node_name, node) in &topology.nodes {
            if node.image.is_none() && !node.is_foreign() {
                ops.push(Op::DnsNetnsEtc {
                    node: node_name.clone(),
                    ns: topology.namespace_name(node_name),
                });
            }
        }
    }

    Ok(ops)
}
