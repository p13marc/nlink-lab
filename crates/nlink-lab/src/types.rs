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

    /// The endpoints of [`impairments`](Self::impairments) a link's `->` or
    /// `<-` declared: one direction, on purpose. Read by `lint` and
    /// `render` (#223); not persisted, and no part of what a lab is.
    #[serde(skip)]
    pub directional_impairments: std::collections::BTreeSet<String>,

    /// Per-interface rate limiting.
    #[serde(default)]
    pub rate_limits: BTreeMap<String, RateLimit>,

    /// Per-interface root qdisc other than netem (`qdisc a:eth0 tbf { … }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub qdiscs: BTreeMap<String, QdiscConfig>,

    /// Post-deploy reachability assertions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<Assertion>,

    /// How many times in all to evaluate a failing assertion before
    /// reporting it (`retries 1`, or none, is one attempt), and how long
    /// to wait between attempts (`validate retries 20 interval 2s { … }`).
    ///
    /// Assertions run the moment the deploy finishes, which is too
    /// early for anything that has to converge: a `routing frr { bgp }`
    /// lab needs tens of seconds before `reach` or `route-has` can pass,
    /// so `examples/frr-bgp.nll` asserted against a routing table that
    /// did not exist yet. `tcp-connect` has had per-assertion
    /// `retries`/`interval` since it was added; this is the same idea
    /// for the whole block, because convergence is a property of the
    /// lab rather than of one assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion_retries: Option<u32>,

    /// Delay between assertion attempts. See
    /// [`assertion_retries`](Self::assertion_retries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion_interval: Option<String>,

    /// Timed test scenarios (fault injection + validation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<Scenario>,

    /// Performance benchmarks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benchmarks: Vec<Benchmark>,

    /// The nodes each `site` block declared, by site (#234). The node
    /// names already carry the site's prefix; this keeps the group, for
    /// views that draw it. `render` writes the flat topology without it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sites: BTreeMap<String, Vec<String>>,

    /// The directory a container's relative host paths (`config`
    /// sources, `overlay`, `env-file`) are relative to: the topology
    /// file's, set when it is parsed from one (#285); a daemon deploy
    /// sets the client's. `None` (parsed from text): relative to the
    /// directory the deploy runs in. `render` leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
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
        /// Target rate (iperf3 `-b`, e.g. `"50mbit"`). Without it a UDP
        /// test runs at iperf3's default of 1 Mbit/s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<String>,
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
    Eq,
    Ne,
}

impl CompareOp {
    /// `value OP threshold`.
    pub fn holds<T: PartialOrd>(self, value: T, threshold: T) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Lt => value < threshold,
            Self::Gte => value >= threshold,
            Self::Lte => value <= threshold,
            Self::Eq => value == threshold,
            Self::Ne => value != threshold,
        }
    }

    /// The NLL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Gte => ">=",
            Self::Lte => "<=",
            Self::Eq => "==",
            Self::Ne => "!=",
        }
    }
}

/// A timed test scenario with fault injection and validation steps.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Scenario {
    /// Scenario name.
    pub name: String,
    /// Ordered steps (sorted by time).
    pub steps: Vec<ScenarioStep>,
    /// Leave what the scenario changed as it is when it ends (`keep`).
    /// Without it, its impairments, partitions, pair impairments and the
    /// interfaces it took down are put back as they were before it ran
    /// (#326).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep: bool,
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
    /// Set an endpoint's impairment, as `impair EP …` does (#326).
    Impair {
        endpoint: String,
        impairment: Impairment,
    },
    /// Partition an endpoint, and with `both` the other end of its link,
    /// as `impair EP --partition` does (#326).
    Partition { endpoint: String, both: bool },
    /// End a partition (`both`: of the link's two ends) (#326).
    Heal { endpoint: String, both: bool },
    /// Set a network's impairment from `src` to `dst`, as `impair --pair`
    /// does (#326).
    PairImpair {
        network: String,
        src: String,
        dst: String,
        impairment: Impairment,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate_cap: Option<String>,
    },
    /// Play a recorded trace (a CSV of delay, jitter, loss and rate over
    /// time) on an endpoint or a per-pair impairment, while the rest of
    /// the scenario runs (#340). `file` is relative to the topology's
    /// file.
    Replay {
        target: crate::replay::ReplayTarget,
        file: String,
        #[serde(default = "default_replay_step_ms")]
        step_ms: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        looped: bool,
    },
}

/// How often a replay applies its trace, unless it says `step` (#340).
pub const DEFAULT_REPLAY_STEP_MS: u64 = 100;

fn default_replay_step_ms() -> u64 {
    DEFAULT_REPLAY_STEP_MS
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
    /// Assert on a named firewall counter of `node`
    /// (`counter NODE NAME [packets|bytes] OP N`).
    Counter {
        node: String,
        name: String,
        #[serde(default)]
        metric: CounterMetric,
        op: CompareOp,
        value: u64,
    },
}

/// What a [`Assertion::Counter`] reads.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CounterMetric {
    #[default]
    Packets,
    Bytes,
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

    /// The lab's seed (`seed 42`, `deploy --seed`, `#[lab_test(seed =
    /// N)]`): every netem qdisc's seed is derived from it and its place in
    /// the lab, so a redeploy loses and corrupts the same packets of the
    /// same traffic (#337). A deploy without one draws one and records it
    /// here; a kernel that ignores netem seeds gets none. At most
    /// [`crate::seed::MAX`], since the state stores it as TOML's i64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
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

    /// Append `-SUFFIX` to the lab's name, and to its `prefix` when it
    /// sets one: namespaces, containers and FRR pathspaces are named
    /// after the prefix, so renaming the lab alone left two suffixed
    /// copies of a prefixed topology on the same names (#221, #222).
    pub fn add_suffix(&mut self, suffix: &str) {
        self.name = format!("{}-{suffix}", self.name);
        if let Some(prefix) = &mut self.prefix {
            *prefix = format!("{prefix}-{suffix}");
        }
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

    /// The root-namespace bridge a foreign tap node's tap joins (#344):
    /// `nt{hash8}`, of the lab and the node.
    pub fn tap_bridge_name(&self, node: &str) -> String {
        format!("nt{}", name_hash_str(&format!("{}/{node}", self.name)))
    }

    /// The root-namespace end of the `k`th link to a foreign tap node,
    /// on its bridge: `nt{hash8}p{k}`.
    pub fn tap_port_name(&self, node: &str, k: usize) -> String {
        format!("{}p{k}", self.tap_bridge_name(node))
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

    /// How long deploy waits for the health check to pass, in all
    /// (default 30s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck_timeout: Option<String>,

    /// Give up after this many failed probes, before the timeout (#217).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck_retries: Option<u32>,

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

    /// `kind foreign { … }`: a namespace, container or tap nlink-lab did
    /// not create (#344). It links to it and never manages it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign: Option<Foreign>,
}

impl Node {
    /// Returns true if this node should be deployed as a container.
    pub fn is_container(&self) -> bool {
        self.image.is_some()
    }

    /// Whether this node is `kind foreign` (#344).
    pub fn is_foreign(&self) -> bool {
        self.foreign.is_some()
    }
}

/// A node another tool made (#344): nlink-lab creates the lab side of its
/// links, and never creates, stops or deletes the target itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Foreign {
    pub target: ForeignTarget,
    /// Configure the addresses and routes declared on its side of its
    /// links (`manage addresses`). Off: its side is left as it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manage_addresses: bool,
    /// Put the impairments declared on its side of its links inside it
    /// (`manage tc`). Off: they are refused, and a link's impairments
    /// apply on the lab side only, as egress toward it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manage_tc: bool,
}

/// What a foreign node is (#344).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ForeignTarget {
    /// A network namespace: a path, or a name under `/run/netns`.
    Netns(String),
    /// A container another tool runs, by name or id.
    Container(String),
    /// A tap in the host's namespace (a VM's NIC).
    Tap(String),
}

impl ForeignTarget {
    /// The NLL keyword: `netns`, `container`, `tap`.
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Netns(_) => "netns",
            Self::Container(_) => "container",
            Self::Tap(_) => "tap",
        }
    }

    /// What it names: the path or name, the container, the tap.
    pub fn name(&self) -> &str {
        match self {
            Self::Netns(s) | Self::Container(s) | Self::Tap(s) => s,
        }
    }

    /// A netns target's path: a bare name is under `/run/netns`.
    pub fn netns_path(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Netns(s) if s.contains('/') => Some(s.into()),
            Self::Netns(s) => Some(std::path::Path::new("/run/netns").join(s)),
            _ => None,
        }
    }
}

impl std::fmt::Display for ForeignTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?}", self.keyword(), self.name())
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

    /// nlink's policy of the same name.
    pub fn to_nlink(self) -> nlink::netlink::config::BondXmitHashPolicy {
        use nlink::netlink::config::BondXmitHashPolicy as P;
        match self {
            XmitHashPolicy::Layer2 => P::Layer2,
            XmitHashPolicy::Layer34 => P::Layer34,
            XmitHashPolicy::Layer23 => P::Layer23,
            XmitHashPolicy::Encap23 => P::Encap23,
            XmitHashPolicy::Encap34 => P::Encap34,
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

    /// Packet loss, in `tc`'s grammar: independent loss (`"0.1%"`), or a
    /// netem Markov model for **bursty** loss — Gilbert-Elliott
    /// `"gemodel p [r [1-h [1-k]]]"` (e.g. `"gemodel 1% 30% 50% 0.1%"`) or
    /// the 4-state `"state p13 [p31 [p32 [p23 [p14]]]]"` (#153). A model's
    /// values and defaults are nlink's, checked against iproute2.
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

    /// netem's loss "correlation" (e.g., "25%"). **It does not make loss
    /// bursty, it lowers the loss rate**: netem averages each random draw
    /// with the previous one, so a small loss probability almost never
    /// triggers (`loss 0.5%` with `loss-correlation 25%` drops ~0 %). See
    /// [`Impairment::loss_correlation_warning`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_correlation: Option<String>,

    /// netem queue limit in packets (default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,

    /// The seed of this netem's random numbers (`seed 42`), in place of
    /// the one derived from the lab's seed (#337). It makes the loss and
    /// corruption decisions repeat; see [`LabConfig::seed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// The loss rate, in percent, that netem's correlated generator produces
/// for `loss_pct` with correlation `corr_pct`: the kernel's `get_crandom`
/// (`value = (1 - ρ)·rand + ρ·last`, lost when `value < p`), run for a fixed
/// number of draws from a fixed seed so the answer is deterministic.
fn netem_correlated_rate(loss_pct: f64, corr_pct: f64) -> f64 {
    const DRAWS: u32 = 200_000;
    let (p, rho) = (loss_pct / 100.0, corr_pct / 100.0);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15; // xorshift64*, fixed seed
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut last = next();
    let mut lost = 0u32;
    for _ in 0..DRAWS {
        last = (1.0 - rho) * next() + rho * last;
        if last < p {
            lost += 1;
        }
    }
    100.0 * f64::from(lost) / f64::from(DRAWS)
}

impl Impairment {
    /// Parse an impairment as people type it (#233): `delay 50ms loss 1%`,
    /// or `delay=50ms,loss=1%`, the grammar of `impair`, `top`'s prompt and
    /// the GUI. A `loss` value runs up to the next property, so a netem
    /// Markov model (`loss gemodel 1% 30% 70% 0.1%`) is one value. Obvious
    /// mistakes (a duration without a unit, a percentage over 100%, an
    /// unknown key) are refused here, where they can be corrected.
    pub fn parse_spec(input: &str) -> Result<Impairment, String> {
        let mut words: Vec<String> = Vec::new();
        for token in input
            .split([' ', '\t', '\n', ','])
            .filter(|t| !t.is_empty())
        {
            match token.split_once('=') {
                Some((k, v)) => {
                    words.push(k.to_string());
                    words.push(v.to_string());
                }
                None => words.push(token.to_string()),
            }
        }
        if words.is_empty() {
            return Err("empty impairment spec — try: delay 50ms loss 1%".to_string());
        }
        let is_key = |w: &str| Self::PROPERTIES.contains(&w.to_ascii_lowercase().as_str());
        let mut imp = Impairment::default();
        let mut seen: Vec<String> = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let key = words[i].to_ascii_lowercase();
            if !is_key(&key) {
                return Err(format!(
                    "unknown impairment {key:?}: one of {}",
                    Self::PROPERTIES.join(", ")
                ));
            }
            let mut end = i + 2;
            if key == "loss" {
                while end < words.len() && !is_key(&words[end]) {
                    end += 1;
                }
            }
            let Some(value) =
                (i + 1 < words.len()).then(|| words[i + 1..end.min(words.len())].join(" "))
            else {
                return Err(format!(
                    "{key:?}: expected a value (KEY VALUE pairs, e.g. delay 50ms loss 1%)"
                ));
            };
            if seen.contains(&key) {
                return Err(format!("duplicate key {key:?}"));
            }
            Self::validate_property(&key, &value)?;
            imp.set_property(&key, &value)?;
            seen.push(key);
            i = end;
        }
        Ok(imp)
    }

    /// Refuse a value that is wrong whatever tc's rules: an empty one, a
    /// duration without a unit, a percentage outside 0..100, a packet
    /// count that is not a number. `rate` is left to tc's own spelling.
    pub fn validate_property(key: &str, value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err(format!("{key:?}: empty value"));
        }
        let percent = |v: &str| -> Result<(), String> {
            let n: f64 = v
                .trim_end_matches('%')
                .parse()
                .map_err(|_| format!("{key:?}: expected a percentage, got {v:?}"))?;
            if !(0.0..=100.0).contains(&n) {
                return Err(format!("{key:?}: {v} is outside 0%..100%"));
            }
            Ok(())
        };
        match key {
            "delay" | "jitter" => crate::helpers::parse_duration(value)
                .map(|_| ())
                .map_err(|e| format!("{key:?}: {e}")),
            "limit" => value
                .parse::<u32>()
                .map(|_| ())
                .map_err(|_| format!("{key:?}: expected a packet count, got {value:?}")),
            "seed" => crate::seed::parse(value)
                .map(|_| ())
                .map_err(|e| format!("{key:?}: {e}")),
            // a Markov model: its parameters are percentages
            "loss" if value.starts_with("gemodel ") || value.starts_with("state ") => {
                value.split_whitespace().skip(1).try_for_each(percent)
            }
            "loss" | "corrupt" | "reorder" | "duplicate" | "delay-correlation"
            | "loss-correlation" => percent(value),
            _ => Ok(()),
        }
    }

    /// The warning to show when `loss` is combined with `loss-correlation`,
    /// or `None`.
    ///
    /// netem's correlation is not a statistical correlation; `tc-netem(8)`
    /// calls it "an approximation". Each random draw is averaged with the
    /// previous one, `value = (1 - ρ)·rand + ρ·last`, and a packet is lost
    /// when that value falls below the loss probability. Averaging pulls the
    /// value towards the middle of its range, so a small probability almost
    /// never triggers: `loss 0.5%` with `loss-correlation 25%` dropped 0 of
    /// 40 000 packets on Linux 6.12, where `loss 0.5%` alone dropped 0.48 %.
    /// The documentation used to promise "bursty loss" instead (#152).
    ///
    /// The message quotes the effective rate, computed from that formula
    /// with a fixed seed, so it is the same on every run. Unparseable
    /// values return `None`; the validator reports those on its own.
    pub fn loss_correlation_warning(&self) -> Option<String> {
        let loss = crate::helpers::parse_percent(self.loss.as_deref()?).ok()?;
        let corr = crate::helpers::parse_percent(self.loss_correlation.as_deref()?).ok()?;
        if loss == 0.0 || corr == 0.0 {
            return None;
        }
        let effective = netem_correlated_rate(loss, corr);
        Some(format!(
            "loss {loss}% with loss-correlation {corr}% drops about {effective:.2}% of packets, \
             not {loss}%: netem's correlation averages each random draw with the previous one, \
             which lowers a small loss probability instead of making loss bursty. Drop \
             loss-correlation for {loss}% independent loss, or use a loss model \
             (`loss gemodel …`) for bursty loss"
        ))
    }

    /// Every property name [`Impairment::set_property`] accepts, in the
    /// order they are listed to the user.
    pub const PROPERTIES: [&'static str; 11] = [
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
        "seed",
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
            "seed" => self.seed = value,
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
            ("seed", &self.seed),
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
        /// ECN marking: `Some(true)` (`ecn`), `Some(false)` (`noecn`), or
        /// the kernel's default, which is on (#173).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ecn: Option<bool>,
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
    /// Parse a qdisc as written on the command line (#233):
    /// `KIND[,K=V…]`, e.g. `tbf,rate=10mbit,burst=32kb,limit=64kb`,
    /// `fq_codel,target=5ms,noecn`, `prio,bands=3`. Bare `ecn`/`noecn` set
    /// fq_codel's ECN (the kernel's default is on).
    pub fn parse_spec(spec: &str) -> Result<QdiscKind, String> {
        let mut items = spec.split(',').map(str::trim).filter(|s| !s.is_empty());
        let kind = items
            .next()
            .ok_or_else(|| "missing qdisc kind".to_string())?;
        let mut kv: BTreeMap<&str, String> = BTreeMap::new();
        let mut ecn: Option<bool> = None;
        for item in items {
            match item.split_once('=') {
                Some((k, v)) => {
                    kv.insert(k.trim(), v.trim().to_string());
                }
                None if item == "ecn" || item == "noecn" => {
                    let on = item == "ecn";
                    if ecn.is_some_and(|was| was != on) {
                        return Err("`ecn` and `noecn` contradict each other".into());
                    }
                    ecn = Some(on);
                }
                None => return Err(format!("bad item {item:?} (expected K=V)")),
            }
        }
        let num = |k: &str| -> Result<Option<u32>, String> {
            kv.get(k)
                .map(|v| {
                    v.parse::<u32>()
                        .map_err(|_| format!("{k}: expected an integer, got {v:?}"))
                })
                .transpose()
        };
        let allowed = |names: &[&str]| -> Result<(), String> {
            match kv.keys().find(|k| !names.contains(k)) {
                Some(k) => Err(format!(
                    "unknown {kind} parameter {k:?} (allowed: {})",
                    names.join(", ")
                )),
                None => Ok(()),
            }
        };
        if ecn.is_some() && kind != "fq_codel" {
            return Err(format!("`ecn`/`noecn` is a fq_codel setting, not {kind}'s"));
        }
        Ok(match kind {
            "tbf" => {
                allowed(&["rate", "burst", "limit", "peakrate", "mtu"])?;
                QdiscKind::Tbf {
                    rate: kv.get("rate").cloned().ok_or("tbf requires rate=")?,
                    burst: kv.get("burst").cloned().ok_or("tbf requires burst=")?,
                    // No limit means no queue: the TBF would drop everything.
                    limit: Some(
                        kv.get("limit")
                            .cloned()
                            .ok_or("tbf requires limit= (a byte size, e.g. limit=64kb)")?,
                    ),
                    peakrate: kv.get("peakrate").cloned(),
                    mtu: num("mtu")?,
                }
            }
            "fq_codel" => {
                allowed(&["target", "interval", "limit", "flows", "quantum"])?;
                QdiscKind::FqCodel {
                    target: kv.get("target").cloned(),
                    interval: kv.get("interval").cloned(),
                    limit: num("limit")?,
                    flows: num("flows")?,
                    quantum: num("quantum")?,
                    ecn,
                }
            }
            "sfq" => {
                allowed(&["perturb", "limit", "quantum"])?;
                QdiscKind::Sfq {
                    perturb: kv.get("perturb").cloned(),
                    limit: num("limit")?,
                    quantum: num("quantum")?,
                }
            }
            "prio" => {
                allowed(&["bands"])?;
                QdiscKind::Prio {
                    bands: num("bands")?
                        .map(|b| u8::try_from(b).map_err(|_| format!("bands {b} out of range")))
                        .transpose()?,
                }
            }
            other => {
                return Err(format!(
                    "unknown qdisc kind {other:?} (tbf, fq_codel, sfq, prio)"
                ));
            }
        })
    }

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

/// Firewall configuration (nftables): one `nlink-lab` table per node.
///
/// `policy` and `rules` are the **input** chain (traffic to the node
/// itself) — the shape every earlier version wrote, so it keeps loading.
/// `forward` (traffic routed through the node) and `output` (traffic the
/// node sends) are separate chains with their own policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallConfig {
    /// Input chain policy: "accept" (the default) or "drop".
    pub policy: Option<String>,

    /// Input chain rules, in order (first match wins).
    #[serde(default)]
    pub rules: Vec<FirewallRule>,

    /// Forward chain. Left out, it is installed with policy accept and no
    /// rules (before 0.14 it took the input chain's policy and could hold
    /// no rule, so `policy drop` on a router dropped everything routed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward: Option<FirewallChain>,

    /// Output chain. Left out, none is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<FirewallChain>,

    /// Named sets rules match against (`src @blocked`), by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sets: BTreeMap<String, FirewallSet>,
}

/// A named nftables set (`set blocked [10.0.0.0/8, 192.0.2.7]`): what a
/// rule's `@name` looks up. An nftables set holds one key type, so an IPv4
/// and an IPv6 blocklist are two sets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallSet {
    /// The key: one field, or several concatenated (`ipv4 . port`).
    pub key: Vec<SetKey>,

    /// Elements as written: an address, a prefix (`10.0.0.0/24`), a range
    /// (`10.0.0.1-10.0.0.9`, `8000-8999`), or for a concatenated key its
    /// parts joined by ` . ` (`10.0.1.10 . 22`). A prefix or a range makes
    /// it an interval set.
    #[serde(default)]
    pub elements: Vec<String>,

    /// Maximum number of elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,

    /// Default element lifetime (`300s`): elements expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Filled at run time (by hand, `nft add element`): `apply` adds the
    /// declared elements and leaves the others.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dynamic: bool,
}

/// One field of a set key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SetKey {
    /// An IPv4 address (`src`, `dst`).
    Ipv4,
    /// An IPv6 address (`src`, `dst`).
    Ipv6,
    /// A TCP/UDP port (`tcp dport`, …).
    Port,
    /// A packet mark (`mark`).
    Mark,
}

impl SetKey {
    /// The NLL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
            Self::Port => "port",
            Self::Mark => "mark",
        }
    }

    /// Parse the NLL spelling.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ipv4" => Self::Ipv4,
            "ipv6" => Self::Ipv6,
            "port" => Self::Port,
            "mark" => Self::Mark,
            _ => return None,
        })
    }
}

/// One firewall chain besides input: its policy and rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallChain {
    /// "accept" (the default) or "drop".
    pub policy: Option<String>,

    /// Rules, in order (first match wins).
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
}

impl FirewallConfig {
    /// True when the input chain is declared (a policy or a rule): a
    /// profile's input chain is replaced only by a node that declares one.
    pub fn input_declared(&self) -> bool {
        self.policy.is_some() || !self.rules.is_empty()
    }

    /// True when this declares anything at all.
    pub fn declares_anything(&self) -> bool {
        self.input_declared()
            || self.forward.is_some()
            || self.output.is_some()
            || !self.sets.is_empty()
    }

    /// Lay `other`'s declared chains over these, chain by chain, and its
    /// sets by name: a node's `firewall forward { … }` keeps the input
    /// chain its profile gave it.
    pub fn merge_from(&mut self, other: &FirewallConfig) {
        for (name, set) in &other.sets {
            self.sets.insert(name.clone(), set.clone());
        }
        if other.input_declared() {
            self.policy.clone_from(&other.policy);
            self.rules.clone_from(&other.rules);
        }
        if other.forward.is_some() {
            self.forward.clone_from(&other.forward);
        }
        if other.output.is_some() {
            self.output.clone_from(&other.output);
        }
    }

    /// Every rule of every chain.
    pub fn all_rules(&self) -> impl Iterator<Item = &FirewallRule> {
        self.rules
            .iter()
            .chain(self.forward.iter().flat_map(|c| c.rules.iter()))
            .chain(self.output.iter().flat_map(|c| c.rules.iter()))
    }

    /// Every rule of every chain, mutably.
    pub fn all_rules_mut(&mut self) -> impl Iterator<Item = &mut FirewallRule> {
        self.rules
            .iter_mut()
            .chain(self.forward.iter_mut().flat_map(|c| c.rules.iter_mut()))
            .chain(self.output.iter_mut().flat_map(|c| c.rules.iter_mut()))
    }
}

/// A single firewall rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallRule {
    /// Match expression (e.g., "tcp dport 80", "ct state established,related").
    /// Empty or absent: the rule matches every packet.
    #[serde(rename = "match")]
    pub match_expr: Option<String>,

    /// Verdict: "accept" (the default), "drop" or "reject".
    pub action: Option<String>,

    /// How a `reject` answers. Left out: ICMP port unreachable (ICMPv6
    /// for IPv6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_with: Option<RejectWith>,

    /// Rate limit: the rule acts only on packets under (or, with `over`,
    /// above) the rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<FirewallLimit>,

    /// Count the packets and bytes the rule acts on (`counter`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub counter: bool,

    /// Count into a named counter (`counter name ssh-drops`), which
    /// `validate { counter … }` and `inspect` read. Declared by use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_name: Option<String>,

    /// Log the packets the rule acts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<FirewallLog>,

    /// `mark set V[/M]`: set the packet mark (only the bits under the mask).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_mark: Option<MarkValue>,

    /// `ct mark set V`, `ct mark save`, `ct mark restore` (connmark).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ct_mark: Option<CtMarkAction>,

    /// `classify X:Y`: the packet's priority, which a `prio` qdisc turns
    /// into a band and an HTB into a class (tc hex: `1:10` is minor 0x10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classify: Option<String>,

    /// `clamp mss N|pmtu`: lower a TCP SYN's MSS option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamp_mss: Option<MssClamp>,
}

impl FirewallRule {
    /// True when the rule does something besides its verdict: what a rule
    /// without a verdict (action "continue") is for.
    pub fn has_statement(&self) -> bool {
        self.limit.is_some()
            || self.counter
            || self.counter_name.is_some()
            || self.log.is_some()
            || self.set_mark.is_some()
            || self.ct_mark.is_some()
            || self.classify.is_some()
            || self.clamp_mss.is_some()
    }
}

/// A mark value, with the bits it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MarkValue {
    pub value: u32,
    /// Only these bits; left out, all 32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<u32>,
}

/// What a rule does to the connection mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CtMarkAction {
    /// `ct mark set V`.
    Set(u32),
    /// `ct mark save`: the packet's mark into the connection.
    Save,
    /// `ct mark restore`: the connection's mark onto the packet.
    Restore,
}

/// `clamp mss N` or `clamp mss pmtu`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MssClamp {
    /// To what the path allows (`tcp option maxseg size set rt mtu`).
    Pmtu,
    /// To at most this value.
    Mss(u16),
}

/// `limit [over] N/UNIT [burst N]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallLimit {
    /// Packets (or, with `bytes`, bytes) per `per`.
    pub rate: u64,
    pub per: LimitPer,
    /// Packets (bytes) allowed above the rate in a burst (the kernel's
    /// default is 5 packets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
    /// Act on the packets *above* the rate instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub over: bool,
    /// The rate and burst are bytes (`limit 1mb/second`), not packets
    /// (#189).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bytes: bool,
}

/// The unit of a [`FirewallLimit`] rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LimitPer {
    Second,
    Minute,
    Hour,
    Day,
}

impl LimitPer {
    /// The NLL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    /// Parse the NLL spelling.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "second" => Self::Second,
            "minute" => Self::Minute,
            "hour" => Self::Hour,
            "day" => Self::Day,
            _ => return None,
        })
    }
}

/// `log ["prefix"] [group N]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FirewallLog {
    /// Text put before each logged packet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Send to nflog group N instead of the kernel log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u16>,
    /// The syslog level (`emerg` … `debug`); the kernel's default is
    /// `warn` (#189).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// The log levels `log … level L` takes, as nft names them.
pub const LOG_LEVELS: [&str; 8] = [
    "emerg", "alert", "crit", "err", "warn", "notice", "info", "debug",
];

/// The answer a `reject` rule sends (`reject with …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RejectWith {
    /// A TCP reset. Only meaningful for TCP: the rule must match `tcp`.
    TcpReset,
    /// ICMP(v6) port unreachable — what a bare `reject` sends.
    PortUnreachable,
    /// ICMP(v6) host unreachable.
    HostUnreachable,
    /// ICMP(v6) no route to destination.
    NoRoute,
    /// ICMP(v6) administratively prohibited.
    AdminProhibited,
}

impl RejectWith {
    /// The NLL / nft spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TcpReset => "tcp-reset",
            Self::PortUnreachable => "port-unreachable",
            Self::HostUnreachable => "host-unreachable",
            Self::NoRoute => "no-route",
            Self::AdminProhibited => "admin-prohibited",
        }
    }

    /// Parse the NLL spelling.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tcp-reset" => Self::TcpReset,
            "port-unreachable" => Self::PortUnreachable,
            "host-unreachable" => Self::HostUnreachable,
            "no-route" => Self::NoRoute,
            "admin-prohibited" => Self::AdminProhibited,
            _ => return None,
        })
    }
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
    /// Only this L4 protocol (`dnat … tcp dport 80`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<NatProto>,
    /// Only this destination port (needs `proto`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dport: Option<u16>,
    /// Only packets that came in on this interface (`dnat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iif: Option<String>,
    /// Only packets going out of this interface (`masquerade`, `snat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oif: Option<String>,
    /// The last address of a range (`snat to 203.0.113.1-203.0.113.9`),
    /// `target` the first (#189).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_end: Option<String>,
    /// The last port of a range (`to :40000-40010`), `target_port` the
    /// first (#189).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port_end: Option<u16>,
    /// `random`, `fully-random`, `persistent` (#189).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<NatFlag>,
}

/// How a NAT rule picks its port or address (#189).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NatFlag {
    /// `random`: the port is picked at random.
    Random,
    /// `fully-random`: the port is picked fully at random.
    FullyRandom,
    /// `persistent`: a client gets the same address every time.
    Persistent,
}

impl NatFlag {
    /// The NLL / nft word.
    pub fn name(self) -> &'static str {
        match self {
            NatFlag::Random => "random",
            NatFlag::FullyRandom => "fully-random",
            NatFlag::Persistent => "persistent",
        }
    }
}

/// The L4 protocol a NAT rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NatProto {
    Tcp,
    Udp,
}

/// NAT action type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Translate,
    /// `redirect … to :PORT`: to this node itself (#189).
    Redirect,
}

/// Process to execute in a node.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

    /// Peers written out longhand (`peer hub-b { endpoint … allowed-ips
    /// […] }`), by node name: what to use instead of what the planner
    /// infers for them (#147). Each is in `peers` too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub peer_settings: BTreeMap<String, WireguardPeer>,
}

/// What a longhand `peer` block sets for one WireGuard peer (#147).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WireguardPeer {
    /// `IP`, `IP:PORT` or `[IPv6]:PORT`; the port defaults to the peer
    /// interface's `listen` port. Without it, the address the two nodes
    /// share a link or network on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Prefixes routed to this peer, in place of its tunnel addresses and
    /// the node's routes into the tunnel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_ips: Vec<String>,
}

/// macvlan interface configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

impl From<&Impairment> for nlink_lab_shared::api::ImpairmentSpec {
    fn from(i: &Impairment) -> Self {
        Self {
            delay: i.delay.clone(),
            jitter: i.jitter.clone(),
            loss: i.loss.clone(),
            rate: i.rate.clone(),
            corrupt: i.corrupt.clone(),
            reorder: i.reorder.clone(),
            duplicate: i.duplicate.clone(),
            delay_correlation: i.delay_correlation.clone(),
            loss_correlation: i.loss_correlation.clone(),
            limit: i.limit.clone(),
            seed: i.seed.clone(),
        }
    }
}

impl From<&nlink_lab_shared::api::ImpairmentSpec> for Impairment {
    fn from(s: &nlink_lab_shared::api::ImpairmentSpec) -> Self {
        Self {
            delay: s.delay.clone(),
            jitter: s.jitter.clone(),
            loss: s.loss.clone(),
            rate: s.rate.clone(),
            corrupt: s.corrupt.clone(),
            reorder: s.reorder.clone(),
            duplicate: s.duplicate.clone(),
            delay_correlation: s.delay_correlation.clone(),
            loss_correlation: s.loss_correlation.clone(),
            limit: s.limit.clone(),
            seed: s.seed.clone(),
        }
    }
}

/// What [`Topology::redacted`] puts where a secret was.
pub const REDACTED: &str = "<redacted>";

impl Topology {
    /// This topology without its secrets (#234): WireGuard private keys
    /// (other than `auto`), Wi-Fi passphrases and container `env` values
    /// become [`REDACTED`]. For every view of a lab that leaves this
    /// process: the daemon's API, its telemetry, the GUI.
    pub fn redacted(&self) -> Topology {
        let mut t = self.clone();
        for node in t.nodes.values_mut() {
            for wg in node.wireguard.values_mut() {
                if let Some(key) = &mut wg.private_key
                    && key != "auto"
                {
                    *key = REDACTED.to_string();
                }
            }
            for wifi in &mut node.wifi {
                if let Some(pass) = &mut wifi.passphrase {
                    *pass = REDACTED.to_string();
                }
            }
            if let Some(env) = &mut node.env {
                for value in env.values_mut() {
                    *value = REDACTED.to_string();
                }
            }
        }
        t
    }

    /// Put the containers' relative host paths under `base` (#285):
    /// `config` sources, `overlay` and `env-file`; absolute ones stay.
    /// An imported module's paths are its file's: lowering puts them
    /// under the import's directory, so they stay right from the entry's.
    /// `volumes` stay as written: a bare name there is a named volume.
    pub fn rebase_host_paths(&mut self, base: &std::path::Path) {
        let abs = |p: &mut String| {
            if !std::path::Path::new(p.as_str()).is_absolute() {
                *p = base.join(p.as_str()).to_string_lossy().into_owned();
            }
        };
        for node in self.nodes.values_mut() {
            for (host, _) in &mut node.configs {
                abs(host);
            }
            if let Some(dir) = &mut node.overlay {
                abs(dir);
            }
            if let Some(file) = &mut node.env_file {
                abs(file);
            }
        }
    }

    /// Deploy this topology, creating all namespaces and network configuration.
    ///
    /// Validates the topology first, then creates the lab. Returns a
    /// [`RunningLab`](crate::RunningLab) handle.
    pub async fn deploy(&self) -> crate::Result<crate::running::RunningLab> {
        crate::deploy::deploy(self).await
    }

    /// [`deploy`](Self::deploy), reporting its progress to `ctx` and
    /// rolled back when `ctx` is cancelled (#236).
    pub async fn deploy_with(
        &self,
        ctx: &crate::ops::OpCtx,
    ) -> crate::Result<crate::running::RunningLab> {
        crate::deploy::deploy_with(self, ctx).await
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

    /// The firewall a node runs: its profiles' and its own, merged chain by
    /// chain (a declared chain replaces the one before it).
    pub fn effective_firewall<'a>(
        &'a self,
        node: &'a Node,
    ) -> Option<std::borrow::Cow<'a, FirewallConfig>> {
        use std::borrow::Cow;
        // Chain by chain: each profile, in order, then the node, lays the
        // chains it declares over the ones before (later profiles override
        // earlier ones, mirroring sysctl merge order).
        let layers: Vec<&FirewallConfig> = node
            .profiles
            .iter()
            .filter_map(|name| self.profiles.get(name).and_then(|p| p.firewall.as_ref()))
            .chain(node.firewall.as_ref())
            .collect();
        match layers.as_slice() {
            [] => None,
            [only] => Some(Cow::Borrowed(*only)),
            [first, rest @ ..] => {
                let mut fw = (*first).clone();
                for layer in rest {
                    fw.merge_from(layer);
                }
                Some(Cow::Owned(fw))
            }
        }
    }
}

#[cfg(test)]
mod name_hash_tests {
    use super::*;

    #[test]
    fn impairment_specs_parse_in_every_spelling() {
        let words = Impairment::parse_spec("delay 50ms loss 1%").unwrap();
        assert_eq!(Impairment::parse_spec("delay=50ms,loss=1%").unwrap(), words);
        assert_eq!(Impairment::parse_spec("delay=50ms loss=1%").unwrap(), words);
        // a loss model is one value, up to the next property
        let ge = Impairment::parse_spec("loss gemodel 1% 30% 70% 0.1% delay 10ms").unwrap();
        assert_eq!(ge.loss.as_deref(), Some("gemodel 1% 30% 70% 0.1%"));
        assert_eq!(ge.delay.as_deref(), Some("10ms"));
        assert!(Impairment::parse_spec("loss gemodel 1% 130%").is_err());
        assert!(Impairment::parse_spec("delay 50").is_err(), "no unit");
        assert!(
            Impairment::parse_spec("bogus 1")
                .unwrap_err()
                .contains("delay")
        );
        // the API's spec carries the same values
        let spec = nlink_lab_shared::api::ImpairmentSpec::from(&ge);
        assert_eq!(Impairment::from(&spec), ge);
    }

    #[test]
    fn qdisc_specs_parse() {
        assert!(matches!(
            QdiscKind::parse_spec("fq_codel,target=5ms,noecn").unwrap(),
            QdiscKind::FqCodel { ecn: Some(false), target: Some(t), .. } if t == "5ms"
        ));
        assert!(
            QdiscKind::parse_spec("tbf,rate=1mbit,burst=32kb")
                .unwrap_err()
                .contains("limit")
        );
        assert!(
            QdiscKind::parse_spec("sfq,ecn")
                .unwrap_err()
                .contains("fq_codel")
        );
        assert!(
            QdiscKind::parse_spec("cake")
                .unwrap_err()
                .contains("unknown")
        );
    }

    #[test]
    fn a_redacted_topology_has_no_secrets() {
        // #234: the backend published WireGuard keys and passphrases
        let t = crate::parser::parse(
            r#"lab "s"
node a {
  wireguard wg0 { key "YNqHbfBQKaGvzefSSS8HAXBFOtFmGWd1mz0UXbb3eUs=" address 10.9.0.1/32 }
  wifi wlan0 mode ap { ssid "lab" wpa2 "a passphrase" 10.0.0.1/24 }
}
node b { wireguard wg0 { key auto address 10.9.0.2/32 } }
node c image "alpine" { env ["TOKEN=s3cret"] }
"#,
        )
        .unwrap();
        let r = t.redacted();
        let json = serde_json::to_string(&r).unwrap();
        for secret in ["YNqHbfBQ", "a passphrase", "s3cret"] {
            assert!(!json.contains(secret), "{secret} in {json}");
        }
        assert_eq!(
            r.nodes["b"].wireguard["wg0"].private_key.as_deref(),
            Some("auto")
        );
        assert_eq!(r.nodes["c"].env.as_ref().unwrap()["TOKEN"], REDACTED);
        // the original is untouched
        assert_eq!(t.nodes["c"].env.as_ref().unwrap()["TOKEN"], "s3cret");
    }

    #[test]
    fn host_paths_go_under_a_base_but_volumes_stay() {
        let mut t = crate::parser::parse(
            r#"lab "p"
node c image "alpine" {
  config "conf/a.conf" "/etc/a.conf"
  config "/abs/b.conf" "/etc/b.conf"
  overlay "ov"
  env-file "c.env"
  volumes ["data:/data"]
}
"#,
        )
        .unwrap();
        t.rebase_host_paths(std::path::Path::new("/work"));
        let c = &t.nodes["c"];
        assert_eq!(c.configs[0].0, "/work/conf/a.conf");
        assert_eq!(c.configs[1].0, "/abs/b.conf");
        assert_eq!(c.overlay.as_deref(), Some("/work/ov"));
        assert_eq!(c.env_file.as_deref(), Some("/work/c.env"));
        assert_eq!(
            c.volumes.as_ref().unwrap()[0],
            "data:/data",
            "a named volume"
        );
    }

    #[test]
    fn site_blocks_are_remembered_with_their_prefixes() {
        // #234: lowering only prefixed the names
        let t = crate::parser::parse(
            r#"lab "s"
site dc1 {
  node r
  site rack1 { node h1 node h2 }
}
site dc2 { node r }
node lone
"#,
        )
        .unwrap();
        assert_eq!(
            t.sites,
            BTreeMap::from([
                (
                    "dc1".into(),
                    vec!["dc1-r".into(), "dc1-rack1-h1".into(), "dc1-rack1-h2".into()]
                ),
                (
                    "dc1-rack1".into(),
                    vec!["dc1-rack1-h1".into(), "dc1-rack1-h2".into()]
                ),
                ("dc2".into(), vec!["dc2-r".into()]),
            ])
        );
    }

    #[test]
    fn a_suffix_renames_the_prefix_too() {
        // #221: `--suffix b` of a prefixed topology planned the same
        // namespaces as `--suffix a`
        let mut lab = LabConfig {
            name: "dc".into(),
            prefix: Some("p".into()),
            ..Default::default()
        };
        lab.add_suffix("a");
        assert_eq!((lab.name.as_str(), lab.prefix()), ("dc-a", "p-a"));
        let mut plain = LabConfig {
            name: "x".into(),
            ..Default::default()
        };
        plain.add_suffix("1");
        assert_eq!((plain.name.as_str(), plain.prefix()), ("x-1", "x-1"));
    }

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

#[cfg(test)]
mod impairment_tests {
    use super::*;

    /// The estimate behind the #152 warning has to agree with netem: the
    /// same formula simulated independently gave 0.00 / 1.77 / 9.99 %.
    #[test]
    fn netem_correlated_rate_matches_the_formula() {
        assert!(netem_correlated_rate(0.5, 25.0) < 0.01);
        let r = netem_correlated_rate(10.0, 25.0);
        assert!((1.5..2.1).contains(&r), "{r}");
        let r = netem_correlated_rate(10.0, 0.0);
        assert!((9.7..10.3).contains(&r), "{r}");
    }

    #[test]
    fn loss_correlation_warning_needs_both_and_is_deterministic() {
        let mut imp = Impairment::default();
        imp.set_property("loss", "0.5%").unwrap();
        assert!(
            imp.loss_correlation_warning().is_none(),
            "loss alone is fine"
        );
        imp.set_property("loss-correlation", "25%").unwrap();
        let w = imp.loss_correlation_warning().expect("must warn");
        assert_eq!(imp.loss_correlation_warning().as_deref(), Some(w.as_str()));
        imp.set_property("loss-correlation", "0%").unwrap();
        assert!(
            imp.loss_correlation_warning().is_none(),
            "zero correlation is off"
        );
        imp.loss = None;
        imp.set_property("loss-correlation", "25%").unwrap();
        assert!(
            imp.loss_correlation_warning().is_none(),
            "no loss, nothing lost"
        );
    }
}
