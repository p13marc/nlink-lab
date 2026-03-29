//! Render a [`Topology`] back to NLL syntax.
//!
//! Used by `nlink-lab render` to show the fully-expanded topology after
//! loops, variables, and imports have been resolved.

use std::fmt::Write;

use crate::types::{LabConfig, Link, Node, Profile, Topology};

/// Render a topology as valid NLL syntax.
pub fn render(topology: &Topology) -> String {
    let mut out = String::new();

    render_lab(&mut out, &topology.lab);
    render_profiles(&mut out, topology);
    render_nodes(&mut out, topology);
    render_links(&mut out, topology);
    render_networks(&mut out, topology);
    render_impairments(&mut out, topology);
    render_rate_limits(&mut out, topology);

    out
}

fn render_lab(out: &mut String, lab: &LabConfig) {
    write!(out, "lab \"{}\"", lab.name).unwrap();
    let has_block = lab.description.is_some()
        || lab.prefix.is_some()
        || lab.version.is_some()
        || lab.author.is_some()
        || !lab.tags.is_empty()
        || lab.mgmt_subnet.is_some();

    if has_block {
        out.push_str(" {\n");
        if let Some(desc) = &lab.description {
            writeln!(out, "  description \"{desc}\"").unwrap();
        }
        if let Some(prefix) = &lab.prefix {
            writeln!(out, "  prefix \"{prefix}\"").unwrap();
        }
        if let Some(version) = &lab.version {
            writeln!(out, "  version \"{version}\"").unwrap();
        }
        if let Some(author) = &lab.author {
            writeln!(out, "  author \"{author}\"").unwrap();
        }
        if !lab.tags.is_empty() {
            let tags: Vec<_> = lab.tags.iter().map(|t| t.as_str()).collect();
            writeln!(out, "  tags [{}]", tags.join(", ")).unwrap();
        }
        if let Some(mgmt) = &lab.mgmt_subnet {
            writeln!(out, "  mgmt {mgmt}").unwrap();
        }
        out.push_str("}\n");
    }
    out.push('\n');
}

fn render_profiles(out: &mut String, topo: &Topology) {
    for (name, profile) in &topo.profiles {
        render_profile(out, name, profile);
    }
    if !topo.profiles.is_empty() {
        out.push('\n');
    }
}

fn render_profile(out: &mut String, name: &str, profile: &Profile) {
    write!(out, "profile {name} {{").unwrap();
    // Check for ip_forward sysctls — render as shorthand
    if profile
        .sysctls
        .get("net.ipv4.ip_forward")
        .map(|v| v.as_str())
        == Some("1")
    {
        out.push_str(" forward ipv4");
    }
    if profile
        .sysctls
        .get("net.ipv6.conf.all.forwarding")
        .map(|v| v.as_str())
        == Some("1")
    {
        out.push_str(" forward ipv6");
    }
    for (k, v) in &profile.sysctls {
        // Skip forwarding sysctls already rendered as shorthand
        if k == "net.ipv4.ip_forward" || k == "net.ipv6.conf.all.forwarding" {
            continue;
        }
        write!(out, " sysctl \"{k}\" \"{v}\"").unwrap();
    }
    out.push_str(" }\n");
}

fn render_nodes(out: &mut String, topo: &Topology) {
    for (name, node) in &topo.nodes {
        render_node(out, name, node);
    }
    if !topo.nodes.is_empty() {
        out.push('\n');
    }
}

fn render_node(out: &mut String, name: &str, node: &Node) {
    write!(out, "node {name}").unwrap();
    if let Some(profile) = &node.profile {
        write!(out, " : {profile}").unwrap();
    }
    if let Some(image) = &node.image {
        write!(out, " image \"{image}\"").unwrap();
    }

    let has_props = !node.routes.is_empty()
        || node.firewall.is_some()
        || !node.interfaces.is_empty()
        || !node.wireguard.is_empty()
        || !node.vrfs.is_empty()
        || node.cpu.is_some()
        || node.memory.is_some()
        || node.privileged
        || !node.cap_add.is_empty()
        || node.entrypoint.is_some()
        || node.hostname.is_some()
        || node.workdir.is_some()
        || !node.labels.is_empty()
        || node.pull.is_some()
        || !node.container_exec.is_empty()
        || node.healthcheck.is_some()
        || node.startup_delay.is_some()
        || !node.depends_on.is_empty()
        || !node.configs.is_empty()
        || node.overlay.is_some()
        || node.env_file.is_some();

    if has_props {
        out.push_str(" {\n");
        // Container properties
        if let Some(cpu) = &node.cpu {
            writeln!(out, "  cpu \"{cpu}\"").unwrap();
        }
        if let Some(mem) = &node.memory {
            writeln!(out, "  memory \"{mem}\"").unwrap();
        }
        if node.privileged {
            writeln!(out, "  privileged").unwrap();
        }
        if !node.cap_add.is_empty() {
            writeln!(out, "  cap-add [{}]", node.cap_add.join(", ")).unwrap();
        }
        if !node.cap_drop.is_empty() {
            writeln!(out, "  cap-drop [{}]", node.cap_drop.join(", ")).unwrap();
        }
        if let Some(ep) = &node.entrypoint {
            writeln!(out, "  entrypoint \"{ep}\"").unwrap();
        }
        if let Some(h) = &node.hostname {
            writeln!(out, "  hostname \"{h}\"").unwrap();
        }
        if let Some(w) = &node.workdir {
            writeln!(out, "  workdir \"{w}\"").unwrap();
        }
        if !node.labels.is_empty() {
            let labels: Vec<_> = node.labels.iter().map(|l| format!("\"{l}\"")).collect();
            writeln!(out, "  labels [{}]", labels.join(", ")).unwrap();
        }
        if let Some(p) = &node.pull {
            writeln!(out, "  pull {p}").unwrap();
        }
        for cmd in &node.container_exec {
            writeln!(out, "  exec \"{cmd}\"").unwrap();
        }
        if let Some(hc) = &node.healthcheck {
            write!(out, "  healthcheck \"{hc}\"").unwrap();
            if node.healthcheck_interval.is_some() || node.healthcheck_timeout.is_some() {
                out.push_str(" {");
                if let Some(iv) = &node.healthcheck_interval {
                    write!(out, " interval {iv}").unwrap();
                }
                if let Some(to) = &node.healthcheck_timeout {
                    write!(out, " timeout {to}").unwrap();
                }
                out.push_str(" }");
            }
            out.push('\n');
        }
        if let Some(d) = &node.startup_delay {
            writeln!(out, "  startup-delay {d}").unwrap();
        }
        if let Some(ef) = &node.env_file {
            writeln!(out, "  env-file \"{ef}\"").unwrap();
        }
        for (h, c) in &node.configs {
            writeln!(out, "  config \"{h}\" \"{c}\"").unwrap();
        }
        if let Some(o) = &node.overlay {
            writeln!(out, "  overlay \"{o}\"").unwrap();
        }
        if !node.depends_on.is_empty() {
            writeln!(out, "  depends-on [{}]", node.depends_on.join(", ")).unwrap();
        }

        for (iface_name, iface) in &node.interfaces {
            if iface_name == "lo" {
                for addr in &iface.addresses {
                    writeln!(out, "  lo {addr}").unwrap();
                }
            }
        }
        for (dest, route) in &node.routes {
            write!(out, "  route {dest}").unwrap();
            if let Some(via) = &route.via {
                write!(out, " via {via}").unwrap();
            }
            if let Some(dev) = &route.dev {
                write!(out, " dev {dev}").unwrap();
            }
            out.push('\n');
        }
        if let Some(fw) = &node.firewall {
            let policy = fw.policy.as_deref().unwrap_or("accept");
            write!(out, "  firewall policy {policy}").unwrap();
            if !fw.rules.is_empty() {
                out.push_str(" {\n");
                for rule in &fw.rules {
                    let action = rule.action.as_deref().unwrap_or("accept");
                    let match_expr = rule.match_expr.as_deref().unwrap_or("");
                    writeln!(out, "    {action} {match_expr}").unwrap();
                }
                out.push_str("  }\n");
            } else {
                out.push('\n');
            }
        }
        out.push_str("}\n");
    } else {
        out.push('\n');
    }
}

fn render_links(out: &mut String, topo: &Topology) {
    for link in &topo.links {
        render_link(out, link);
    }
    if !topo.links.is_empty() {
        out.push('\n');
    }
}

fn render_link(out: &mut String, link: &Link) {
    write!(out, "link {} -- {}", link.endpoints[0], link.endpoints[1]).unwrap();

    let has_block = link.addresses.is_some() || link.mtu.is_some();
    if has_block {
        out.push_str(" { ");
        if let Some(addrs) = &link.addresses {
            write!(out, "{} -- {}", addrs[0], addrs[1]).unwrap();
        }
        if let Some(mtu) = link.mtu {
            write!(out, " mtu {mtu}").unwrap();
        }
        out.push_str(" }");
    }
    out.push('\n');
}

fn render_networks(out: &mut String, topo: &Topology) {
    for (name, net) in &topo.networks {
        writeln!(out, "network {name} {{").unwrap();
        if !net.members.is_empty() {
            writeln!(out, "  members [{}]", net.members.join(", ")).unwrap();
        }
        out.push_str("}\n");
    }
}

fn render_impairments(out: &mut String, topo: &Topology) {
    for (endpoint, imp) in &topo.impairments {
        write!(out, "impair {endpoint}").unwrap();
        if let Some(d) = &imp.delay {
            write!(out, " delay {d}").unwrap();
        }
        if let Some(j) = &imp.jitter {
            write!(out, " jitter {j}").unwrap();
        }
        if let Some(l) = &imp.loss {
            write!(out, " loss {l}").unwrap();
        }
        if let Some(r) = &imp.rate {
            write!(out, " rate {r}").unwrap();
        }
        if let Some(c) = &imp.corrupt {
            write!(out, " corrupt {c}").unwrap();
        }
        if let Some(r) = &imp.reorder {
            write!(out, " reorder {r}").unwrap();
        }
        out.push('\n');
    }
}

fn render_rate_limits(out: &mut String, topo: &Topology) {
    for (endpoint, rl) in &topo.rate_limits {
        write!(out, "rate {endpoint}").unwrap();
        if let Some(e) = &rl.egress {
            write!(out, " egress {e}").unwrap();
        }
        if let Some(i) = &rl.ingress {
            write!(out, " ingress {i}").unwrap();
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    #[test]
    fn test_render_roundtrip() {
        let input = r#"lab "test"

node a
node b { route default via 10.0.0.1 }
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#;
        let topo = parser::parse(input).unwrap();
        let rendered = render(&topo);

        // Re-parse the rendered output
        let topo2 = parser::parse(&rendered).unwrap();
        assert_eq!(topo2.nodes.len(), topo.nodes.len());
        assert_eq!(topo2.links.len(), topo.links.len());
        assert_eq!(topo2.lab.name, "test");
    }

    #[test]
    fn test_render_with_metadata() {
        let input = r#"lab "mylab" {
  description "Test lab"
  version "1.0"
  author "Test Author"
  tags [networking, test]
}

node a
"#;
        let topo = parser::parse(input).unwrap();
        let rendered = render(&topo);
        assert!(rendered.contains("version \"1.0\""));
        assert!(rendered.contains("author \"Test Author\""));
        assert!(rendered.contains("tags [networking, test]"));
    }
}
