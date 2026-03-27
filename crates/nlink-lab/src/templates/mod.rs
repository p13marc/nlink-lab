//! Built-in topology templates for `nlink-lab init`.
//!
//! Templates are ready-to-deploy topology files that demonstrate common
//! network patterns. They serve as starting points for custom topologies.
//!
//! # Usage
//!
//! ```
//! use nlink_lab::templates;
//!
//! // List all templates
//! for t in templates::list() {
//!     println!("{:<15} {}", t.name, t.description);
//! }
//!
//! // Get a specific template
//! let t = templates::get("router").unwrap();
//! let (toml, nll) = templates::render(t, Some("my-lab"));
//! ```

/// A built-in topology template.
pub struct Template {
    /// Short name used on CLI.
    pub name: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of links.
    pub link_count: usize,
    /// Key features demonstrated.
    pub features: &'static [&'static str],
    /// TOML content.
    pub toml: &'static str,
    /// NLL content.
    pub nll: &'static str,
}

const TEMPLATES: &[Template] = &[
    Template {
        name: "simple",
        description: "Two nodes with one link and netem impairment",
        node_count: 2,
        link_count: 1,
        features: &["veth", "addresses", "routes", "netem"],
        toml: include_str!("../../../../examples/simple.toml"),
        nll: include_str!("../../../../examples/simple.nll"),
    },
    Template {
        name: "router",
        description: "Router between two subnets with IP forwarding",
        node_count: 3,
        link_count: 2,
        features: &["profiles", "ip-forwarding", "default-routes"],
        toml: include_str!("../../../../examples/router.toml"),
        nll: include_str!("../../../../examples/router.nll"),
    },
    Template {
        name: "spine-leaf",
        description: "Datacenter fabric: 2 spines, 2 leaves, 2 servers",
        node_count: 6,
        link_count: 8,
        features: &["profiles", "loopback", "multi-hop", "netem"],
        toml: include_str!("../../../../examples/spine-leaf.toml"),
        nll: include_str!("../../../../examples/spine-leaf.nll"),
    },
    Template {
        name: "wan",
        description: "Two sites over impaired WAN link with rate limiting",
        node_count: 4,
        link_count: 3,
        features: &["delay", "loss", "rate-limiting", "jitter"],
        toml: include_str!("../../../../examples/wan-impairment.toml"),
        nll: include_str!("../../../../examples/wan-impairment.nll"),
    },
    Template {
        name: "firewall",
        description: "Server behind a stateful nftables firewall",
        node_count: 3,
        link_count: 2,
        features: &["nftables", "conntrack", "policy", "rules"],
        toml: include_str!("../../../../examples/firewall.toml"),
        nll: include_str!("../../../../examples/firewall.nll"),
    },
    Template {
        name: "vlan-trunk",
        description: "Bridge with VLAN filtering, trunk and access ports",
        node_count: 4,
        link_count: 0,
        features: &["bridge", "vlan-filtering", "pvid", "tagged"],
        toml: include_str!("../../../../examples/vlan-trunk.toml"),
        nll: include_str!("../../../../examples/vlan-trunk.nll"),
    },
    Template {
        name: "vrf",
        description: "PE router with VRF tenant isolation",
        node_count: 3,
        link_count: 2,
        features: &["vrf", "routing-tables", "tenant-isolation"],
        toml: include_str!("../../../../examples/vrf-multitenant.toml"),
        nll: include_str!("../../../../examples/vrf-multitenant.nll"),
    },
    Template {
        name: "wireguard",
        description: "Site-to-site WireGuard VPN tunnel",
        node_count: 2,
        link_count: 1,
        features: &["wireguard", "encryption", "tunnel"],
        toml: include_str!("../../../../examples/wireguard-vpn.toml"),
        nll: include_str!("../../../../examples/wireguard-vpn.nll"),
    },
    Template {
        name: "vxlan",
        description: "VXLAN overlay between two VTEPs",
        node_count: 2,
        link_count: 1,
        features: &["vxlan", "overlay", "underlay"],
        toml: include_str!("../../../../examples/vxlan-overlay.toml"),
        nll: include_str!("../../../../examples/vxlan-overlay.nll"),
    },
    Template {
        name: "container",
        description: "Alpine container connected to a bare namespace host",
        node_count: 2,
        link_count: 1,
        features: &["container", "mixed-topology", "docker"],
        toml: include_str!("../../../../examples/container.toml"),
        nll: include_str!("../../../../examples/container.nll"),
    },
    Template {
        name: "mesh",
        description: "Full mesh of 4 nodes (6 links)",
        node_count: 4,
        link_count: 6,
        features: &["full-mesh", "point-to-point"],
        toml: include_str!("../../../../examples/mesh.toml"),
        nll: include_str!("../../../../examples/mesh.nll"),
    },
    Template {
        name: "iperf",
        description: "Throughput test with iperf3 and rate limiting",
        node_count: 2,
        link_count: 1,
        features: &["iperf3", "rate-limiting", "exec"],
        toml: include_str!("../../../../examples/iperf-benchmark.toml"),
        nll: include_str!("../../../../examples/iperf-benchmark.nll"),
    },
];

/// Get all available templates.
pub fn list() -> &'static [Template] {
    TEMPLATES
}

/// Find a template by name.
pub fn get(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// Render a template with an optional lab name override.
///
/// Returns `(toml_content, nll_content)` with the lab name replaced.
pub fn render(template: &Template, name: Option<&str>) -> (String, String) {
    match name {
        Some(new_name) => {
            let old_name = extract_lab_name(template.toml);
            let toml = template.toml.replacen(&old_name, new_name, 1);
            let nll = template.nll.replacen(&old_name, new_name, 1);
            (toml, nll)
        }
        None => (template.toml.to_string(), template.nll.to_string()),
    }
}

/// Extract the lab name from TOML content.
fn extract_lab_name(toml: &str) -> String {
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with("name") {
            if let Some(val) = line.split('=').nth(1) {
                return val.trim().trim_matches('"').to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_templates_parse_and_validate() {
        for template in list() {
            // Test TOML parsing
            let topo = crate::parser::parse(template.toml)
                .unwrap_or_else(|e| panic!("template '{}' TOML parse failed: {e}", template.name));
            let result = topo.validate();
            assert!(
                !result.has_errors(),
                "template '{}' TOML has validation errors: {:?}",
                template.name,
                result.errors().collect::<Vec<_>>()
            );

            // Test NLL parsing
            let topo_nll = crate::parser::nll::parse(template.nll)
                .unwrap_or_else(|e| panic!("template '{}' NLL parse failed: {e}", template.name));
            let result_nll = topo_nll.validate();
            assert!(
                !result_nll.has_errors(),
                "template '{}' NLL has validation errors: {:?}",
                template.name,
                result_nll.errors().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn render_replaces_name() {
        let t = get("simple").unwrap();
        let (toml, nll) = render(t, Some("my-custom-lab"));
        assert!(toml.contains("my-custom-lab"));
        assert!(nll.contains("my-custom-lab"));
        assert!(!toml.contains("name = \"simple\""));
    }

    #[test]
    fn get_nonexistent_returns_none() {
        assert!(get("nonexistent").is_none());
    }

    #[test]
    fn list_returns_all() {
        assert!(list().len() >= 12);
    }
}
