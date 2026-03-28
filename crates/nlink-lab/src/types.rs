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
}

impl LabConfig {
    /// Get the effective namespace prefix.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or(&self.name)
    }
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

    /// Processes to spawn in this namespace.
    #[serde(default)]
    pub exec: Vec<ExecConfig>,

    /// VRF definitions.
    #[serde(default)]
    pub vrfs: HashMap<String, VrfConfig>,

    /// WireGuard interfaces.
    #[serde(default)]
    pub wireguard: HashMap<String, WireguardConfig>,
}

impl Node {
    /// Returns true if this node should be deployed as a container.
    pub fn is_container(&self) -> bool {
        self.image.is_some()
    }
}

/// Explicit interface configuration (for interfaces not created by links).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterfaceConfig {
    /// Interface type (vxlan, bond, vlan, dummy, etc.).
    pub kind: Option<String>,

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

// ─────────────────────────────────────────────────
// Serde helpers
// ─────────────────────────────────────────────────

/// Deserialize a `HashMap<u16, V>` from TOML tables where keys are strings.
fn deserialize_u16_keys<'de, V, D>(deserializer: D) -> std::result::Result<HashMap<u16, V>, D::Error>
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
        if let Some(profile_name) = &node.profile {
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
        if let Some(profile_name) = &node.profile {
            if let Some(profile) = self.profiles.get(profile_name) {
                return profile.firewall.as_ref();
            }
        }
        None
    }
}
