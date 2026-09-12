//! Topology validation.
//!
//! Validates a parsed [`Topology`] before deployment, catching semantic errors
//! that the NLL parser cannot detect (cross-references, subnet overlaps,
//! value ranges, …). Every rule has a stable kebab-case id; see
//! [`rule_ids`].
//!
//! # Example
//!
//! ```ignore
//! let topology = nlink_lab::parser::parse_file("lab.nll")?;
//! let result = topology.validate();
//! if result.has_errors() {
//!     for issue in result.errors() {
//!         eprintln!("ERROR: {issue}");
//!     }
//! }
//! result.bail()?;
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;

use crate::helpers::{
    ip_in_subnet, network_address, parse_cidr, parse_duration, parse_percent, parse_rate_bps,
    validate_interface_name,
};
use crate::types::{
    Assertion, BenchmarkTest, EndpointRef, Impairment, InterfaceKind, ScenarioAction, Topology,
};

/// Every rule identifier the validator can emit, in dispatch order.
///
/// Error-level rules come first, warning-level rules last. The
/// `rule_ids_are_stable` unit test cross-checks this list against every
/// rule literal emitted in this file, so adding a rule without listing it
/// here (or listing one that is never emitted) fails the build's tests.
pub const RULE_IDS: &[&str] = &[
    // Error-level
    "invalid-name",
    "unresolved-interpolation",
    "valid-cidr",
    "endpoint-format",
    "dangling-node-ref",
    "dangling-profile-ref",
    "interface-uniqueness",
    "vlan-range",
    "impairment-ref-valid",
    "network-impair-needs-subnet",
    "network-impair-self-pair",
    "network-impair-member",
    "rate-limit-ref-valid",
    "route-gateway-type",
    "interface-name-length",
    "wireguard-peer-exists",
    "vrf-table-unique",
    "duplicate-link-endpoint",
    "link-endpoints-same-subnet",
    "overlapping-subnets",
    "invalid-impairment-value",
    "invalid-nat-cidr",
    "invalid-route-dest",
    "mgmt-ipv6-unsupported",
    "mgmt-subnet-capacity",
    "vxlan-vni-range",
    "wifi-channel-range",
    "macvlan-parent-set",
    "vrf-interface-exists",
    "assertion-node-exists",
    "assertion-endpoint-exists",
    "container-requires-image",
    "empty-image",
    "depends-on-exists",
    "depends-on-cycle",
    // Warning-level
    "mgmt-subnet-not-network-address",
    "unique-ips",
    "mtu-consistency",
    "route-reachability",
    "unreferenced-node",
    "empty-exec-cmd",
];

/// All rule identifiers this validator can emit (for `validate --list-rules`).
pub fn rule_ids() -> &'static [&'static str] {
    RULE_IDS
}

/// The rules that emit at [`Severity::Warning`]; every other id in
/// [`RULE_IDS`] is an error. Kept as a list (not derived from position)
/// so `rule_severity` is explicit; `warning_rules_are_listed` checks it.
const WARNING_RULE_IDS: &[&str] = &[
    "mgmt-subnet-not-network-address",
    "unique-ips",
    "mtu-consistency",
    "route-reachability",
    "unreferenced-node",
    "empty-exec-cmd",
];

/// Default severity of a rule, `None` for an unknown id.
pub fn rule_severity(id: &str) -> Option<Severity> {
    if WARNING_RULE_IDS.contains(&id) {
        Some(Severity::Warning)
    } else if RULE_IDS.contains(&id) {
        Some(Severity::Error)
    } else {
        None
    }
}

/// Per-rule severity overrides for [`Topology::validate_with`]:
/// `deny` promotes a warning to an error, `allow` silences a warning
/// (errors cannot be silenced — they describe topologies that cannot
/// deploy). `strict` promotes every warning.
#[derive(Debug, Clone, Default)]
pub struct RuleOptions {
    pub strict: bool,
    pub deny: Vec<String>,
    pub allow: Vec<String>,
}

impl RuleOptions {
    /// Reject unknown rule ids up front so a typo in `--deny` is loud.
    pub fn check_known(&self) -> std::result::Result<(), String> {
        let unknown: Vec<&str> = self
            .deny
            .iter()
            .chain(self.allow.iter())
            .map(String::as_str)
            .filter(|id| rule_severity(id).is_none())
            .collect();
        if unknown.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "unknown validation rule(s): {} (see `validate --list-rules`)",
                unknown.join(", ")
            ))
        }
    }

    fn apply(&self, mut issue: ValidationIssue) -> Option<ValidationIssue> {
        if issue.severity == Severity::Warning {
            if self.allow.iter().any(|a| a == issue.rule) {
                return None;
            }
            if self.strict || self.deny.iter().any(|d| d == issue.rule) {
                issue.severity = Severity::Error;
            }
        }
        Some(issue)
    }
}

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
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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
    Macvlan,
    Ipvlan,
    Wifi,
}

impl fmt::Display for InterfaceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => write!(f, "interfaces"),
            Self::Link(i) => write!(f, "links[{i}]"),
            Self::Network(n) => write!(f, "networks.{n}"),
            Self::Wireguard => write!(f, "wireguard"),
            Self::Macvlan => write!(f, "macvlans"),
            Self::Ipvlan => write!(f, "ipvlans"),
            Self::Wifi => write!(f, "wifi"),
        }
    }
}

/// Iterate a string-keyed map in name order so issue output is deterministic.
fn sorted<V>(map: &BTreeMap<String, V>) -> Vec<(&String, &V)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

/// Collect all interfaces per node from all sources.
fn collect_interfaces(topology: &Topology) -> BTreeMap<String, BTreeMap<String, InterfaceSource>> {
    let mut result: BTreeMap<String, BTreeMap<String, InterfaceSource>> = BTreeMap::new();

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
        // Host-attached and radio interfaces
        for mv in &node.macvlans {
            ifaces
                .entry(mv.name.clone())
                .or_insert(InterfaceSource::Macvlan);
        }
        for iv in &node.ipvlans {
            ifaces
                .entry(iv.name.clone())
                .or_insert(InterfaceSource::Ipvlan);
        }
        for wifi in &node.wifi {
            ifaces
                .entry(wifi.name.clone())
                .or_insert(InterfaceSource::Wifi);
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
    /// [`validate`](Self::validate) with per-rule severity overrides.
    pub fn validate_with(&self, opts: &RuleOptions) -> ValidationResult {
        let base = self.validate();
        ValidationResult {
            issues: base
                .issues
                .into_iter()
                .filter_map(|i| opts.apply(i))
                .collect(),
        }
    }

    pub fn validate(&self) -> ValidationResult {
        let mut issues = Vec::new();
        let interfaces = collect_interfaces(self);

        // Error-level rules
        validate_names(self, &interfaces, &mut issues);
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
        validate_link_endpoint_subnets(self, &mut issues);
        validate_overlapping_subnets(self, &mut issues);
        validate_impairment_values(self, &mut issues);
        validate_nat_addresses(self, &mut issues);
        validate_route_addresses(self, &mut issues);
        validate_mgmt_subnet(self, &mut issues);
        validate_vxlan_vni(self, &mut issues);
        validate_wifi_channels(self, &mut issues);
        validate_macvlan_parents(self, &mut issues);
        validate_vrf_interfaces(self, &interfaces, &mut issues);
        validate_test_refs(self, &interfaces, &mut issues);
        validate_container_fields(self, &mut issues);

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

/// All address strings must be valid CIDR notation.
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

/// All endpoints must match "node:interface" format.
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

/// Endpoint node names must exist in topology.nodes.
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
            // Port name can be "node" or "node:iface" — check node part
            let node_part = if let Some(ep) = EndpointRef::parse(port_name) {
                ep.node
            } else {
                port_name.to_string()
            };
            if !topology.nodes.contains_key(&node_part) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "dangling-node-ref",
                    message: format!("node '{node_part}' does not exist"),
                    location: Some(format!("networks.{net_name}.ports.{port_name}")),
                });
            }
        }
    }
}

/// Profile references must exist.
fn validate_dangling_profile_refs(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        for (idx, profile_name) in node.profiles.iter().enumerate() {
            if topology.profiles.contains_key(profile_name) {
                continue;
            }
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "dangling-profile-ref",
                message: format!(
                    "profile '{profile_name}' referenced by node '{node_name}' does not exist"
                ),
                location: Some(format!("nodes.{node_name}.profiles[{idx}]")),
            });
        }
    }
}

/// No duplicate interface names within a node.
fn validate_interface_uniqueness(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // We need to find duplicates — track all sources for each (node, iface) pair.
    let mut node_ifaces: BTreeMap<String, BTreeMap<String, Vec<InterfaceSource>>> = BTreeMap::new();

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

/// VLAN IDs must be 1-4094.
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

/// Impairment keys must reference interfaces that exist on the node.
fn validate_impairment_refs(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
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

    // Network-level per-pair impairments.
    for (net_name, network) in &topology.networks {
        // Build a set of node names in this network's members for fast lookup.
        let member_nodes: std::collections::HashSet<String> = network
            .members
            .iter()
            .filter_map(|m| EndpointRef::parse(m).map(|e| e.node))
            .collect();

        let needs_subnet_for_dispatch = !network.impairments.is_empty();
        if needs_subnet_for_dispatch && network.subnet.is_none() {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "network-impair-needs-subnet",
                message: format!(
                    "network '{net_name}' has per-pair impairments but no subnet — \
                     a subnet is required so destination IPs can be resolved"
                ),
                location: Some(format!("networks.{net_name}.subnet")),
            });
        }

        for (i, imp) in network.impairments.iter().enumerate() {
            if imp.src == imp.dst {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "network-impair-self-pair",
                    message: format!(
                        "network '{net_name}' impair {} -- {}: src and dst must differ",
                        imp.src, imp.dst
                    ),
                    location: Some(format!("networks.{net_name}.impairments[{i}]")),
                });
                continue;
            }

            for (which, who) in [("src", &imp.src), ("dst", &imp.dst)] {
                if !topology.nodes.contains_key(who) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "dangling-node-ref",
                        message: format!("node '{who}' does not exist"),
                        location: Some(format!("networks.{net_name}.impairments[{i}].{which}")),
                    });
                } else if !member_nodes.contains(who) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "network-impair-member",
                        message: format!(
                            "node '{who}' is not a member of network '{net_name}' \
                             (cannot impair traffic on a network the node is not on)"
                        ),
                        location: Some(format!("networks.{net_name}.impairments[{i}].{which}")),
                    });
                }
            }
        }
    }
}

/// Rate limit keys must reference interfaces that exist on the node.
fn validate_rate_limit_refs(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
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

/// Routes must have at least `via` or `dev`.
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

/// Interface names must not exceed 15 characters (Linux IFNAMSIZ - 1).
fn validate_interface_name_length(
    _topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
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

/// WireGuard peers must reference existing nodes with WireGuard interfaces.
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

/// VRF table IDs must be unique within a node.
fn validate_vrf_table_unique(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in &topology.nodes {
        let mut seen: BTreeMap<u32, &str> = BTreeMap::new();
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

/// The same endpoint should not appear in multiple links.
fn validate_duplicate_link_endpoints(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
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

/// Report a literal `${…}` that the lowerer left behind. Returns true if reported.
fn check_unresolved(value: &str, location: String, issues: &mut Vec<ValidationIssue>) -> bool {
    if value.contains("${") {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "unresolved-interpolation",
            message: format!("'{value}' contains an unresolved ${{…}} interpolation"),
            location: Some(location),
        });
        true
    } else {
        false
    }
}

/// `^[A-Za-z0-9_][A-Za-z0-9_.-]*$` — names flow into filesystem paths
/// (`state_dir`, `/etc/netns/<ns>`) and kernel object names.
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Lab, prefix, node, network, profile, VRF and interface names must be
/// path- and kernel-safe.
fn validate_names(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut check = |kind: &str, name: &str, location: String| {
        if check_unresolved(name, location.clone(), issues) {
            return;
        }
        if !is_valid_name(name) {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "invalid-name",
                message: format!(
                    "{kind} name '{name}' is invalid: must match [A-Za-z0-9_][A-Za-z0-9_.-]*"
                ),
                location: Some(location),
            });
        }
    };

    check("lab", &topology.lab.name, "lab.name".into());
    if let Some(prefix) = &topology.lab.prefix {
        check("prefix", prefix, "lab.prefix".into());
    }
    for (name, _) in sorted(&topology.profiles) {
        check("profile", name, format!("profiles.{name}"));
    }
    for (name, node) in sorted(&topology.nodes) {
        check("node", name, format!("nodes.{name}"));
        for (vrf_name, _) in sorted(&node.vrfs) {
            check("VRF", vrf_name, format!("nodes.{name}.vrfs.{vrf_name}"));
        }
    }
    for (name, _) in sorted(&topology.networks) {
        check("network", name, format!("networks.{name}"));
    }
    for (node_name, ifaces) in sorted(interfaces) {
        for (iface_name, _) in sorted(ifaces) {
            check(
                "interface",
                iface_name,
                format!("nodes.{node_name}.{iface_name}"),
            );
        }
    }
}

/// Both ends of an addressed link must sit in the same subnet.
fn validate_link_endpoint_subnets(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (i, link) in topology.links.iter().enumerate() {
        let Some(addrs) = &link.addresses else {
            continue;
        };
        let (Ok((a, pa)), Ok((b, pb))) = (parse_cidr(&addrs[0]), parse_cidr(&addrs[1])) else {
            continue; // valid-cidr reports it
        };
        if pa != pb || network_address(a, pa) != network_address(b, pb) {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "link-endpoints-same-subnet",
                message: format!(
                    "link endpoints '{}' and '{}' are not in the same subnet",
                    addrs[0], addrs[1]
                ),
                location: Some(format!("links[{i}].addresses")),
            });
        }
    }
}

/// Who "owns" a subnet — addresses on the same owner share it by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubnetOwner<'a> {
    Mgmt,
    Link(usize),
    Network(&'a str),
    /// A node-level interface (dummy, VLAN, bond, VXLAN, WireGuard, macvlan,
    /// ipvlan, Wi-Fi). Identical subnets across such interfaces are shared
    /// segments reached by other means (tunnels, trunks, radios), so only
    /// strict containment is a conflict.
    Interface,
}

struct SubnetEntry<'a> {
    net: IpAddr,
    prefix: u8,
    owner: SubnetOwner<'a>,
    location: String,
}

impl SubnetEntry<'_> {
    fn cidr(&self) -> String {
        format!("{}/{}", self.net, self.prefix)
    }
}

fn push_subnet<'a>(
    out: &mut Vec<SubnetEntry<'a>>,
    cidr: &str,
    owner: SubnetOwner<'a>,
    location: String,
) {
    let Ok((ip, prefix)) = parse_cidr(cidr) else {
        return; // valid-cidr reports it
    };
    let net = network_address(ip, prefix);
    // One entry per (subnet, owner): a link's two endpoints or a network's
    // ports would otherwise each report the same conflict.
    if out
        .iter()
        .any(|e| e.net == net && e.prefix == prefix && e.owner == owner)
    {
        return;
    }
    out.push(SubnetEntry {
        net,
        prefix,
        owner,
        location,
    });
}

/// Every subnet declared in the topology, in a deterministic order.
fn collect_subnets(topology: &Topology) -> Vec<SubnetEntry<'_>> {
    let mut out = Vec::new();

    if let Some(mgmt) = &topology.lab.mgmt_subnet {
        push_subnet(&mut out, mgmt, SubnetOwner::Mgmt, "lab.mgmt".into());
    }

    for (i, link) in topology.links.iter().enumerate() {
        if let Some(addrs) = &link.addresses {
            for (j, addr) in addrs.iter().enumerate() {
                push_subnet(
                    &mut out,
                    addr,
                    SubnetOwner::Link(i),
                    format!("links[{i}].addresses[{j}]"),
                );
            }
        }
    }

    for (net_name, network) in sorted(&topology.networks) {
        if let Some(subnet) = &network.subnet {
            push_subnet(
                &mut out,
                subnet,
                SubnetOwner::Network(net_name),
                format!("networks.{net_name}.subnet"),
            );
        }
        for (port_name, port) in sorted(&network.ports) {
            for (k, addr) in port.addresses.iter().enumerate() {
                push_subnet(
                    &mut out,
                    addr,
                    SubnetOwner::Network(net_name),
                    format!("networks.{net_name}.ports.{port_name}.addresses[{k}]"),
                );
            }
        }
    }

    for (node_name, node) in sorted(&topology.nodes) {
        for (iface_name, iface) in sorted(&node.interfaces) {
            for (k, addr) in iface.addresses.iter().enumerate() {
                push_subnet(
                    &mut out,
                    addr,
                    SubnetOwner::Interface,
                    format!("nodes.{node_name}.interfaces.{iface_name}.addresses[{k}]"),
                );
            }
        }
        for (wg_name, wg) in sorted(&node.wireguard) {
            for (k, addr) in wg.addresses.iter().enumerate() {
                push_subnet(
                    &mut out,
                    addr,
                    SubnetOwner::Interface,
                    format!("nodes.{node_name}.wireguard.{wg_name}.addresses[{k}]"),
                );
            }
        }
        let host_attached = node
            .macvlans
            .iter()
            .enumerate()
            .map(|(i, mv)| ("macvlans", i, &mv.addresses))
            .chain(
                node.ipvlans
                    .iter()
                    .enumerate()
                    .map(|(i, iv)| ("ipvlans", i, &iv.addresses)),
            )
            .chain(
                node.wifi
                    .iter()
                    .enumerate()
                    .map(|(i, w)| ("wifi", i, &w.addresses)),
            );
        for (field, i, addresses) in host_attached {
            for (k, addr) in addresses.iter().enumerate() {
                push_subnet(
                    &mut out,
                    addr,
                    SubnetOwner::Interface,
                    format!("nodes.{node_name}.{field}[{i}].addresses[{k}]"),
                );
            }
        }
    }

    out
}

/// Two distinct subnets must not intersect. A link's two endpoints and a
/// network's ports share their subnet by design; node-level interfaces may
/// repeat an identical subnet across nodes (tunnel/trunk/radio segments)
/// but may not nest inside a different one.
fn validate_overlapping_subnets(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    let entries = collect_subnets(topology);
    for (i, a) in entries.iter().enumerate() {
        for b in &entries[i + 1..] {
            if a.owner != SubnetOwner::Interface && a.owner == b.owner {
                continue; // same link / network / mgmt segment
            }
            if a.net.is_ipv4() != b.net.is_ipv4() {
                continue;
            }
            let shorter = a.prefix.min(b.prefix);
            if !ip_in_subnet(b.net, a.net, shorter) {
                continue;
            }
            let identical = a.prefix == b.prefix;
            if identical && (a.owner == SubnetOwner::Interface || b.owner == SubnetOwner::Interface)
            {
                continue;
            }
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "overlapping-subnets",
                message: format!(
                    "subnet {} overlaps {} (declared at {})",
                    b.cidr(),
                    a.cidr(),
                    a.location
                ),
                location: Some(b.location.clone()),
            });
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ValueKind {
    Duration,
    Percent,
    Rate,
    /// A positive packet count (`limit`).
    Packets,
}

fn check_value(kind: ValueKind, value: &str, location: String, issues: &mut Vec<ValidationIssue>) {
    if check_unresolved(value, location.clone(), issues) {
        return;
    }
    let err = match kind {
        ValueKind::Duration => parse_duration(value).err(),
        ValueKind::Percent => match parse_percent(value) {
            Ok(p) if !(0.0..=100.0).contains(&p) => Some(crate::Error::invalid_topology(format!(
                "percentage {p} out of range (0-100)"
            ))),
            Ok(_) => None,
            Err(e) => Some(e),
        },
        ValueKind::Rate => parse_rate_bps(value).err(),
        ValueKind::Packets => match value.parse::<u32>() {
            Ok(0) | Err(_) => Some(crate::Error::invalid_topology(
                "expected a positive packet count".to_string(),
            )),
            Ok(_) => None,
        },
    };
    if let Some(e) = err {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "invalid-impairment-value",
            message: format!("invalid value '{value}': {e}"),
            location: Some(location),
        });
    }
}

fn check_impairment(imp: &Impairment, prefix: &str, issues: &mut Vec<ValidationIssue>) {
    let fields = [
        ("delay", &imp.delay, ValueKind::Duration),
        ("jitter", &imp.jitter, ValueKind::Duration),
        ("loss", &imp.loss, ValueKind::Percent),
        ("corrupt", &imp.corrupt, ValueKind::Percent),
        ("reorder", &imp.reorder, ValueKind::Percent),
        ("rate", &imp.rate, ValueKind::Rate),
        ("duplicate", &imp.duplicate, ValueKind::Percent),
        (
            "delay-correlation",
            &imp.delay_correlation,
            ValueKind::Percent,
        ),
        (
            "loss-correlation",
            &imp.loss_correlation,
            ValueKind::Percent,
        ),
        ("limit", &imp.limit, ValueKind::Packets),
    ];
    for (name, value, kind) in fields {
        if let Some(v) = value {
            check_value(kind, v, format!("{prefix}.{name}"), issues);
        }
    }
}

/// Every netem / rate-limit literal must parse and percentages must be 0-100.
fn validate_impairment_values(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (key, imp) in sorted(&topology.impairments) {
        check_impairment(imp, &format!("impairments.\"{key}\""), issues);
    }

    for (net_name, network) in sorted(&topology.networks) {
        for (i, pair) in network.impairments.iter().enumerate() {
            let prefix = format!("networks.{net_name}.impairments[{i}]");
            check_impairment(&pair.impairment, &prefix, issues);
            if let Some(cap) = &pair.rate_cap {
                check_value(ValueKind::Rate, cap, format!("{prefix}.rate_cap"), issues);
            }
        }
    }

    for (key, rl) in sorted(&topology.rate_limits) {
        for (name, value) in [
            ("egress", &rl.egress),
            ("ingress", &rl.ingress),
            ("burst", &rl.burst),
        ] {
            if let Some(v) = value {
                check_value(
                    ValueKind::Rate,
                    v,
                    format!("rate_limits.\"{key}\".{name}"),
                    issues,
                );
            }
        }
    }
}

fn is_ip_or_cidr(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok() || parse_cidr(s).is_ok()
}

/// NAT `src` / `dst` / `target` must be an IP address or CIDR.
fn validate_nat_addresses(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in sorted(&topology.nodes) {
        let Some(nat) = &node.nat else {
            continue;
        };
        for (i, rule) in nat.rules.iter().enumerate() {
            for (field, value) in [
                ("src", &rule.src),
                ("dst", &rule.dst),
                ("target", &rule.target),
            ] {
                let Some(v) = value else {
                    continue;
                };
                let location = format!("nodes.{node_name}.nat.rules[{i}].{field}");
                if check_unresolved(v, location.clone(), issues) {
                    continue;
                }
                if !is_ip_or_cidr(v) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "invalid-nat-cidr",
                        message: format!("NAT {field} '{v}' is not an IP address or CIDR"),
                        location: Some(location),
                    });
                }
            }
        }
    }
}

fn check_route_dest(dest: &str, location: String, issues: &mut Vec<ValidationIssue>) {
    if check_unresolved(dest, location.clone(), issues) {
        return;
    }
    if dest != "default" && !is_ip_or_cidr(dest) {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "invalid-route-dest",
            message: format!("route destination '{dest}' is not 'default', an IP or a CIDR"),
            location: Some(location),
        });
    }
}

fn check_gateway(via: &str, location: String, issues: &mut Vec<ValidationIssue>) {
    if check_unresolved(via, location.clone(), issues) {
        return;
    }
    if via.parse::<IpAddr>().is_err() {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "invalid-route-dest",
            message: format!("route gateway '{via}' is not an IP address"),
            location: Some(location),
        });
    }
}

/// Visit every assertion — top-level and inside scenario `validate` steps.
fn for_each_assertion<'a>(topology: &'a Topology, mut f: impl FnMut(&'a Assertion, String)) {
    for (i, a) in topology.assertions.iter().enumerate() {
        f(a, format!("assertions[{i}]"));
    }
    for (s, scenario) in topology.scenarios.iter().enumerate() {
        for (t, step) in scenario.steps.iter().enumerate() {
            for (k, action) in step.actions.iter().enumerate() {
                if let ScenarioAction::Validate(list) = action {
                    for (i, a) in list.iter().enumerate() {
                        f(
                            a,
                            format!("scenarios[{s}].steps[{t}].actions[{k}].validate[{i}]"),
                        );
                    }
                }
            }
        }
    }
}

/// Route destinations must be `default`, an IP or a CIDR; gateways an IP.
fn validate_route_addresses(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in sorted(&topology.nodes) {
        for (dest, route) in sorted(&node.routes) {
            let location = format!("nodes.{node_name}.routes.{dest}");
            check_route_dest(dest, location.clone(), issues);
            if let Some(via) = &route.via {
                check_gateway(via, format!("{location}.via"), issues);
            }
        }
        for (vrf_name, vrf) in sorted(&node.vrfs) {
            for (dest, route) in sorted(&vrf.routes) {
                let location = format!("nodes.{node_name}.vrfs.{vrf_name}.routes.{dest}");
                check_route_dest(dest, location.clone(), issues);
                if let Some(via) = &route.via {
                    check_gateway(via, format!("{location}.via"), issues);
                }
            }
        }
    }

    for_each_assertion(topology, |assertion, location| {
        if let Assertion::RouteHas {
            destination, via, ..
        } = assertion
        {
            check_route_dest(destination, format!("{location}.destination"), issues);
            if let Some(via) = via {
                check_gateway(via, format!("{location}.via"), issues);
            }
        }
    });
}

/// The management subnet must be IPv4, sized for every node plus the
/// bridge (`.1`), and given as a network address.
fn validate_mgmt_subnet(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    let Some(mgmt) = &topology.lab.mgmt_subnet else {
        return;
    };
    let location = "lab.mgmt".to_string();
    if check_unresolved(mgmt, location.clone(), issues) {
        return;
    }
    let (ip, prefix) = match parse_cidr(mgmt) {
        Ok(parsed) => parsed,
        Err(e) => {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                rule: "valid-cidr",
                message: format!("invalid CIDR '{mgmt}': {e}"),
                location: Some(location),
            });
            return;
        }
    };
    if ip.is_ipv6() {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "mgmt-ipv6-unsupported",
            message: format!("management subnet '{mgmt}' is IPv6; only IPv4 is supported"),
            location: Some(location),
        });
        return;
    }

    let node_count = topology.nodes.len() as u64;
    let needed = node_count + 1;
    let usable = if prefix >= 31 {
        0
    } else {
        (1u64 << (32 - u32::from(prefix))) - 2
    };
    if usable < needed {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "mgmt-subnet-capacity",
            message: format!(
                "management subnet '{mgmt}' has {usable} usable host addresses but {needed} \
                 are needed ({node_count} nodes + bridge)"
            ),
            location: Some(location.clone()),
        });
    }

    let network = network_address(ip, prefix);
    if network != ip {
        issues.push(ValidationIssue {
            severity: Severity::Warning,
            rule: "mgmt-subnet-not-network-address",
            message: format!(
                "management subnet '{mgmt}' is not a network address (expected {network}/{prefix})"
            ),
            location: Some(location),
        });
    }
}

/// VXLAN interfaces need a VNI in 1..=16777215.
fn validate_vxlan_vni(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in sorted(&topology.nodes) {
        for (iface_name, iface) in sorted(&node.interfaces) {
            if iface.kind != Some(InterfaceKind::Vxlan) {
                continue;
            }
            let location = Some(format!("nodes.{node_name}.interfaces.{iface_name}.vni"));
            match iface.vni {
                None => issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "vxlan-vni-range",
                    message: format!("VXLAN interface '{iface_name}' has no VNI"),
                    location,
                }),
                Some(vni) if !(1..=16_777_215).contains(&vni) => issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "vxlan-vni-range",
                    message: format!("VNI {vni} out of range (1-16777215)"),
                    location,
                }),
                Some(_) => {}
            }
        }
    }
}

/// Channels the deployer can map to a frequency (mirrors
/// `deploy::freq_from_channel`): 2.4 GHz 1-14 and 5 GHz 36/40/44/48.
fn is_supported_wifi_channel(channel: u32) -> bool {
    (1..=14).contains(&channel) || matches!(channel, 36 | 40 | 44 | 48)
}

fn validate_wifi_channels(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in sorted(&topology.nodes) {
        for (i, wifi) in node.wifi.iter().enumerate() {
            if let Some(channel) = wifi.channel
                && !is_supported_wifi_channel(channel)
            {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "wifi-channel-range",
                    message: format!(
                        "Wi-Fi channel {channel} on '{}' is unsupported (use 1-14 or 36/40/44/48)",
                        wifi.name
                    ),
                    location: Some(format!("nodes.{node_name}.wifi[{i}].channel")),
                });
            }
        }
    }
}

/// macvlan / ipvlan interfaces need a valid host parent interface.
fn validate_macvlan_parents(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    for (node_name, node) in sorted(&topology.nodes) {
        let attached = node
            .macvlans
            .iter()
            .enumerate()
            .map(|(i, mv)| ("macvlan", "macvlans", i, &mv.name, &mv.parent))
            .chain(
                node.ipvlans
                    .iter()
                    .enumerate()
                    .map(|(i, iv)| ("ipvlan", "ipvlans", i, &iv.name, &iv.parent)),
            );
        for (kind, field, i, name, parent) in attached {
            let location = format!("nodes.{node_name}.{field}[{i}].parent");
            if check_unresolved(parent, location.clone(), issues) {
                continue;
            }
            let problem = if parent.is_empty() {
                Some("no parent interface set".to_string())
            } else {
                validate_interface_name(parent).err().map(|e| e.to_string())
            };
            if let Some(problem) = problem {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    rule: "macvlan-parent-set",
                    message: format!("{kind} '{name}': {problem}"),
                    location: Some(location),
                });
            }
        }
    }
}

/// Every interface enslaved to a VRF must exist on that node.
fn validate_vrf_interfaces(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for (node_name, node) in sorted(&topology.nodes) {
        let node_ifaces = interfaces.get(node_name);
        for (vrf_name, vrf) in sorted(&node.vrfs) {
            for (i, iface) in vrf.interfaces.iter().enumerate() {
                let location = format!("nodes.{node_name}.vrfs.{vrf_name}.interfaces[{i}]");
                if check_unresolved(iface, location.clone(), issues) {
                    continue;
                }
                if !node_ifaces.is_some_and(|m| m.contains_key(iface)) {
                    issues.push(ValidationIssue {
                        severity: Severity::Error,
                        rule: "vrf-interface-exists",
                        message: format!(
                            "VRF '{vrf_name}' lists interface '{iface}' which does not exist \
                             on node '{node_name}'"
                        ),
                        location: Some(location),
                    });
                }
            }
        }
    }
}

fn check_node_ref(
    topology: &Topology,
    name: &str,
    location: String,
    issues: &mut Vec<ValidationIssue>,
) {
    if check_unresolved(name, location.clone(), issues) {
        return;
    }
    if !topology.nodes.contains_key(name) {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "assertion-node-exists",
            message: format!("node '{name}' does not exist"),
            location: Some(location),
        });
    }
}

fn check_endpoint_ref(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
    endpoint: &str,
    location: String,
    issues: &mut Vec<ValidationIssue>,
) {
    if check_unresolved(endpoint, location.clone(), issues) {
        return;
    }
    let Some(ep) = EndpointRef::parse(endpoint) else {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "endpoint-format",
            message: format!("invalid endpoint '{endpoint}': expected 'node:interface' format"),
            location: Some(location),
        });
        return;
    };
    if !topology.nodes.contains_key(&ep.node) {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "assertion-node-exists",
            message: format!("node '{}' does not exist", ep.node),
            location: Some(location),
        });
        return;
    }
    // `lo` always exists; `mgmt0` exists whenever a management network is declared.
    let implicit = ep.iface == "lo" || (ep.iface == "mgmt0" && topology.lab.mgmt_subnet.is_some());
    let declared = interfaces
        .get(&ep.node)
        .is_some_and(|m| m.contains_key(&ep.iface));
    if !implicit && !declared {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "assertion-endpoint-exists",
            message: format!("node '{}' has no interface '{}'", ep.node, ep.iface),
            location: Some(location),
        });
    }
}

fn assertion_node_refs(assertion: &Assertion) -> Vec<(&'static str, &str)> {
    match assertion {
        Assertion::Reach { from, to }
        | Assertion::NoReach { from, to }
        | Assertion::TcpConnect { from, to, .. }
        | Assertion::LatencyUnder { from, to, .. } => vec![("from", from), ("to", to)],
        Assertion::RouteHas { node, .. } => vec![("node", node)],
        Assertion::DnsResolves { from, .. } => vec![("from", from)],
    }
}

/// Nodes and endpoints named by assertions, scenarios and benchmarks must exist.
fn validate_test_refs(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
    issues: &mut Vec<ValidationIssue>,
) {
    for_each_assertion(topology, |assertion, location| {
        for (field, name) in assertion_node_refs(assertion) {
            check_node_ref(topology, name, format!("{location}.{field}"), issues);
        }
    });

    for (s, scenario) in topology.scenarios.iter().enumerate() {
        for (t, step) in scenario.steps.iter().enumerate() {
            for (k, action) in step.actions.iter().enumerate() {
                let location = format!("scenarios[{s}].steps[{t}].actions[{k}]");
                match action {
                    ScenarioAction::Down(ep)
                    | ScenarioAction::Up(ep)
                    | ScenarioAction::Clear(ep) => {
                        check_endpoint_ref(topology, interfaces, ep, location, issues);
                    }
                    ScenarioAction::Exec { node, .. } => {
                        check_node_ref(topology, node, format!("{location}.node"), issues);
                    }
                    ScenarioAction::Validate(_) | ScenarioAction::Log(_) => {}
                }
            }
        }
    }

    for (b, bench) in topology.benchmarks.iter().enumerate() {
        for (t, test) in bench.tests.iter().enumerate() {
            let (BenchmarkTest::Iperf3 { from, to, .. } | BenchmarkTest::Ping { from, to, .. }) =
                test;
            let location = format!("benchmarks[{b}].tests[{t}]");
            check_node_ref(topology, from, format!("{location}.from"), issues);
            check_node_ref(topology, to, format!("{location}.to"), issues);
        }
    }
}

// ─────────────────────────────────────────────────────
// Warning-level rules
// ─────────────────────────────────────────────────────

/// No duplicate IP addresses across the topology.
fn validate_unique_ips(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    // Collect all (ip, location) pairs
    let mut seen: BTreeMap<String, String> = BTreeMap::new(); // ip_str -> location

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

/// Connected interfaces should have matching MTUs.
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

/// Route gateways should be reachable from a connected subnet.
fn validate_route_reachability(
    topology: &Topology,
    _interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
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

/// Nodes with no links or network connections are likely a mistake.
fn validate_unreferenced_nodes(
    topology: &Topology,
    interfaces: &BTreeMap<String, BTreeMap<String, InterfaceSource>>,
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

/// Exec commands should not be empty.
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
            // Container-only properties require image.
            // Note: healthcheck, startup-delay, and depends-on work on
            // namespace nodes too (for integration testing orchestration).
            let container_checks: &[(&str, bool)] = &[
                ("entrypoint", node.entrypoint.is_some()),
                ("hostname", node.hostname.is_some()),
                ("workdir", node.workdir.is_some()),
                ("privileged", node.privileged),
                ("pull", node.pull.is_some()),
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

    // Validate depends-on cycle detection (Kahn's algorithm)
    validate_depends_on_cycle(topology, issues);
}

/// Detect cycles in depends_on using Kahn's algorithm (BFS-based topological sort).
fn validate_depends_on_cycle(topology: &Topology, issues: &mut Vec<ValidationIssue>) {
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for (name, node) in &topology.nodes {
        in_degree.entry(name.as_str()).or_insert(0);
        for dep in &node.depends_on {
            // A dependency on a nonexistent node is reported by
            // `depends-on-exists`; it must not masquerade as a cycle.
            if !topology.nodes.contains_key(dep) {
                continue;
            }
            adj.entry(dep.as_str()).or_default().push(name.as_str());
            *in_degree.entry(name.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut visited = 0usize;

    while let Some(n) = queue.pop() {
        visited += 1;
        if let Some(dependents) = adj.get(n) {
            for dep in dependents {
                if let Some(d) = in_degree.get_mut(dep) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push(dep);
                    }
                }
            }
        }
    }

    if visited < topology.nodes.len() {
        // Find the nodes in the cycle (those with in_degree > 0)
        let mut cycle_nodes: Vec<&str> = in_degree
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(n, _)| *n)
            .collect();
        cycle_nodes.sort_unstable();
        issues.push(ValidationIssue {
            severity: Severity::Error,
            rule: "depends-on-cycle",
            message: format!(
                "depends-on cycle detected involving nodes: {}",
                cycle_nodes.join(", ")
            ),
            location: None,
        });
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
        node.profiles = vec!["nonexistent".into()];
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
    fn test_network_impair_self_pair_rejected() {
        let result = parse_and_validate(
            r#"lab "self-pair"
node a
node b
network lan {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- a { delay 10ms }
}
"#,
        );
        assert!(result.has_errors());
        assert!(
            result
                .errors()
                .any(|e| e.rule == "network-impair-self-pair")
        );
    }

    #[test]
    fn test_network_impair_non_member_rejected() {
        let result = parse_and_validate(
            r#"lab "nonmember"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- c { delay 10ms }
}
"#,
        );
        assert!(result.has_errors());
        assert!(result.errors().any(|e| e.rule == "network-impair-member"));
    }

    #[test]
    fn test_network_impair_needs_subnet() {
        let result = parse_and_validate(
            r#"lab "no-subnet"
node a
node b
network lan {
  members [a:eth0, b:eth0]
  impair a -- b { delay 10ms }
}
"#,
        );
        assert!(result.has_errors());
        assert!(
            result
                .errors()
                .any(|e| e.rule == "network-impair-needs-subnet")
        );
    }

    #[test]
    fn test_network_impair_valid_passes() {
        let result = parse_and_validate(
            r#"lab "ok"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 15ms loss 1% }
  impair a -- c { delay 40ms rate-cap 100mbit }
}
"#,
        );
        // No errors related to network impair
        let related: Vec<_> = result
            .errors()
            .filter(|e| {
                matches!(
                    e.rule,
                    "network-impair-self-pair"
                        | "network-impair-member"
                        | "network-impair-needs-subnet"
                )
            })
            .collect();
        assert!(related.is_empty(), "unexpected errors: {related:?}");
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

    // ─── Rule registry ────────────────────────────────

    /// Every rule that is ever emitted at `Severity::Warning` must be in
    /// `WARNING_RULE_IDS`, and nothing else may be.
    #[test]
    fn warning_rules_are_listed() {
        let src = include_str!("validator.rs");
        // A warning emission is a struct literal whose severity field
        // names the Warning variant, with the rule id field nearby.
        let mut warned = std::collections::BTreeSet::new();
        for (i, _) in src.match_indices("severity: Severity::Warning") {
            let tail = &src[i..src.len().min(i + 400)];
            if let Some(j) = tail.find("rule: \"") {
                let rest = &tail[j + 7..];
                let id: String = rest.chars().take_while(|c| *c != '"').collect();
                warned.insert(id);
            }
        }
        let listed: std::collections::BTreeSet<String> =
            WARNING_RULE_IDS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            warned, listed,
            "WARNING_RULE_IDS drifted from the emitted warnings"
        );
        for id in RULE_IDS {
            assert!(rule_severity(id).is_some());
        }
        assert_eq!(rule_severity("no-such-rule"), None);
    }

    #[test]
    fn rule_options_promote_and_silence_warnings() {
        let t = crate::parser::parse("lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\nnode lonely\n").unwrap();
        let base = t.validate();
        assert!(base.warnings().any(|w| w.rule == "unreferenced-node"));
        assert!(!base.has_errors());
        let strict = t.validate_with(&RuleOptions {
            strict: true,
            ..Default::default()
        });
        assert!(strict.has_errors());
        let denied = t.validate_with(&RuleOptions {
            deny: vec!["unreferenced-node".into()],
            ..Default::default()
        });
        assert!(denied.errors().any(|e| e.rule == "unreferenced-node"));
        let allowed = t.validate_with(&RuleOptions {
            allow: vec!["unreferenced-node".into()],
            ..Default::default()
        });
        assert!(
            !allowed
                .issues()
                .iter()
                .any(|i| i.rule == "unreferenced-node")
        );
        assert!(
            RuleOptions {
                deny: vec!["bogus".into()],
                ..Default::default()
            }
            .check_known()
            .is_err()
        );
    }

    #[test]
    fn rule_ids_are_stable() {
        use std::collections::BTreeSet;

        // Every rule literal in this file must be listed in RULE_IDS
        // and vice versa, so neither can drift without touching the other.
        let src = include_str!("validator.rs");
        let mut emitted = BTreeSet::new();
        for (idx, _) in src.match_indices("rule: \"") {
            let rest = &src[idx + "rule: \"".len()..];
            let end = rest.find('"').expect("unterminated rule literal");
            emitted.insert(&rest[..end]);
        }
        let listed: BTreeSet<&str> = RULE_IDS.iter().copied().collect();
        assert_eq!(
            listed.len(),
            RULE_IDS.len(),
            "RULE_IDS contains a duplicate id"
        );
        assert_eq!(
            emitted, listed,
            "RULE_IDS drifted from the rule literals emitted in validator.rs"
        );
        for id in RULE_IDS {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "rule id '{id}' is not kebab-case"
            );
        }
        assert_eq!(rule_ids(), RULE_IDS);
    }

    // ─── Test-block references ────────────────────────

    fn rules_of<'a>(result: &'a ValidationResult, rule: &'a str) -> Vec<&'a ValidationIssue> {
        result.issues().iter().filter(|i| i.rule == rule).collect()
    }

    #[test]
    fn test_assertion_node_missing() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
validate { reach a ghost }
"#,
        );
        let hits = rules_of(&result, "assertion-node-exists");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(hits[0].location.as_deref(), Some("assertions[0].to"));
    }

    #[test]
    fn test_scenario_endpoint_missing() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
scenario "s" {
  at 1s { down a:eth9 }
  at 2s { up ghost:eth0 }
  at 3s { validate { no-reach a ghost } }
}
"#,
        );
        let ep = rules_of(&result, "assertion-endpoint-exists");
        assert_eq!(ep.len(), 1, "{:?}", result.issues());
        assert_eq!(
            ep[0].location.as_deref(),
            Some("scenarios[0].steps[0].actions[0]")
        );
        let nodes = rules_of(&result, "assertion-node-exists");
        assert_eq!(nodes.len(), 2, "{:?}", result.issues());
    }

    #[test]
    fn test_benchmark_node_missing() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
benchmark "perf" {
  ping a ghost { count 3 }
}
"#,
        );
        let hits = rules_of(&result, "assertion-node-exists");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("benchmarks[0].tests[0].to")
        );
    }

    #[test]
    fn test_test_refs_valid_pass() {
        let result = parse_and_validate(
            r#"lab "t" { mgmt 172.20.0.0/24 }
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
validate { reach a b }
scenario "s" {
  at 1s { down a:eth0 }
  at 2s { up a:eth0 clear b:eth0 down a:mgmt0 }
  at 3s { validate { reach a b } }
}
benchmark "perf" {
  ping a b { count 3 }
}
"#,
        );
        assert!(
            !result.has_errors(),
            "unexpected errors: {:?}",
            result.issues()
        );
    }

    #[test]
    fn test_unresolved_interpolation_rejected() {
        let mut topo = crate::Lab::new("t")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .build();
        topo.assertions.push(Assertion::Reach {
            from: "${x}".into(),
            to: "b".into(),
        });
        topo.scenarios.push(crate::types::Scenario {
            name: "s".into(),
            steps: vec![crate::types::ScenarioStep {
                time_ms: 0,
                actions: vec![ScenarioAction::Down("a:eth${i}".into())],
            }],
        });
        let result = validate_topo(topo);
        let hits = rules_of(&result, "unresolved-interpolation");
        assert_eq!(hits.len(), 2, "{:?}", result.issues());
        // The unresolved names must not also be reported as missing.
        assert!(rules_of(&result, "assertion-node-exists").is_empty());
        assert!(rules_of(&result, "assertion-endpoint-exists").is_empty());
    }

    // ─── Subnets ──────────────────────────────────────

    #[test]
    fn test_overlapping_subnets_nested() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
node c
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link b:eth1 -- c:eth0 { 10.0.0.129/25 -- 10.0.0.130/25 }
"#,
        );
        let hits = rules_of(&result, "overlapping-subnets");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert!(hits[0].message.contains("10.0.0.128/25"));
        assert!(hits[0].message.contains("10.0.0.0/24"));
    }

    #[test]
    fn test_overlapping_subnets_identical_links() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
node c
node d
link a:eth0 -- b:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
link c:eth0 -- d:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
"#,
        );
        assert!(
            !rules_of(&result, "overlapping-subnets").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    #[test]
    fn test_overlapping_subnets_mgmt_vs_link() {
        let result = parse_and_validate(
            r#"lab "t" { mgmt 10.0.0.0/16 }
node a
node b
link a:eth0 -- b:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
"#,
        );
        let hits = rules_of(&result, "overlapping-subnets");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert!(hits[0].message.contains("lab.mgmt"));
    }

    #[test]
    fn test_overlapping_subnets_allowed_cases() {
        // Same-link endpoints, distinct /30s from a pool, a network's own
        // ports, and Wi-Fi/WireGuard interfaces sharing one segment subnet
        // across nodes are all fine.
        let result = parse_and_validate(
            r#"lab "t"
pool fabric 10.0.0.0/16 /30
node a
node b
node c
node d {
  wifi wlan0 mode ap { ssid "x" 10.9.0.1/24 }
  wireguard wg0 { key auto address 10.8.0.1/24 peers [e] }
}
node e {
  wifi wlan0 mode station { ssid "x" 10.9.0.2/24 }
  wireguard wg0 { key auto address 10.8.0.2/24 peers [d] }
}
link a:eth0 -- b:eth0 { pool fabric }
link b:eth1 -- c:eth0 { pool fabric }
network lan {
  members [a:eth1, c:eth1]
  subnet 10.1.0.0/24
}
"#,
        );
        assert!(
            rules_of(&result, "overlapping-subnets").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    #[test]
    fn test_link_endpoints_different_subnets() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 192.168.1.2/24 }
"#,
        );
        let hits = rules_of(&result, "link-endpoints-same-subnet");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(hits[0].location.as_deref(), Some("links[0].addresses"));
    }

    #[test]
    fn test_link_endpoints_same_subnet_passes() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(rules_of(&result, "link-endpoints-same-subnet").is_empty());
    }

    // ─── Impairment values ────────────────────────────

    #[test]
    fn test_invalid_impairment_values() {
        let mut topo = crate::Lab::new("t")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        topo.impairments.insert(
            "a:eth0".into(),
            Impairment {
                delay: Some("soon".into()),
                loss: Some("150%".into()),
                rate: Some("100mbit".into()),
                ..Default::default()
            },
        );
        topo.rate_limits.insert(
            "b:eth0".into(),
            crate::types::RateLimit {
                egress: Some("fast".into()),
                ingress: Some("10mbit".into()),
                burst: None,
            },
        );
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-impairment-value");
        let locations: Vec<_> = hits.iter().filter_map(|h| h.location.as_deref()).collect();
        assert_eq!(hits.len(), 3, "{:?}", result.issues());
        assert!(locations.contains(&"impairments.\"a:eth0\".delay"));
        assert!(locations.contains(&"impairments.\"a:eth0\".loss"));
        assert!(locations.contains(&"rate_limits.\"b:eth0\".egress"));
    }

    #[test]
    fn test_network_impairment_rate_cap_invalid() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        topo.nodes.insert("a".into(), Default::default());
        topo.nodes.insert("b".into(), Default::default());
        topo.networks.insert(
            "lan".into(),
            crate::types::Network {
                members: vec!["a:eth0".into(), "b:eth0".into()],
                subnet: Some("10.0.0.0/24".into()),
                impairments: vec![crate::types::NetworkImpairment {
                    src: "a".into(),
                    dst: "b".into(),
                    impairment: Impairment {
                        delay: Some("10ms".into()),
                        ..Default::default()
                    },
                    rate_cap: Some("lots".into()),
                }],
                ..Default::default()
            },
        );
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-impairment-value");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("networks.lan.impairments[0].rate_cap")
        );
    }

    #[test]
    fn test_valid_impairment_values_pass() {
        let result = parse_and_validate(
            r#"lab "t"
node a
node b
node c
link a:eth0 -- b:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms loss 0.1% rate 10mbit
}
network lan {
  members [b:eth1, c:eth0]
  subnet 10.1.0.0/24
  impair b -- c { delay 15ms loss 1% rate-cap 100mbit }
}
rate a:eth0 egress 100mbit ingress 50mbit
"#,
        );
        assert!(
            rules_of(&result, "invalid-impairment-value").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    // ─── NAT and routes ───────────────────────────────

    #[test]
    fn test_invalid_nat_cidr() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut node = crate::types::Node::default();
        node.nat = Some(crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Dnat,
                src: Some("everyone".into()),
                dst: Some("203.0.113.0/24".into()),
                target: Some("10.0.1.2".into()),
                target_port: None,
            }],
        });
        topo.nodes.insert("fw".into(), node);
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-nat-cidr");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("nodes.fw.nat.rules[0].src")
        );
    }

    #[test]
    fn test_valid_nat_passes() {
        let result = parse_and_validate(
            r#"lab "t"
node fw {
  nat {
    masquerade src 10.0.0.0/16
    dnat dst 203.0.113.0/24 to 10.0.1.2
    translate 144.0.0.0/8 to 172.100.0.0/16
  }
}
node h
link fw:eth0 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(
            rules_of(&result, "invalid-nat-cidr").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    #[test]
    fn test_invalid_route_dest_and_gateway() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut node = crate::types::Node::default();
        node.routes.insert(
            "everywhere".into(),
            crate::types::RouteConfig {
                via: Some("10.0.0.1".into()),
                ..Default::default()
            },
        );
        node.routes.insert(
            "default".into(),
            crate::types::RouteConfig {
                via: Some("gateway".into()),
                ..Default::default()
            },
        );
        node.vrfs.insert(
            "red".into(),
            crate::types::VrfConfig {
                table: 10,
                interfaces: vec![],
                routes: [(
                    "10.0.0.0/8".to_string(),
                    crate::types::RouteConfig {
                        via: Some("nowhere".into()),
                        ..Default::default()
                    },
                )]
                .into(),
            },
        );
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-route-dest");
        let locations: Vec<_> = hits.iter().filter_map(|h| h.location.as_deref()).collect();
        assert_eq!(hits.len(), 3, "{:?}", result.issues());
        assert!(locations.contains(&"nodes.a.routes.everywhere"));
        assert!(locations.contains(&"nodes.a.routes.default.via"));
        assert!(locations.contains(&"nodes.a.vrfs.red.routes.10.0.0.0/8.via"));
    }

    #[test]
    fn test_unresolved_route_gateway() {
        // The lowerer leaves dangling cross-references in place; the
        // validator must refuse them.
        let result = parse_and_validate(
            r#"lab "t"
node a { route default via ${ghost.eth0} }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        let hits = rules_of(&result, "unresolved-interpolation");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("nodes.a.routes.default.via")
        );
    }

    #[test]
    fn test_valid_routes_pass() {
        let result = parse_and_validate(
            r#"lab "t"
node a {
  route default via 10.0.0.2
  route 10.1.0.0/16 dev eth0
  route 10.9.9.9/32 via 10.0.0.2
  vrf red table 10 {
    interfaces [eth0]
    route default dev eth0
  }
}
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
validate { route-has a 10.1.0.0/16 dev eth0 }
"#,
        );
        assert!(
            rules_of(&result, "invalid-route-dest").is_empty()
                && rules_of(&result, "unresolved-interpolation").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    // ─── Names ────────────────────────────────────────

    #[test]
    fn test_invalid_names() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "/etc/x".into();
        topo.lab.prefix = Some("-dash".into());
        topo.nodes.insert("bad node".into(), Default::default());
        topo.profiles.insert("p/q".into(), Default::default());
        topo.networks.insert("".into(), Default::default());
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-name");
        let locations: Vec<_> = hits.iter().filter_map(|h| h.location.as_deref()).collect();
        assert_eq!(hits.len(), 5, "{:?}", result.issues());
        assert!(locations.contains(&"lab.name"));
        assert!(locations.contains(&"lab.prefix"));
        assert!(locations.contains(&"nodes.bad node"));
        assert!(locations.contains(&"profiles.p/q"));
        assert!(locations.contains(&"networks."));
    }

    #[test]
    fn test_invalid_interface_name() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        topo.nodes.insert("a".into(), Default::default());
        topo.nodes.insert("b".into(), Default::default());
        topo.links.push(crate::types::Link {
            endpoints: ["a:eth/0".into(), "b:eth0".into()],
            addresses: None,
            mtu: None,
        });
        let result = validate_topo(topo);
        let hits = rules_of(&result, "invalid-name");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(hits[0].location.as_deref(), Some("nodes.a.eth/0"));
    }

    #[test]
    fn test_valid_names_pass() {
        let result = parse_and_validate(
            r#"lab "site-demo_v2.1"
profile _router { forward ipv4 }
node dc1-router : _router
node dc1.server
link dc1-router:eth0.100 -- dc1.server:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(
            rules_of(&result, "invalid-name").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    // ─── Management subnet ────────────────────────────

    #[test]
    fn test_mgmt_subnet_capacity() {
        let result = parse_and_validate(
            r#"lab "t" { mgmt 172.20.0.0/30 }
node a
node b
node c
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link b:eth1 -- c:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
"#,
        );
        let hits = rules_of(&result, "mgmt-subnet-capacity");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(hits[0].location.as_deref(), Some("lab.mgmt"));
        assert!(hits[0].message.contains("2 usable"));
        assert!(hits[0].message.contains("4 are needed"));
    }

    #[test]
    fn test_mgmt_subnet_ipv6_unsupported() {
        let mut topo = crate::Lab::new("t")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .build();
        topo.lab.mgmt_subnet = Some("fd00:20::/64".into());
        let result = validate_topo(topo);
        assert_eq!(rules_of(&result, "mgmt-ipv6-unsupported").len(), 1);
        assert!(rules_of(&result, "mgmt-subnet-capacity").is_empty());
    }

    #[test]
    fn test_mgmt_subnet_not_network_address_warns() {
        let result = parse_and_validate(
            r#"lab "t" { mgmt 172.20.0.5/24 }
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(!result.has_errors(), "{:?}", result.issues());
        let hits = rules_of(&result, "mgmt-subnet-not-network-address");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].severity, Severity::Warning);
        assert!(hits[0].message.contains("172.20.0.0/24"));
    }

    #[test]
    fn test_mgmt_subnet_valid_passes() {
        let result = parse_and_validate(
            r#"lab "t" { mgmt 172.20.0.0/24 host-reachable }
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(!result.has_errors(), "{:?}", result.issues());
        assert!(!result.has_warnings(), "{:?}", result.issues());
    }

    // ─── VXLAN / Wi-Fi / macvlan / VRF ────────────────

    fn vxlan_topo(vni: Option<u32>) -> crate::types::Topology {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut node = crate::types::Node::default();
        node.interfaces.insert(
            "vxlan100".into(),
            crate::types::InterfaceConfig {
                kind: Some(InterfaceKind::Vxlan),
                vni,
                ..Default::default()
            },
        );
        topo.nodes.insert("vtep".into(), node);
        topo
    }

    #[test]
    fn test_vxlan_vni_range() {
        for bad in [Some(0), Some(16_777_216), None] {
            let result = validate_topo(vxlan_topo(bad));
            let hits = rules_of(&result, "vxlan-vni-range");
            assert_eq!(hits.len(), 1, "vni {bad:?}: {:?}", result.issues());
            assert_eq!(
                hits[0].location.as_deref(),
                Some("nodes.vtep.interfaces.vxlan100.vni")
            );
        }
        for ok in [1, 100, 16_777_215] {
            let result = validate_topo(vxlan_topo(Some(ok)));
            assert!(rules_of(&result, "vxlan-vni-range").is_empty());
        }
    }

    #[test]
    fn test_wifi_channel_range() {
        let result = parse_and_validate(
            r#"lab "t"
node ap {
  wifi wlan0 mode ap { ssid "x" channel 15 10.0.0.1/24 }
}
node sta {
  wifi wlan0 mode station { ssid "x" channel 36 10.0.0.2/24 }
}
"#,
        );
        let hits = rules_of(&result, "wifi-channel-range");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("nodes.ap.wifi[0].channel")
        );
    }

    #[test]
    fn test_macvlan_parent_set() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut node = crate::types::Node::default();
        node.macvlans.push(crate::types::MacvlanConfig {
            name: "mv0".into(),
            parent: String::new(),
            mode: Default::default(),
            addresses: vec!["192.168.1.10/24".into()],
        });
        node.ipvlans.push(crate::types::IpvlanConfig {
            name: "iv0".into(),
            parent: "this-parent-name-is-too-long".into(),
            mode: Default::default(),
            addresses: vec![],
        });
        topo.nodes.insert("gw".into(), node);
        let result = validate_topo(topo);
        let hits = rules_of(&result, "macvlan-parent-set");
        let locations: Vec<_> = hits.iter().filter_map(|h| h.location.as_deref()).collect();
        assert_eq!(hits.len(), 2, "{:?}", result.issues());
        assert!(locations.contains(&"nodes.gw.macvlans[0].parent"));
        assert!(locations.contains(&"nodes.gw.ipvlans[0].parent"));
    }

    #[test]
    fn test_macvlan_parent_valid_passes() {
        let result = parse_and_validate(
            r#"lab "t"
node gw {
  macvlan eth0 parent "enp3s0" mode bridge { 192.168.1.100/24 }
}
node h
link gw:veth0 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert!(
            rules_of(&result, "macvlan-parent-set").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    #[test]
    fn test_vrf_interface_exists() {
        let result = parse_and_validate(
            r#"lab "t"
node pe {
  vrf red table 10 { interfaces [eth1, eth9] }
}
node h
link pe:eth1 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        let hits = rules_of(&result, "vrf-interface-exists");
        assert_eq!(hits.len(), 1, "{:?}", result.issues());
        assert_eq!(
            hits[0].location.as_deref(),
            Some("nodes.pe.vrfs.red.interfaces[1]")
        );
        assert!(hits[0].message.contains("eth9"));
    }

    // ─── depends-on ───────────────────────────────────

    #[test]
    fn test_depends_on_missing_is_not_a_cycle() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut node = crate::types::Node::default();
        node.depends_on = vec!["ghost".into()];
        topo.nodes.insert("a".into(), node);
        let result = validate_topo(topo);
        assert_eq!(rules_of(&result, "depends-on-exists").len(), 1);
        assert!(
            rules_of(&result, "depends-on-cycle").is_empty(),
            "{:?}",
            result.issues()
        );
    }

    #[test]
    fn test_depends_on_cycle_detected() {
        let mut topo = crate::types::Topology::default();
        topo.lab.name = "t".into();
        let mut a = crate::types::Node::default();
        a.depends_on = vec!["b".into()];
        let mut b = crate::types::Node::default();
        b.depends_on = vec!["a".into()];
        topo.nodes.insert("a".into(), a);
        topo.nodes.insert("b".into(), b);
        let result = validate_topo(topo);
        let hits = rules_of(&result, "depends-on-cycle");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].message.contains("a, b"));
        assert!(rules_of(&result, "depends-on-exists").is_empty());
    }
}
