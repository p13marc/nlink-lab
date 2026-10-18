//! A topology as a graph to draw: nodes by kind, the networks as hubs,
//! the host's interfaces a lab reaches, sites as regions, and every way
//! two of them are joined.

use std::collections::{BTreeMap, BTreeSet};

use nlink_lab::Topology;
use nlink_lab::types::InterfaceKind;

/// What a node of the drawing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    /// A namespace node.
    Host,
    /// A node that forwards (`forward ipv4`, an `ip_forward` sysctl, FRR).
    Router,
    /// A container node.
    Container,
    /// A network: a bridge its members hang off.
    Bridge,
    /// An interface of the host itself (a macvlan's or ipvlan's parent).
    External,
}

/// A node of the drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GNode {
    /// The lab's node name; `net:NAME` for a network, `nic:NAME` for a host
    /// interface, `mgmt` for the management network.
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub site: Option<String>,
    pub image: Option<String>,
}

/// How two nodes are joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// A veth pair between two nodes.
    Link,
    /// A node's interface on a network.
    Port,
    /// WireGuard peers (drawn dashed).
    Tunnel,
    /// A VXLAN between two nodes (drawn dashed).
    Vxlan,
    /// A macvlan or ipvlan on a host interface.
    HostLink,
    /// A node's interface on the management network (hidden by default).
    Mgmt,
}

/// One end of an edge: a node, and its interface when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    pub node: String,
    pub iface: Option<String>,
}

impl End {
    pub(crate) fn of(node: &str, iface: Option<&str>) -> Self {
        Self {
            node: node.to_string(),
            iface: iface.map(str::to_string),
        }
    }

    /// `node:iface`, the topology's name for the endpoint.
    pub fn endpoint(&self) -> Option<String> {
        self.iface.as_ref().map(|i| format!("{}:{i}", self.node))
    }
}

/// An edge of the drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GEdge {
    /// Unique within the graph, stable across rebuilds of one topology.
    pub id: String,
    pub kind: EdgeKind,
    pub a: End,
    pub b: End,
    /// The addresses at each end, for a link.
    pub addresses: Option<[String; 2]>,
}

/// A site: the nodes `site NAME { … }` grouped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub id: String,
    pub nodes: Vec<String>,
}

/// What the graph shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphOptions {
    /// The management network and its ports.
    pub show_mgmt: bool,
}

/// A topology as nodes, edges and regions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopoGraph {
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    pub regions: Vec<Region>,
}

/// `node:iface` split.
fn split(endpoint: &str) -> Option<(&str, &str)> {
    endpoint.split_once(':')
}

/// The address without its prefix length.
fn host_part(cidr: &str) -> &str {
    cidr.split('/').next().unwrap_or(cidr)
}

fn forwards(t: &Topology, name: &str, node: &nlink_lab::types::Node) -> bool {
    let on = |sysctls: &BTreeMap<String, String>| {
        [
            "net.ipv4.ip_forward",
            "net.ipv4.conf.all.forwarding",
            "net.ipv6.conf.all.forwarding",
        ]
        .iter()
        .any(|k| sysctls.get(*k).is_some_and(|v| v == "1"))
    };
    let _ = name;
    node.frr.is_some()
        || on(&node.sysctls)
        || node.profiles.iter().any(|p| {
            t.profiles
                .get(p)
                .is_some_and(|profile| on(&profile.sysctls) || profile.frr.is_some())
        })
}

impl TopoGraph {
    /// The graph of `t`.
    pub fn from_topology(t: &Topology, opts: &GraphOptions) -> Self {
        let site_of: BTreeMap<&str, &str> = t
            .sites
            .iter()
            .flat_map(|(site, nodes)| nodes.iter().map(move |n| (n.as_str(), site.as_str())))
            .collect();
        let mut g = TopoGraph::default();
        for (name, node) in &t.nodes {
            let kind = if node.image.is_some() {
                NodeKind::Container
            } else if forwards(t, name, node) {
                NodeKind::Router
            } else {
                NodeKind::Host
            };
            g.nodes.push(GNode {
                id: name.clone(),
                kind,
                label: name.clone(),
                site: site_of.get(name.as_str()).map(|s| s.to_string()),
                image: node.image.clone(),
            });
        }
        for (i, link) in t.links.iter().enumerate() {
            let (Some((an, ai)), Some((bn, bi))) =
                (split(&link.endpoints[0]), split(&link.endpoints[1]))
            else {
                continue;
            };
            g.edges.push(GEdge {
                id: format!("link:{i}"),
                kind: EdgeKind::Link,
                a: End::of(an, Some(ai)),
                b: End::of(bn, Some(bi)),
                addresses: link.addresses.clone(),
            });
        }
        for (name, network) in &t.networks {
            let hub = format!("net:{name}");
            g.nodes.push(GNode {
                id: hub.clone(),
                kind: NodeKind::Bridge,
                label: name.clone(),
                site: None,
                image: None,
            });
            for member in &network.members {
                if let Some((node, iface)) = split(member) {
                    g.edges.push(GEdge {
                        id: format!("port:{name}:{member}"),
                        kind: EdgeKind::Port,
                        a: End::of(node, Some(iface)),
                        b: End::of(&hub, None),
                        addresses: None,
                    });
                }
            }
        }
        // WireGuard: one dashed edge per pair of peers
        let mut tunnels = BTreeSet::new();
        for (name, node) in &t.nodes {
            for (iface, wg) in &node.wireguard {
                for peer in &wg.peers {
                    if !t.nodes.contains_key(peer) {
                        continue;
                    }
                    let pair = if name < peer {
                        (name.clone(), peer.clone())
                    } else {
                        (peer.clone(), name.clone())
                    };
                    if tunnels.insert(pair.clone()) {
                        g.edges.push(GEdge {
                            id: format!("wg:{}:{}", pair.0, pair.1),
                            kind: EdgeKind::Tunnel,
                            a: End::of(name, Some(iface)),
                            b: End::of(peer, None),
                            addresses: None,
                        });
                    }
                }
            }
        }
        // VXLAN: to the node that owns the remote address
        let owner: BTreeMap<&str, &str> = t
            .links
            .iter()
            .flat_map(|l| {
                l.addresses.iter().flat_map(move |a| {
                    l.endpoints
                        .iter()
                        .zip(a.iter())
                        .filter_map(|(ep, addr)| Some((host_part(addr), split(ep)?.0)))
                })
            })
            .chain(t.nodes.iter().flat_map(|(n, node)| {
                node.interfaces
                    .values()
                    .flat_map(move |i| i.addresses.iter().map(move |a| (host_part(a), n.as_str())))
            }))
            .collect();
        let mut vxlans = BTreeSet::new();
        for (name, node) in &t.nodes {
            for (iface, cfg) in &node.interfaces {
                if !matches!(cfg.kind, Some(InterfaceKind::Vxlan)) {
                    continue;
                }
                let Some(peer) = cfg.remote.as_deref().and_then(|r| owner.get(host_part(r))) else {
                    continue;
                };
                if *peer == name {
                    continue;
                }
                let pair = if name.as_str() < *peer {
                    (name.clone(), peer.to_string())
                } else {
                    (peer.to_string(), name.clone())
                };
                if vxlans.insert(pair.clone()) {
                    g.edges.push(GEdge {
                        id: format!("vxlan:{}:{}", pair.0, pair.1),
                        kind: EdgeKind::Vxlan,
                        a: End::of(name, Some(iface)),
                        b: End::of(peer, None),
                        addresses: None,
                    });
                }
            }
        }
        // the host's interfaces a node reaches
        let mut nics = BTreeSet::new();
        for (name, node) in &t.nodes {
            let parents = node
                .macvlans
                .iter()
                .map(|m| (&m.name, &m.parent))
                .chain(node.ipvlans.iter().map(|m| (&m.name, &m.parent)));
            for (iface, parent) in parents {
                let id = format!("nic:{parent}");
                if nics.insert(parent.clone()) {
                    g.nodes.push(GNode {
                        id: id.clone(),
                        kind: NodeKind::External,
                        label: parent.clone(),
                        site: None,
                        image: None,
                    });
                }
                g.edges.push(GEdge {
                    id: format!("host:{name}:{iface}"),
                    kind: EdgeKind::HostLink,
                    a: End::of(name, Some(iface)),
                    b: End::of(&id, None),
                    addresses: None,
                });
            }
        }
        if opts.show_mgmt && t.lab.mgmt_subnet.is_some() {
            g.nodes.push(GNode {
                id: "mgmt".into(),
                kind: NodeKind::Bridge,
                label: "mgmt".into(),
                site: None,
                image: None,
            });
            for name in t.nodes.keys() {
                g.edges.push(GEdge {
                    id: format!("mgmt:{name}"),
                    kind: EdgeKind::Mgmt,
                    a: End::of(name, Some("mgmt0")),
                    b: End::of("mgmt", None),
                    addresses: None,
                });
            }
        }
        g.regions = t
            .sites
            .iter()
            .map(|(site, nodes)| Region {
                id: site.clone(),
                nodes: nodes.clone(),
            })
            .collect();
        g
    }

    pub fn node(&self, id: &str) -> Option<&GNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn edge(&self, id: &str) -> Option<&GEdge> {
        self.edges.iter().find(|e| e.id == id)
    }

    /// The nodes joined to `id`, each once.
    pub fn neighbors(&self, id: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .edges
            .iter()
            .filter_map(|e| {
                if e.a.node == id {
                    Some(e.b.node.as_str())
                } else if e.b.node == id {
                    Some(e.a.node.as_str())
                } else {
                    None
                }
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// How many edges `id` has.
    pub fn degree(&self, id: &str) -> usize {
        self.edges
            .iter()
            .filter(|e| e.a.node == id || e.b.node == id)
            .count()
    }

    /// The edge on the endpoint `node:iface`, if one is.
    pub fn edge_at(&self, endpoint: &str) -> Option<&GEdge> {
        self.edges.iter().find(|e| {
            e.a.endpoint().as_deref() == Some(endpoint)
                || e.b.endpoint().as_deref() == Some(endpoint)
        })
    }
}

#[cfg(test)]
pub(crate) fn sample() -> Topology {
    nlink_lab::parser::parse(
        r#"lab "g"
profile router { forward ipv4 }
node r1 : router
node r2 : router
node h1
node h2
node c1 { image "alpine" }
link r1:eth0 -- r2:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link r1:eth1 -- h1:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 }
link r1:eth2 -- r2:eth2 { 10.2.0.1/24 -- 10.2.0.2/24 }
network lan {
  subnet 10.9.0.0/24
  members [r2:eth1, h2:eth0, c1:eth0]
}
"#,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_have_their_kind_and_networks_are_hubs() {
        let g = TopoGraph::from_topology(&sample(), &GraphOptions::default());
        assert_eq!(g.node("r1").unwrap().kind, NodeKind::Router);
        assert_eq!(g.node("h1").unwrap().kind, NodeKind::Host);
        assert_eq!(g.node("c1").unwrap().kind, NodeKind::Container);
        assert_eq!(g.node("net:lan").unwrap().kind, NodeKind::Bridge);
        assert_eq!(g.degree("net:lan"), 3);
        assert_eq!(g.neighbors("r1"), vec!["h1", "r2"]);
        // two links between r1 and r2: two edges
        let r1r2 = g
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Link && e.a.node == "r1" && e.b.node == "r2")
            .count();
        assert_eq!(r1r2, 2);
        assert_eq!(g.edge_at("h2:eth0").unwrap().kind, EdgeKind::Port);
        assert!(g.node("mgmt").is_none());
    }

    #[test]
    fn tunnels_and_host_interfaces_are_edges_too() {
        let t = nlink_lab::parser::parse(
            r#"lab "t"
node a {
  vxlan vx0 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
  }
  macvlan m0 parent "eth9" mode bridge {
    192.168.1.100/24
  }
}
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let g = TopoGraph::from_topology(&t, &GraphOptions::default());
        let vx = g
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Vxlan)
            .expect("a vxlan edge");
        assert_eq!((vx.a.node.as_str(), vx.b.node.as_str()), ("a", "b"));
        assert_eq!(g.node("nic:eth9").unwrap().kind, NodeKind::External);
        assert!(g.edges.iter().any(|e| e.kind == EdgeKind::HostLink));
    }
}
