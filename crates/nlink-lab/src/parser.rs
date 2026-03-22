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

    #[test]
    fn test_parse_datacenter_sim() {
        // Full datacenter-sim example from NLINK_LAB.md section 4.3
        let toml = r#"
[lab]
name = "datacenter-sim"
description = "Simulated datacenter with spine-leaf topology"
prefix = "dc"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1", "net.ipv6.conf.all.forwarding" = "1" }

[profiles.router.firewall]
policy = "accept"

[profiles.host]
sysctls = { "net.ipv4.ip_forward" = "0" }

[nodes.spine1]
profile = "router"

[nodes.spine1.interfaces.lo]
addresses = ["10.255.0.1/32"]

[nodes.spine2]
profile = "router"

[nodes.spine2.interfaces.lo]
addresses = ["10.255.0.2/32"]

[nodes.leaf1]
profile = "router"

[nodes.leaf1.interfaces.lo]
addresses = ["10.255.1.1/32"]

[nodes.leaf1.routes]
default = { via = "10.0.11.1" }
"10.0.0.0/8" = { via = "10.0.11.1", metric = 100 }

[nodes.leaf2]
profile = "router"

[nodes.leaf2.interfaces.lo]
addresses = ["10.255.1.2/32"]

[nodes.server1]
profile = "host"

[nodes.server1.routes]
default = { via = "10.1.1.1" }

[[nodes.server1.exec]]
cmd = ["iperf3", "-s"]
background = true

[nodes.server2]
profile = "host"

[nodes.server2.routes]
default = { via = "10.1.2.1" }

[[nodes.server2.exec]]
cmd = ["iperf3", "-s"]
background = true

[[links]]
endpoints = ["spine1:eth1", "leaf1:eth1"]
addresses = ["10.0.11.1/30", "10.0.11.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine1:eth2", "leaf2:eth1"]
addresses = ["10.0.12.1/30", "10.0.12.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine2:eth1", "leaf1:eth2"]
addresses = ["10.0.21.1/30", "10.0.21.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine2:eth2", "leaf2:eth2"]
addresses = ["10.0.22.1/30", "10.0.22.2/30"]
mtu = 9000

[[links]]
endpoints = ["leaf1:eth3", "server1:eth0"]
addresses = ["10.1.1.1/24", "10.1.1.10/24"]

[[links]]
endpoints = ["leaf2:eth3", "server2:eth0"]
addresses = ["10.1.2.1/24", "10.1.2.10/24"]

[impairments."spine1:eth1"]
delay = "10ms"
jitter = "2ms"

[impairments."spine1:eth2"]
delay = "10ms"
jitter = "2ms"
loss = "0.1%"

[impairments."leaf2:eth3"]
delay = "50ms"
jitter = "5ms"
loss = "0.5%"
rate = "100mbit"
corrupt = "0.01%"
reorder = "0.5%"

[nodes.server1.firewall]
policy = "drop"

[[nodes.server1.firewall.rules]]
match = "ct state established,related"
action = "accept"

[[nodes.server1.firewall.rules]]
match = "tcp dport 5201"
action = "accept"

[[nodes.server1.firewall.rules]]
match = "icmp"
action = "accept"

[rate_limits."server1:eth0"]
egress = "1gbit"
ingress = "1gbit"
burst = "10mbit"

[rate_limits."server2:eth0"]
egress = "100mbit"
ingress = "100mbit"
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.lab.name, "datacenter-sim");
        assert_eq!(topo.lab.prefix(), "dc");
        assert_eq!(topo.profiles.len(), 2);
        assert_eq!(topo.nodes.len(), 6);
        assert_eq!(topo.links.len(), 6);
        assert_eq!(topo.impairments.len(), 3);
        assert_eq!(topo.rate_limits.len(), 2);
        assert_eq!(topo.nodes["server1"].exec.len(), 1);
        assert!(topo.nodes["server1"].exec[0].background);
        assert_eq!(topo.nodes["leaf1"].routes.len(), 2);
        assert_eq!(
            topo.nodes["leaf1"].routes["10.0.0.0/8"].metric,
            Some(100)
        );
        let fw = topo.nodes["server1"].firewall.as_ref().unwrap();
        assert_eq!(fw.rules.len(), 3);
    }

    #[test]
    fn test_parse_wireguard() {
        let toml = r#"
[lab]
name = "vpn-lab"

[nodes.office]
profile = "router"

[nodes.office.wireguard.wg0]
private_key = "auto"
listen_port = 51820
addresses = ["10.100.0.1/24"]
peers = ["remote"]

[nodes.remote]
profile = "router"

[nodes.remote.wireguard.wg0]
private_key = "auto"
addresses = ["10.100.0.2/24"]
peers = ["office"]

[[links]]
endpoints = ["office:eth0", "remote:eth0"]
addresses = ["203.0.113.1/24", "203.0.113.2/24"]

[impairments."office:eth0"]
delay = "30ms"
jitter = "5ms"
loss = "0.1%"
rate = "50mbit"
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.nodes.len(), 2);
        let wg = &topo.nodes["office"].wireguard["wg0"];
        assert_eq!(wg.private_key.as_deref(), Some("auto"));
        assert_eq!(wg.listen_port, Some(51820));
        assert_eq!(wg.addresses, vec!["10.100.0.1/24"]);
        assert_eq!(wg.peers, vec!["remote"]);
    }

    #[test]
    fn test_parse_vxlan_overlay() {
        let toml = r#"
[lab]
name = "overlay-lab"

[nodes.vtep1]
profile = "router"

[nodes.vtep1.interfaces.vxlan100]
kind = "vxlan"
vni = 100
local = "10.0.0.1"
remote = "10.0.0.2"
port = 4789
addresses = ["192.168.100.1/24"]

[nodes.vtep2]
profile = "router"

[nodes.vtep2.interfaces.vxlan100]
kind = "vxlan"
vni = 100
local = "10.0.0.2"
remote = "10.0.0.1"
port = 4789
addresses = ["192.168.100.2/24"]

[[links]]
endpoints = ["vtep1:eth0", "vtep2:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]
"#;
        let topo = parse(toml).unwrap();
        assert_eq!(topo.nodes.len(), 2);
        let vxlan = &topo.nodes["vtep1"].interfaces["vxlan100"];
        assert_eq!(vxlan.kind.as_deref(), Some("vxlan"));
        assert_eq!(vxlan.vni, Some(100));
        assert_eq!(vxlan.local.as_deref(), Some("10.0.0.1"));
        assert_eq!(vxlan.remote.as_deref(), Some("10.0.0.2"));
        assert_eq!(vxlan.port, Some(4789));
        assert_eq!(vxlan.addresses, vec!["192.168.100.1/24"]);
    }

    #[test]
    fn test_parse_malformed_toml() {
        let result = parse("this is not valid TOML {{{");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), crate::Error::TomlParse(_)));
    }

    #[test]
    fn test_parse_missing_lab_section() {
        let result = parse("[nodes.a]");
        assert!(result.is_err());
    }
}
