//! Canonical "node → IP address" lookup derived from a [`Topology`].
//!
//! Several engines need to turn a node name into an address they can
//! `ping` / connect to: the post-deploy `validate` assertions, the CI
//! test runner, the scenario engine, and the benchmark engine. Each used
//! to carry its own copy of the lookup, and every copy only looked at
//! point-to-point `link` addresses — so on a topology built from
//! `network` (bridge) blocks every assertion logged `SKIP: no IP found`
//! (issue #34).
//!
//! This module is the single source of truth. It walks every place the
//! parser can put an address on a node, in a deterministic order:
//!
//! 1. `link` endpoint addresses (in declaration order),
//! 2. `network.ports[node:iface].addresses` (networks and ports sorted
//!    by name — both are `BTreeMap`s),
//! 3. `node.interfaces[*].addresses` — dummies, VXLANs, VLANs, bonds,
//!    `lo` (sorted by interface name),
//! 4. `node.wireguard[*].addresses` (sorted by interface name),
//! 5. `node.macvlans` / `node.ipvlans` / `node.wifi` (in declaration
//!    order).
//!
//! The order matters because [`build_ip_map`] keeps the *first* address
//! per node. Links come first so the historical behaviour ("first link
//! address wins", relied on by the `latency-under` and `no-reach`
//! assertions in existing topologies) is preserved; the new sources only
//! kick in for nodes that have no link address at all.
//!
//! `dns.rs` collects the same link + network sources for `/etc/hosts`
//! generation; keep the two in step if you add a source here.

use std::collections::BTreeMap;

use crate::types::{EndpointRef, Topology};

/// One address assigned to one interface of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAddr {
    /// Bare address with the prefix length stripped (`10.0.0.1`).
    pub ip: String,
    /// Address as written in the topology, including any prefix
    /// length (`10.0.0.1/24`).
    pub cidr: String,
    /// Interface the address lives on (`eth0`, `wg0`, `lo`, …).
    pub iface: String,
}

/// Collect every address of every node, keyed by node name.
///
/// The outer map is a `BTreeMap` so callers iterating it get a stable
/// order; the inner `Vec` follows the source order documented in the
/// module docs, so `[0]` is the same address [`build_ip_map`] returns.
/// Nodes without any address are absent from the map.
pub fn collect_node_addrs(topology: &Topology) -> BTreeMap<String, Vec<NodeAddr>> {
    let mut out: BTreeMap<String, Vec<NodeAddr>> = BTreeMap::new();

    let mut push = |node: &str, iface: &str, cidr: &str| {
        let ip = strip_prefix_len(cidr);
        if ip.is_empty() {
            return;
        }
        out.entry(node.to_string()).or_default().push(NodeAddr {
            ip: ip.to_string(),
            cidr: cidr.to_string(),
            iface: iface.to_string(),
        });
    };

    // 1. Point-to-point links.
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for (ep, addr) in link.endpoints.iter().zip(addrs.iter()) {
                if let Some(ep_ref) = EndpointRef::parse(ep) {
                    push(&ep_ref.node, &ep_ref.iface, addr);
                }
            }
        }
    }

    // 2. Bridge network ports. Keys are `node:iface`; a bare `node`
    //    key (legacy `port host1 { pvid … }` syntax) carries no
    //    addresses today, but tolerate it by falling back to the
    //    port's `interface` field.
    let mut networks: Vec<_> = topology.networks.iter().collect();
    networks.sort_by(|a, b| a.0.cmp(b.0));
    for (_, network) in networks {
        let mut ports: Vec<_> = network.ports.iter().collect();
        ports.sort_by(|a, b| a.0.cmp(b.0));
        for (key, port) in ports {
            let (node, iface) = match EndpointRef::parse(key) {
                Some(ep) => (ep.node, ep.iface),
                None => match &port.interface {
                    Some(iface) => (key.clone(), iface.clone()),
                    None => continue,
                },
            };
            for addr in &port.addresses {
                push(&node, &iface, addr);
            }
        }
    }

    // 3–5. Per-node interface families.
    let mut nodes: Vec<_> = topology.nodes.iter().collect();
    nodes.sort_by(|a, b| a.0.cmp(b.0));
    for (node_name, node) in nodes {
        let mut ifaces: Vec<_> = node.interfaces.iter().collect();
        ifaces.sort_by(|a, b| a.0.cmp(b.0));
        for (iface, cfg) in ifaces {
            for addr in &cfg.addresses {
                push(node_name, iface, addr);
            }
        }

        let mut wgs: Vec<_> = node.wireguard.iter().collect();
        wgs.sort_by(|a, b| a.0.cmp(b.0));
        for (iface, cfg) in wgs {
            for addr in &cfg.addresses {
                push(node_name, iface, addr);
            }
        }

        for mv in &node.macvlans {
            for addr in &mv.addresses {
                push(node_name, &mv.name, addr);
            }
        }
        for iv in &node.ipvlans {
            for addr in &iv.addresses {
                push(node_name, &iv.name, addr);
            }
        }
        for wifi in &node.wifi {
            for addr in &wifi.addresses {
                push(node_name, &wifi.name, addr);
            }
        }
    }

    out
}

/// Build the "first address per node" map used by the assertion,
/// scenario, and benchmark engines to pick a ping / connect target.
///
/// Values are bare addresses without a prefix length. Source priority
/// is documented on the module; see [`collect_node_addrs`] for the
/// full per-interface list.
pub fn build_ip_map(topology: &Topology) -> BTreeMap<String, String> {
    collect_node_addrs(topology)
        .into_iter()
        .filter_map(|(node, addrs)| addrs.into_iter().next().map(|a| (node, a.ip)))
        .collect()
}

/// `10.0.0.1/24` → `10.0.0.1`; already-bare input is returned unchanged.
fn strip_prefix_len(addr: &str) -> &str {
    addr.split('/').next().unwrap_or(addr).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Topology {
        crate::parser::parse(src).unwrap()
    }

    #[test]
    fn test_build_ip_map_links() {
        let topo = parse(
            r#"
lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("a").unwrap(), "10.0.0.1");
        assert_eq!(ip_map.get("b").unwrap(), "10.0.0.2");
    }

    #[test]
    fn test_build_ip_map_multi_homed_first_link_wins() {
        let topo = parse(
            r#"
lab "t"
node r
node a
node b
link r:eth0 -- a:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
link r:eth1 -- b:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("r").unwrap(), "10.0.1.1");

        let all = collect_node_addrs(&topo);
        let r: Vec<(&str, &str)> = all["r"]
            .iter()
            .map(|a| (a.iface.as_str(), a.ip.as_str()))
            .collect();
        assert_eq!(r, vec![("eth0", "10.0.1.1"), ("eth1", "10.0.2.1")]);
        assert_eq!(all["r"][0].cidr, "10.0.1.1/24");
    }

    /// Issue #34: a bridge-`network` topology has no `link` blocks, so
    /// the old per-engine copies found nothing and every assertion
    /// was skipped.
    #[test]
    fn test_build_ip_map_network_ports() {
        let topo = parse(
            r#"
lab "t"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.0.1.0/24
}
validate {
  reach a b
}
"#,
        );
        assert!(topo.links.is_empty(), "test premise: bridge-only topology");
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("a").unwrap(), "10.0.1.1");
        assert_eq!(ip_map.get("b").unwrap(), "10.0.1.2");
        assert_eq!(ip_map.get("c").unwrap(), "10.0.1.3");
        assert_eq!(collect_node_addrs(&topo)["b"][0].iface, "eth0");
    }

    #[test]
    fn test_build_ip_map_dummy_lo_wireguard_wifi() {
        let topo = parse(
            r#"
lab "t"
node d { lo 10.255.0.1/32 }
node v {
  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    address 192.168.100.1/24
  }
}
node w {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
  }
}
node ap {
  wifi wlan0 mode ap {
    ssid "testnet"
    10.9.0.1/24
  }
}
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("d").unwrap(), "10.255.0.1");
        assert_eq!(ip_map.get("v").unwrap(), "192.168.100.1");
        assert_eq!(ip_map.get("w").unwrap(), "192.168.255.1");
        assert_eq!(ip_map.get("ap").unwrap(), "10.9.0.1");

        let all = collect_node_addrs(&topo);
        assert_eq!(all["d"][0].iface, "lo");
        assert_eq!(all["v"][0].iface, "vxlan100");
        assert_eq!(all["w"][0].iface, "wg0");
        assert_eq!(all["ap"][0].iface, "wlan0");
    }

    #[test]
    fn test_build_ip_map_macvlan_ipvlan() {
        let topo = parse(
            r#"
lab "t"
node gw {
  macvlan eth0 parent "enp3s0" mode bridge {
    192.168.1.100/24
  }
}
node iv {
  ipvlan eth0 parent "enp3s0" mode l2 {
    192.168.1.101/24
  }
}
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("gw").unwrap(), "192.168.1.100");
        assert_eq!(ip_map.get("iv").unwrap(), "192.168.1.101");
    }

    /// A link address, when present, must still win over the node's
    /// other interfaces so existing topologies keep their targets.
    #[test]
    fn test_build_ip_map_link_beats_loopback_and_network() {
        let topo = parse(
            r#"
lab "t"
node r { lo 10.255.0.1/32 }
node h
network lan {
  members [r:eth1, h:eth0]
  subnet 10.0.9.0/24
}
link r:eth0 -- h:eth1 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert_eq!(ip_map.get("r").unwrap(), "10.0.0.1");
        assert_eq!(ip_map.get("h").unwrap(), "10.0.0.2");

        let all = collect_node_addrs(&topo);
        let r_ifaces: Vec<&str> = all["r"].iter().map(|a| a.iface.as_str()).collect();
        assert_eq!(r_ifaces, vec!["eth0", "eth1", "lo"]);
    }

    #[test]
    fn test_build_ip_map_node_without_address_is_absent() {
        let topo = parse(
            r#"
lab "t"
node a
node b
link a:eth0 -- b:eth0
"#,
        );
        let ip_map = build_ip_map(&topo);
        assert!(ip_map.is_empty());
        assert!(collect_node_addrs(&topo).is_empty());
    }

    #[test]
    fn test_strip_prefix_len() {
        assert_eq!(strip_prefix_len("10.0.0.1/24"), "10.0.0.1");
        assert_eq!(strip_prefix_len("10.0.0.1"), "10.0.0.1");
        assert_eq!(strip_prefix_len("fd00::1/64"), "fd00::1");
    }
}
