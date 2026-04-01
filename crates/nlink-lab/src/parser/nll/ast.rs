//! AST types for the NLL language.
//!
//! These types represent the parsed syntax tree before lowering to [`crate::types::Topology`].
//! The AST preserves `for` loops and `let` bindings for expansion during lowering.

/// A complete NLL file.
#[derive(Debug)]
pub struct File {
    pub imports: Vec<ImportDef>,
    pub lab: LabDecl,
    pub statements: Vec<Statement>,
}

/// Import declaration: `import "path.nll" as alias` or `import "file.nll" as alias(key=val)`.
#[derive(Debug, Clone)]
pub struct ImportDef {
    pub path: String,
    pub alias: String,
    pub params: Vec<(String, String)>,
}

/// Module parameter declaration: `param name default value`.
#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: String,
    pub default: Option<String>,
}

/// Lab declaration at the top of the file.
#[derive(Debug)]
pub struct LabDecl {
    pub name: String,
    pub description: Option<String>,
    pub prefix: Option<String>,
    pub runtime: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub tags: Vec<String>,
    pub mgmt: Option<String>,
    pub dns: Option<String>,
}

/// Top-level statement.
#[derive(Debug, Clone)]
pub enum Statement {
    Profile(ProfileDef),
    Node(NodeDef),
    Link(LinkDef),
    Network(NetworkDef),
    Impair(ImpairDef),
    Rate(RateDef),
    Defaults(DefaultsDef),
    Param(ParamDef),
    Pool(PoolDef),
    Pattern(PatternDef),
    Validate(ValidateDef),
    Let(LetDef),
    For(ForLoop),
    Scenario(ScenarioDef),
    Benchmark(BenchmarkDef),
    Site(SiteDef),
}

/// Site definition (groups nodes/links with a common prefix).
#[derive(Debug, Clone)]
pub struct SiteDef {
    pub name: String,
    pub description: Option<String>,
    pub body: Vec<Statement>,
}

/// Benchmark definition.
#[derive(Debug, Clone)]
pub struct BenchmarkDef {
    pub name: String,
    pub tests: Vec<BenchmarkTestDef>,
}

/// Single benchmark test.
#[derive(Debug, Clone)]
pub enum BenchmarkTestDef {
    Iperf3 {
        from: String,
        to: String,
        duration: Option<String>,
        streams: Option<u32>,
        udp: bool,
        assertions: Vec<BenchmarkAssertionDef>,
    },
    Ping {
        from: String,
        to: String,
        count: Option<u32>,
        assertions: Vec<BenchmarkAssertionDef>,
    },
}

/// Benchmark assertion: `assert metric op value`.
#[derive(Debug, Clone)]
pub struct BenchmarkAssertionDef {
    pub metric: String,
    pub op: String,
    pub value: String,
}

/// Named subnet pool: `pool fabric 10.0.0.0/16 /30`.
#[derive(Debug, Clone)]
pub struct PoolDef {
    pub name: String,
    pub base: String,
    pub prefix: u8,
}

/// Reachability assertions: `validate { reach host1 host2 }`.
#[derive(Debug, Clone)]
pub struct ValidateDef {
    pub assertions: Vec<AssertionDef>,
}

/// Single reachability assertion.
#[derive(Debug, Clone)]
pub enum AssertionDef {
    Reach {
        from: String,
        to: String,
    },
    NoReach {
        from: String,
        to: String,
    },
    TcpConnect {
        from: String,
        to: String,
        port: u16,
        timeout: Option<String>,
    },
    LatencyUnder {
        from: String,
        to: String,
        max: String,
        samples: Option<u32>,
    },
    RouteHas {
        node: String,
        destination: String,
        via: Option<String>,
        dev: Option<String>,
    },
    DnsResolves {
        from: String,
        name: String,
        expected_ip: String,
    },
}

/// Topology pattern: `mesh`, `ring`, `star`.
#[derive(Debug, Clone)]
pub struct PatternDef {
    pub kind: PatternKind,
    pub name: String,
    pub nodes: Vec<String>,
    pub count: Option<i64>,
    pub pool: Option<String>,
    pub profile: Option<String>,
}

/// Type of topology pattern.
#[derive(Debug, Clone)]
pub enum PatternKind {
    Mesh,
    Ring,
    Star { hub: String },
}

/// Defaults block: `defaults link { mtu 9000 }` or `defaults impair { delay 5ms }`.
#[derive(Debug, Clone)]
pub struct DefaultsDef {
    pub kind: DefaultsKind,
    pub mtu: Option<u32>,
    pub impair: Option<ImpairProps>,
    pub rate: Option<RateProps>,
}

/// What a defaults block applies to.
#[derive(Debug, Clone, PartialEq)]
pub enum DefaultsKind {
    Link,
    Impair,
    Rate,
    /// Named link profile (e.g., `defaults radio { delay 15ms }`)
    Named(String),
}

/// Profile definition.
#[derive(Debug, Clone)]
pub struct ProfileDef {
    pub name: String,
    pub props: Vec<NodeProp>,
}

/// Node definition.
#[derive(Debug, Clone)]
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,
    pub image: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    // Container properties
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub entrypoint: Option<String>,
    pub hostname: Option<String>,
    pub workdir: Option<String>,
    pub labels: Vec<String>,
    pub pull: Option<String>,
    pub container_exec: Vec<String>,
    // Lifecycle (plan 096)
    pub healthcheck: Option<String>,
    pub healthcheck_interval: Option<String>,
    pub healthcheck_timeout: Option<String>,
    pub startup_delay: Option<String>,
    pub env_file: Option<String>,
    pub configs: Vec<(String, String)>,
    pub overlay: Option<String>,
    pub depends_on: Vec<String>,
    pub props: Vec<NodeProp>,
}

/// IP version for forward shorthand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    Ipv4,
    Ipv6,
}

/// Node property (used in both nodes and profiles).
#[derive(Debug, Clone)]
pub enum NodeProp {
    Forward(IpVersion),
    Sysctl(String, String),
    Lo(String),
    Route(RouteDef),
    Firewall(FirewallDef),
    Nat(NatDef),
    Vrf(VrfDef),
    Wireguard(WireguardDef),
    Vxlan(VxlanDef),
    Dummy(DummyDef),
    Macvlan(MacvlanDef),
    Ipvlan(IpvlanDef),
    Wifi(WifiDef),
    Run(RunDef),
    /// For loop that generates node properties.
    ForLoop(PropForLoop),
}

/// A `for` loop inside a node/profile block that generates properties.
#[derive(Debug, Clone)]
pub struct PropForLoop {
    pub var: String,
    pub range: ForRange,
    pub body: Vec<NodeProp>,
}

/// Wi-Fi interface definition.
#[derive(Debug, Clone)]
pub struct WifiDef {
    pub name: String,
    pub mode: String,
    pub ssid: Option<String>,
    pub channel: Option<u32>,
    pub passphrase: Option<String>,
    pub mesh_id: Option<String>,
    pub addresses: Vec<String>,
}

/// macvlan interface definition.
#[derive(Debug, Clone)]
pub struct MacvlanDef {
    pub name: String,
    pub parent: String,
    pub mode: Option<String>,
    pub addresses: Vec<String>,
}

/// ipvlan interface definition.
#[derive(Debug, Clone)]
pub struct IpvlanDef {
    pub name: String,
    pub parent: String,
    pub mode: Option<String>,
    pub addresses: Vec<String>,
}

/// Route definition.
#[derive(Debug, Clone)]
pub struct RouteDef {
    pub destination: String,
    pub via: Option<String>,
    pub dev: Option<String>,
    pub metric: Option<u32>,
}

/// Firewall definition.
#[derive(Debug, Clone)]
pub struct FirewallDef {
    pub policy: String,
    pub rules: Vec<FirewallRuleDef>,
}

/// A single firewall rule.
#[derive(Debug, Clone)]
pub struct FirewallRuleDef {
    pub action: String,
    pub match_expr: String,
}

/// NAT definition.
#[derive(Debug, Clone)]
pub struct NatDef {
    pub rules: Vec<NatRuleDef>,
}

/// A single NAT rule.
#[derive(Debug, Clone)]
pub struct NatRuleDef {
    pub action: String,
    pub src: Option<String>,
    pub dst: Option<String>,
    pub target: Option<String>,
    pub target_port: Option<u16>,
}

/// VRF definition.
#[derive(Debug, Clone)]
pub struct VrfDef {
    pub name: String,
    pub table: u32,
    pub interfaces: Vec<String>,
    pub routes: Vec<RouteDef>,
}

/// WireGuard interface definition.
#[derive(Debug, Clone)]
pub struct WireguardDef {
    pub name: String,
    pub key: Option<String>,
    pub listen_port: Option<u16>,
    pub addresses: Vec<String>,
    pub peers: Vec<String>,
}

/// VXLAN interface definition.
#[derive(Debug, Clone)]
pub struct VxlanDef {
    pub name: String,
    pub vni: u32,
    pub local: Option<String>,
    pub remote: Option<String>,
    pub port: Option<u16>,
    pub addresses: Vec<String>,
}

/// Dummy interface definition.
#[derive(Debug, Clone)]
pub struct DummyDef {
    pub name: String,
    pub addresses: Vec<String>,
}

/// Process execution definition.
#[derive(Debug, Clone)]
pub struct RunDef {
    pub cmd: Vec<String>,
    pub background: bool,
}

/// Impairment properties.
#[derive(Debug, Clone, Default)]
pub struct ImpairProps {
    pub delay: Option<String>,
    pub jitter: Option<String>,
    pub loss: Option<String>,
    pub rate: Option<String>,
    pub corrupt: Option<String>,
    pub reorder: Option<String>,
}

/// Rate limiting properties.
#[derive(Debug, Clone, Default)]
pub struct RateProps {
    pub egress: Option<String>,
    pub ingress: Option<String>,
    pub burst: Option<String>,
}

/// Link definition.
#[derive(Debug, Clone)]
pub struct LinkDef {
    pub left_node: String,
    pub left_iface: String,
    pub right_node: String,
    pub right_iface: String,
    pub left_addr: Option<String>,
    pub right_addr: Option<String>,
    /// Single subnet for auto-assignment (e.g., "10.0.0.0/30" → .1 and .2).
    pub subnet: Option<String>,
    /// Pool reference for auto-allocation (e.g., "fabric").
    pub pool: Option<String>,
    pub mtu: Option<u32>,
    /// Symmetric impairment (both directions).
    pub impairment: Option<ImpairProps>,
    /// Left→Right impairment (->).
    pub left_impair: Option<ImpairProps>,
    /// Right→Left impairment (<-).
    pub right_impair: Option<ImpairProps>,
    /// Rate limiting.
    pub rate: Option<RateProps>,
    /// Named link profile reference (e.g., `: radio`).
    pub profile: Option<String>,
}

/// Network (bridge) definition.
#[derive(Debug, Clone)]
pub struct NetworkDef {
    pub name: String,
    pub members: Vec<String>,
    pub vlan_filtering: bool,
    pub mtu: Option<u32>,
    pub subnet: Option<String>,
    pub vlans: Vec<VlanDef>,
    pub ports: Vec<PortDef>,
}

/// VLAN definition within a network.
#[derive(Debug, Clone)]
pub struct VlanDef {
    pub id: u16,
    pub name: Option<String>,
}

/// Port configuration within a network.
#[derive(Debug, Clone)]
pub struct PortDef {
    pub endpoint: String,
    pub pvid: Option<u16>,
    pub vlans: Vec<u16>,
    pub tagged: bool,
    pub untagged: bool,
    pub addresses: Vec<String>,
}

/// Standalone impairment.
#[derive(Debug, Clone)]
pub struct ImpairDef {
    pub node: String,
    pub iface: String,
    pub props: ImpairProps,
}

/// Standalone rate limit.
#[derive(Debug, Clone)]
pub struct RateDef {
    pub node: String,
    pub iface: String,
    pub props: RateProps,
}

/// Scenario definition.
#[derive(Debug, Clone)]
pub struct ScenarioDef {
    pub name: String,
    pub steps: Vec<ScenarioStepDef>,
}

/// A timed step in a scenario.
#[derive(Debug, Clone)]
pub struct ScenarioStepDef {
    pub time: String,
    pub actions: Vec<ScenarioActionDef>,
}

/// An action in a scenario step.
#[derive(Debug, Clone)]
pub enum ScenarioActionDef {
    Down(String),
    Up(String),
    Clear(String),
    Validate(Vec<AssertionDef>),
    Exec { node: String, cmd: Vec<String> },
    Log(String),
}

/// Variable binding.
#[derive(Debug, Clone)]
pub struct LetDef {
    pub name: String,
    pub value: String,
}

/// Range for a for-loop: integer range or list of values.
#[derive(Debug, Clone)]
pub enum ForRange {
    /// Inclusive integer range: `for i in 1..4`
    IntRange { start: i64, end: i64 },
    /// List of string values: `for role in [web, api, db]`
    List(Vec<String>),
}

/// For loop.
#[derive(Debug, Clone)]
pub struct ForLoop {
    pub var: String,
    pub range: ForRange,
    pub body: Vec<Statement>,
}
