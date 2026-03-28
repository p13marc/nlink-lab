//! Lowering pass: AST → Topology.
//!
//! Expands `for` loops, substitutes `let` variables, resolves profiles,
//! and maps AST nodes to the [`crate::types::Topology`] struct.

use std::collections::HashMap;

use super::ast;
use crate::error::Result;
use crate::types;

/// Lower an NLL AST into a Topology.
pub fn lower(file: &ast::File) -> Result<types::Topology> {
    let mut ctx = LowerCtx::new();

    // First pass: collect profiles and variables
    for stmt in &file.statements {
        match stmt {
            ast::Statement::Profile(p) => ctx.add_profile(p),
            ast::Statement::Let(l) => ctx.add_variable(l),
            _ => {}
        }
    }

    // Pre-lowering validation
    validate_ast(file, &ctx)?;

    // Second pass: expand loops and collect all concrete statements
    let expanded = ctx.expand_statements(&file.statements)?;

    // Third pass: lower to Topology
    let mut topology = types::Topology::default();
    topology.lab = lower_lab(&file.lab);

    // Add profiles to topology (for validator cross-referencing)
    for (name, profile_def) in &ctx.profiles {
        topology.profiles.insert(name.clone(), lower_profile(profile_def));
    }

    for stmt in &expanded {
        match stmt {
            ast::Statement::Node(n) => lower_node(&mut topology, n, &ctx)?,
            ast::Statement::Link(l) => lower_link(&mut topology, l),
            ast::Statement::Network(n) => lower_network(&mut topology, n),
            ast::Statement::Impair(i) => lower_impair(&mut topology, i),
            ast::Statement::Rate(r) => lower_rate(&mut topology, r),
            ast::Statement::Profile(_) | ast::Statement::Let(_) | ast::Statement::For(_) => {}
        }
    }

    Ok(topology)
}

// ─── Context ──────────────────────────────────────────────

struct LowerCtx {
    profiles: HashMap<String, ast::ProfileDef>,
    variables: HashMap<String, String>,
}

impl LowerCtx {
    fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            variables: HashMap::new(),
        }
    }

    fn add_profile(&mut self, p: &ast::ProfileDef) {
        self.profiles.insert(p.name.clone(), p.clone());
    }

    fn add_variable(&mut self, l: &ast::LetDef) {
        self.variables.insert(l.name.clone(), l.value.clone());
    }

    fn expand_statements(&self, stmts: &[ast::Statement]) -> Result<Vec<ast::Statement>> {
        let mut result = Vec::new();
        let mut vars = self.variables.clone();

        for stmt in stmts {
            match stmt {
                ast::Statement::For(f) => {
                    let expanded = self.expand_for(f, &mut vars)?;
                    result.extend(expanded);
                }
                ast::Statement::Let(l) => {
                    // Process variable — may contain interpolation
                    let value = interpolate(&l.value, &vars);
                    vars.insert(l.name.clone(), value);
                }
                other => {
                    let expanded = interpolate_statement(other, &vars);
                    result.push(expanded);
                }
            }
        }

        Ok(result)
    }

    fn expand_for(
        &self,
        for_loop: &ast::ForLoop,
        vars: &mut HashMap<String, String>,
    ) -> Result<Vec<ast::Statement>> {
        let mut result = Vec::new();

        for i in for_loop.start..=for_loop.end {
            vars.insert(for_loop.var.clone(), i.to_string());

            for stmt in &for_loop.body {
                match stmt {
                    ast::Statement::For(nested) => {
                        let expanded = self.expand_for(nested, vars)?;
                        result.extend(expanded);
                    }
                    ast::Statement::Let(l) => {
                        let value = interpolate(&l.value, vars);
                        vars.insert(l.name.clone(), value);
                    }
                    other => {
                        let expanded = interpolate_statement(other, vars);
                        result.push(expanded);
                    }
                }
            }
        }

        vars.remove(&for_loop.var);
        Ok(result)
    }
}

// ─── Interpolation ────────────────────────────────────────

/// Replace `${var}` with its value from the variables map.
/// Supports simple arithmetic: `${var + N}`, `${var - N}`, `${var * N}`, `${var / N}`.
fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut expr = String::new();
            while let Some(&c) = chars.peek() {
                if c == '}' {
                    chars.next();
                    break;
                }
                expr.push(c);
                chars.next();
            }

            let value = eval_expr(&expr, vars);
            result.push_str(&value);
        } else {
            result.push(ch);
        }
    }

    result
}

/// Evaluate a simple expression: variable name, or `var op int`.
fn eval_expr(expr: &str, vars: &HashMap<String, String>) -> String {
    let expr = expr.trim();

    // Try: var + N, var - N, var * N, var / N
    for op in [" + ", " - ", " * ", " / "] {
        if let Some((left, right)) = expr.split_once(op) {
            let left = left.trim();
            let right = right.trim();

            let left_val = vars
                .get(left)
                .cloned()
                .unwrap_or_else(|| left.to_string());
            let left_num: i64 = match left_val.parse() {
                Ok(n) => n,
                Err(_) => return format!("${{{expr}}}"), // can't evaluate
            };
            let right_num: i64 = match right.parse() {
                Ok(n) => n,
                Err(_) => {
                    // right might also be a variable
                    let rv = vars.get(right).cloned().unwrap_or_else(|| right.to_string());
                    match rv.parse() {
                        Ok(n) => n,
                        Err(_) => return format!("${{{expr}}}"),
                    }
                }
            };

            let result = match op.trim() {
                "+" => left_num + right_num,
                "-" => left_num - right_num,
                "*" => left_num * right_num,
                "/" => {
                    if right_num == 0 {
                        return format!("${{{expr}}}");
                    }
                    left_num / right_num
                }
                _ => unreachable!(),
            };

            return result.to_string();
        }
    }

    // Simple variable lookup
    vars.get(expr)
        .cloned()
        .unwrap_or_else(|| format!("${{{expr}}}"))
}

/// Interpolate all string fields in a statement.
fn interpolate_statement(
    stmt: &ast::Statement,
    vars: &HashMap<String, String>,
) -> ast::Statement {
    match stmt {
        ast::Statement::Node(n) => ast::Statement::Node(interpolate_node(n, vars)),
        ast::Statement::Link(l) => ast::Statement::Link(interpolate_link(l, vars)),
        ast::Statement::Network(n) => ast::Statement::Network(interpolate_network(n, vars)),
        ast::Statement::Impair(i) => ast::Statement::Impair(interpolate_impair_def(i, vars)),
        ast::Statement::Rate(r) => ast::Statement::Rate(interpolate_rate_def(r, vars)),
        ast::Statement::Profile(p) => ast::Statement::Profile(p.clone()),
        ast::Statement::Let(l) => ast::Statement::Let(l.clone()),
        ast::Statement::For(f) => ast::Statement::For(f.clone()),
    }
}

fn i(s: &str, vars: &HashMap<String, String>) -> String {
    interpolate(s, vars)
}

fn io(s: &Option<String>, vars: &HashMap<String, String>) -> Option<String> {
    s.as_ref().map(|s| interpolate(s, vars))
}

fn interpolate_node(n: &ast::NodeDef, vars: &HashMap<String, String>) -> ast::NodeDef {
    ast::NodeDef {
        name: i(&n.name, vars),
        profile: n.profile.clone(),
        image: n.image.as_ref().map(|s| i(s, vars)),
        cmd: n.cmd.clone(),
        env: n.env.iter().map(|s| i(s, vars)).collect(),
        volumes: n.volumes.iter().map(|s| i(s, vars)).collect(),
        props: n.props.iter().map(|p| interpolate_prop(p, vars)).collect(),
    }
}

fn interpolate_prop(p: &ast::NodeProp, vars: &HashMap<String, String>) -> ast::NodeProp {
    match p {
        ast::NodeProp::Forward(v) => ast::NodeProp::Forward(*v),
        ast::NodeProp::Sysctl(k, v) => ast::NodeProp::Sysctl(i(k, vars), i(v, vars)),
        ast::NodeProp::Lo(addr) => ast::NodeProp::Lo(i(addr, vars)),
        ast::NodeProp::Route(r) => ast::NodeProp::Route(interpolate_route(r, vars)),
        ast::NodeProp::Firewall(fw) => ast::NodeProp::Firewall(fw.clone()),
        ast::NodeProp::Vrf(v) => ast::NodeProp::Vrf(interpolate_vrf(v, vars)),
        ast::NodeProp::Wireguard(wg) => ast::NodeProp::Wireguard(interpolate_wg(wg, vars)),
        ast::NodeProp::Vxlan(vx) => ast::NodeProp::Vxlan(interpolate_vxlan(vx, vars)),
        ast::NodeProp::Dummy(d) => ast::NodeProp::Dummy(ast::DummyDef {
            name: i(&d.name, vars),
            addresses: d.addresses.iter().map(|s| i(s, vars)).collect(),
        }),
        ast::NodeProp::Run(r) => ast::NodeProp::Run(r.clone()),
    }
}

fn interpolate_route(r: &ast::RouteDef, vars: &HashMap<String, String>) -> ast::RouteDef {
    ast::RouteDef {
        destination: i(&r.destination, vars),
        via: io(&r.via, vars),
        dev: io(&r.dev, vars),
        metric: r.metric,
    }
}

fn interpolate_vrf(v: &ast::VrfDef, vars: &HashMap<String, String>) -> ast::VrfDef {
    ast::VrfDef {
        name: i(&v.name, vars),
        table: v.table,
        interfaces: v.interfaces.iter().map(|s| i(s, vars)).collect(),
        routes: v.routes.iter().map(|r| interpolate_route(r, vars)).collect(),
    }
}

fn interpolate_wg(wg: &ast::WireguardDef, vars: &HashMap<String, String>) -> ast::WireguardDef {
    ast::WireguardDef {
        name: i(&wg.name, vars),
        key: wg.key.clone(),
        listen_port: wg.listen_port,
        addresses: wg.addresses.iter().map(|s| i(s, vars)).collect(),
        peers: wg.peers.iter().map(|s| i(s, vars)).collect(),
    }
}

fn interpolate_vxlan(vx: &ast::VxlanDef, vars: &HashMap<String, String>) -> ast::VxlanDef {
    ast::VxlanDef {
        name: i(&vx.name, vars),
        vni: vx.vni,
        local: io(&vx.local, vars),
        remote: io(&vx.remote, vars),
        port: vx.port,
        addresses: vx.addresses.iter().map(|s| i(s, vars)).collect(),
    }
}

fn interpolate_link(l: &ast::LinkDef, vars: &HashMap<String, String>) -> ast::LinkDef {
    ast::LinkDef {
        left_node: i(&l.left_node, vars),
        left_iface: i(&l.left_iface, vars),
        right_node: i(&l.right_node, vars),
        right_iface: i(&l.right_iface, vars),
        left_addr: io(&l.left_addr, vars),
        right_addr: io(&l.right_addr, vars),
        mtu: l.mtu,
        impairment: l.impairment.as_ref().map(|p| interpolate_impair_props(p, vars)),
        left_impair: l.left_impair.as_ref().map(|p| interpolate_impair_props(p, vars)),
        right_impair: l.right_impair.as_ref().map(|p| interpolate_impair_props(p, vars)),
        rate: l.rate.as_ref().map(|p| interpolate_rate_props(p, vars)),
    }
}

fn interpolate_impair_props(
    p: &ast::ImpairProps,
    vars: &HashMap<String, String>,
) -> ast::ImpairProps {
    ast::ImpairProps {
        delay: io(&p.delay, vars),
        jitter: io(&p.jitter, vars),
        loss: io(&p.loss, vars),
        rate: io(&p.rate, vars),
        corrupt: io(&p.corrupt, vars),
        reorder: io(&p.reorder, vars),
    }
}

fn interpolate_rate_props(
    p: &ast::RateProps,
    vars: &HashMap<String, String>,
) -> ast::RateProps {
    ast::RateProps {
        egress: io(&p.egress, vars),
        ingress: io(&p.ingress, vars),
        burst: io(&p.burst, vars),
    }
}

fn interpolate_network(n: &ast::NetworkDef, vars: &HashMap<String, String>) -> ast::NetworkDef {
    ast::NetworkDef {
        name: i(&n.name, vars),
        members: n.members.iter().map(|s| i(s, vars)).collect(),
        vlan_filtering: n.vlan_filtering,
        mtu: n.mtu,
        vlans: n.vlans.clone(),
        ports: n.ports.clone(),
    }
}

fn interpolate_impair_def(
    imp: &ast::ImpairDef,
    vars: &HashMap<String, String>,
) -> ast::ImpairDef {
    ast::ImpairDef {
        node: i(&imp.node, vars),
        iface: i(&imp.iface, vars),
        props: interpolate_impair_props(&imp.props, vars),
    }
}

fn interpolate_rate_def(r: &ast::RateDef, vars: &HashMap<String, String>) -> ast::RateDef {
    ast::RateDef {
        node: i(&r.node, vars),
        iface: i(&r.iface, vars),
        props: interpolate_rate_props(&r.props, vars),
    }
}

// ─── Lowering to Topology types ───────────────────────────

// ─── Pre-lowering validation ──────────────────────────────

fn validate_ast(file: &ast::File, ctx: &LowerCtx) -> Result<()> {
    let mut errors = Vec::new();

    for stmt in &file.statements {
        validate_stmt(stmt, ctx, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(crate::Error::NllParse(errors.join("; ")))
    }
}

fn validate_stmt(stmt: &ast::Statement, ctx: &LowerCtx, errors: &mut Vec<String>) {
    match stmt {
        ast::Statement::Node(n) => {
            // Check profile exists
            if let Some(ref profile) = n.profile {
                if !ctx.profiles.contains_key(profile) {
                    errors.push(format!(
                        "node '{}' references undefined profile '{profile}'",
                        n.name
                    ));
                }
            }
        }
        ast::Statement::For(f) => {
            if f.start > f.end {
                errors.push(format!(
                    "for loop '{}' has empty range {}..{}",
                    f.var, f.start, f.end
                ));
            }
            for stmt in &f.body {
                validate_stmt(stmt, ctx, errors);
            }
        }
        _ => {}
    }
}

// ─── Profile lowering ─────────────────────────────────────

fn lower_profile(profile: &ast::ProfileDef) -> types::Profile {
    let mut p = types::Profile::default();
    for prop in &profile.props {
        match prop {
            ast::NodeProp::Forward(version) => {
                let key = match version {
                    ast::IpVersion::Ipv4 => "net.ipv4.ip_forward",
                    ast::IpVersion::Ipv6 => "net.ipv6.conf.all.forwarding",
                };
                p.sysctls.insert(key.to_string(), "1".to_string());
            }
            ast::NodeProp::Sysctl(k, v) => {
                p.sysctls.insert(k.clone(), v.clone());
            }
            ast::NodeProp::Firewall(fw) => {
                p.firewall = Some(types::FirewallConfig {
                    policy: Some(fw.policy.clone()),
                    rules: fw
                        .rules
                        .iter()
                        .map(|r| types::FirewallRule {
                            match_expr: Some(r.match_expr.clone()),
                            action: Some(r.action.clone()),
                        })
                        .collect(),
                });
            }
            _ => {} // Other props not applicable to profiles
        }
    }
    p
}

fn lower_lab(lab: &ast::LabDecl) -> types::LabConfig {
    types::LabConfig {
        name: lab.name.clone(),
        description: lab.description.clone(),
        prefix: lab.prefix.clone(),
        runtime: lab.runtime.as_deref().map(|s| match s {
            "docker" => types::ContainerRuntime::Docker,
            "podman" => types::ContainerRuntime::Podman,
            _ => types::ContainerRuntime::Auto,
        }),
    }
}

fn lower_node(
    topo: &mut types::Topology,
    node: &ast::NodeDef,
    ctx: &LowerCtx,
) -> Result<()> {
    let mut n = types::Node {
        profile: node.profile.clone(),
        image: node.image.clone(),
        cmd: node.cmd.clone(),
        ..Default::default()
    };

    // Container env/volumes
    if !node.env.is_empty() {
        let map: HashMap<String, String> = node
            .env
            .iter()
            .filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        n.env = Some(map);
    }
    if !node.volumes.is_empty() {
        n.volumes = Some(node.volumes.clone());
    }

    // Apply profile properties first
    if let Some(profile_name) = &node.profile {
        if let Some(profile) = ctx.profiles.get(profile_name) {
            apply_node_props(&mut n, &profile.props);
        }
    }

    // Apply node's own properties (overrides profile)
    apply_node_props(&mut n, &node.props);

    topo.nodes.insert(node.name.clone(), n);
    Ok(())
}

fn apply_node_props(node: &mut types::Node, props: &[ast::NodeProp]) {
    for prop in props {
        match prop {
            ast::NodeProp::Forward(version) => {
                let key = match version {
                    ast::IpVersion::Ipv4 => "net.ipv4.ip_forward",
                    ast::IpVersion::Ipv6 => "net.ipv6.conf.all.forwarding",
                };
                node.sysctls.insert(key.to_string(), "1".to_string());
            }
            ast::NodeProp::Sysctl(k, v) => {
                node.sysctls.insert(k.clone(), v.clone());
            }
            ast::NodeProp::Lo(addr) => {
                let lo = node.interfaces.entry("lo".to_string()).or_default();
                lo.addresses.push(addr.clone());
            }
            ast::NodeProp::Route(r) => {
                node.routes.insert(
                    r.destination.clone(),
                    types::RouteConfig {
                        via: r.via.clone(),
                        dev: r.dev.clone(),
                        metric: r.metric,
                    },
                );
            }
            ast::NodeProp::Firewall(fw) => {
                node.firewall = Some(types::FirewallConfig {
                    policy: Some(fw.policy.clone()),
                    rules: fw
                        .rules
                        .iter()
                        .map(|r| types::FirewallRule {
                            match_expr: Some(r.match_expr.clone()),
                            action: Some(r.action.clone()),
                        })
                        .collect(),
                });
            }
            ast::NodeProp::Vrf(v) => {
                node.vrfs.insert(
                    v.name.clone(),
                    types::VrfConfig {
                        table: v.table,
                        interfaces: v.interfaces.clone(),
                        routes: v
                            .routes
                            .iter()
                            .map(|r| {
                                (
                                    r.destination.clone(),
                                    types::RouteConfig {
                                        via: r.via.clone(),
                                        dev: r.dev.clone(),
                                        metric: r.metric,
                                    },
                                )
                            })
                            .collect(),
                    },
                );
            }
            ast::NodeProp::Wireguard(wg) => {
                node.wireguard.insert(
                    wg.name.clone(),
                    types::WireguardConfig {
                        private_key: wg.key.clone(),
                        listen_port: wg.listen_port,
                        addresses: wg.addresses.clone(),
                        peers: wg.peers.clone(),
                    },
                );
            }
            ast::NodeProp::Vxlan(vx) => {
                node.interfaces.insert(
                    vx.name.clone(),
                    types::InterfaceConfig {
                        kind: Some("vxlan".to_string()),
                        vni: Some(vx.vni),
                        local: vx.local.clone(),
                        remote: vx.remote.clone(),
                        port: vx.port,
                        addresses: vx.addresses.clone(),
                        ..Default::default()
                    },
                );
            }
            ast::NodeProp::Dummy(d) => {
                node.interfaces.insert(
                    d.name.clone(),
                    types::InterfaceConfig {
                        kind: Some("dummy".to_string()),
                        addresses: d.addresses.clone(),
                        ..Default::default()
                    },
                );
            }
            ast::NodeProp::Run(r) => {
                node.exec.push(types::ExecConfig {
                    cmd: r.cmd.clone(),
                    background: r.background,
                });
            }
        }
    }
}

fn lower_link(topo: &mut types::Topology, link: &ast::LinkDef) {
    let endpoints = [
        format!("{}:{}", link.left_node, link.left_iface),
        format!("{}:{}", link.right_node, link.right_iface),
    ];

    let addresses = match (&link.left_addr, &link.right_addr) {
        (Some(l), Some(r)) => Some([l.clone(), r.clone()]),
        _ => None,
    };

    topo.links.push(types::Link {
        endpoints,
        addresses,
        mtu: link.mtu,
    });

    // Lower symmetric impairment → both endpoints
    if let Some(imp) = &link.impairment {
        let left_ep = format!("{}:{}", link.left_node, link.left_iface);
        let right_ep = format!("{}:{}", link.right_node, link.right_iface);
        topo.impairments
            .insert(left_ep, lower_impair_props(imp));
        topo.impairments
            .insert(right_ep, lower_impair_props(imp));
    }

    // Lower directional impairments
    if let Some(imp) = &link.left_impair {
        let ep = format!("{}:{}", link.left_node, link.left_iface);
        topo.impairments.insert(ep, lower_impair_props(imp));
    }
    if let Some(imp) = &link.right_impair {
        let ep = format!("{}:{}", link.right_node, link.right_iface);
        topo.impairments.insert(ep, lower_impair_props(imp));
    }

    // Lower rate (both endpoints)
    if let Some(rate) = &link.rate {
        let left_ep = format!("{}:{}", link.left_node, link.left_iface);
        let right_ep = format!("{}:{}", link.right_node, link.right_iface);
        let rl = types::RateLimit {
            egress: rate.egress.clone(),
            ingress: rate.ingress.clone(),
            burst: rate.burst.clone(),
        };
        topo.rate_limits.insert(left_ep, rl.clone());
        topo.rate_limits.insert(right_ep, rl);
    }
}

fn lower_impair_props(props: &ast::ImpairProps) -> types::Impairment {
    types::Impairment {
        delay: props.delay.clone(),
        jitter: props.jitter.clone(),
        loss: props.loss.clone(),
        rate: props.rate.clone(),
        corrupt: props.corrupt.clone(),
        reorder: props.reorder.clone(),
    }
}

fn lower_network(topo: &mut types::Topology, net: &ast::NetworkDef) {
    let mut network = types::Network {
        kind: Some("bridge".to_string()),
        vlan_filtering: if net.vlan_filtering { Some(true) } else { None },
        mtu: net.mtu,
        members: net.members.clone(),
        ..Default::default()
    };

    for vlan in &net.vlans {
        network.vlans.insert(
            vlan.id,
            types::VlanConfig {
                name: vlan.name.clone(),
            },
        );
    }

    for port in &net.ports {
        network.ports.insert(
            port.endpoint.clone(),
            types::PortConfig {
                interface: None,
                vlans: port.vlans.clone(),
                tagged: if port.tagged { Some(true) } else { None },
                pvid: port.pvid,
                untagged: if port.untagged { Some(true) } else { None },
                addresses: Vec::new(),
            },
        );
    }

    topo.networks.insert(net.name.clone(), network);
}

fn lower_impair(topo: &mut types::Topology, imp: &ast::ImpairDef) {
    let ep = format!("{}:{}", imp.node, imp.iface);
    topo.impairments.insert(ep, lower_impair_props(&imp.props));
}

fn lower_rate(topo: &mut types::Topology, rate: &ast::RateDef) {
    let ep = format!("{}:{}", rate.node, rate.iface);
    topo.rate_limits.insert(
        ep,
        types::RateLimit {
            egress: rate.props.egress.clone(),
            ingress: rate.props.ingress.clone(),
            burst: rate.props.burst.clone(),
        },
    );
}

#[cfg(test)]
mod tests {
    use crate::parser::nll;

    fn parse_and_lower(input: &str) -> crate::types::Topology {
        nll::parse(input).unwrap()
    }

    #[test]
    fn test_lower_simple() {
        let topo = parse_and_lower(
            r#"lab "simple"

node router { forward ipv4 }
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}"#,
        );
        assert_eq!(topo.lab.name, "simple");
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.links.len(), 1);
        assert_eq!(
            topo.nodes["router"].sysctls["net.ipv4.ip_forward"],
            "1"
        );
        assert_eq!(
            topo.nodes["host"].routes["default"].via.as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(topo.links[0].addresses.as_ref().unwrap()[0], "10.0.0.1/24");
        // Symmetric impairment → both endpoints
        assert_eq!(topo.impairments.len(), 2);
        assert_eq!(
            topo.impairments["router:eth0"].delay.as_deref(),
            Some("10ms")
        );
        assert_eq!(
            topo.impairments["host:eth0"].delay.as_deref(),
            Some("10ms")
        );
    }

    #[test]
    fn test_lower_profile_inheritance() {
        let topo = parse_and_lower(
            r#"lab "t"

profile router { forward ipv4 }

node r1 : router
node r2 : router { forward ipv6 }"#,
        );
        assert_eq!(topo.nodes["r1"].sysctls["net.ipv4.ip_forward"], "1");
        // r2 inherits ipv4 and adds ipv6
        assert_eq!(topo.nodes["r2"].sysctls["net.ipv4.ip_forward"], "1");
        assert_eq!(
            topo.nodes["r2"].sysctls["net.ipv6.conf.all.forwarding"],
            "1"
        );
    }

    #[test]
    fn test_lower_for_loop() {
        let topo = parse_and_lower(
            r#"lab "t"

for i in 1..3 {
  node r${i}
}"#,
        );
        assert_eq!(topo.nodes.len(), 3);
        assert!(topo.nodes.contains_key("r1"));
        assert!(topo.nodes.contains_key("r2"));
        assert!(topo.nodes.contains_key("r3"));
    }

    #[test]
    fn test_lower_nested_for() {
        let topo = parse_and_lower(
            r#"lab "t"

for s in 1..2 {
  for l in 1..2 {
    link spine${s}:eth${l} -- leaf${l}:eth${s} {
      10.${s}.${l}.1/30 -- 10.${s}.${l}.2/30
    }
  }
}"#,
        );
        assert_eq!(topo.links.len(), 4);
        // Check one specific link
        let link = topo.links.iter().find(|l| l.endpoints[0] == "spine1:eth1").unwrap();
        assert_eq!(link.endpoints[1], "leaf1:eth1");
        assert_eq!(link.addresses.as_ref().unwrap()[0], "10.1.1.1/30");
    }

    #[test]
    fn test_lower_let_variable() {
        let topo = parse_and_lower(
            r#"lab "t"

let wan_delay = 30ms

link a:e0 -- b:e0 {
  10.0.0.1/30 -- 10.0.0.2/30
  delay ${wan_delay}
}"#,
        );
        assert_eq!(
            topo.impairments["a:e0"].delay.as_deref(),
            Some("30ms")
        );
    }

    #[test]
    fn test_lower_asymmetric_impairment() {
        let topo = parse_and_lower(
            r#"lab "t"

link a:e0 -- b:e0 {
  10.0.0.1/30 -- 10.0.0.2/30
  -> delay 500ms rate 10mbit
  <- delay 500ms rate 2mbit
}"#,
        );
        assert_eq!(topo.impairments.len(), 2);
        assert_eq!(
            topo.impairments["a:e0"].rate.as_deref(),
            Some("10mbit")
        );
        assert_eq!(
            topo.impairments["b:e0"].rate.as_deref(),
            Some("2mbit")
        );
    }

    #[test]
    fn test_lower_forward_to_sysctl() {
        let topo = parse_and_lower(
            r#"lab "t"

node r1 {
  forward ipv4
  forward ipv6
}"#,
        );
        assert_eq!(topo.nodes["r1"].sysctls["net.ipv4.ip_forward"], "1");
        assert_eq!(
            topo.nodes["r1"].sysctls["net.ipv6.conf.all.forwarding"],
            "1"
        );
    }

    #[test]
    fn test_lower_firewall() {
        let topo = parse_and_lower(
            r#"lab "t"

node server {
  firewall policy drop {
    accept ct established,related
    accept tcp dport 80
  }
}"#,
        );
        let fw = topo.nodes["server"].firewall.as_ref().unwrap();
        assert_eq!(fw.policy.as_deref(), Some("drop"));
        assert_eq!(fw.rules.len(), 2);
        assert_eq!(fw.rules[0].action.as_deref(), Some("accept"));
        assert_eq!(
            fw.rules[0].match_expr.as_deref(),
            Some("ct state established,related")
        );
    }

    #[test]
    fn test_lower_vrf() {
        let topo = parse_and_lower(
            r#"lab "t"

node pe {
  vrf red table 10 {
    interfaces [eth1]
    route default dev eth1
  }
}"#,
        );
        let vrf = &topo.nodes["pe"].vrfs["red"];
        assert_eq!(vrf.table, 10);
        assert_eq!(vrf.interfaces, vec!["eth1"]);
        assert_eq!(vrf.routes["default"].dev.as_deref(), Some("eth1"));
    }

    #[test]
    fn test_lower_wireguard() {
        let topo = parse_and_lower(
            r#"lab "t"

node gw {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
}"#,
        );
        let wg = &topo.nodes["gw"].wireguard["wg0"];
        assert_eq!(wg.private_key.as_deref(), Some("auto"));
        assert_eq!(wg.listen_port, Some(51820));
        assert_eq!(wg.addresses, vec!["192.168.255.1/32"]);
        assert_eq!(wg.peers, vec!["gw-b"]);
    }

    #[test]
    fn test_lower_vxlan() {
        let topo = parse_and_lower(
            r#"lab "t"

node vtep1 {
  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    port 4789
    address 192.168.100.1/24
  }
}"#,
        );
        let iface = &topo.nodes["vtep1"].interfaces["vxlan100"];
        assert_eq!(iface.kind.as_deref(), Some("vxlan"));
        assert_eq!(iface.vni, Some(100));
        assert_eq!(iface.local.as_deref(), Some("10.0.0.1"));
        assert_eq!(iface.remote.as_deref(), Some("10.0.0.2"));
        assert_eq!(iface.port, Some(4789));
        assert_eq!(iface.addresses, vec!["192.168.100.1/24"]);
    }

    #[test]
    fn test_lower_run() {
        let topo = parse_and_lower(
            r#"lab "t"

node server {
  run background ["iperf3", "-s"]
  run ["ip", "link"]
}"#,
        );
        assert_eq!(topo.nodes["server"].exec.len(), 2);
        assert!(topo.nodes["server"].exec[0].background);
        assert_eq!(topo.nodes["server"].exec[0].cmd, vec!["iperf3", "-s"]);
        assert!(!topo.nodes["server"].exec[1].background);
    }

    #[test]
    fn test_lower_rate_limit() {
        let topo = parse_and_lower(
            r#"lab "t"

link a:e0 -- b:e0 {
  10.0.0.1/24 -- 10.0.0.2/24
  rate egress 100mbit ingress 100mbit
}"#,
        );
        let rl = &topo.rate_limits["a:e0"];
        assert_eq!(rl.egress.as_deref(), Some("100mbit"));
        assert_eq!(rl.ingress.as_deref(), Some("100mbit"));
    }

    #[test]
    fn test_lower_network() {
        let topo = parse_and_lower(
            r#"lab "t"

network fabric {
  members [switch:br0, host1:eth0]
  vlan-filtering
  vlan 100 "sales"
  port host1 { pvid 100  untagged }
}"#,
        );
        let net = &topo.networks["fabric"];
        assert_eq!(net.members, vec!["switch:br0", "host1:eth0"]);
        assert_eq!(net.vlan_filtering, Some(true));
        assert_eq!(net.vlans[&100].name.as_deref(), Some("sales"));
        assert_eq!(net.ports["host1"].pvid, Some(100));
        assert_eq!(net.ports["host1"].untagged, Some(true));
    }

    #[test]
    fn test_interpolation_arithmetic() {
        let topo = parse_and_lower(
            r#"lab "t"

for i in 1..2 {
  node r${i} { lo 10.255.0.${i}/32 }
}"#,
        );
        let lo1 = &topo.nodes["r1"].interfaces["lo"];
        assert_eq!(lo1.addresses, vec!["10.255.0.1/32"]);
        let lo2 = &topo.nodes["r2"].interfaces["lo"];
        assert_eq!(lo2.addresses, vec!["10.255.0.2/32"]);
    }

    #[test]
    fn test_undefined_profile_error() {
        let result = nll::parse(r#"lab "t"
node r1 : nonexistent"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("undefined profile"), "got: {err}");
    }

    // ─── Example file tests ───────────────────────────────

    fn examples_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
    }

    fn parse_example(name: &str) -> crate::types::Topology {
        let path = examples_dir().join(name);
        let content = std::fs::read_to_string(&path).unwrap();
        nll::parse(&content).unwrap()
    }

    #[test]
    fn test_example_firewall() {
        let topo = parse_example("firewall.nll");
        let fw = topo.nodes["server"].firewall.as_ref().unwrap();
        assert_eq!(fw.policy.as_deref(), Some("drop"));
        assert_eq!(fw.rules.len(), 3);
    }

    #[test]
    fn test_example_vxlan() {
        let topo = parse_example("vxlan-overlay.nll");
        let vxlan = &topo.nodes["vtep1"].interfaces["vxlan100"];
        assert_eq!(vxlan.kind.as_deref(), Some("vxlan"));
        assert_eq!(vxlan.vni, Some(100));
    }

    #[test]
    fn test_example_vrf() {
        let topo = parse_example("vrf-multitenant.nll");
        let vrf = &topo.nodes["pe"].vrfs["red"];
        assert_eq!(vrf.table, 10);
        assert_eq!(vrf.interfaces, vec!["eth1"]);
    }

    #[test]
    fn test_example_wireguard() {
        let topo = parse_example("wireguard-vpn.nll");
        let wg = &topo.nodes["gw-a"].wireguard["wg0"];
        assert_eq!(wg.private_key.as_deref(), Some("auto"));
        assert_eq!(wg.listen_port, Some(51820));
    }

    #[test]
    fn test_example_iperf() {
        let topo = parse_example("iperf-benchmark.nll");
        assert_eq!(topo.rate_limits.len(), 2);
    }

    #[test]
    fn test_all_nll_examples_parse() {
        let dir = examples_dir();
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("nll") {
                let content = std::fs::read_to_string(&path).unwrap();
                let topo = nll::parse(&content).unwrap_or_else(|e| {
                    panic!("failed to parse {}: {e}", path.display())
                });
                let diags = topo.validate();
                assert!(
                    !diags.has_errors(),
                    "{} has validation errors: {:?}",
                    path.display(),
                    diags
                );
                count += 1;
            }
        }
        assert!(count >= 12, "expected at least 12 .nll examples, found {count}");
    }
}
