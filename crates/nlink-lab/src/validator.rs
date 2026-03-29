//! Topology validation.
//!
//! Validates a parsed [`Topology`] before deployment, catching semantic errors
//! that the TOML parser cannot detect.
//!
//! # Example
//!
//! ```ignore
//! let topology = nlink_lab::parser::parse_file("lab.toml")?;
//! let result = topology.validate();
//! if result.has_errors() {
//!     for issue in result.errors() {
//!         eprintln!("ERROR: {issue}");
//!     }
//! }
//! result.bail()?;
//! ```

use std::collections::HashMap;
use std::fmt;

use crate::helpers::{ip_in_subnet, parse_cidr};
use crate::types::{EndpointRef, Topology};

/// Result of topology validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    issues: Vec<ValidationIssue>,
}

impl ValidationResult {
    /// Returns true if there are any error-level issues.
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Returns true if there are any warning-level issues.
    pub fn has_warnings(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Warning)
    }

    /// Iterate over error-level issues.
    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues.iter().filter(|i| i.severity == Severity::Error)
    }

    /// Iterate over warning-level issues.
    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
    }

    /// All issues.
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.issues
    }

    /// Return `Err` if there are error-level issues, `Ok(())` otherwise.
    pub fn bail(&self) -> crate::Result<()> {
        if self.has_errors() {
            let errors: Vec<ValidationIssue> = self.errors().cloned().collect();
            Err(crate::Error::ValidationErrors(errors))
        } else {
            Ok(())
        }
    }
}

/// A single validation issue.
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    /// Severity level.
    pub severity: Severity,
    /// Rule identifier (e.g., "valid-cidr", "dangling-node-ref").
    pub rule: &'static str,
    /// Human-readable description.
    pub message: String,
    /// Location in the topology (e.g., `links.endpoints`).
    pub location: Option<String>,
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.rule, self.message)?;
        if let Some(loc) = &self.location {
            write!(f, " at {loc}")?;
        }
        Ok(())
    }
}

/// Issue severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocks deployment.
    Error,
    /// Non-blocking, informational.
    Warning,
}

/// Where an interface on a node originates from.
#[derive(Debug, Clone)]
enum InterfaceSource {
    Explicit,
    Link(usize),
    Network(String),
    Wireguard,
}

impl fmt::Display for InterfaceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => write!(f, "interfaces"),
            Self::Link(i) => write!(f, "links[{i}]"),
            Self::Network(n) => write!(f, "networks.{n}"),
            Self::Wireguard => write!(f, "wireguard"),
        }
    }
}

/// Collect all interfaces per node from all sources.
fn collect_interfaces(topology: &Topology) -> HashMap<String, HashMap<String, InterfaceSource>> {
    let mut result: HashMap<String, HashMap<String, InterfaceSource>> = HashMap::new();

    // Ensure all nodes have an entry
    for node_name in topology.nodes.keys() {
        result.entry(node_name.clone()).or_default();
    }

    // Explicit interfaces
    for (node_name, node) in &topology.nodes {
        let ifaces = result.entry(node_name.clone()).or_default();
        for iface_name in node.interfaces.keys() {
            ifaces
                .entry(iface_name.clone())
                .or_insert(InterfaceSource::Explicit);
        }
        // WireGuard interfaces
        for wg_name in node.wireguard.keys() {
            ifaces
                .entry(wg_name.clone())
                .or_insert(InterfaceSource::Wireguard);
        }
    }

    // Interfaces from links
    for (i, link) in topology.links.iter().enumerate() {
        for ep_str in &link.endpoints {
            if let Some(ep) = EndpointRef::parse(ep_str) {
                let ifaces = result.entry(ep.node.clone()).or_default();
                ifaces
                    .entry(ep.iface.clone())
                    .or_insert(InterfaceSource::Link(i));
            }
        }
    }

    // Interfaces from network members
    for (net_name, network) in &topology.networks {
        for member in &network.members {
            if let Some(ep) = EndpointRef::parse(member) {
                let ifaces = result.entry(ep.node.clone()).or_default();
                ifaces
                    .entry(ep.iface.clone())
                    .or_insert(InterfaceSource::Network(net_name.clone()));
            }
        }
    }

    result
}

impl Topology {
    /// Validate this topology. Returns a [`ValidationResult`] containing any issues found.
    pub fn validate(&self) -> ValidationResult {
        let mut issues = Vec::new();
        let interfaces = collect_interfaces(self);

        // Error-level rules
        validate_cidrs(self, &mut issues);
        validate_endpoint_format(self, &mut issues);
        validate_dangling_node_refs(self, &mut issues);
        validate_dangling_profile_refs(self, &mut issues);
        validate_interface_uniqueness(self, &mut issues);
        validate_vlan_range(self, &mut issues);
        validate_impairment_refs(self, &interfaces, &mut issues);
        validate_rate_limit_refs(self, &interfaces, &mut issues);
        validate_route_config(self, &mut issues);
        validate_interface_name_length(self, &interfaces, &mut issues);
        validate_wireguard_peers(self, &mut issues);
        validate_vrf_table_unique(self, &mut issues);
        validate_duplicate_link_endpoints(self, &mut issues);

        // Warning-level rules
        validate_unique_ips(self, &mut issues);
        validate_mtu_consistency(self, &mut issues);
        validate_route_reachability(self, &interfaces, &mut issues);
        validate_unreferenced_nodes(self, &interfaces, &mut issues);
        validate_exec_cmds(self, &mut issues);
        validate_container_fields(self, &mut issues);

        ValidationResult { issues }
    }
}

// ─────────────────────────────────────────────────────
// Error-level rules
// ─────────────────────────────────────────────────────

/// Rule 1: All address strings must be valid CIDR notation.
fn validate_cidrs(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // Link addresses
    for (i, link) in topology.links.iter().enumerate() {
        if let Some(addresses) = &link.addresses {
            for (j, addr) in addresses.iter().enumerate() {
                if let Err(e) = parse_cidr(addr) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "valid-cidr",
                        message: format!("invalid CIDR '{addr}': {e}"),
                        location: Some(format!("links[{i}].addresses[{j}]")),
                    });
                }
            }
        }
    }

    // Explicit interface addresses
    for (node_name, node) in &topology.nodes {
        for (iface_name, iface) in &node.interfaces {
            for (k, addr) in iface.addresses.iter().enumerate() {
                if let Err(e) = parse_cidr(addr) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "valid-cidr",
                        message: format!("invalid CIDR '{addr}': {e}"),
                        location: Some(format!(
                            "nodes.{node_name}.interfaces.{iface_name}.addresses[{k}]"
                        )),
                    });
                }
            }
        }

        // WireGuard addresses
        for (wg_name, wg) in &node.wireguard {
            for (k, addr) in wg.addresses.iter().enumerate() {
                if let Err(e) = parse_cidr(addr) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "valid-cidr",
                        message: format!("invalid CIDR '{addr}': {e}"),
                        location: Some(format!(
                            "nodes.{node_name}.wireguard.{wg_name}.addresses[{k}]"
                        )),
                    });
                }
            }
        }
    }

    // Network port addresses
    for (net_name, network) in &topology.networks {
        for (port_name, port) in &network.ports {
            for (k, addr) in port.addresses.iter().enumerate() {
                if let Err(e) = parse_cidr(addr) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "valid-cidr",
                        message: format!("invalid CIDR '{addr}': {e}"),
                        location: Some(format!(
                            "networks.{net_name}.ports.{port_name}.addresses[{k}]"
                        )),
                    });
                }
            }
        }

        // Network subnet
        if let Some(subnet) = &network.subnet
            && let Err(e) = parse_cidr(subnet)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "valid-cidr",
                message: format!("invalid CIDR '{subnet}': {e}"),
                location: Some(format!("networks.{net_name}.subnet")),
            });
        }
    }
}

/// Rule 2: All endpoints must match "node:interface" format.
fn validate_endpoint_format(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (i, link) in topology.links.iter().enumerate() {
        for (j, ep) in link.endpoints.iter().enumerate() {
            if EndpointRef::parse(ep).is_none() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "endpoint-format",
                    message: format!("invalid endpoint '{ep}': expected 'node:interface' format"),
                    location: Some(format!("links[{i}].endpoints[{j}]")),
                });
            }
        }
    }

    for key in topology.impairments.keys() {
        if EndpointRef::parse(key).is_none() {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "endpoint-format",
                message: format!("invalid endpoint '{key}': expected 'node:interface' format"),
                location: Some(format!("impairments.\"{key}\"")),
            });
        }
    }

    for key in topology.rate_limits.keys() {
        if EndpointRef::parse(key).is_none() {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "endpoint-format",
                message: format!("invalid endpoint '{key}': expected 'node:interface' format"),
                location: Some(format!("rate_limits.\"{key}\"")),
            });
        }
    }

    for (net_name, network) in &topology.networks {
        for (k, member) in network.members.iter().enumerate() {
            if EndpointRef::parse(member).is_none() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "endpoint-format",
                    message: format!(
                        "invalid endpoint '{member}': expected 'node:interface' format"
                    ),
                    location: Some(format!("networks.{net_name}.members[{k}]")),
                });
            }
        }
    }
}

/// Rule 3: Endpoint node names must exist in topology.nodes.
fn validate_dangling_node_refs(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (i, link) in topology.links.iter().enumerate() {
        for (j, ep_str) in link.endpoints.iter().enumerate() {
            if let Some(ep) = EndpointRef::parse(ep_str)
                && !topology.nodes.contains_key(&ep.node)
            {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{}' does not exist", ep.node),
                    location: Some(format!("links[{i}].endpoints[{j}]")),
                });
            }
        }
    }

    for key in topology.impairments.keys() {
        if let Some(ep) = EndpointRef::parse(key)
            && !topology.nodes.contains_key(&ep.node)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "dangling-node-ref",
                message: format!("node '{}' does not exist", ep.node),
                location: Some(format!("impairments.\"{key}\"")),
            });
        }
    }

    for key in topology.rate_limits.keys() {
        if let Some(ep) = EndpointRef::parse(key)
            && !topology.nodes.contains_key(&ep.node)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "dangling-node-ref",
                message: format!("node '{}' does not exist", ep.node),
                location: Some(format!("rate_limits.\"{key}\"")),
            });
        }
    }

    for (net_name, network) in &topology.networks {
        for (k, member) in network.members.iter().enumerate() {
            if let Some(ep) = EndpointRef::parse(member)
                && !topology.nodes.contains_key(&ep.node)
            {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{}' does not exist", ep.node),
                    location: Some(format!("networks.{net_name}.members[{k}]")),
                });
            }
        }

        for port_name in network.ports.keys() {
            if !topology.nodes.contains_key(port_name) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{port_name}' does not exist"),
                    location: Some(format!("networks.{net_name}.ports.{port_name}")),
                });
            }
        }
    }
}

/// Rule 4: Profile references must exist.
fn validate_dangling_profile_refs(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        if let Some(profile_name) = &node.profile
            && !topology.profiles.contains_key(profile_name)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "dangling-profile-ref",
                message: format!(
                    "profile '{profile_name}' referenced by node '{node_name}' does not exist"
                ),
                location: Some(format!("nodes.{node_name}.profile")),
            });
        }
    }
}

/// Rule 5: No duplicate interface names within a node.
fn validate_interface_uniqueness(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // We need to find duplicates — track all sources for each (node, iface) pair.
    let mut node_ifaces: HashMap<String, HashMap<String, Vec<InterfaceSource>>> = HashMap::new();

    // Explicit interfaces
    for (node_name, node) in &topology.nodes {
        let ifaces = node_ifaces.entry(node_name.clone()).or_default();
        for iface_name in node.interfaces.keys() {
            ifaces
                .entry(iface_name.clone())
                .or_default()
                .push(InterfaceSource::Explicit);
        }
        for wg_name in node.wireguard.keys() {
            ifaces
                .entry(wg_name.clone())
                .or_default()
                .push(InterfaceSource::Wireguard);
        }
    }

    // Interfaces from links
    for (i, link) in topology.links.iter().enumerate() {
        for ep_str in &link.endpoints {
            if let Some(ep) = EndpointRef::parse(ep_str) {
                let ifaces = node_ifaces.entry(ep.node.clone()).or_default();
                ifaces
                    .entry(ep.iface.clone())
                    .or_default()
                    .push(InterfaceSource::Link(i));
            }
        }
    }

    // Interfaces from networks
    for (net_name, network) in &topology.networks {
        for member in &network.members {
            if let Some(ep) = EndpointRef::parse(member) {
                let ifaces = node_ifaces.entry(ep.node.clone()).or_default();
                ifaces
                    .entry(ep.iface.clone())
                    .or_default()
                    .push(InterfaceSource::Network(net_name.clone()));
            }
        }
    }

    // Report duplicates
    for (node_name, ifaces) in &node_ifaces {
        for (iface_name, sources) in ifaces {
            if sources.len() > 1 {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "interface-uniqueness",
                    message: format!(
                        "duplicate interface '{iface_name}' on node '{node_name}' (from {} and {})",
                        sources[0], sources[1]
                    ),
                    location: Some(format!("nodes.{node_name}")),
                });
            }
        }
    }
}

/// Rule 6: VLAN IDs must be 1-4094.
fn validate_vlan_range(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (net_name, network) in &topology.networks {
        for &vid in network.vlans.keys() {
            if vid == 0 || vid > 4094 {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "vlan-range",
                    message: format!("VLAN ID {vid} out of range (1-4094)"),
                    location: Some(format!("networks.{net_name}.vlans.{vid}")),
                });
            }
        }

        for (port_name, port) in &network.ports {
            for &vid in &port.vlans {
                if vid == 0 || vid > 4094 {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "vlan-range",
                        message: format!("VLAN ID {vid} out of range (1-4094)"),
                        location: Some(format!("networks.{net_name}.ports.{port_name}.vlans")),
                    });
                }
            }
            if let Some(pvid) = port.pvid
                && (pvid == 0 || pvid > 4094)
            {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "vlan-range",
                    message: format!("PVID {pvid} out of range (1-4094)"),
                    location: Some(format!("networks.{net_name}.ports.{port_name}.pvid")),
                });
            }
        }
    }
}

/// Rule 7: Impairment keys must reference interfaces that exist on the node.
fn validate_impairment_refs(
    topology: &Topology,
    interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for key in topology.impairments.keys() {
        if let Some(ep) = EndpointRef::parse(key)
            && let Some(node_ifaces) = interfaces.get(&ep.node)
            && !node_ifaces.contains_key(&ep.iface)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "impairment-ref-valid",
                message: format!("node '{}' has no interface '{}'", ep.node, ep.iface),
                location: Some(format!("impairments.\"{key}\"")),
            });
        }
        // If node doesn't exist, dangling-node-ref will catch it
    }
}

/// Rule 8: Rate limit keys must reference interfaces that exist on the node.
fn validate_rate_limit_refs(
    topology: &Topology,
    interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for key in topology.rate_limits.keys() {
        if let Some(ep) = EndpointRef::parse(key)
            && let Some(node_ifaces) = interfaces.get(&ep.node)
            && !node_ifaces.contains_key(&ep.iface)
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "rate-limit-ref-valid",
                message: format!("node '{}' has no interface '{}'", ep.node, ep.iface),
                location: Some(format!("rate_limits.\"{key}\"")),
            });
        }
    }
}

/// Rule 9: Routes must have at least `via` or `dev`.
fn validate_route_config(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        for (dest, route) in &node.routes {
            if route.via.is_none() && route.dev.is_none() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "route-gateway-type",
                    message: format!("route '{dest}' has neither 'via' nor 'dev'"),
                    location: Some(format!("nodes.{node_name}.routes.{dest}")),
                });
            }
        }
    }
}

/// Rule 15: Interface names must not exceed 15 characters (Linux IFNAMSIZ - 1).
fn validate_interface_name_length(
    _topology: &Topology,
    interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for (node_name, ifaces) in interfaces {
        for iface_name in ifaces.keys() {
            if iface_name.len() > 15 {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "interface-name-length",
                    message: format!(
                        "interface '{iface_name}' on node '{node_name}' is {} chars (max 15)",
                        iface_name.len()
                    ),
                    location: Some(format!("nodes.{node_name}.{iface_name}")),
                });
            }
        }
    }
}

/// Rule 17: WireGuard peers must reference existing nodes with WireGuard interfaces.
fn validate_wireguard_peers(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        for (wg_name, wg_config) in &node.wireguard {
            for peer in &wg_config.peers {
                if !topology.nodes.contains_key(peer) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "wireguard-peer-exists",
                        message: format!(
                            "WireGuard peer '{peer}' referenced from {node_name}:{wg_name} does not exist"
                        ),
                        location: Some(format!("nodes.{node_name}.wireguard.{wg_name}.peers")),
                    });
                } else if topology.nodes[peer].wireguard.is_empty() {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "wireguard-peer-exists",
                        message: format!(
                            "WireGuard peer '{peer}' referenced from {node_name}:{wg_name} has no WireGuard interfaces"
                        ),
                        location: Some(format!("nodes.{node_name}.wireguard.{wg_name}.peers")),
                    });
                }
            }
        }
    }
}

/// Rule 18: VRF table IDs must be unique within a node.
fn validate_vrf_table_unique(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        let mut seen: HashMap<u32, &str> = HashMap::new();
        for (vrf_name, vrf_config) in &node.vrfs {
            if let Some(existing) = seen.get(&vrf_config.table) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "vrf-table-unique",
                    message: format!(
                        "VRF '{vrf_name}' and '{existing}' on node '{node_name}' share table {}",
                        vrf_config.table
                    ),
                    location: Some(format!("nodes.{node_name}.vrfs.{vrf_name}")),
                });
            }
            seen.insert(vrf_config.table, vrf_name);
        }
    }
}

/// Rule 19: The same endpoint should not appear in multiple links.
fn validate_duplicate_link_endpoints(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, link) in topology.links.iter().enumerate() {
        for ep in &link.endpoints {
            if let Some(prev) = seen.insert(ep.clone(), i) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "duplicate-link-endpoint",
                    message: format!("endpoint '{ep}' used in both link {prev} and link {i}"),
                    location: Some(format!("links[{i}]")),
                });
            }
        }
    }
}

// ─────────────────────────────────────────────────────
// Warning-level rules
// ─────────────────────────────────────────────────────

/// Rule 10: No duplicate IP addresses across the topology.
fn validate_unique_ips(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // Collect all (ip, location) pairs
    let mut seen: HashMap<String, String> = HashMap::new(); // ip_str -> location

    for (i, link) in topology.links.iter().enumerate() {
        if let Some(addresses) = &link.addresses {
            for (j, addr) in addresses.iter().enumerate() {
                if let Ok((ip, _)) = parse_cidr(addr) {
                    let ip_str = ip.to_string();
                    let location = format!("links[{i}].addresses[{j}]");
                    if let Some(prev) = seen.get(&ip_str) {
                        issues.push(ValidationIssue {
                            severity: Severity::Warning,
                            rule: "unique-ips",
                            message: format!("duplicate address '{ip_str}' (also at {prev})"),
                            location: Some(location),
                        });
                    } else {
                        seen.insert(ip_str, location);
                    }
                }
            }
        }
    }

    for (node_name, node) in &topology.nodes {
        for (iface_name, iface) in &node.interfaces {
            for (k, addr) in iface.addresses.iter().enumerate() {
                if let Ok((ip, _)) = parse_cidr(addr) {
                    let ip_str = ip.to_string();
                    let location =
                        format!("nodes.{node_name}.interfaces.{iface_name}.addresses[{k}]");
                    if let Some(prev) = seen.get(&ip_str) {
                        issues.push(ValidationIssue {
                            severity: Severity::Warning,
                            rule: "unique-ips",
                            message: format!("duplicate address '{ip_str}' (also at {prev})"),
                            location: Some(location),
                        });
                    } else {
                        seen.insert(ip_str, location);
                    }
                }
            }
        }
    }
}

/// Rule 11: Connected interfaces should have matching MTUs.
fn validate_mtu_consistency(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // For links with explicit MTU, no cross-link check needed (each link is self-consistent).
    // But check if a link MTU conflicts with an explicit interface MTU.
    for (i, link) in topology.links.iter().enumerate() {
        if let Some(link_mtu) = link.mtu {
            for (j, ep_str) in link.endpoints.iter().enumerate() {
                if let Some(ep) = EndpointRef::parse(ep_str)
                    && let Some(node) = topology.nodes.get(&ep.node)
                    && let Some(iface) = node.interfaces.get(&ep.iface)
                    && let Some(iface_mtu) = iface.mtu
                    && iface_mtu != link_mtu
                {
                    issues.push(ValidationIssue {
                        severity: Severity::Warning,
                        rule: "mtu-consistency",
                        message: format!(
                            "link MTU {link_mtu} differs from interface MTU {iface_mtu} on {ep_str}"
                        ),
                        location: Some(format!("links[{i}].endpoints[{j}]")),
                    });
                }
            }
        }
    }
}

/// Rule 12: Route gateways should be reachable from a connected subnet.
fn validate_route_reachability(
    topology: &Topology,
    _interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    // For each node, collect all subnets from link addresses and explicit interfaces
    for (node_name, node) in &topology.nodes {
        // Collect subnets assigned to this node
        let mut subnets = Vec::new();

        // From links
        for link in &topology.links {
            if let Some(addresses) = &link.addresses {
                for (j, ep_str) in link.endpoints.iter().enumerate() {
                    if let Some(ep) = EndpointRef::parse(ep_str)
                        && ep.node == *node_name
                        && let Ok((ip, prefix)) = parse_cidr(&addresses[j])
                    {
                        subnets.push((ip, prefix));
                    }
                }
            }
        }

        // From explicit interfaces
        for iface in node.interfaces.values() {
            for addr in &iface.addresses {
                if let Ok((ip, prefix)) = parse_cidr(addr) {
                    subnets.push((ip, prefix));
                }
            }
        }

        // Check each route's gateway
        for (dest, route) in &node.routes {
            if let Some(via_str) = &route.via
                && let Ok(gw) = via_str.parse::<std::net::IpAddr>()
            {
                let reachable = subnets
                    .iter()
                    .any(|(net, prefix)| ip_in_subnet(gw, *net, *prefix));
                if !reachable && !subnets.is_empty() {
                    issues.push(ValidationIssue {
                            severity: Severity::Warning,
                            rule: "route-reachability",
                            message: format!(
                                "gateway '{via_str}' not reachable from any connected subnet on node '{node_name}'"
                            ),
                            location: Some(format!("nodes.{node_name}.routes.{dest}")),
                        });
                }
            }
        }
    }
}

/// Rule 13: Nodes with no links or network connections are likely a mistake.
fn validate_unreferenced_nodes(
    topology: &Topology,
    interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for node_name in topology.nodes.keys() {
        if let Some(ifaces) = interfaces.get(node_name) {
            // Check if the node has any interfaces from links or networks
            let has_connections = ifaces
                .values()
                .any(|src| matches!(src, InterfaceSource::Link(_) | InterfaceSource::Network(_)));
            if !has_connections {
                issues.push(ValidationIssue {
                    severity: Severity::Warning,
                    rule: "unreferenced-node",
                    message: format!("node '{node_name}' has no links or network connections"),
                    location: Some(format!("nodes.{node_name}")),
                });
            }
        }
    }
}

/// Rule 14: Exec commands should not be empty.
fn validate_exec_cmds(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        for (i, exec) in node.exec.iter().enumerate() {
            if exec.cmd.is_empty() {
                issues.push(ValidationIssue {
                    severity: Severity::Warning,
                    rule: "empty-exec-cmd",
                    message: format!("exec[{i}] has empty cmd"),
                    location: Some(format!("nodes.{node_name}.exec[{i}]")),
                });
            }
        }
    }
}

/// Container field validation: cmd/env/volumes require image.
fn validate_container_fields(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        if node.image.is_none() {
            if node.cmd.is_some() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "container-requires-image",
                    message: "cmd requires image".to_string(),
                    location: Some(format!("nodes.{node_name}.cmd")),
                });
            }
            if node.env.is_some() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "container-requires-image",
                    message: "env requires image".to_string(),
                    location: Some(format!("nodes.{node_name}.env")),
                });
            }
            if node.volumes.is_some() {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "container-requires-image",
                    message: "volumes requires image".to_string(),
                    location: Some(format!("nodes.{node_name}.volumes")),
                });
            }
            // All container properties require image
            let container_checks: &[(&str, bool)] = &[
                ("cpu", node.cpu.is_some()),
                ("memory", node.memory.is_some()),
                ("entrypoint", node.entrypoint.is_some()),
                ("hostname", node.hostname.is_some()),
                ("workdir", node.workdir.is_some()),
                ("healthcheck", node.healthcheck.is_some()),
                ("privileged", node.privileged),
                ("pull", node.pull.is_some()),
                ("startup-delay", node.startup_delay.is_some()),
                ("env-file", node.env_file.is_some()),
                ("overlay", node.overlay.is_some()),
                ("cap-add", !node.cap_add.is_empty()),
                ("cap-drop", !node.cap_drop.is_empty()),
                ("labels", !node.labels.is_empty()),
                ("exec", !node.container_exec.is_empty()),
                ("configs", !node.configs.is_empty()),
            ];
            for (prop, has_value) in container_checks {
                if *has_value {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "container-requires-image",
                        message: format!("{prop} requires image"),
                        location: Some(format!("nodes.{node_name}.{prop}")),
                    });
                }
            }
        } else if let Some(image) = &node.image
            && image.is_empty()
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "empty-image",
                message: "image must not be empty".to_string(),
                location: Some(format!("nodes.{node_name}.image")),
            });
        }
    }

    // Validate depends-on references
    for (node_name, node) in &topology.nodes {
        for dep in &node.depends_on {
            if !topology.nodes.contains_key(dep) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "depends-on-exists",
                    message: format!("depends-on references undefined node '{dep}'"),
                    location: Some(format!("nodes.{node_name}.depends-on")),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    fn parse_and_validate(nll: &str) -> ValidationResult {
        let topo = parser::parse(nll).unwrap();
        topo.validate()
    }

    /// Build a topology directly for tests that can't be expressed in NLL.
    fn validate_topo(topo: crate::types::Topology) -> ValidationResult {
        topo.validate()
    }

    #[test]
    fn test_valid_topology() {
        let result = parse_and_validate(
            r#"lab "valid"
profile router { forward ipv4 }
node r1 : router
node h1 { route default via 10.0.0.1 }
link r1:eth0 -- h1:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(
            !result.has_errors(),
            "unexpected errors: {:?}",
            result.issues()
        );
    }

    #[test]
    fn test_invalid_cidr() {
        // Use builder: NLL parser enforces CIDR format during parsing
        let topo = crate::Lab::new("bad-cidr")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1", "10.0.0.2/24")
            })
            .build();
        let result = validate_topo(topo);
        assert!(result.has_errors());
        let errors: Vec<_> = result.errors().collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].rule, "valid-cidr");
    }

    #[test]
    fn test_invalid_cidr_prefix_too_large() {
        let topo = crate::Lab::new("bad-prefix")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/33", "10.0.0.2/24")
            })
            .build();
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "valid-cidr"));
    }

    #[test]
    fn test_bad_endpoint_format() {
        // NLL parser enforces endpoint format, so use builder with raw link
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "bad-ep".into();
        topo.nodes.insert("a".into(), Default::default());
        topo.nodes.insert("b".into(), Default::default());
        topo.links.push(crate::types::Link {
            endpoints: ["nocolon".into(), "b:eth0".into()],
            addresses: None,
            mtu: None,
        });
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "endpoint-format"));
    }

    #[test]
    fn test_dangling_node_ref() {
        let result = parse_and_validate(
            r#"lab "dangling"
node a
link a:eth0 -- nonexistent:eth0
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "dangling-node-ref"));
    }

    #[test]
    fn test_dangling_profile_ref() {
        // NLL lowerer catches undefined profiles during lowering, so use direct construction
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "dangling-profile".into();
        let mut node = crate::types::Node::default();
        node.profile = Some("nonexistent".into());
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "dangling-profile-ref"));
    }

    #[test]
    fn test_duplicate_interface() {
        // NLL can't create explicit interfaces with the same name as link endpoints
        // easily, so use the builder
        let mut topo = crate::Lab::new("dup-iface")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        // Add an explicit interface with the same name
        topo.nodes
            .get_mut("a")
            .unwrap()
            .interfaces
            .insert("eth0".into(), Default::default());
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "interface-uniqueness"));
    }

    #[test]
    fn test_vlan_out_of_range_port() {
        // VLAN out-of-range requires specific numeric values that are easier with builder
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "bad-vlan".into();
        topo.nodes.insert("a".into(), Default::default());
        let mut net = crate::types::Network {
            kind: Some("bridge".to_string()),
            members: vec!["a:eth0".into()],
            ..Default::default()
        };
        net.ports.insert(
            "a".into(),
            crate::types::PortConfig {
                interface: Some("eth0".into()),
                vlans: vec![0, 4095],
                pvid: Some(0),
                ..Default::default()
            },
        );
        topo.networks.insert("test".into(), net);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        let vlan_errors: Vec<_> = result.errors().filter(|e| e.rule == "vlan-range").collect();
        assert_eq!(vlan_errors.len(), 3); // vlans[0]=0, vlans[1]=4095, pvid=0
    }

    #[test]
    fn test_impairment_ref_invalid() {
        let result = parse_and_validate(
            r#"lab "bad-impairment"
node a
node b
link a:eth0 -- b:eth0
impair a:eth99 delay 10ms
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "impairment-ref-valid"));
    }

    #[test]
    fn test_rate_limit_ref_invalid() {
        let result = parse_and_validate(
            r#"lab "bad-rl"
node a
node b
link a:eth0 -- b:eth0
rate a:eth99 egress 100mbit
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "rate-limit-ref-valid"));
    }

    #[test]
    fn test_route_missing_via_and_dev() {
        // Route without via/dev requires direct type construction
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "bad-route".into();
        let mut node = crate::types::Node::default();
        node.routes.insert(
            "default".into(),
            crate::types::RouteConfig {
                via: None,
                dev: None,
                metric: None,
            },
        );
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "route-gateway-type"));
    }

    #[test]
    fn test_duplicate_ip_warning() {
        let result = parse_and_validate(
            r#"lab "dup-ip"
node a
node b
node c
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link b:eth1 -- c:eth0 { 10.0.0.1/24 -- 10.0.0.3/24 }
"#,
        );
        assert!(result.has_warnings());
        assert!(result.warnings().any(|w| w.rule == "unique-ips"));
    }

    #[test]
    fn test_unreferenced_node_warning() {
        let result = parse_and_validate(
            r#"lab "isolated"
node connected-a
node connected-b
node isolated
link connected-a:eth0 -- connected-b:eth0
"#,
        );
        assert!(result.has_warnings());
        assert!(
            result
                .warnings()
                .any(|w| w.rule == "unreferenced-node" && w.message.contains("isolated"))
        );
    }

    #[test]
    fn test_route_reachability_warning() {
        let result = parse_and_validate(
            r#"lab "unreachable-gw"
node a { route default via 192.168.1.1 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(result.has_warnings());
        assert!(result.warnings().any(|w| w.rule == "route-reachability"));
    }

    #[test]
    fn test_empty_exec_cmd_warning() {
        // Empty exec cmd can't be expressed in NLL, use builder
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "empty-cmd".into();
        let mut node = crate::types::Node::default();
        node.exec.push(crate::types::ExecConfig {
            cmd: Vec::new(),
            background: false,
        });
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_warnings());
        assert!(result.warnings().any(|w| w.rule == "empty-exec-cmd"));
    }

    #[test]
    fn test_bail_on_errors() {
        let result = parse_and_validate(
            r#"lab "bad"
node a
link a:eth0 -- missing:eth0
"#,
        );
        assert!(result.bail().is_err());
    }

    #[test]
    fn test_bail_ok_on_warnings_only() {
        // Warning-only: use empty exec cmd via builder
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "warnings-only".into();
        let mut node = crate::types::Node::default();
        node.exec.push(crate::types::ExecConfig {
            cmd: Vec::new(),
            background: false,
        });
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.bail().is_ok());
    }

    #[test]
    fn test_duplicate_link_endpoint() {
        let result = parse_and_validate(
            r#"lab "dup-ep"
node a
node b
node c
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link a:eth0 -- c:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "duplicate-link-endpoint"));
    }

    #[test]
    fn test_interface_name_length() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "long-iface".into();
        topo.nodes.insert("a".into(), Default::default());
        topo.nodes.insert("b".into(), Default::default());
        // 16-char interface name exceeds Linux's 15-char IFNAMSIZ limit
        topo.links.push(crate::types::Link {
            endpoints: ["a:this_is_too_long".into(), "b:eth0".into()],
            addresses: None,
            mtu: None,
        });
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "interface-name-length"));
    }

    #[test]
    fn test_mtu_consistency_warning() {
        let mut topo = crate::Lab::new("mtu-mismatch")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l.mtu(9000))
            .build();
        // Set a conflicting MTU on the explicit interface
        let iface = topo
            .nodes
            .get_mut("a")
            .unwrap()
            .interfaces
            .entry("eth0".into())
            .or_default();
        iface.mtu = Some(1500);
        let result = validate_topo(topo);
        assert!(result.has_warnings());
        assert!(result.warnings().any(|w| w.rule == "mtu-consistency"));
    }

    #[test]
    fn test_vrf_table_unique() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "dup-vrf-table".into();
        let mut node = crate::types::Node::default();
        node.vrfs.insert(
            "vrf1".into(),
            crate::types::VrfConfig {
                table: 100,
                interfaces: vec![],
                routes: Default::default(),
            },
        );
        node.vrfs.insert(
            "vrf2".into(),
            crate::types::VrfConfig {
                table: 100, // same table — conflict
                interfaces: vec![],
                routes: Default::default(),
            },
        );
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "vrf-table-unique"));
    }

    #[test]
    fn test_wireguard_peer_exists() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "wg-bad-peer".into();
        let mut node = crate::types::Node::default();
        node.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                peers: vec!["nonexistent".into()],
                ..Default::default()
            },
        );
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "wireguard-peer-exists"));
    }

    #[test]
    fn test_container_requires_image() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "no-image".into();
        let mut node = crate::types::Node::default();
        // Set container fields without an image
        node.env = Some([("FOO".into(), "bar".into())].into());
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(
            result
                .errors()
                .any(|e| e.rule == "container-requires-image")
        );
    }

    #[test]
    fn test_empty_image() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "empty-img".into();
        let mut node = crate::types::Node::default();
        node.image = Some(String::new());
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "empty-image"));
    }
}
