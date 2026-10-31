//! Programmatic topology builder DSL.
//!
//! Build topologies in Rust code instead of TOML files. Useful for integration
//! tests and dynamic topology generation.
//!
//! # Example
//!
//! ```
//! use nlink_lab::builder::TopologyBuilder;
//!
//! let topology = TopologyBuilder::new("my-lab")
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

use std::collections::BTreeMap;

use crate::types::{
    ExecConfig, FirewallChain, FirewallConfig, FirewallRule, FirewallSet, Impairment,
    InterfaceConfig, LabConfig, Link, Network, Node, PortConfig, Profile, RateLimit, RejectWith,
    RouteConfig, SetKey, Topology, VlanConfig, VrfConfig, WireguardConfig,
};

/// Top-level lab builder.
pub struct TopologyBuilder {
    topology: Topology,
    /// Names declared twice (`node "a"`, `network "lan"`, `profile "p"`):
    /// the later one replaced the earlier silently, where NLL refuses it
    /// (#218). [`TopologyBuilder::build_validated`] reports them.
    duplicates: Vec<String>,
}

impl TopologyBuilder {
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
            duplicates: Vec::new(),
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

    /// Set the container runtime (docker, podman, or auto-detect).
    pub fn runtime(mut self, runtime: crate::types::ContainerRuntime) -> Self {
        self.topology.lab.runtime = Some(runtime);
        self
    }

    /// Set the DNS resolution mode.
    pub fn dns(mut self, mode: crate::types::DnsMode) -> Self {
        self.topology.lab.dns = mode;
        self
    }

    /// Add a reusable profile.
    pub fn profile(mut self, name: &str, f: impl FnOnce(ProfileBuilder) -> ProfileBuilder) -> Self {
        let builder = f(ProfileBuilder::new());
        if self
            .topology
            .profiles
            .insert(name.to_string(), builder.build())
            .is_some()
        {
            self.duplicates.push(format!("profile '{name}'"));
        }
        self
    }

    /// Add a node.
    pub fn node(mut self, name: &str, f: impl FnOnce(NodeBuilder) -> NodeBuilder) -> Self {
        let builder = f(NodeBuilder::new());
        if self
            .topology
            .nodes
            .insert(name.to_string(), builder.build())
            .is_some()
        {
            self.duplicates.push(format!("node '{name}'"));
        }
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
    pub fn network(mut self, name: &str, f: impl FnOnce(NetworkBuilder) -> NetworkBuilder) -> Self {
        let builder = f(NetworkBuilder::new());
        if self
            .topology
            .networks
            .insert(name.to_string(), builder.build())
            .is_some()
        {
            self.duplicates.push(format!("network '{name}'"));
        }
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

    /// Set a non-netem root qdisc (`tbf`, `fq_codel`, `sfq`, `prio`) on an
    /// endpoint (issue #67).
    pub fn qdisc(mut self, endpoint: &str, kind: crate::types::QdiscKind) -> Self {
        self.topology
            .qdiscs
            .insert(endpoint.to_string(), crate::types::QdiscConfig { kind });
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
        for d in &self.duplicates {
            tracing::warn!("builder: {d} declared twice; the later one is kept");
        }
        self.topology
    }

    /// Finalize, validate, and return the topology.
    ///
    /// Returns an error if the topology has validation errors, or if a
    /// node, network or profile name was declared twice.
    pub fn build_validated(self) -> crate::error::Result<Topology> {
        if !self.duplicates.is_empty() {
            return Err(crate::Error::invalid_topology(format!(
                "declared twice: {}",
                self.duplicates.join(", ")
            )));
        }
        let topo = self.build();
        topo.validate().bail()?;
        Ok(topo)
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
        self.node.profiles.push(name.to_string());
        self
    }

    /// Set a container image (makes this node a container instead of a bare namespace).
    pub fn image(mut self, image: &str) -> Self {
        self.node.image = Some(image.to_string());
        self
    }

    /// Set the container command override (requires `image`).
    pub fn cmd(mut self, cmd: &[&str]) -> Self {
        self.node.cmd = Some(cmd.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Add a container environment variable (requires `image`).
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.node
            .env
            .get_or_insert_with(BTreeMap::new)
            .insert(key.to_string(), value.to_string());
        self
    }

    /// Add a container bind mount in "host:container" format (requires `image`).
    pub fn volume(mut self, mount: &str) -> Self {
        self.node
            .volumes
            .get_or_insert_with(Vec::new)
            .push(mount.to_string());
        self
    }

    /// Add a sysctl key-value pair.
    pub fn sysctl(mut self, key: &str, value: &str) -> Self {
        self.node.sysctls.insert(key.to_string(), value.to_string());
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
    pub fn route(mut self, dest: &str, f: impl FnOnce(RouteBuilder) -> RouteBuilder) -> Self {
        let builder = f(RouteBuilder::new());
        self.node.routes.insert(dest.to_string(), builder.build());
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

    /// Set the interface kind.
    pub fn kind(mut self, kind: crate::types::InterfaceKind) -> Self {
        self.config.kind = Some(kind);
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

    /// Add a member endpoint (once: a `port` with an interface adds its
    /// own).
    pub fn member(mut self, endpoint: &str) -> Self {
        if !self.network.members.iter().any(|m| m == endpoint) {
            self.network.members.push(endpoint.to_string());
        }
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
    ///
    /// If the port specifies an `interface(...)`, the endpoint
    /// (`node:iface`) is auto-added to `members` (idempotent).
    /// Without this, callers had to write both
    /// `.member("host_a:eth0")` and `.port("host_a", |p|
    /// p.interface("eth0"))` — and forgetting the member silently
    /// produced an empty `members` list, which the deploy step
    /// iterates to create veth pairs. Empty members → no veth in
    /// the node namespace → `ip addr show eth0` fails at exec time.
    ///
    /// The port is keyed by its endpoint `node:iface`, as NLL keys it:
    /// keyed by the bare node, two ports of one node replaced each other,
    /// and `dns hosts`, WireGuard endpoints, `routing auto` and FRR, which
    /// read `node:iface` keys, skipped it (#218). A port without an
    /// interface is keyed by its node until the network is built, which
    /// resolves it to that node's member.
    pub fn port(mut self, node: &str, f: impl FnOnce(PortBuilder) -> PortBuilder) -> Self {
        let port = f(PortBuilder::new()).build();
        let key = match port.interface.as_deref() {
            Some(iface) => {
                let endpoint = format!("{node}:{iface}");
                if !self.network.members.iter().any(|m| m == &endpoint) {
                    self.network.members.push(endpoint.clone());
                }
                endpoint
            }
            None => node.to_string(),
        };
        self.network.ports.insert(key, port);
        self
    }

    fn build(mut self) -> Network {
        // a bare-node port: its node's member, when it has one
        let bare: Vec<String> = self
            .network
            .ports
            .keys()
            .filter(|k| !k.contains(':'))
            .cloned()
            .collect();
        for node in bare {
            let mut of_node = self
                .network
                .members
                .iter()
                .filter(|m| m.split_once(':').is_some_and(|(n, _)| n == node));
            if let (Some(member), None) = (of_node.next(), of_node.next()) {
                let member = member.clone();
                if let Some(port) = self.network.ports.remove(&node) {
                    self.network.ports.insert(member, port);
                }
            }
        }
        // `subnet` assigns member addresses, as in NLL: it was stored and
        // nothing was addressed (#218). `auto/N` waits for deploy.
        if let Some(subnet) = self.network.subnet.clone() {
            let explicit: std::collections::BTreeMap<String, Vec<String>> = self
                .network
                .ports
                .iter()
                .filter(|(_, p)| !p.addresses.is_empty())
                .map(|(k, p)| (k.clone(), p.addresses.clone()))
                .collect();
            match crate::parser::nll::lower::subnet_auto_assign(
                &subnet,
                &self.network.members,
                &explicit,
            ) {
                Ok(assigned) => {
                    for (member, addr) in assigned.into_iter().flatten() {
                        self.network
                            .ports
                            .entry(member)
                            .or_default()
                            .addresses
                            .push(addr);
                    }
                }
                // the validator reports the subnet
                Err(e) => tracing::warn!("builder: subnet {subnet}: {e}"),
            }
        }
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

    /// Set the input chain's policy ("accept" or "drop").
    pub fn policy(mut self, policy: &str) -> Self {
        self.config.policy = Some(policy.to_string());
        self
    }

    /// Add an input chain rule: an nft-style match (`"tcp dport 22"`,
    /// empty for every packet) and a verdict ("accept", "drop", "reject").
    pub fn rule(mut self, match_expr: &str, action: &str) -> Self {
        self.config.rules.push(chain_rule(match_expr, action, None));
        self
    }

    /// Add an input chain `reject with …` rule.
    pub fn reject_with(mut self, match_expr: &str, with: RejectWith) -> Self {
        self.config
            .rules
            .push(chain_rule(match_expr, "reject", Some(with)));
        self
    }

    /// Add a complete input chain rule (statements included: counters,
    /// log, limit, marks, classify, MSS clamping).
    pub fn push_rule(mut self, rule: FirewallRule) -> Self {
        self.config.rules.push(rule);
        self
    }

    /// Declare the forward chain (traffic routed through the node).
    pub fn forward(mut self, f: impl FnOnce(FirewallChainBuilder) -> FirewallChainBuilder) -> Self {
        self.config.forward = Some(f(FirewallChainBuilder::default()).chain);
        self
    }

    /// Declare the output chain (traffic the node sends).
    pub fn output(mut self, f: impl FnOnce(FirewallChainBuilder) -> FirewallChainBuilder) -> Self {
        self.config.output = Some(f(FirewallChainBuilder::default()).chain);
        self
    }

    /// Declare a named set rules look up as `@name` (`"ip saddr @name"`).
    pub fn set(
        mut self,
        name: &str,
        f: impl FnOnce(FirewallSetBuilder) -> FirewallSetBuilder,
    ) -> Self {
        self.config
            .sets
            .insert(name.to_string(), f(FirewallSetBuilder::default()).set);
        self
    }

    fn build(self) -> FirewallConfig {
        self.config
    }
}

fn chain_rule(match_expr: &str, action: &str, reject_with: Option<RejectWith>) -> FirewallRule {
    FirewallRule {
        match_expr: (!match_expr.is_empty()).then(|| match_expr.to_string()),
        action: Some(action.to_string()),
        reject_with,
        ..Default::default()
    }
}

/// Builder for a [`FirewallSet`].
#[derive(Default)]
pub struct FirewallSetBuilder {
    set: FirewallSet,
}

impl FirewallSetBuilder {
    /// The key: one field, or several for a concatenation.
    pub fn key(mut self, key: &[SetKey]) -> Self {
        self.set.key = key.to_vec();
        self
    }

    /// Add an element: an address, prefix, range or port, or the parts of
    /// a concatenated key joined by `" . "`.
    pub fn element(mut self, element: &str) -> Self {
        self.set.elements.push(element.to_string());
        self
    }

    /// Cap the number of elements.
    pub fn size(mut self, size: u32) -> Self {
        self.set.size = Some(size);
        self
    }

    /// Give elements a default lifetime (`"300s"`).
    pub fn timeout(mut self, timeout: &str) -> Self {
        self.set.timeout = Some(timeout.to_string());
        self
    }

    /// The set is filled at run time; `apply` keeps what it finds.
    pub fn dynamic(mut self) -> Self {
        self.set.dynamic = true;
        self
    }
}

/// Builder for a forward or output [`FirewallChain`].
#[derive(Default)]
pub struct FirewallChainBuilder {
    chain: FirewallChain,
}

impl FirewallChainBuilder {
    /// Set the chain's policy ("accept" or "drop").
    pub fn policy(mut self, policy: &str) -> Self {
        self.chain.policy = Some(policy.to_string());
        self
    }

    /// Add a rule (see [`FirewallBuilder::rule`]).
    pub fn rule(mut self, match_expr: &str, action: &str) -> Self {
        self.chain.rules.push(chain_rule(match_expr, action, None));
        self
    }

    /// Add a `reject with …` rule.
    pub fn reject_with(mut self, match_expr: &str, with: RejectWith) -> Self {
        self.chain
            .rules
            .push(chain_rule(match_expr, "reject", Some(with)));
        self
    }

    /// Add a complete rule (see [`FirewallBuilder::push_rule`]).
    pub fn push_rule(mut self, rule: FirewallRule) -> Self {
        self.chain.rules.push(rule);
        self
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
    pub fn route(mut self, dest: &str, f: impl FnOnce(RouteBuilder) -> RouteBuilder) -> Self {
        let builder = f(RouteBuilder::new());
        self.config.routes.insert(dest.to_string(), builder.build());
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
        let topo = TopologyBuilder::new("test")
            .description("A test lab")
            .prefix("t")
            .build();

        assert_eq!(topo.lab.name, "test");
        assert_eq!(topo.lab.description.as_deref(), Some("A test lab"));
        assert_eq!(topo.lab.prefix(), "t");
    }

    #[test]
    fn test_profiles_nodes_links() {
        let topo = TopologyBuilder::new("net")
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
        assert_eq!(topo.nodes["r1"].profiles, vec!["router"]);
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
        let topo = TopologyBuilder::new("imp")
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
        let topo = TopologyBuilder::new("vlan")
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
        assert_eq!(net.vlans[&10].name.as_deref(), Some("engineering"));
        assert_eq!(net.ports["switch:eth0"].vlans, vec![10, 20]);
        assert_eq!(net.ports["switch:eth0"].tagged, Some(true));
        assert_eq!(net.ports["pc1:eth0"].pvid, Some(10));
    }

    /// Ports keyed by endpoint, a concrete subnet assigning addresses, and
    /// duplicates refused (#218).
    #[test]
    fn ports_subnets_and_duplicates_behave_as_in_nll() {
        let topo = TopologyBuilder::new("b")
            .node("a", |n| n)
            .node("b", |n| n)
            .network("lan", |net| {
                net.subnet("10.0.0.0/24")
                    .member("a:eth0")
                    .member("b:eth0")
                    .port("a", |p| p.interface("eth1").pvid(3))
                    .member("a:eth1")
            })
            .build();
        let lan = &topo.networks["lan"];
        assert_eq!(lan.ports["a:eth0"].addresses, vec!["10.0.0.1/24"]);
        assert_eq!(lan.ports["b:eth0"].addresses, vec!["10.0.0.2/24"]);
        assert_eq!(lan.ports["a:eth1"].pvid, Some(3));
        assert_eq!(lan.ports["a:eth1"].addresses, vec!["10.0.0.3/24"]);
        assert!(crate::render::try_render(&topo).is_ok());

        let twice = TopologyBuilder::new("b").node("a", |n| n).node("a", |n| n);
        let err = twice.build_validated().unwrap_err().to_string();
        assert!(err.contains("node 'a'"), "{err}");
    }

    #[test]
    fn test_port_auto_adds_to_members() {
        // Regression test: `.port(node, |p| p.interface("eth0"))` must
        // auto-populate `members` so the deploy step actually creates
        // the veth in the node's namespace. Before this fix, callers
        // had to specify both `.member("host_a:eth0")` and
        // `.port("host_a", |p| p.interface("eth0"))` separately, and
        // forgetting `.member` silently produced an empty members list
        // — surfaced by `deploy_networks_with_shared_prefix` failing
        // with `ip addr show eth0` returning exit 1.
        let topo = TopologyBuilder::new("auto-members")
            .node("host_a", |n| n)
            .node("host_b", |n| n)
            .network("lan_a", |net| {
                net.subnet("10.1.0.0/24")
                    .port("host_a", |p| p.interface("eth0").address("10.1.0.2/24"))
            })
            .network("lan_b", |net| {
                net.subnet("10.2.0.0/24")
                    .port("host_b", |p| p.interface("eth0").address("10.2.0.2/24"))
            })
            .build();

        assert_eq!(
            topo.networks["lan_a"].members,
            vec!["host_a:eth0".to_string()]
        );
        assert_eq!(
            topo.networks["lan_b"].members,
            vec!["host_b:eth0".to_string()]
        );
    }

    #[test]
    fn test_port_auto_add_is_idempotent_with_explicit_member() {
        // If the user calls both `.member("host:eth0")` and
        // `.port("host", |p| p.interface("eth0"))`, the endpoint
        // should appear exactly once in members.
        let topo = TopologyBuilder::new("idempotent")
            .node("host", |n| n)
            .network("lan", |net| {
                net.member("host:eth0")
                    .port("host", |p| p.interface("eth0").address("10.0.0.2/24"))
            })
            .build();

        assert_eq!(
            topo.networks["lan"].members,
            vec!["host:eth0".to_string()],
            "expected single member entry, not duplicated"
        );
    }

    #[test]
    fn test_port_without_interface_does_not_add_to_members() {
        // VLAN-only port configs (no .interface(...)) shouldn't pollute
        // members — the user hasn't said which iface attaches.
        let topo = TopologyBuilder::new("vlan-only")
            .node("sw", |n| n)
            .network("fab", |net| {
                net.member("sw:eth0").port("sw", |p| p.pvid(10))
            })
            .build();

        assert_eq!(
            topo.networks["fab"].members,
            vec!["sw:eth0".to_string()],
            "port without interface() must not add to members"
        );
    }

    #[test]
    fn test_vrf() {
        let topo = TopologyBuilder::new("vrf")
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
        let topo = TopologyBuilder::new("wg")
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
    fn test_dns_hosts() {
        let topo = TopologyBuilder::new("dns-test")
            .dns(crate::types::DnsMode::Hosts)
            .node("server", |n| n)
            .node("client", |n| n)
            .link("server:eth0", "client:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .build();

        assert_eq!(topo.lab.dns, crate::types::DnsMode::Hosts);

        let entries = crate::dns::generate_hosts_entries(&topo);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_firewall() {
        let topo = TopologyBuilder::new("fw")
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
        let topo = TopologyBuilder::new("exec")
            .node("server", |n| {
                n.exec_background(&["iperf3", "-s"])
                    .exec(&["echo", "hello"])
            })
            .build();

        assert_eq!(topo.nodes["server"].exec.len(), 2);
        assert!(topo.nodes["server"].exec[0].background);
        assert!(!topo.nodes["server"].exec[1].background);
        assert_eq!(topo.nodes["server"].exec[0].cmd, vec!["iperf3", "-s"]);
    }

    #[test]
    fn test_builder_matches_nll_parsing() {
        // Build topology via builder
        let built = TopologyBuilder::new("simple")
            .description("Minimal two-node lab")
            .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
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

        // Parse equivalent NLL
        let parsed = crate::parser::parse(
            r#"lab "simple" {
    description "Minimal two-node lab"
}
profile router { sysctl "net.ipv4.ip_forward" "1" }
node router : router { route 10.0.0.0/8 via 10.0.0.2 }
node host { route default via 10.0.0.1 }
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
impair router:eth0 delay 10ms jitter 2ms
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
            built.nodes["router"].profiles,
            parsed.nodes["router"].profiles
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

    #[test]
    fn test_build_validated_catches_errors() {
        let result = TopologyBuilder::new("bad")
            .node("a", |n| n)
            .link("a:eth0", "missing:eth0", |l| l)
            .build_validated();
        assert!(result.is_err());
    }

    #[test]
    fn test_build_validated_ok() {
        let result = TopologyBuilder::new("good")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .build_validated();
        assert!(result.is_ok());
    }

    #[test]
    fn test_duplicate_link_endpoints_caught() {
        let topo = TopologyBuilder::new("dup")
            .node("a", |n| n)
            .node("b", |n| n)
            .node("c", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .link("a:eth0", "c:eth0", |l| l) // a:eth0 used twice
            .build();
        let result = topo.validate();
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "duplicate-link-endpoint"));
    }
}
