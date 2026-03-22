//! TOML topology parser.
//!
//! Parses topology files into [`Topology`] structs.
//!
//! # Example
//!
//! ```ignore
//! use nlink_lab::parser;
//!
//! let topology = parser::parse_file("datacenter.toml")?;
//! println!("Lab: {}", topology.lab.name);
//! println!("Nodes: {}", topology.nodes.len());
//! ```

use std::path::Path;

use crate::error::Result;
use crate::types::Topology;

/// Parse a TOML string into a topology.
///
/// # Example
///
/// ```ignore
/// use nlink_lab::parser;
///
/// let toml = r#"
/// [lab]
/// name = "test"
///
/// [nodes.a]
/// [nodes.b]
///
/// [[links]]
/// endpoints = ["a:eth0", "b:eth0"]
/// "#;
///
/// let topology = parser::parse(toml)?;
/// assert_eq!(topology.lab.name, "test");
/// ```
pub fn parse(toml_str: &str) -> Result<Topology> {
    let topology: Topology = toml::from_str(toml_str)?;
    Ok(topology)
}

/// Parse a TOML file into a topology.
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let contents = std::fs::read_to_string(path)?;
    parse(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let toml = r#"
[lab]
name = "minimal"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.lab.name, "minimal");
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.links.len(), 1);
    }

    #[test]
    fn test_parse_with_profiles() {
        let toml = r#"
[lab]
name = "profiles"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.r1]
profile = "router"

[nodes.r1.interfaces.lo]
addresses = ["10.0.0.1/32"]

[nodes.r1.routes]
default = { via = "10.0.1.1" }

[nodes.h1]
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.profiles.len(), 1);
        assert_eq!(topo.profiles["router"].sysctls["net.ipv4.ip_forward"], "1");
        assert_eq!(topo.nodes["r1"].profile.as_deref(), Some("router"));
        assert_eq!(topo.nodes["r1"].interfaces["lo"].addresses, vec!["10.0.0.1/32"]);
        assert_eq!(topo.nodes["r1"].routes["default"].via.as_deref(), Some("10.0.1.1"));
    }

    #[test]
    fn test_parse_with_impairments() {
        let toml = r#"
[lab]
name = "impair"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]

[impairments."a:eth0"]
delay = "10ms"
jitter = "2ms"
loss = "0.1%"
"#;
        let topo = parse(toml).unwrap();
        let imp = &topo.impairments["a:eth0"];
        assert_eq!(imp.delay.as_deref(), Some("10ms"));
        assert_eq!(imp.jitter.as_deref(), Some("2ms"));
        assert_eq!(imp.loss.as_deref(), Some("0.1%"));
    }

    #[test]
    fn test_parse_with_exec() {
        let toml = r#"
[lab]
name = "exec"

[nodes.server]

[[nodes.server.exec]]
cmd = ["iperf3", "-s"]
background = true
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.nodes["server"].exec.len(), 1);
        assert_eq!(topo.nodes["server"].exec[0].cmd, vec!["iperf3", "-s"]);
        assert!(topo.nodes["server"].exec[0].background);
    }

    #[test]
    fn test_parse_with_firewall() {
        let toml = r#"
[lab]
name = "fw"

[nodes.server]

[nodes.server.firewall]
policy = "drop"

[[nodes.server.firewall.rules]]
match = "ct state established,related"
action = "accept"

[[nodes.server.firewall.rules]]
match = "tcp dport 80"
action = "accept"
"#;
        let topo = parse(toml).unwrap();
        let fw = topo.nodes["server"].firewall.as_ref().unwrap();
        assert_eq!(fw.policy.as_deref(), Some("drop"));
        assert_eq!(fw.rules.len(), 2);
        assert_eq!(fw.rules[0].action.as_deref(), Some("accept"));
    }

    #[test]
    fn test_parse_with_rate_limits() {
        let toml = r#"
[lab]
name = "rate"

[nodes.a]
[nodes.b]

[[links]]
endpoints = ["a:eth0", "b:eth0"]

[rate_limits."a:eth0"]
egress = "100mbit"
ingress = "1gbit"
burst = "10mbit"
"#;
        let topo = parse(toml).unwrap();
        let rl = &topo.rate_limits["a:eth0"];
        assert_eq!(rl.egress.as_deref(), Some("100mbit"));
        assert_eq!(rl.ingress.as_deref(), Some("1gbit"));
        assert_eq!(rl.burst.as_deref(), Some("10mbit"));
    }

    #[test]
    fn test_parse_with_vrf() {
        let toml = r#"
[lab]
name = "vrf"

[nodes.pe]
profile = "router"

[nodes.pe.vrfs.red]
table = 100
interfaces = ["eth1"]
routes = { "0.0.0.0/0" = { via = "10.0.1.2" } }
"#;
        let topo = parse(toml).unwrap();
        let vrf = &topo.nodes["pe"].vrfs["red"];
        assert_eq!(vrf.table, 100);
        assert_eq!(vrf.interfaces, vec!["eth1"]);
        assert_eq!(vrf.routes["0.0.0.0/0"].via.as_deref(), Some("10.0.1.2"));
    }

    #[test]
    fn test_effective_sysctls_merging() {
        let toml = r#"
[lab]
name = "merge"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1", "net.ipv6.conf.all.forwarding" = "1" }

[nodes.r1]
profile = "router"
sysctls = { "net.ipv4.ip_forward" = "0" }
"#;
        let topo = parse(toml).unwrap();
        let sysctls = topo.effective_sysctls(&topo.nodes["r1"]);
        // Node-level overrides profile
        assert_eq!(sysctls["net.ipv4.ip_forward"], "0");
        // Profile value preserved
        assert_eq!(sysctls["net.ipv6.conf.all.forwarding"], "1");
    }

    #[test]
    fn test_namespace_name() {
        let toml = r#"
[lab]
name = "dc"
prefix = "lab"

[nodes.spine1]
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.namespace_name("spine1"), "lab-spine1");
    }

    #[test]
    fn test_namespace_name_default_prefix() {
        let toml = r#"
[lab]
name = "dc"

[nodes.spine1]
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.namespace_name("spine1"), "dc-spine1");
    }

    #[test]
    fn test_endpoint_ref_parse() {
        use crate::types::EndpointRef;

        let ep = EndpointRef::parse("spine1:eth0").unwrap();
        assert_eq!(ep.node, "spine1");
        assert_eq!(ep.iface, "eth0");

        assert!(EndpointRef::parse("nocolon").is_none());
        assert!(EndpointRef::parse(":eth0").is_none());
        assert!(EndpointRef::parse("node:").is_none());
    }
}
