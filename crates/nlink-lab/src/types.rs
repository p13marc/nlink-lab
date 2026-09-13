//! Core topology types.
//!
//! These types represent the topology of a network lab. They can be constructed
//! from an NLL file via [`crate::parser::parse`] or programmatically via the
//! builder DSL ([`crate::Lab`]).
//!
//! The type hierarchy:
//!
//! ```text
//! Topology
//! ├── lab: LabConfig
//! ├── profiles: BTreeMap<String, Profile>
//! ├── nodes: BTreeMap<String, Node>
//! ├── links: Vec<Link>
//! ├── networks: BTreeMap<String, Network>
//! ├── impairments: BTreeMap<String, Impairment>
//! └── rate_limits: BTreeMap<String, RateLimit>
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Complete topology definition for a network lab.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Topology {
    /// Lab metadata.
    pub lab: LabConfig,

    /// Reusable node profiles.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,

    /// Node definitions (each becomes a network namespace).
    #[serde(default)]
    pub nodes: BTreeMap<String, Node>,

    /// Point-to-point links between nodes (veth pairs).
    #[serde(default)]
    pub links: Vec<Link>,

    /// Shared L2 segments (bridges).
    #[serde(default)]
    pub networks: BTreeMap<String, Network>,

    /// Per-interface network impairment (netem).
    #[serde(default)]
    pub impairments: BTreeMap<String, Impairment>,

    /// Per-interface rate limiting.
    #[serde(default)]
    pub rate_limits: BTreeMap<String, RateLimit>,

    /// Per-interface root qdisc other than netem (`qdisc a:eth0 tbf { … }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub qdiscs: BTreeMap<String, QdiscConfig>,

    /// Post-deploy reachability assertions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<Assertion>,

    /// Timed test scenarios (fault injection + validation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<Scenario>,

    /// Performance benchmarks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benchmarks: Vec<Benchmark>,
}

/// A performance benchmark definition.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Benchmark {
    /// Benchmark name.
    pub name: String,
    /// Individual benchmark tests.
    pub tests: Vec<BenchmarkTest>,
}

/// A single benchmark test.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum BenchmarkTest {
    /// iperf3 throughput/jitter test.
    Iperf3 {
        from: String,
        to: String,
        duration: Option<String>,
        streams: Option<u32>,
        udp: bool,
        assertions: Vec<BenchmarkAssertion>,
    },
    /// Ping latency/loss test.
    Ping {
        from: String,
        to: String,
        count: Option<u32>,
        assertions: Vec<BenchmarkAssertion>,
    },
}

/// A benchmark assertion (metric comparison).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkAssertion {
    /// Metric name (bandwidth, jitter, avg, p99, loss).
    pub metric: String,
    /// Comparison operator.
    pub op: CompareOp,
    /// Threshold value (e.g., "900mbit", "5ms", "1%").
    pub value: String,
}

/// Comparison operator for benchmark assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum CompareOp {
    Gt,
    Lt,
    Gte,
    Lte,
}

/// A timed test scenario with fault injection and validation steps.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Scenario {
    /// Scenario name.
    pub name: String,
    /// Ordered steps (sorted by time).
    pub steps: Vec<ScenarioStep>,
}

/// A single step in a scenario, executed at a specific time offset.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioStep {
    /// Time offset from scenario start (milliseconds).
    pub time_ms: u64,
    /// Actions to execute at this time.
    pub actions: Vec<ScenarioAction>,
}

/// An action within a scenario step.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum ScenarioAction {
    /// Bring interface down.
    Down(String),
    /// Bring interface up.
    Up(String),
    /// Remove all impairments from interface.
    Clear(String),
    /// Run validation assertions.
    Validate(Vec<Assertion>),
    /// Execute command in a node.
    Exec { node: String, cmd: Vec<String> },
    /// Print a log message.
    Log(String),
}

/// Post-deploy reachability assertion.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Assertion {
    /// Assert that `from` can reach `to` (ping succeeds).
    Reach { from: String, to: String },
    /// Assert that `from` cannot reach `to` (ping fails).
    NoReach { from: String, to: String },
    /// Assert TCP connection to `to:port` succeeds from `from`.
    TcpConnect {
        from: String,
        to: String,
        port: u16,
        timeout: Option<String>,
        retries: Option<u32>,
        interval: Option<String>,
    },
    /// Assert that latency from `from` to `to` is under `max`.
    LatencyUnder {
        from: String,
        to: String,
        max: String,
        samples: Option<u32>,
    },
    /// Assert that a route exists in `node`'s routing table.
    RouteHas {
        node: String,
        destination: String,
        via: Option<String>,
        dev: Option<String>,
    },
    /// Assert that DNS resolution works (requires `dns hosts`).
    DnsResolves {
        from: String,
        name: String,
        expected_ip: String,
    },
}

/// DNS resolution mode for lab nodes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    /// No DNS configuration (default).
    #[default]
    Off,
    /// Auto-generate /etc/hosts entries from topology.
    Hosts,
}

/// Routing mode for automatic static route generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    /// No auto-routing (default).
    #[default]
    Manual,
    /// Routers run FRR daemons (`routing frr { ospf }`, per-node `frr { … }`);
    /// non-router nodes still get `auto`-style static defaults (#65).
    Frr,
    /// Compute static routes from topology graph.
    Auto,
}

/// Container runtime selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ContainerRuntime {
    /// Auto-detect: prefer podman, fall back to docker.
    #[default]
    Auto,
    /// Use Docker.
    Docker,
    /// Use Podman.
    Podman,
}

/// Lab metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LabConfig {
    /// Lab name (used for namespace prefix and state tracking).
    pub name: String,

    /// Human-readable description.
    pub description: Option<String>,

    /// Prefix for namespace names (defaults to lab name).
    pub prefix: Option<String>,

    /// Container runtime to use when nodes specify an image.
    pub runtime: Option<ContainerRuntime>,

    /// Version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Author name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    /// Tags for categorization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Management network subnet (auto-creates OOB bridge connecting all nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mgmt_subnet: Option<String>,

    /// Whether the management bridge lives in the root namespace (host-reachable).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mgmt_host_reachable: bool,

    /// DNS resolution mode.
    #[serde(default, skip_serializing_if = "is_dns_off")]
    pub dns: DnsMode,

    /// Routing mode.
    #[serde(default, skip_serializing_if = "is_routing_manual")]
    pub routing: RoutingMode,

    /// Lab-wide FRR defaults (`routing frr { ospf area … }`): every
    /// forwarding namespace node without its own `frr { … }` runs this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frr: Option<FrrConfig>,
}

/// FRR routing daemons for one node (issue #65).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FrrConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ospf: Option<OspfConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bgp: Option<BgpConfig>,
}

/// `ospf { area … router-id … passive [...] hello … dead … redistribute [...] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OspfConfig {
    /// OSPF area (`0.0.0.0` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_id: Option<String>,
    /// Interfaces that advertise their prefix but form no adjacency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passive: Vec<String>,
    /// Hello interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<u32>,
    /// Dead interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead: Option<u32>,
    /// `connected`, `static`, `bgp`, `kernel`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redistribute: Vec<String>,
}

/// `bgp { as … router-id … neighbor … network … redistribute [...] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BgpConfig {
    /// Local autonomous system number.
    pub asn: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub neighbors: Vec<BgpNeighbor>,
    /// Prefixes to originate (`network 10.10.0.0/24`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
    /// `connected`, `static`, `ospf`, `kernel`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redistribute: Vec<String>,
}

/// `neighbor NODE [as N] [remote IP]` — the address defaults to the
/// neighbour's IP on the segment shared with this node, the remote AS to
/// the neighbour's own `bgp { as … }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BgpNeighbor {
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_as: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

fn is_dns_off(mode: &DnsMode) -> bool {
    *mode == DnsMode::Off
}

fn is_routing_manual(mode: &RoutingMode) -> bool {
    *mode == RoutingMode::Manual
}

impl LabConfig {
    /// Get the effective namespace prefix.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or(&self.name)
    }

    fn name_hash(&self) -> String {
        name_hash_str(&self.name)
    }

    /// Root-namespace management bridge name: `nl{hash}` (10 chars, always unique).
    pub fn mgmt_bridge_name(&self) -> String {
        mgmt_bridge_name_for(&self.name)
    }

    /// Root-namespace management veth peer name for a node at the given index.
    pub fn mgmt_peer_name(&self, idx: usize) -> String {
        let h = self.name_hash();
        // nm{hash_8chars}{idx} — fits 15 chars for idx up to 99999
        format!("nm{h}{idx}")
    }
}

/// DJB2 hash of a string, returned as 8 hex chars.
fn name_hash_str(name: &str) -> String {
    let mut hash: u32 = 5381;
    for b in name.as_bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(*b as u32);
    }
    format!("{hash:08x}")
}

/// Compute the root-namespace management bridge name for a lab name.
/// Uses a deterministic hash to avoid 15-char Linux interface name truncation.
pub fn mgmt_bridge_name_for(lab_name: &str) -> String {
    format!("nl{}", name_hash_str(lab_name))
}

/// Compute the mgmt-namespace veth peer name for a bridge network port.
///
/// Format: `np{hash8}{idx}` (11–14 chars, fits the 15-char Linux ifname
/// budget for idx < 10_000). The hash is over `net_name` only — collisions
/// across different labs are not a concern because the network bridge lives
/// in the lab's mgmt namespace.
///
/// Replaces an earlier scheme (`br{prefix4}p{idx}`) that truncated
/// `net_name` to 4 characters and silently collided whenever two networks
/// shared a 4-char prefix (e.g. `lan_a`/`lan_b` both → `brlan_p{idx}`).
pub fn network_peer_name_for(net_name: &str, idx: usize) -> String {
    format!("np{}{}", name_hash_str(net_name), idx)
}

/// Compute the bridge name for a shared L2 network.
///
/// Format: `nb{hash8}` (10 chars, always within the 15-char Linux ifname
/// budget). The hash is over `net_name` only — bridges live in the lab's
/// mgmt namespace, so cross-lab collisions aren't a concern.
///
/// Replaces an earlier scheme (`{prefix}-{net_name}` truncated to 15 chars)
/// that silently collided whenever the lab prefix grew long enough that
/// the truncation chopped off the distinguishing part of `net_name`. That
/// failure mode was not theoretical — `#[lab_test]` rewrites lab names to
/// `{base}-test-{fn_name}-{pid}`, easily 30+ chars, which guaranteed a
/// collision between any two networks in the same lab.
pub fn network_bridge_name_for(net_name: &str) -> String {
    format!("nb{}", name_hash_str(net_name))
}

/// Reusable node template.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Profile {
    /// Sysctl values to apply.
    #[serde(default)]
    pub sysctls: BTreeMap<String, String>,

    /// Firewall configuration.
    pub firewall: Option<FirewallConfig>,

    /// FRR daemons for nodes using this profile (#65).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frr: Option<FrrConfig>,
}

/// Accept `"router"`, `null`, or `["a", "b"]` for `Node::profiles`.
fn deserialize_profiles<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
        None,
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
        OneOrMany::None => Vec::new(),
    })
}

/// Node definition — becomes a network namespace or container.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Node {
    /// Profiles to inherit from, in order (later ones override earlier
    /// ones — `node r : base, override`). Serialised as `profiles`;
    /// state files written before 0.9 carry a single `profile` string,
    /// which is still accepted.
    #[serde(
        default,
        alias = "profile",
        deserialize_with = "deserialize_profiles",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub profiles: Vec<String>,

    /// Container image (when set, node is deployed as a container instead of bare namespace).
    pub image: Option<String>,

    /// Container command override (requires `image`).
    pub cmd: Option<Vec<String>>,

    /// Container environment variables (requires `image`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,

    /// Container bind mounts in "host:container" format (requires `image`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumes: Option<Vec<String>>,

    /// CPU limit (e.g., "1.5").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,

    /// Memory limit (e.g., "512m").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,

    /// Run container in privileged mode (default: false, uses cap-add instead).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub privileged: bool,

    /// Linux capabilities to add (e.g., NET_ADMIN, NET_RAW).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<String>,

    /// Linux capabilities to drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_drop: Vec<String>,

    /// Container entrypoint override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,

    /// Container hostname (default: node name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,

    /// Container working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,

    /// Container labels (e.g., "key=value").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,

    /// Image pull policy: "always", "never", "missing" (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull: Option<String>,

    /// One-shot commands to execute after container start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub container_exec: Vec<String>,

    /// Health check command (executed inside container).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<String>,

    /// Health check polling interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck_interval: Option<String>,

    /// Health check timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck_timeout: Option<String>,

    /// Startup delay before proceeding with deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_delay: Option<String>,

    /// Environment variables file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<String>,

    /// Config file mounts: (host_path, container_path).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<(String, String)>,

    /// Overlay directory (Kathara-style, mirrors into container root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,

    /// Nodes this node depends on (deployed after dependencies are healthy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// FRR daemons on this node (`frr { ospf … bgp … }`, #65). Overrides
    /// the profile's and the lab's `routing frr { … }` defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frr: Option<FrrConfig>,

    /// Sysctl values (merged with profile).
    #[serde(default)]
    pub sysctls: BTreeMap<String, String>,

    /// Explicitly declared interfaces (beyond those created by links).
    #[serde(default)]
    pub interfaces: BTreeMap<String, InterfaceConfig>,

    /// Routing table entries.
    #[serde(default)]
    pub routes: BTreeMap<String, RouteConfig>,

    /// Firewall rules (overrides profile firewall).
    pub firewall: Option<FirewallConfig>,

    /// NAT rules (masquerade, SNAT, DNAT).
    pub nat: Option<NatConfig>,

    /// Processes to spawn in this namespace.
    #[serde(default)]
    pub exec: Vec<ExecConfig>,

    /// VRF definitions.
    #[serde(default)]
    pub vrfs: BTreeMap<String, VrfConfig>,

    /// WireGuard interfaces.
    #[serde(default)]
    pub wireguard: BTreeMap<String, WireguardConfig>,

    /// macvlan interfaces (attach to host physical NIC).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macvlans: Vec<MacvlanConfig>,

    /// ipvlan interfaces (attach to host physical NIC, shared MAC).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ipvlans: Vec<IpvlanConfig>,

    /// Wi-Fi interfaces (mac80211_hwsim).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wifi: Vec<WifiConfig>,
}

impl Node {
    /// Returns true if this node should be deployed as a container.
    pub fn is_container(&self) -> bool {
        self.image.is_some()
    }
}

/// Interface type for explicit interfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Dummy,
    Vxlan,
    Vlan,
    Bond,
    Loopback,
}

/// Explicit interface configuration (for interfaces not created by links).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InterfaceConfig {
    /// Interface type.
    pub kind: Option<InterfaceKind>,

    /// IP addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,

    /// VXLAN VNI.
    pub vni: Option<u32>,

    /// VXLAN/tunnel local address.
    pub local: Option<String>,

    /// VXLAN/tunnel remote address.
    pub remote: Option<String>,

    /// VXLAN destination port.
    pub port: Option<u16>,

    /// VXLAN underlay parent device name (`IFLA_VXLAN_LINK`).
    /// Plan 159 follow-up — surface the 0.19 `vxlan_underlay_dev`
    /// setter in NLL so users can pin the VXLAN tunnel to a
    /// specific underlay device (e.g. when the namespace has
    /// multiple routes to the remote VTEP).
    pub underlay: Option<String>,

    /// MTU.
    pub mtu: Option<u32>,

    /// Parent interface (for VLAN sub-interfaces).
    pub parent: Option<String>,

    /// Member interfaces (for bond interfaces).
    #[serde(default)]
    pub members: Vec<String>,

    /// Bonding options (`bond … { mode … }`); kernel defaults when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bond: Option<BondOptions>,

    /// VLAN tag protocol for `vlan` sub-interfaces: 802.1Q (default) or
    /// 802.1ad (Q-in-Q outer tag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan_protocol: Option<VlanProtocol>,
}

/// Bonding driver options (issue #75: nlink 0.26 `LinkBuilder::bond_*`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BondOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<BondMode>,
    /// Link monitoring interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miimon: Option<u32>,
    /// LACP rate (802.3ad only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lacp_rate: Option<LacpRate>,
    /// Transmit hash policy (balance-xor / 802.3ad).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xmit_hash: Option<XmitHashPolicy>,
    /// Minimum active links before the bond is up (802.3ad).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_links: Option<u32>,
    /// Delay before enabling a link after it comes up, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updelay: Option<u32>,
    /// Delay before disabling a link after it goes down, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downdelay: Option<u32>,
}

/// Bonding mode (NLL spelling in parentheses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BondMode {
    /// `balance-rr`
    BalanceRr,
    /// `active-backup`
    ActiveBackup,
    /// `balance-xor`
    BalanceXor,
    /// `broadcast`
    Broadcast,
    /// `802.3ad` (LACP)
    #[serde(rename = "802.3ad")]
    Lacp,
    /// `balance-tlb`
    BalanceTlb,
    /// `balance-alb`
    BalanceAlb,
}

impl BondMode {
    /// Parse the NLL / iproute2 spelling.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "balance-rr" => BondMode::BalanceRr,
            "active-backup" => BondMode::ActiveBackup,
            "balance-xor" => BondMode::BalanceXor,
            "broadcast" => BondMode::Broadcast,
            "802.3ad" | "lacp" => BondMode::Lacp,
            "balance-tlb" => BondMode::BalanceTlb,
            "balance-alb" => BondMode::BalanceAlb,
            _ => return None,
        })
    }

    /// The NLL / iproute2 spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            BondMode::BalanceRr => "balance-rr",
            BondMode::ActiveBackup => "active-backup",
            BondMode::BalanceXor => "balance-xor",
            BondMode::Broadcast => "broadcast",
            BondMode::Lacp => "802.3ad",
            BondMode::BalanceTlb => "balance-tlb",
            BondMode::BalanceAlb => "balance-alb",
        }
    }
}

/// LACPDU rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LacpRate {
    /// Every 30 s.
    Slow,
    /// Every second.
    Fast,
}

/// Transmit hash policy (iproute2 spelling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum XmitHashPolicy {
    #[serde(rename = "layer2")]
    Layer2,
    #[serde(rename = "layer3+4")]
    Layer34,
    #[serde(rename = "layer2+3")]
    Layer23,
    #[serde(rename = "encap2+3")]
    Encap23,
    #[serde(rename = "encap3+4")]
    Encap34,
}

impl XmitHashPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "layer2" => XmitHashPolicy::Layer2,
            "layer3+4" => XmitHashPolicy::Layer34,
            "layer2+3" => XmitHashPolicy::Layer23,
            "encap2+3" => XmitHashPolicy::Encap23,
            "encap3+4" => XmitHashPolicy::Encap34,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            XmitHashPolicy::Layer2 => "layer2",
            XmitHashPolicy::Layer34 => "layer3+4",
            XmitHashPolicy::Layer23 => "layer2+3",
            XmitHashPolicy::Encap23 => "encap2+3",
            XmitHashPolicy::Encap34 => "encap3+4",
        }
    }

    /// Kernel `xmit_hash_policy` value (`IFLA_BOND_XMIT_HASH_POLICY`).
    pub fn kernel_value(&self) -> u8 {
        match self {
            XmitHashPolicy::Layer2 => 0,
            XmitHashPolicy::Layer34 => 1,
            XmitHashPolicy::Layer23 => 2,
            XmitHashPolicy::Encap23 => 3,
            XmitHashPolicy::Encap34 => 4,
        }
    }
}

/// VLAN tag protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum VlanProtocol {
    #[serde(rename = "802.1q")]
    Dot1q,
    #[serde(rename = "802.1ad")]
    Dot1ad,
}

impl VlanProtocol {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "802.1q" | "dot1q" => Some(VlanProtocol::Dot1q),
            "802.1ad" | "dot1ad" | "qinq" => Some(VlanProtocol::Dot1ad),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            VlanProtocol::Dot1q => "802.1q",
            VlanProtocol::Dot1ad => "802.1ad",
        }
    }
}

/// Route configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RouteConfig {
    /// Next-hop gateway address.
    pub via: Option<String>,

    /// Output device name.
    pub dev: Option<String>,

    /// Route metric.
    pub metric: Option<u32>,
}

/// Point-to-point link between two nodes (creates a veth pair).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Link {
    /// Endpoints in `"node:interface"` format.
    pub endpoints: [String; 2],

    /// IP addresses in CIDR notation for each endpoint.
    pub addresses: Option<[String; 2]>,

    /// MTU for both ends.
    pub mtu: Option<u32>,
}

/// Shared L2 segment (bridge network).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Network {
    /// Network type (currently only "bridge").
    pub kind: Option<String>,

    /// Enable VLAN filtering on the bridge.
    pub vlan_filtering: Option<bool>,

    /// MTU for the bridge.
    pub mtu: Option<u32>,

    /// Subnet for auto-address assignment.
    pub subnet: Option<String>,

    /// Bridge members as simple list.
    #[serde(default)]
    pub members: Vec<String>,

    /// VLAN definitions.
    #[serde(default, deserialize_with = "deserialize_u16_keys")]
    pub vlans: BTreeMap<u16, VlanConfig>,

    /// Port configurations.
    #[serde(default)]
    pub ports: BTreeMap<String, PortConfig>,

    /// Per-pair impairment rules. Each rule installs a per-destination
    /// netem leaf on the source node's bridge-side interface.
    #[serde(default)]
    pub impairments: Vec<NetworkImpairment>,
}

/// Per-pair impairment within a shared network.
///
/// `src` and `dst` are node names (not endpoints) — the bridge
/// determines the interface. The configured `impairment` is applied
/// to traffic leaving `src`'s network interface destined for `dst`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkImpairment {
    /// Source node name.
    pub src: String,

    /// Destination node name.
    pub dst: String,

    /// netem configuration applied to this pair.
    pub impairment: Impairment,

    /// Optional per-pair rate cap (HTB ceil). Independent of
    /// `impairment.rate` — `rate_cap` builds an HTB shaper on top of
    /// netem, while `impairment.rate` uses netem's built-in
    /// (token-bucket-like) rate limiting.
    pub rate_cap: Option<String>,
}

/// VLAN definition within a network.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VlanConfig {
    /// Human-readable VLAN name.
    pub name: Option<String>,
}

/// Port configuration within a bridge network.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PortConfig {
    /// Interface name on the node.
    pub interface: Option<String>,

    /// VLAN IDs this port carries.
    #[serde(default)]
    pub vlans: Vec<u16>,

    /// Whether this port carries tagged traffic.
    pub tagged: Option<bool>,

    /// Native VLAN ID (PVID).
    pub pvid: Option<u16>,

    /// Whether to strip VLAN tags on egress.
    pub untagged: Option<bool>,

    /// IP addresses for this port.
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// Network impairment configuration (netem).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Impairment {
    /// Delay (e.g., "10ms", "100us").
    pub delay: Option<String>,

    /// Jitter (e.g., "2ms").
    pub jitter: Option<String>,

    /// Packet loss (e.g., "0.1%", "5%").
    pub loss: Option<String>,

    /// Bandwidth rate limit (e.g., "100mbit", "1gbit").
    pub rate: Option<String>,

    /// Packet corruption (e.g., "0.01%").
    pub corrupt: Option<String>,

    /// Packet reordering (e.g., "0.5%").
    pub reorder: Option<String>,

    /// Packet duplication (e.g., "1%").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<String>,

    /// Correlation of successive delay values (e.g., "25%").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_correlation: Option<String>,

    /// Correlation of successive loss decisions (e.g., "25%").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_correlation: Option<String>,

    /// netem queue limit in packets (default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
}

impl Impairment {
    /// Every property name [`Impairment::set_property`] accepts, in the
    /// order they are listed to the user.
    pub const PROPERTIES: [&'static str; 10] = [
        "delay",
        "jitter",
        "loss",
        "rate",
        "corrupt",
        "reorder",
        "duplicate",
        "delay-correlation",
        "loss-correlation",
        "limit",
    ];

    /// Set one property by its NLL/CLI name.
    ///
    /// Values are kept verbatim: the tc planner is what parses units, so
    /// `impair --loss`, `edit --set-impair` and the `top` prompt all
    /// behave identically. Returns the list of accepted names on an
    /// unknown key, so callers do not each maintain their own copy.
    pub fn set_property(&mut self, key: &str, value: &str) -> Result<(), String> {
        let value = Some(value.trim().to_string());
        match key.trim() {
            "delay" => self.delay = value,
            "jitter" => self.jitter = value,
            "loss" => self.loss = value,
            "rate" => self.rate = value,
            "corrupt" => self.corrupt = value,
            "reorder" => self.reorder = value,
            "duplicate" => self.duplicate = value,
            "delay-correlation" => self.delay_correlation = value,
            "loss-correlation" => self.loss_correlation = value,
            "limit" => self.limit = value,
            other => {
                return Err(format!(
                    "unknown property {other:?} ({})",
                    Self::PROPERTIES.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// The inverse of [`Impairment::set_property`]: a one-line
    /// `delay 50ms loss 1%` rendering, empty when nothing is set.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (key, value) in [
            ("delay", &self.delay),
            ("jitter", &self.jitter),
            ("loss", &self.loss),
            ("rate", &self.rate),
            ("corrupt", &self.corrupt),
            ("reorder", &self.reorder),
            ("duplicate", &self.duplicate),
            ("delay-correlation", &self.delay_correlation),
            ("loss-correlation", &self.loss_correlation),
            ("limit", &self.limit),
        ] {
            if let Some(v) = value {
                parts.push(format!("{key} {v}"));
            }
        }
        parts.join(" ")
    }
}

/// Per-interface rate limiting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RateLimit {
    /// Egress rate (e.g., "1gbit").
    pub egress: Option<String>,

    /// Ingress rate (e.g., "1gbit").
    pub ingress: Option<String>,

    /// Burst size (e.g., "10mbit").
    pub burst: Option<String>,
}

/// A root qdisc other than netem on one interface (issue #67).
///
/// Values keep the NLL spelling (`"10mbit"`, `"32kb"`, `"5ms"`) like
/// [`Impairment`]; the planner parses them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QdiscConfig {
    #[serde(flatten)]
    pub kind: QdiscKind,
}

/// Which classless qdisc and its parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QdiscKind {
    /// Token bucket filter: `rate` and `burst` are mandatory.
    Tbf {
        rate: String,
        burst: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peakrate: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtu: Option<u32>,
    },
    /// Fair queuing with controlled delay.
    FqCodel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flows: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantum: Option<u32>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ecn: bool,
    },
    /// Stochastic fairness queuing.
    Sfq {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        perturb: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantum: Option<u32>,
    },
    /// Priority bands.
    Prio {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bands: Option<u8>,
    },
}

impl QdiscKind {
    /// The tc kind name (`tbf`, `fq_codel`, `sfq`, `prio`).
    pub fn name(&self) -> &'static str {
        match self {
            QdiscKind::Tbf { .. } => "tbf",
            QdiscKind::FqCodel { .. } => "fq_codel",
            QdiscKind::Sfq { .. } => "sfq",
            QdiscKind::Prio { .. } => "prio",
        }
    }
}

/// Firewall configuration (nftables).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallConfig {
    /// Default chain policy ("accept" or "drop").
    pub policy: Option<String>,

    /// Firewall rules.
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
}

/// A single firewall rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallRule {
    /// Match expression (e.g., "tcp dport 80", "ct state established,related").
    #[serde(rename = "match")]
    pub match_expr: Option<String>,

    /// Action ("accept", "drop", "reject").
    pub action: Option<String>,
}

/// NAT configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NatConfig {
    /// NAT rules.
    #[serde(default)]
    pub rules: Vec<NatRule>,
}

/// A single NAT rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NatRule {
    /// NAT action.
    pub action: NatAction,
    /// Source CIDR match.
    pub src: Option<String>,
    /// Destination CIDR match.
    pub dst: Option<String>,
    /// Target address for SNAT/DNAT.
    pub target: Option<String>,
    /// Target port for DNAT.
    pub target_port: Option<u16>,
}

/// NAT action type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Translate,
}

/// Process to execute in a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecConfig {
    /// Command and arguments.
    pub cmd: Vec<String>,

    /// Run in background.
    #[serde(default)]
    pub background: bool,
}

/// VRF (Virtual Routing and Forwarding) configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VrfConfig {
    /// Routing table ID.
    pub table: u32,

    /// Interfaces to enslave to this VRF.
    #[serde(default)]
    pub interfaces: Vec<String>,

    /// Routes within this VRF.
    #[serde(default)]
    pub routes: BTreeMap<String, RouteConfig>,
}

/// WireGuard interface configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WireguardConfig {
    /// Private key ("auto" to auto-generate).
    pub private_key: Option<String>,

    /// Listen port.
    pub listen_port: Option<u16>,

    /// Routing mark applied to outbound tunnel packets. Plan 159
    /// follow-up — surfaces nlink 0.19's
    /// `DeclaredWgDeviceBuilder::fwmark` in NLL so policy-routing
    /// setups can match WG-encapsulated traffic.
    pub fwmark: Option<u32>,

    /// Interface addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,

    /// Peer node names (resolved during deployment).
    #[serde(default)]
    pub peers: Vec<String>,
}

/// macvlan interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MacvlanConfig {
    /// Interface name inside the namespace.
    pub name: String,
    /// Host parent interface (e.g., "enp3s0").
    pub parent: String,
    /// macvlan mode.
    #[serde(default)]
    pub mode: MacvlanMode,
    /// IP addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// macvlan mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MacvlanMode {
    #[default]
    Bridge,
    Private,
    Vepa,
    Passthru,
}

/// ipvlan interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IpvlanConfig {
    /// Interface name inside the namespace.
    pub name: String,
    /// Host parent interface (e.g., "enp3s0").
    pub parent: String,
    /// ipvlan mode.
    #[serde(default)]
    pub mode: IpvlanMode,
    /// IP addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// ipvlan mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IpvlanMode {
    L2,
    #[default]
    L3,
    L3S,
}

/// Wi-Fi interface configuration (mac80211_hwsim).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WifiConfig {
    /// Interface name inside the namespace (e.g., "wlan0").
    pub name: String,
    /// Wi-Fi mode.
    pub mode: WifiMode,
    /// Network SSID (required for AP and Station modes).
    pub ssid: Option<String>,
    /// Wi-Fi channel number.
    pub channel: Option<u32>,
    /// WPA2-PSK passphrase (omit for open network).
    pub passphrase: Option<String>,
    /// Mesh network identifier (required for Mesh mode).
    pub mesh_id: Option<String>,
    /// IP addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// Wi-Fi interface mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WifiMode {
    /// Access point (runs hostapd).
    Ap,
    /// Client station (runs wpa_supplicant).
    Station,
    /// 802.11s mesh point.
    Mesh,
}

// ─────────────────────────────────────────────────
// Serde helpers
// ─────────────────────────────────────────────────

/// Deserialize a `BTreeMap<u16, V>` from TOML tables where keys are strings.
fn deserialize_u16_keys<'de, V, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<u16, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    let string_map: BTreeMap<String, V> = BTreeMap::deserialize(deserializer)?;
    string_map
        .into_iter()
        .map(|(k, v)| {
            let key: u16 = k.parse().map_err(serde::de::Error::custom)?;
            Ok((key, v))
        })
        .collect()
}

// ─────────────────────────────────────────────────
// Helper methods
// ─────────────────────────────────────────────────

/// A parsed endpoint reference ("node:interface").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointRef {
    /// Node name.
    pub node: String,
    /// Interface name.
    pub iface: String,
}

impl EndpointRef {
    /// Parse a "node:interface" string.
    pub fn parse(s: &str) -> Option<Self> {
        let (node, iface) = s.split_once(':')?;
        if node.is_empty() || iface.is_empty() {
            return None;
        }
        Some(Self {
            node: node.to_string(),
            iface: iface.to_string(),
        })
    }
}

impl std::fmt::Display for EndpointRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.node, self.iface)
    }
}

impl Topology {
    /// Deploy this topology, creating all namespaces and network configuration.
    ///
    /// Validates the topology first, then creates the lab. Returns a
    /// [`RunningLab`](crate::RunningLab) handle.
    pub async fn deploy(&self) -> crate::Result<crate::running::RunningLab> {
        crate::deploy::deploy(self).await
    }

    /// Get the effective namespace name for a node.
    pub fn namespace_name(&self, node_name: &str) -> String {
        format!("{}-{}", self.lab.prefix(), node_name)
    }

    /// Get the effective sysctls for a node (profile + node-level merged).
    /// The FRR configuration a node runs under `routing frr`: the node's
    /// own `frr { … }`, else the last profile's, else the lab default
    /// (`routing frr { … }`) for every forwarding namespace node.
    /// `None` when the lab is not in FRR mode or the node runs nothing.
    pub fn effective_frr(&self, node: &Node) -> Option<FrrConfig> {
        if self.lab.routing != RoutingMode::Frr || node.is_container() {
            return None;
        }
        if let Some(f) = &node.frr {
            return Some(f.clone());
        }
        for profile_name in node.profiles.iter().rev() {
            if let Some(f) = self.profiles.get(profile_name).and_then(|p| p.frr.as_ref()) {
                return Some(f.clone());
            }
        }
        let forwards = self
            .effective_sysctls(node)
            .get("net.ipv4.ip_forward")
            .is_some_and(|v| v == "1");
        if forwards {
            return self.lab.frr.clone();
        }
        None
    }

    pub fn effective_sysctls(&self, node: &Node) -> BTreeMap<String, String> {
        let mut sysctls = BTreeMap::new();

        // Start with profile sysctls, in declaration order (later
        // profiles override earlier ones)
        for profile_name in &node.profiles {
            if let Some(profile) = self.profiles.get(profile_name) {
                sysctls.extend(profile.sysctls.clone());
            }
        }

        // Node-level sysctls override profile
        sysctls.extend(node.sysctls.clone());

        sysctls
    }

    /// Get the effective firewall config for a node (node overrides profile).
    pub fn effective_firewall<'a>(&'a self, node: &'a Node) -> Option<&'a FirewallConfig> {
        if node.firewall.is_some() {
            return node.firewall.as_ref();
        }
        // Last profile carrying a firewall wins (later profiles override
        // earlier ones, mirroring sysctl merge order).
        node.profiles
            .iter()
            .rev()
            .find_map(|name| self.profiles.get(name).and_then(|p| p.firewall.as_ref()))
    }
}

#[cfg(test)]
mod name_hash_tests {
    use super::*;

    #[test]
    fn network_peer_name_fits_ifname_budget() {
        // Linux IFNAMSIZ is 16 (15 + NUL). Names must be ≤ 15 chars.
        for idx in 0..10_000usize {
            let n = network_peer_name_for("anything", idx);
            assert!(
                n.len() <= 15,
                "peer name {n:?} ({} chars) exceeds 15-char ifname budget",
                n.len()
            );
        }
    }

    #[test]
    fn network_peer_name_disambiguates_shared_prefixes() {
        // Regression test: before the hash migration, `lan_a` and `lan_b`
        // both truncated to `lan_` and produced colliding `brlan_p{idx}`
        // peer names, causing the second veth create to EEXIST. Hash-based
        // names must differ.
        let a0 = network_peer_name_for("lan_a", 0);
        let b0 = network_peer_name_for("lan_b", 0);
        let c0 = network_peer_name_for("lan_c", 0);
        assert_ne!(a0, b0);
        assert_ne!(a0, c0);
        assert_ne!(b0, c0);
    }

    #[test]
    fn network_peer_name_is_deterministic() {
        assert_eq!(
            network_peer_name_for("radio", 3),
            network_peer_name_for("radio", 3)
        );
    }

    #[test]
    fn network_peer_name_uses_np_prefix() {
        let n = network_peer_name_for("mynet", 0);
        assert!(n.starts_with("np"), "expected np prefix, got {n}");
    }

    #[test]
    fn network_bridge_name_fits_ifname_budget() {
        // `nb{hash8}` = 10 chars, always within the 15-char budget.
        for net_name in ["a", "lan", "very-long-network-name-that-would-overflow"] {
            let n = network_bridge_name_for(net_name);
            assert!(
                n.len() <= 15,
                "bridge name {n:?} ({} chars) exceeds 15-char ifname budget",
                n.len(),
            );
        }
    }

    #[test]
    fn network_bridge_name_disambiguates_shared_prefixes() {
        // Regression test for the bridge-naming bug: when the lab
        // prefix grew long (as #[lab_test] forces), the old
        // `{prefix}-{net_name}[..15]` truncation collapsed
        // `lan_a`/`lan_b` to the same name. Hash-based names must
        // differ.
        let a = network_bridge_name_for("lan_a");
        let b = network_bridge_name_for("lan_b");
        let c = network_bridge_name_for("lan_c");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn network_bridge_name_is_deterministic() {
        assert_eq!(
            network_bridge_name_for("radio"),
            network_bridge_name_for("radio"),
        );
    }

    #[test]
    fn network_bridge_name_uses_nb_prefix() {
        let n = network_bridge_name_for("mynet");
        assert!(n.starts_with("nb"), "expected nb prefix, got {n}");
    }
}
