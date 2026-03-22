//! Programmatic topology builder DSL.
//!
//! Build topologies in Rust code instead of TOML files. Useful for integration
//! tests and dynamic topology generation.
//!
//! # Example
//!
//! ```
//! use nlink_lab::builder::Lab;
//!
//! let topology = Lab::new("my-lab")
//!     .profile("router", |p| p
//!         .sysctl("net.ipv4.ip_forward", "1"))
//!     .node("r1", |n| n
//!         .profile("router")
//!         .route("default", |r| r.via("10.0.1.1")))
//!     .node("h1", |n| n
//!         .route("default", |r| r.via("10.0.0.1")))
//!     .link("r1:eth0", "h1:eth0", |l| l
//!         .addresses("10.0.0.1/24", "10.0.0.2/24"))
//!     .impair("r1:eth0", |i| i.delay("10ms"))
//!     .build();
//!
//! assert_eq!(topology.lab.name, "my-lab");
//! assert_eq!(topology.nodes.len(), 2);
//! ```

use crate::types::{
    ExecConfig, FirewallConfig, FirewallRule, Impairment, InterfaceConfig, LabConfig, Link,
    Network, Node, PortConfig, Profile, RateLimit, RouteConfig, Topology, VlanConfig, VrfConfig,
    WireguardConfig,
};

/// Top-level lab builder.
pub struct Lab {
    topology: Topology,
}

impl Lab {
    /// Create a new lab with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            topology: Topology {
                lab: LabConfig {
                    name: name.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// Set the lab description.
    pub fn description(mut self, desc: &str) -> Self {
        self.topology.lab.description = Some(desc.to_string());
        self
    }

    /// Set the namespace prefix (defaults to lab name).
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.topology.lab.prefix = Some(prefix.to_string());
        self
    }

    /// Add a reusable profile.
    pub fn profile(mut self, name: &str, f: impl FnOnce(ProfileBuilder) -> ProfileBuilder) -> Self {
        let builder = f(ProfileBuilder::new());
        self.topology
            .profiles
            .insert(name.to_string(), builder.build());
        self
    }

    /// Add a node.
    pub fn node(mut self, name: &str, f: impl FnOnce(NodeBuilder) -> NodeBuilder) -> Self {
        let builder = f(NodeBuilder::new());
        self.topology
            .nodes
            .insert(name.to_string(), builder.build());
        self
    }

    /// Add a point-to-point link between two endpoints.
    pub fn link(
        mut self,
        ep1: &str,
        ep2: &str,
        f: impl FnOnce(LinkBuilder) -> LinkBuilder,
    ) -> Self {
        let builder = f(LinkBuilder::new(ep1, ep2));
        self.topology.links.push(builder.build());
        self
    }

    /// Add a shared L2 network (bridge).
    pub fn network(
        mut self,
        name: &str,
        f: impl FnOnce(NetworkBuilder) -> NetworkBuilder,
    ) -> Self {
        let builder = f(NetworkBuilder::new());
        self.topology
            .networks
            .insert(name.to_string(), builder.build());
        self
    }

    /// Add a netem impairment to an endpoint.
    pub fn impair(
        mut self,
        endpoint: &str,
        f: impl FnOnce(ImpairmentBuilder) -> ImpairmentBuilder,
    ) -> Self {
        let builder = f(ImpairmentBuilder::new());
        self.topology
            .impairments
            .insert(endpoint.to_string(), builder.build());
        self
    }

    /// Add rate limiting to an endpoint.
    pub fn rate_limit(
        mut self,
        endpoint: &str,
        f: impl FnOnce(RateLimitBuilder) -> RateLimitBuilder,
    ) -> Self {
        let builder = f(RateLimitBuilder::new());
        self.topology
            .rate_limits
            .insert(endpoint.to_string(), builder.build());
        self
    }

    /// Finalize and return the topology.
    pub fn build(self) -> Topology {
        self.topology
    }
}

// ─────────────────────────────────────────────────
// Profile builder
// ─────────────────────────────────────────────────

/// Builder for [`Profile`].
pub struct ProfileBuilder {
    profile: Profile,
}

impl ProfileBuilder {
    fn new() -> Self {
        Self {
            profile: Profile::default(),
        }
    }

    /// Add a sysctl key-value pair.
    pub fn sysctl(mut self, key: &str, value: &str) -> Self {
        self.profile
            .sysctls
            .insert(key.to_string(), value.to_string());
        self
    }

    /// Set the firewall configuration.
    pub fn firewall(mut self, f: impl FnOnce(FirewallBuilder) -> FirewallBuilder) -> Self {
        let builder = f(FirewallBuilder::new());
        self.profile.firewall = Some(builder.build());
        self
    }

    fn build(self) -> Profile {
        self.profile
    }
}

// ─────────────────────────────────────────────────
// Node builder
// ─────────────────────────────────────────────────

/// Builder for [`Node`].
pub struct NodeBuilder {
    node: Node,
}

impl NodeBuilder {
    fn new() -> Self {
        Self {
            node: Node::default(),
        }
    }

    /// Set the profile to inherit from.
    pub fn profile(mut self, name: &str) -> Self {
        self.node.profile = Some(name.to_string());
        self
    }

    /// Add a sysctl key-value pair.
    pub fn sysctl(mut self, key: &str, value: &str) -> Self {
        self.node
            .sysctls
            .insert(key.to_string(), value.to_string());
        self
    }

    /// Add an explicit interface.
    pub fn interface(
        mut self,
        name: &str,
        f: impl FnOnce(InterfaceBuilder) -> InterfaceBuilder,
    ) -> Self {
        let builder = f(InterfaceBuilder::new());
        self.node
            .interfaces
            .insert(name.to_string(), builder.build());
        self
    }

    /// Add a route.
    pub fn route(
        mut self,
        dest: &str,
        f: impl FnOnce(RouteBuilder) -> RouteBuilder,
    ) -> Self {
        let builder = f(RouteBuilder::new());
        self.node
            .routes
            .insert(dest.to_string(), builder.build());
        self
    }

    /// Set the firewall configuration.
    pub fn firewall(mut self, f: impl FnOnce(FirewallBuilder) -> FirewallBuilder) -> Self {
        let builder = f(FirewallBuilder::new());
        self.node.firewall = Some(builder.build());
        self
    }

    /// Add a background process.
    pub fn exec_background(mut self, cmd: &[&str]) -> Self {
        self.node.exec.push(ExecConfig {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            background: true,
        });
        self
    }

    /// Add a foreground process (runs during deploy).
    pub fn exec(mut self, cmd: &[&str]) -> Self {
        self.node.exec.push(ExecConfig {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            background: false,
        });
        self
    }

    /// Add a VRF.
    pub fn vrf(mut self, name: &str, f: impl FnOnce(VrfBuilder) -> VrfBuilder) -> Self {
        let builder = f(VrfBuilder::new());
        self.node.vrfs.insert(name.to_string(), builder.build());
        self
    }

    /// Add a WireGuard interface.
    pub fn wireguard(
        mut self,
        name: &str,
        f: impl FnOnce(WireguardBuilder) -> WireguardBuilder,
    ) -> Self {
        let builder = f(WireguardBuilder::new());
        self.node
            .wireguard
            .insert(name.to_string(), builder.build());
        self
    }

    fn build(self) -> Node {
        self.node
    }
}

// ─────────────────────────────────────────────────
// Interface builder
// ─────────────────────────────────────────────────

/// Builder for [`InterfaceConfig`].
pub struct InterfaceBuilder {
    config: InterfaceConfig,
}

impl InterfaceBuilder {
    fn new() -> Self {
        Self {
            config: InterfaceConfig::default(),
        }
    }

    /// Set the interface kind (vxlan, bond, dummy, etc.).
    pub fn kind(mut self, kind: &str) -> Self {
        self.config.kind = Some(kind.to_string());
        self
    }

    /// Add an address in CIDR notation.
    pub fn address(mut self, cidr: &str) -> Self {
        self.config.addresses.push(cidr.to_string());
        self
    }

    /// Set the VXLAN VNI.
    pub fn vni(mut self, vni: u32) -> Self {
        self.config.vni = Some(vni);
        self
    }

    /// Set the local tunnel address.
    pub fn local(mut self, addr: &str) -> Self {
        self.config.local = Some(addr.to_string());
        self
    }

    /// Set the remote tunnel address.
    pub fn remote(mut self, addr: &str) -> Self {
        self.config.remote = Some(addr.to_string());
        self
    }

    /// Set the destination port.
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = Some(port);
        self
    }

    /// Set the MTU.
    pub fn mtu(mut self, mtu: u32) -> Self {
        self.config.mtu = Some(mtu);
        self
    }

    fn build(self) -> InterfaceConfig {
        self.config
    }
}

// ─────────────────────────────────────────────────
// Route builder
// ─────────────────────────────────────────────────

/// Builder for [`RouteConfig`].
pub struct RouteBuilder {
    config: RouteConfig,
}

impl RouteBuilder {
    fn new() -> Self {
        Self {
            config: RouteConfig::default(),
        }
    }

    /// Set the next-hop gateway.
    pub fn via(mut self, gateway: &str) -> Self {
        self.config.via = Some(gateway.to_string());
        self
    }

    /// Set the output device.
    pub fn dev(mut self, dev: &str) -> Self {
        self.config.dev = Some(dev.to_string());
        self
    }

    /// Set the route metric.
    pub fn metric(mut self, metric: u32) -> Self {
        self.config.metric = Some(metric);
        self
    }

    fn build(self) -> RouteConfig {
        self.config
    }
}

// ─────────────────────────────────────────────────
// Link builder
// ─────────────────────────────────────────────────

/// Builder for [`Link`].
pub struct LinkBuilder {
    link: Link,
}

impl LinkBuilder {
    fn new(ep1: &str, ep2: &str) -> Self {
        Self {
            link: Link {
                endpoints: [ep1.to_string(), ep2.to_string()],
                addresses: None,
                mtu: None,
            },
        }
    }

    /// Set addresses for both endpoints in CIDR notation.
    pub fn addresses(mut self, addr1: &str, addr2: &str) -> Self {
        self.link.addresses = Some([addr1.to_string(), addr2.to_string()]);
        self
    }

    /// Set the MTU for both ends.
    pub fn mtu(mut self, mtu: u32) -> Self {
        self.link.mtu = Some(mtu);
        self
    }

    fn build(self) -> Link {
        self.link
    }
}

// ─────────────────────────────────────────────────
// Network builder
// ─────────────────────────────────────────────────

/// Builder for [`Network`].
pub struct NetworkBuilder {
    network: Network,
}

impl NetworkBuilder {
    fn new() -> Self {
        Self {
            network: Network::default(),
        }
    }

    /// Set the network kind (default: "bridge").
    pub fn kind(mut self, kind: &str) -> Self {
        self.network.kind = Some(kind.to_string());
        self
    }

    /// Enable VLAN filtering.
    pub fn vlan_filtering(mut self, enabled: bool) -> Self {
        self.network.vlan_filtering = Some(enabled);
        self
    }

    /// Set the MTU.
    pub fn mtu(mut self, mtu: u32) -> Self {
        self.network.mtu = Some(mtu);
        self
    }

    /// Set the subnet for auto-address assignment.
    pub fn subnet(mut self, subnet: &str) -> Self {
        self.network.subnet = Some(subnet.to_string());
        self
    }

    /// Add a member endpoint.
    pub fn member(mut self, endpoint: &str) -> Self {
        self.network.members.push(endpoint.to_string());
        self
    }

    /// Add a VLAN definition.
    pub fn vlan(mut self, vid: u16, name: Option<&str>) -> Self {
        self.network.vlans.insert(
            vid,
            VlanConfig {
                name: name.map(|s| s.to_string()),
            },
        );
        self
    }

    /// Add a port configuration.
    pub fn port(
        mut self,
        node: &str,
        f: impl FnOnce(PortBuilder) -> PortBuilder,
    ) -> Self {
        let builder = f(PortBuilder::new());
        self.network
            .ports
            .insert(node.to_string(), builder.build());
        self
    }

    fn build(self) -> Network {
        self.network
    }
}

// ─────────────────────────────────────────────────
// Port builder
// ─────────────────────────────────────────────────

/// Builder for [`PortConfig`].
pub struct PortBuilder {
    config: PortConfig,
}

impl PortBuilder {
    fn new() -> Self {
        Self {
            config: PortConfig::default(),
        }
    }

    /// Set the interface name.
    pub fn interface(mut self, name: &str) -> Self {
        self.config.interface = Some(name.to_string());
        self
    }

    /// Add VLAN IDs this port carries.
    pub fn vlans(mut self, vids: &[u16]) -> Self {
        self.config.vlans.extend_from_slice(vids);
        self
    }

    /// Set whether this port carries tagged traffic.
    pub fn tagged(mut self, tagged: bool) -> Self {
        self.config.tagged = Some(tagged);
        self
    }

    /// Set the native VLAN (PVID).
    pub fn pvid(mut self, pvid: u16) -> Self {
        self.config.pvid = Some(pvid);
        self
    }

    /// Set whether to strip VLAN tags on egress.
    pub fn untagged(mut self, untagged: bool) -> Self {
        self.config.untagged = Some(untagged);
        self
    }

    /// Add an address.
    pub fn address(mut self, cidr: &str) -> Self {
        self.config.addresses.push(cidr.to_string());
        self
    }

    fn build(self) -> PortConfig {
        self.config
    }
}

// ─────────────────────────────────────────────────
// Impairment builder
// ─────────────────────────────────────────────────

/// Builder for [`Impairment`].
pub struct ImpairmentBuilder {
    impairment: Impairment,
}

impl ImpairmentBuilder {
    fn new() -> Self {
        Self {
            impairment: Impairment::default(),
        }
    }

    /// Set delay (e.g., "10ms").
    pub fn delay(mut self, delay: &str) -> Self {
        self.impairment.delay = Some(delay.to_string());
        self
    }

    /// Set jitter (e.g., "2ms").
    pub fn jitter(mut self, jitter: &str) -> Self {
        self.impairment.jitter = Some(jitter.to_string());
        self
    }

    /// Set packet loss (e.g., "0.1%").
    pub fn loss(mut self, loss: &str) -> Self {
        self.impairment.loss = Some(loss.to_string());
        self
    }

    /// Set rate limit (e.g., "100mbit").
    pub fn rate(mut self, rate: &str) -> Self {
        self.impairment.rate = Some(rate.to_string());
        self
    }

    /// Set packet corruption (e.g., "0.01%").
    pub fn corrupt(mut self, corrupt: &str) -> Self {
        self.impairment.corrupt = Some(corrupt.to_string());
        self
    }

    /// Set packet reordering (e.g., "0.5%").
    pub fn reorder(mut self, reorder: &str) -> Self {
        self.impairment.reorder = Some(reorder.to_string());
        self
    }

    fn build(self) -> Impairment {
        self.impairment
    }
}

// ─────────────────────────────────────────────────
// Rate limit builder
// ─────────────────────────────────────────────────

/// Builder for [`RateLimit`].
pub struct RateLimitBuilder {
    rate_limit: RateLimit,
}

impl RateLimitBuilder {
    fn new() -> Self {
        Self {
            rate_limit: RateLimit::default(),
        }
    }

    /// Set egress rate (e.g., "1gbit").
    pub fn egress(mut self, rate: &str) -> Self {
        self.rate_limit.egress = Some(rate.to_string());
        self
    }

    /// Set ingress rate (e.g., "1gbit").
    pub fn ingress(mut self, rate: &str) -> Self {
        self.rate_limit.ingress = Some(rate.to_string());
        self
    }

    /// Set burst size (e.g., "10mbit").
    pub fn burst(mut self, burst: &str) -> Self {
        self.rate_limit.burst = Some(burst.to_string());
        self
    }

    fn build(self) -> RateLimit {
        self.rate_limit
    }
}

// ─────────────────────────────────────────────────
// Firewall builder
// ─────────────────────────────────────────────────

/// Builder for [`FirewallConfig`].
pub struct FirewallBuilder {
    config: FirewallConfig,
}

impl FirewallBuilder {
    fn new() -> Self {
        Self {
            config: FirewallConfig::default(),
        }
    }

    /// Set the default policy ("accept" or "drop").
    pub fn policy(mut self, policy: &str) -> Self {
        self.config.policy = Some(policy.to_string());
        self
    }

    /// Add a firewall rule.
    pub fn rule(mut self, match_expr: &str, action: &str) -> Self {
        self.config.rules.push(FirewallRule {
            match_expr: Some(match_expr.to_string()),
            action: Some(action.to_string()),
        });
        self
    }

    fn build(self) -> FirewallConfig {
        self.config
    }
}

// ─────────────────────────────────────────────────
// VRF builder
// ─────────────────────────────────────────────────

/// Builder for [`VrfConfig`].
pub struct VrfBuilder {
    config: VrfConfig,
}

impl VrfBuilder {
    fn new() -> Self {
        Self {
            config: VrfConfig::default(),
        }
    }

    /// Set the routing table ID.
    pub fn table(mut self, table: u32) -> Self {
        self.config.table = table;
        self
    }

    /// Add an interface to enslave.
    pub fn interface(mut self, iface: &str) -> Self {
        self.config.interfaces.push(iface.to_string());
        self
    }

    /// Add a route within this VRF.
    pub fn route(
        mut self,
        dest: &str,
        f: impl FnOnce(RouteBuilder) -> RouteBuilder,
    ) -> Self {
        let builder = f(RouteBuilder::new());
        self.config
            .routes
            .insert(dest.to_string(), builder.build());
        self
    }

    fn build(self) -> VrfConfig {
        self.config
    }
}

// ─────────────────────────────────────────────────
// WireGuard builder
// ─────────────────────────────────────────────────

/// Builder for [`WireguardConfig`].
pub struct WireguardBuilder {
    config: WireguardConfig,
}

impl WireguardBuilder {
    fn new() -> Self {
        Self {
            config: WireguardConfig::default(),
        }
    }

    /// Set the private key ("auto" to auto-generate).
    pub fn private_key(mut self, key: &str) -> Self {
        self.config.private_key = Some(key.to_string());
        self
    }

    /// Set the listen port.
    pub fn listen_port(mut self, port: u16) -> Self {
        self.config.listen_port = Some(port);
        self
    }

    /// Add an address in CIDR notation.
    pub fn address(mut self, cidr: &str) -> Self {
        self.config.addresses.push(cidr.to_string());
        self
    }

    /// Add a peer node name.
    pub fn peer(mut self, node: &str) -> Self {
        self.config.peers.push(node.to_string());
        self
    }

    fn build(self) -> WireguardConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_lab() {
        let topo = Lab::new("test")
            .description("A test lab")
            .prefix("t")
            .build();

        assert_eq!(topo.lab.name, "test");
        assert_eq!(topo.lab.description.as_deref(), Some("A test lab"));
        assert_eq!(topo.lab.prefix(), "t");
    }

    #[test]
    fn test_profiles_nodes_links() {
        let topo = Lab::new("net")
            .profile("router", |p| {
                p.sysctl("net.ipv4.ip_forward", "1")
                    .sysctl("net.ipv6.conf.all.forwarding", "1")
            })
            .node("r1", |n| {
                n.profile("router")
                    .interface("lo", |i| i.address("10.255.0.1/32"))
                    .route("default", |r| r.via("10.0.11.1"))
            })
            .node("h1", |n| n.route("default", |r| r.via("10.0.0.1")))
            .link("r1:eth0", "h1:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24").mtu(9000)
            })
            .build();

        assert_eq!(topo.profiles.len(), 1);
        assert_eq!(topo.profiles["router"].sysctls["net.ipv4.ip_forward"], "1");
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.nodes["r1"].profile.as_deref(), Some("router"));
        assert_eq!(
            topo.nodes["r1"].interfaces["lo"].addresses,
            vec!["10.255.0.1/32"]
        );
        assert_eq!(
            topo.nodes["r1"].routes["default"].via.as_deref(),
            Some("10.0.11.1")
        );
        assert_eq!(topo.links.len(), 1);
        assert_eq!(topo.links[0].mtu, Some(9000));
        assert_eq!(
            topo.links[0].addresses.as_ref().unwrap(),
            &["10.0.0.1/24".to_string(), "10.0.0.2/24".to_string()]
        );
    }

    #[test]
    fn test_impairments_and_rate_limits() {
        let topo = Lab::new("imp")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .impair("a:eth0", |i| {
                i.delay("10ms")
                    .jitter("2ms")
                    .loss("0.1%")
                    .rate("100mbit")
                    .corrupt("0.01%")
                    .reorder("0.5%")
            })
            .rate_limit("b:eth0", |r| {
                r.egress("1gbit").ingress("1gbit").burst("10mbit")
            })
            .build();

        let imp = &topo.impairments["a:eth0"];
        assert_eq!(imp.delay.as_deref(), Some("10ms"));
        assert_eq!(imp.jitter.as_deref(), Some("2ms"));
        assert_eq!(imp.loss.as_deref(), Some("0.1%"));
        assert_eq!(imp.rate.as_deref(), Some("100mbit"));
        assert_eq!(imp.corrupt.as_deref(), Some("0.01%"));
        assert_eq!(imp.reorder.as_deref(), Some("0.5%"));

        let rl = &topo.rate_limits["b:eth0"];
        assert_eq!(rl.egress.as_deref(), Some("1gbit"));
        assert_eq!(rl.ingress.as_deref(), Some("1gbit"));
        assert_eq!(rl.burst.as_deref(), Some("10mbit"));
    }

    #[test]
    fn test_network_with_vlans() {
        let topo = Lab::new("vlan")
            .node("switch", |n| n)
            .node("pc1", |n| n)
            .network("office", |net| {
                net.kind("bridge")
                    .vlan_filtering(true)
                    .vlan(10, Some("engineering"))
                    .vlan(20, Some("sales"))
                    .port("switch", |p| {
                        p.interface("eth0").vlans(&[10, 20]).tagged(true)
                    })
                    .port("pc1", |p| {
                        p.interface("eth0")
                            .pvid(10)
                            .untagged(true)
                            .address("10.10.0.2/24")
                    })
            })
            .build();

        let net = &topo.networks["office"];
        assert_eq!(net.kind.as_deref(), Some("bridge"));
        assert_eq!(net.vlan_filtering, Some(true));
        assert_eq!(net.vlans.len(), 2);
        assert_eq!(
            net.vlans[&10].name.as_deref(),
            Some("engineering")
        );
        assert_eq!(net.ports["switch"].vlans, vec![10, 20]);
        assert_eq!(net.ports["switch"].tagged, Some(true));
        assert_eq!(net.ports["pc1"].pvid, Some(10));
    }

    #[test]
    fn test_vrf() {
        let topo = Lab::new("vrf")
            .node("pe", |n| {
                n.profile("router").vrf("red", |v| {
                    v.table(100)
                        .interface("eth1")
                        .route("0.0.0.0/0", |r| r.via("10.0.1.2"))
                })
            })
            .build();

        let vrf = &topo.nodes["pe"].vrfs["red"];
        assert_eq!(vrf.table, 100);
        assert_eq!(vrf.interfaces, vec!["eth1"]);
        assert_eq!(vrf.routes["0.0.0.0/0"].via.as_deref(), Some("10.0.1.2"));
    }

    #[test]
    fn test_wireguard() {
        let topo = Lab::new("wg")
            .node("office", |n| {
                n.wireguard("wg0", |w| {
                    w.private_key("auto")
                        .listen_port(51820)
                        .address("10.100.0.1/24")
                        .peer("remote")
                })
            })
            .build();

        let wg = &topo.nodes["office"].wireguard["wg0"];
        assert_eq!(wg.private_key.as_deref(), Some("auto"));
        assert_eq!(wg.listen_port, Some(51820));
        assert_eq!(wg.addresses, vec!["10.100.0.1/24"]);
        assert_eq!(wg.peers, vec!["remote"]);
    }

    #[test]
    fn test_firewall() {
        let topo = Lab::new("fw")
            .node("server", |n| {
                n.firewall(|f| {
                    f.policy("drop")
                        .rule("ct state established,related", "accept")
                        .rule("tcp dport 80", "accept")
                })
            })
            .build();

        let fw = topo.nodes["server"].firewall.as_ref().unwrap();
        assert_eq!(fw.policy.as_deref(), Some("drop"));
        assert_eq!(fw.rules.len(), 2);
        assert_eq!(fw.rules[0].action.as_deref(), Some("accept"));
    }

    #[test]
    fn test_exec() {
        let topo = Lab::new("exec")
            .node("server", |n| {
                n.exec_background(&["iperf3", "-s"])
                    .exec(&["echo", "hello"])
            })
            .build();

        assert_eq!(topo.nodes["server"].exec.len(), 2);
        assert!(topo.nodes["server"].exec[0].background);
        assert!(!topo.nodes["server"].exec[1].background);
        assert_eq!(
            topo.nodes["server"].exec[0].cmd,
            vec!["iperf3", "-s"]
        );
    }

    #[test]
    fn test_builder_matches_toml_parsing() {
        // Build topology via builder
        let built = Lab::new("simple")
            .description("Minimal two-node lab")
            .profile("router", |p| {
                p.sysctl("net.ipv4.ip_forward", "1")
            })
            .node("router", |n| {
                n.profile("router")
                    .route("10.0.0.0/8", |r| r.via("10.0.0.2"))
            })
            .node("host", |n| n.route("default", |r| r.via("10.0.0.1")))
            .link("router:eth0", "host:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .impair("router:eth0", |i| i.delay("10ms").jitter("2ms"))
            .build();

        // Parse equivalent TOML
        let parsed = crate::parser::parse(
            r#"
[lab]
name = "simple"
description = "Minimal two-node lab"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.router]
profile = "router"

[nodes.router.routes]
"10.0.0.0/8" = { via = "10.0.0.2" }

[nodes.host]

[nodes.host.routes]
default = { via = "10.0.0.1" }

[[links]]
endpoints = ["router:eth0", "host:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]

[impairments."router:eth0"]
delay = "10ms"
jitter = "2ms"
"#,
        )
        .unwrap();

        // Compare key properties
        assert_eq!(built.lab.name, parsed.lab.name);
        assert_eq!(built.lab.description, parsed.lab.description);
        assert_eq!(built.profiles.len(), parsed.profiles.len());
        assert_eq!(
            built.profiles["router"].sysctls,
            parsed.profiles["router"].sysctls
        );
        assert_eq!(built.nodes.len(), parsed.nodes.len());
        assert_eq!(
            built.nodes["router"].profile,
            parsed.nodes["router"].profile
        );
        assert_eq!(built.links.len(), parsed.links.len());
        assert_eq!(built.links[0].endpoints, parsed.links[0].endpoints);
        assert_eq!(built.links[0].addresses, parsed.links[0].addresses);
        assert_eq!(built.impairments.len(), parsed.impairments.len());
        assert_eq!(
            built.impairments["router:eth0"].delay,
            parsed.impairments["router:eth0"].delay
        );
    }
}
