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
            let messages: Vec<String> = self.errors().map(|i| i.to_string()).collect();
            Err(crate::Error::Validation(messages.join("; ")))
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
fn collect_interfaces(
    topology: &Topology,
) -> HashMap<String, HashMap<String, InterfaceSource>> {
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

        // Warning-level rules
        validate_unique_ips(self, &mut issues);
        validate_mtu_consistency(self, &mut issues);
        validate_route_reachability(self, &interfaces, &mut issues);
        validate_unreferenced_nodes(self, &interfaces, &mut issues);
        validate_exec_cmds(self, &mut issues);

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
        if let Some(subnet) = &network.subnet {
            if let Err(e) = parse_cidr(subnet) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "valid-cidr",
                    message: format!("invalid CIDR '{subnet}': {e}"),
                    location: Some(format!("networks.{net_name}.subnet")),
                });
            }
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
                    message: format!(
                        "invalid endpoint '{ep}': expected 'node:interface' format"
                    ),
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
            if let Some(ep) = EndpointRef::parse(ep_str) {
                if !topology.nodes.contains_key(&ep.node) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "dangling-node-ref",
                        message: format!("node '{}' does not exist", ep.node),
                        location: Some(format!("links[{i}].endpoints[{j}]")),
                    });
                }
            }
        }
    }

    for key in topology.impairments.keys() {
        if let Some(ep) = EndpointRef::parse(key) {
            if !topology.nodes.contains_key(&ep.node) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{}' does not exist", ep.node),
                    location: Some(format!("impairments.\"{key}\"")),
                });
            }
        }
    }

    for key in topology.rate_limits.keys() {
        if let Some(ep) = EndpointRef::parse(key) {
            if !topology.nodes.contains_key(&ep.node) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{}' does not exist", ep.node),
                    location: Some(format!("rate_limits.\"{key}\"")),
                });
            }
        }
    }

    for (net_name, network) in &topology.networks {
        for (k, member) in network.members.iter().enumerate() {
            if let Some(ep) = EndpointRef::parse(member) {
                if !topology.nodes.contains_key(&ep.node) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "dangling-node-ref",
                        message: format!("node '{}' does not exist", ep.node),
                        location: Some(format!("networks.{net_name}.members[{k}]")),
                    });
                }
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
        if let Some(profile_name) = &node.profile {
            if !topology.profiles.contains_key(profile_name) {
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
                        location: Some(format!(
                            "networks.{net_name}.ports.{port_name}.vlans"
                        )),
                    });
                }
            }
            if let Some(pvid) = port.pvid {
                if pvid == 0 || pvid > 4094 {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "vlan-range",
                        message: format!("PVID {pvid} out of range (1-4094)"),
                        location: Some(format!(
                            "networks.{net_name}.ports.{port_name}.pvid"
                        )),
                    });
                }
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
        if let Some(ep) = EndpointRef::parse(key) {
            if let Some(node_ifaces) = interfaces.get(&ep.node) {
                if !node_ifaces.contains_key(&ep.iface) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "impairment-ref-valid",
                        message: format!(
                            "node '{}' has no interface '{}'",
                            ep.node, ep.iface
                        ),
                        location: Some(format!("impairments.\"{key}\"")),
                    });
                }
            }
            // If node doesn't exist, dangling-node-ref will catch it
        }
    }
}

/// Rule 8: Rate limit keys must reference interfaces that exist on the node.
fn validate_rate_limit_refs(
    topology: &Topology,
    interfaces: &HashMap<String, HashMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for key in topology.rate_limits.keys() {
        if let Some(ep) = EndpointRef::parse(key) {
            if let Some(node_ifaces) = interfaces.get(&ep.node) {
                if !node_ifaces.contains_key(&ep.iface) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "rate-limit-ref-valid",
                        message: format!(
                            "node '{}' has no interface '{}'",
                            ep.node, ep.iface
                        ),
                        location: Some(format!("rate_limits.\"{key}\"")),
                    });
                }
            }
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
                            message: format!(
                                "duplicate address '{ip_str}' (also at {prev})"
                            ),
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
                            message: format!(
                                "duplicate address '{ip_str}' (also at {prev})"
                            ),
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
                if let Some(ep) = EndpointRef::parse(ep_str) {
                    if let Some(node) = topology.nodes.get(&ep.node) {
                        if let Some(iface) = node.interfaces.get(&ep.iface) {
                            if let Some(iface_mtu) = iface.mtu {
                                if iface_mtu != link_mtu {
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
                    if let Some(ep) = EndpointRef::parse(ep_str) {
                        if ep.node == *node_name {
                            if let Ok((ip, prefix)) = parse_cidr(&addresses[j]) {
                                subnets.push((ip, prefix));
                            }
                        }
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
            if let Some(via_str) = &route.via {
                if let Ok(gw) = via_str.parse::<std::net::IpAddr>() {
                    let reachable = subnets.iter().any(|(net, prefix)| {
                        ip_in_subnet(gw, *net, *prefix)
                    });
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
            let has_connections = ifaces.values().any(|src| {
                matches!(
                    src,
                    InterfaceSource::Link(_) | InterfaceSource::Network(_)
                )
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    fn parse_and_validate(toml: &str) -> ValidationResult {
        let topo = parser::parse(toml).unwrap();
        topo.validate()
    }

    #[test]
    fn test_valid_topology() {
        let result = parse_and_validate(
            r#"
[lab]
name = "valid"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.r1]
profile = "router"

[nodes.h1]

[nodes.h1.routes]
default = { via = "10.0.0.1" }

[[links]]
endpoints = ["r1:eth0", "h1:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]
"#,
        );
        assert!(!result.has_errors(), "unexpected errors: {:?}", result.issues());
    }

    #[test]
    fn test_invalid_cidr() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-cidr"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
addresses = ["10.0.0.1", "10.0.0.2/24"]
"#,
        );
        assert!(result.has_errors());
        let errors: Vec<_> = result.errors().collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].rule, "valid-cidr");
    }

    #[test]
    fn test_invalid_cidr_prefix_too_large() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-prefix"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
addresses = ["10.0.0.1/33", "10.0.0.2/24"]
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "valid-cidr"));
    }

    #[test]
    fn test_bad_endpoint_format() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-ep"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["nocolon", "b:eth0"]
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "endpoint-format"));
    }

    #[test]
    fn test_dangling_node_ref() {
        let result = parse_and_validate(
            r#"
[lab]
name = "dangling"

[nodes.a]

[[links]]
endpoints = ["a:eth0", "nonexistent:eth0"]
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "dangling-node-ref"));
    }

    #[test]
    fn test_dangling_profile_ref() {
        let result = parse_and_validate(
            r#"
[lab]
name = "dangling-profile"

[nodes.a]
profile = "nonexistent"
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "dangling-profile-ref"));
    }

    #[test]
    fn test_duplicate_interface() {
        let result = parse_and_validate(
            r#"
[lab]
name = "dup-iface"

[nodes.a]
[nodes.a.interfaces.eth0]
addresses = ["10.0.0.1/24"]

[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
"#,
        );
        assert!(result.has_errors());
        assert!(result
            .errors()
            .any(|e| e.rule == "interface-uniqueness"));
    }

    #[test]
    fn test_vlan_out_of_range_port() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-vlan"

[nodes.a]

[networks.test]
kind = "bridge"
members = ["a:eth0"]

[networks.test.ports.a]
interface = "eth0"
vlans = [0, 4095]
pvid = 0
"#,
        );
        assert!(result.has_errors());
        let vlan_errors: Vec<_> = result
            .errors()
            .filter(|e| e.rule == "vlan-range")
            .collect();
        assert_eq!(vlan_errors.len(), 3); // vlans[0]=0, vlans[1]=4095, pvid=0
    }

    #[test]
    fn test_impairment_ref_invalid() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-impairment"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]

[impairments."a:eth99"]
delay = "10ms"
"#,
        );
        assert!(result.has_errors());
        assert!(result
            .errors()
            .any(|e| e.rule == "impairment-ref-valid"));
    }

    #[test]
    fn test_rate_limit_ref_invalid() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-rl"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]

[rate_limits."a:eth99"]
egress = "100mbit"
"#,
        );
        assert!(result.has_errors());
        assert!(result
            .errors()
            .any(|e| e.rule == "rate-limit-ref-valid"));
    }

    #[test]
    fn test_route_missing_via_and_dev() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad-route"

[nodes.a]

[nodes.a.routes]
default = {}
"#,
        );
        assert!(result.has_errors());
        assert!(result
            .errors()
            .any(|e| e.rule == "route-gateway-type"));
    }

    #[test]
    fn test_duplicate_ip_warning() {
        let result = parse_and_validate(
            r#"
[lab]
name = "dup-ip"

[nodes.a]
[nodes.b]
[nodes.c]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]

[[links]]
endpoints = ["b:eth1", "c:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.3/24"]
"#,
        );
        assert!(result.has_warnings());
        assert!(result.warnings().any(|w| w.rule == "unique-ips"));
    }

    #[test]
    fn test_unreferenced_node_warning() {
        let result = parse_and_validate(
            r#"
[lab]
name = "isolated"

[nodes.connected_a]
[nodes.connected_b]
[nodes.isolated]

[[links]]
endpoints = ["connected_a:eth0", "connected_b:eth0"]
"#,
        );
        assert!(result.has_warnings());
        assert!(result
            .warnings()
            .any(|w| w.rule == "unreferenced-node"
                && w.message.contains("isolated")));
    }

    #[test]
    fn test_route_reachability_warning() {
        let result = parse_and_validate(
            r#"
[lab]
name = "unreachable-gw"

[nodes.a]
[nodes.b]

[nodes.a.routes]
default = { via = "192.168.1.1" }

[[links]]
endpoints = ["a:eth0", "b:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]
"#,
        );
        assert!(result.has_warnings());
        assert!(result
            .warnings()
            .any(|w| w.rule == "route-reachability"));
    }

    #[test]
    fn test_empty_exec_cmd_warning() {
        let result = parse_and_validate(
            r#"
[lab]
name = "empty-cmd"

[nodes.a]

[[nodes.a.exec]]
cmd = []
"#,
        );
        assert!(result.has_warnings());
        assert!(result
            .warnings()
            .any(|w| w.rule == "empty-exec-cmd"));
    }

    #[test]
    fn test_bail_on_errors() {
        let result = parse_and_validate(
            r#"
[lab]
name = "bad"

[nodes.a]

[[links]]
endpoints = ["a:eth0", "missing:eth0"]
"#,
        );
        assert!(result.bail().is_err());
    }

    #[test]
    fn test_bail_ok_on_warnings_only() {
        let result = parse_and_validate(
            r#"
[lab]
name = "warnings-only"

[nodes.a]

[[nodes.a.exec]]
cmd = []
"#,
        );
        assert!(result.bail().is_ok());
    }
}
