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
//! ├── profiles: HashMap<String, Profile>
//! ├── nodes: HashMap<String, Node>
//! ├── links: Vec<Link>
//! ├── networks: HashMap<String, Network>
//! ├── impairments: HashMap<String, Impairment>
//! └── rate_limits: HashMap<String, RateLimit>
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Complete topology definition for a network lab.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Topology {
    /// Lab metadata.
    pub lab: LabConfig,

    /// Reusable node profiles.
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,

    /// Node definitions (each becomes a network namespace).
    #[serde(default)]
    pub nodes: HashMap<String, Node>,

    /// Point-to-point links between nodes (veth pairs).
    #[serde(default)]
    pub links: Vec<Link>,

    /// Shared L2 segments (bridges).
    #[serde(default)]
    pub networks: HashMap<String, Network>,

    /// Per-interface network impairment (netem).
    #[serde(default)]
    pub impairments: HashMap<String, Impairment>,

    /// Per-interface rate limiting.
    #[serde(default)]
    pub rate_limits: HashMap<String, RateLimit>,

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Benchmark {
    /// Benchmark name.
    pub name: String,
    /// Individual benchmark tests.
    pub tests: Vec<BenchmarkTest>,
}

/// A single benchmark test.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkAssertion {
    /// Metric name (bandwidth, jitter, avg, p99, loss).
    pub metric: String,
    /// Comparison operator.
    pub op: CompareOp,
    /// Threshold value (e.g., "900mbit", "5ms", "1%").
    pub value: String,
}

/// Comparison operator for benchmark assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareOp {
    Gt,
    Lt,
    Gte,
    Lte,
}

/// A timed test scenario with fault injection and validation steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario name.
    pub name: String,
    /// Ordered steps (sorted by time).
    pub steps: Vec<ScenarioStep>,
}

/// A single step in a scenario, executed at a specific time offset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStep {
    /// Time offset from scenario start (milliseconds).
    pub time_ms: u64,
    /// Actions to execute at this time.
    pub actions: Vec<ScenarioAction>,
}

/// An action within a scenario step.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    /// No DNS configuration (default).
    #[default]
    Off,
    /// Auto-generate /etc/hosts entries from topology.
    Hosts,
}

/// Routing mode for automatic static route generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    /// No auto-routing (default).
    #[default]
    Manual,
    /// Compute static routes from topology graph.
    Auto,
}

/// Container runtime selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// Reusable node template.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Sysctl values to apply.
    #[serde(default)]
    pub sysctls: HashMap<String, String>,

    /// Firewall configuration.
    pub firewall: Option<FirewallConfig>,
}

/// Node definition — becomes a network namespace or container.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Node {
    /// Profile to inherit from.
    pub profile: Option<String>,

    /// Container image (when set, node is deployed as a container instead of bare namespace).
    pub image: Option<String>,

    /// Container command override (requires `image`).
    pub cmd: Option<Vec<String>>,

    /// Container environment variables (requires `image`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

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

    /// Sysctl values (merged with profile).
    #[serde(default)]
    pub sysctls: HashMap<String, String>,

    /// Explicitly declared interfaces (beyond those created by links).
    #[serde(default)]
    pub interfaces: HashMap<String, InterfaceConfig>,

    /// Routing table entries.
    #[serde(default)]
    pub routes: HashMap<String, RouteConfig>,

    /// Firewall rules (overrides profile firewall).
    pub firewall: Option<FirewallConfig>,

    /// NAT rules (masquerade, SNAT, DNAT).
    pub nat: Option<NatConfig>,

    /// Processes to spawn in this namespace.
    #[serde(default)]
    pub exec: Vec<ExecConfig>,

    /// VRF definitions.
    #[serde(default)]
    pub vrfs: HashMap<String, VrfConfig>,

    /// WireGuard interfaces.
    #[serde(default)]
    pub wireguard: HashMap<String, WireguardConfig>,

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Dummy,
    Vxlan,
    Vlan,
    Bond,
    Loopback,
}

/// Explicit interface configuration (for interfaces not created by links).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

    /// MTU.
    pub mtu: Option<u32>,

    /// Parent interface (for VLAN sub-interfaces).
    pub parent: Option<String>,

    /// Member interfaces (for bond interfaces).
    #[serde(default)]
    pub members: Vec<String>,
}

/// Route configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Next-hop gateway address.
    pub via: Option<String>,

    /// Output device name.
    pub dev: Option<String>,

    /// Route metric.
    pub metric: Option<u32>,
}

/// Point-to-point link between two nodes (creates a veth pair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    /// Endpoints in `"node:interface"` format.
    pub endpoints: [String; 2],

    /// IP addresses in CIDR notation for each endpoint.
    pub addresses: Option<[String; 2]>,

    /// MTU for both ends.
    pub mtu: Option<u32>,
}

/// Shared L2 segment (bridge network).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub vlans: HashMap<u16, VlanConfig>,

    /// Port configurations.
    #[serde(default)]
    pub ports: HashMap<String, PortConfig>,

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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VlanConfig {
    /// Human-readable VLAN name.
    pub name: Option<String>,
}

/// Port configuration within a bridge network.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
}

/// Per-interface rate limiting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Egress rate (e.g., "1gbit").
    pub egress: Option<String>,

    /// Ingress rate (e.g., "1gbit").
    pub ingress: Option<String>,

    /// Burst size (e.g., "10mbit").
    pub burst: Option<String>,
}

/// Firewall configuration (nftables).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirewallConfig {
    /// Default chain policy ("accept" or "drop").
    pub policy: Option<String>,

    /// Firewall rules.
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
}

/// A single firewall rule.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirewallRule {
    /// Match expression (e.g., "tcp dport 80", "ct state established,related").
    #[serde(rename = "match")]
    pub match_expr: Option<String>,

    /// Action ("accept", "drop", "reject").
    pub action: Option<String>,
}

/// NAT configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NatConfig {
    /// NAT rules.
    #[serde(default)]
    pub rules: Vec<NatRule>,
}

/// A single NAT rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Translate,
}

/// Process to execute in a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecConfig {
    /// Command and arguments.
    pub cmd: Vec<String>,

    /// Run in background.
    #[serde(default)]
    pub background: bool,
}

/// VRF (Virtual Routing and Forwarding) configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VrfConfig {
    /// Routing table ID.
    pub table: u32,

    /// Interfaces to enslave to this VRF.
    #[serde(default)]
    pub interfaces: Vec<String>,

    /// Routes within this VRF.
    #[serde(default)]
    pub routes: HashMap<String, RouteConfig>,
}

/// WireGuard interface configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireguardConfig {
    /// Private key ("auto" to auto-generate).
    pub private_key: Option<String>,

    /// Listen port.
    pub listen_port: Option<u16>,

    /// Interface addresses in CIDR notation.
    #[serde(default)]
    pub addresses: Vec<String>,

    /// Peer node names (resolved during deployment).
    #[serde(default)]
    pub peers: Vec<String>,
}

/// macvlan interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacvlanMode {
    #[default]
    Bridge,
    Private,
    Vepa,
    Passthru,
}

/// ipvlan interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpvlanMode {
    L2,
    #[default]
    L3,
    L3S,
}

/// Wi-Fi interface configuration (mac80211_hwsim).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Deserialize a `HashMap<u16, V>` from TOML tables where keys are strings.
fn deserialize_u16_keys<'de, V, D>(
    deserializer: D,
) -> std::result::Result<HashMap<u16, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    let string_map: HashMap<String, V> = HashMap::deserialize(deserializer)?;
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
    pub fn effective_sysctls(&self, node: &Node) -> HashMap<String, String> {
        let mut sysctls = HashMap::new();

        // Start with profile sysctls
        if let Some(profile_name) = &node.profile
            && let Some(profile) = self.profiles.get(profile_name)
        {
            sysctls.extend(profile.sysctls.clone());
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
        if let Some(profile_name) = &node.profile
            && let Some(profile) = self.profiles.get(profile_name)
        {
            return profile.firewall.as_ref();
        }
        None
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
}
