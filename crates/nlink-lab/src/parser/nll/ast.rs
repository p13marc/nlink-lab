//! AST types for the NLL language.
//!
//! These types represent the parsed syntax tree before lowering to [`Topology`].
//! The AST preserves `for` loops and `let` bindings for expansion during lowering.

/// A complete NLL file.
#[derive(Debug)]
pub struct File {
    pub lab: LabDecl,
    pub statements: Vec<Statement>,
}

/// Lab declaration at the top of the file.
#[derive(Debug)]
pub struct LabDecl {
    pub name: String,
    pub description: Option<String>,
    pub prefix: Option<String>,
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
    Let(LetDef),
    For(ForLoop),
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
    pub profile: Option<String>,
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
    Vrf(VrfDef),
    Wireguard(WireguardDef),
    Vxlan(VxlanDef),
    Dummy(DummyDef),
    Run(RunDef),
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
    pub address: Option<String>,
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
    pub address: Option<String>,
}

/// Dummy interface definition.
#[derive(Debug, Clone)]
pub struct DummyDef {
    pub name: String,
    pub address: Option<String>,
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
    pub mtu: Option<u32>,
    /// Symmetric impairment (both directions).
    pub impairment: Option<ImpairProps>,
    /// Left→Right impairment (->).
    pub left_impair: Option<ImpairProps>,
    /// Right→Left impairment (<-).
    pub right_impair: Option<ImpairProps>,
    /// Rate limiting.
    pub rate: Option<RateProps>,
}

/// Network (bridge) definition.
#[derive(Debug, Clone)]
pub struct NetworkDef {
    pub name: String,
    pub members: Vec<String>,
    pub vlan_filtering: bool,
    pub mtu: Option<u32>,
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

/// Variable binding.
#[derive(Debug, Clone)]
pub struct LetDef {
    pub name: String,
    pub value: String,
}

/// For loop.
#[derive(Debug, Clone)]
pub struct ForLoop {
    pub var: String,
    pub start: i64,
    pub end: i64,
    pub body: Vec<Statement>,
}
