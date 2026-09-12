//! Pure planner for the imperative residue of a deploy: namespaces,
//! containers, hwsim, the mgmt bridge, bridge networks, veths, host-side
//! macvlan/ipvlan, link bring-up, sysctls and DNS overlays.

use std::collections::BTreeMap;

use crate::deploy::op::{Op, PortVlans};
use crate::error::{Error, Result};
use crate::helpers::parse_cidr;
use crate::types::{DnsMode, EndpointRef, Topology};

use super::process::build_create_opts;

/// Ops for `Stage::Namespaces` … `Stage::Dns` (everything except the
/// per-node stack, TC and processes, which have their own planners).
pub(crate) fn plan_topology(topology: &Topology, dns_extra_hosts: &[String]) -> Result<Vec<Op>> {
    let mut ops = Vec::new();

    // ── namespaces / containers ──
    for (node_name, node) in &topology.nodes {
        if let Some(image) = &node.image {
            ops.push(Op::CreateContainer {
                node: node_name.clone(),
                name: format!("{}-{}", topology.lab.prefix(), node_name),
                image: image.clone(),
                pull: node.pull.clone(),
                opts: build_create_opts(node, dns_extra_hosts),
            });
        } else {
            ops.push(Op::CreateNamespace {
                node: node_name.clone(),
                ns: topology.namespace_name(node_name),
            });
        }
    }

    // ── hwsim ──
    let radios = crate::wifi::count_wifi_nodes(topology);
    if radios > 0 {
        let mut order = Vec::new();
        for (node_name, node) in &topology.nodes {
            for wifi in &node.wifi {
                order.push((node_name.clone(), wifi.name.clone()));
            }
        }
        ops.push(Op::LoadHwsim { radios, order });
    }

    // ── host-reachable mgmt bridge ──
    if topology.lab.mgmt_host_reachable
        && let Some(mgmt_subnet) = &topology.lab.mgmt_subnet
    {
        let (base_ip, prefix) = parse_cidr(mgmt_subnet)?;
        let std::net::IpAddr::V4(base_v4) = base_ip else {
            return Err(Error::deploy_failed("mgmt subnet must be IPv4"));
        };
        let base_v4 = match crate::helpers::network_address(std::net::IpAddr::V4(base_v4), prefix) {
            std::net::IpAddr::V4(v) => v,
            std::net::IpAddr::V6(_) => unreachable!(),
        };
        let base = u32::from(base_v4);
        let usable_hosts = (1u64 << (32 - prefix.min(32) as u32)).saturating_sub(2);
        let node_count = topology.nodes.len() as u64;
        if node_count + 1 > usable_hosts {
            return Err(Error::deploy_failed(format!(
                "mgmt subnet {mgmt_subnet} has {usable_hosts} usable host address(es) but the bridge plus {node_count} nodes need {}",
                node_count + 1
            )));
        }
        let bridge = topology.lab.mgmt_bridge_name();
        ops.push(Op::CreateMgmtBridge {
            name: bridge.clone(),
            ip: std::net::Ipv4Addr::from(base + 1),
            prefix,
        });
        // nodes in name order: .2, .3, … (BTreeMap iterates sorted)
        for (idx, node_name) in topology.nodes.keys().enumerate() {
            ops.push(Op::CreateMgmtVeth {
                node: node_name.clone(),
                peer: topology.lab.mgmt_peer_name(idx),
                bridge: bridge.clone(),
                node_ip: std::net::Ipv4Addr::from(base + 2 + idx as u32),
                prefix,
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
                });
            }
        }
    }

    // ── point-to-point links ──
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
        let mut ifaces: Vec<String> = vec!["lo".to_string()];
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
        if topology.lab.mgmt_host_reachable && topology.lab.mgmt_subnet.is_some() {
            ifaces.push("mgmt0".to_string());
        }
        ifaces.extend(node.macvlans.iter().map(|m| m.name.clone()));
        ifaces.extend(node.ipvlans.iter().map(|i| i.name.clone()));
        ifaces.extend(node.wifi.iter().map(|w| w.name.clone()));
        ifaces.sort();
        ifaces.dedup();
        ops.push(Op::LinksUp {
            node: node_name.clone(),
            ifaces,
        });
    }

    // ── sysctls ──
    for (node_name, node) in &topology.nodes {
        let entries: BTreeMap<String, String> = topology.effective_sysctls(node);
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
            if node.image.is_none() {
                ops.push(Op::DnsNetnsEtc {
                    node: node_name.clone(),
                    ns: topology.namespace_name(node_name),
                });
            }
        }
    }

    Ok(ops)
}
