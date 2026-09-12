//! Lowering pass: AST → Topology.
//!
//! Expands `for` loops, substitutes `let` variables, resolves profiles,
//! and maps AST nodes to the [`crate::types::Topology`] struct.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use super::ast;
use crate::error::Result;
use crate::types;

/// Lower an NLL AST into a Topology (no import support).
pub fn lower(file: &ast::File) -> Result<types::Topology> {
    lower_with_base_dir(file, None, &mut HashSet::new())
}

/// Lower an NLL AST with import resolution from a base directory.
pub fn lower_with_imports(file: &ast::File, base_dir: &Path) -> Result<types::Topology> {
    let mut visited = HashSet::new();
    // Track the current file to detect circular imports
    if let Ok(canonical) = std::fs::canonicalize(base_dir) {
        visited.insert(canonical);
    }
    lower_with_base_dir(file, Some(base_dir), &mut visited)
}

/// Lower an NLL AST with external CLI parameters.
///
/// Parameters are matched against `param` declarations in the top-level file,
/// using the same resolution mechanism as parametric imports.
pub fn lower_with_params(
    file: &ast::File,
    base_dir: Option<&Path>,
    cli_params: &[(String, String)],
) -> Result<types::Topology> {
    let mut visited = HashSet::new();
    if let Some(bd) = base_dir
        && let Ok(canonical) = std::fs::canonicalize(bd)
    {
        visited.insert(canonical);
    }
    lower_with_base_dir_and_params(file, base_dir, &mut visited, cli_params)
}

fn lower_with_base_dir(
    file: &ast::File,
    base_dir: Option<&Path>,
    visited: &mut HashSet<std::path::PathBuf>,
) -> Result<types::Topology> {
    lower_with_base_dir_and_params(file, base_dir, visited, &[])
}

fn lower_with_base_dir_and_params(
    file: &ast::File,
    base_dir: Option<&Path>,
    visited: &mut HashSet<std::path::PathBuf>,
    cli_params: &[(String, String)],
) -> Result<types::Topology> {
    let mut ctx = LowerCtx::new();

    // Resolve CLI params: match against param declarations and inject as variables
    if !cli_params.is_empty() {
        let module_params: Vec<&ast::ParamDef> = file
            .statements
            .iter()
            .filter_map(|s| match s {
                ast::Statement::Param(p) => Some(p),
                _ => None,
            })
            .collect();

        for param in &module_params {
            let value = cli_params
                .iter()
                .find(|(k, _)| k == &param.name)
                .map(|(_, v)| v.clone())
                .or_else(|| param.default.clone())
                .ok_or_else(|| {
                    crate::Error::NllParse(format!(
                        "required parameter '{}' not provided (use --set {}=<value>)",
                        param.name, param.name
                    ))
                })?;
            ctx.variables.insert(param.name.clone(), value);
        }

        // Warn about unknown CLI params
        for (key, _) in cli_params {
            if !module_params.iter().any(|p| &p.name == key) {
                tracing::warn!("unknown parameter '{key}' passed via --set");
            }
        }
    } else {
        // Without CLI params every declared param must carry a default;
        // this used to leave `${name}` unresolved and only fail later
        // (or not at all) instead of naming the missing parameter.
        for stmt in &file.statements {
            if let ast::Statement::Param(p) = stmt {
                match &p.default {
                    Some(default) => {
                        ctx.variables.insert(p.name.clone(), default.clone());
                    }
                    None => {
                        return Err(crate::Error::NllParse(format!(
                            "required parameter '{}' not provided (use --set {}=<value>)",
                            p.name, p.name
                        )));
                    }
                }
            }
        }
    }

    // First pass: collect profiles, variables, and defaults
    for stmt in &file.statements {
        match stmt {
            ast::Statement::Profile(p) => ctx.add_profile(p),
            ast::Statement::Let(l) => ctx.add_variable(l),
            ast::Statement::Defaults(d) => match &d.kind {
                ast::DefaultsKind::Link => ctx.default_link_mtu = d.mtu,
                ast::DefaultsKind::Impair => ctx.default_impair = d.impair.clone(),
                ast::DefaultsKind::Rate => ctx.default_rate = d.rate.clone(),
                ast::DefaultsKind::Named(name) => {
                    ctx.link_profiles.insert(name.clone(), d.clone());
                }
            },
            ast::Statement::Pool(p) => {
                let (ip, prefix) = crate::helpers::parse_cidr(&p.base).map_err(|e| {
                    crate::Error::NllParse(format!(
                        "pool '{}': invalid base '{}': {e}",
                        p.name, p.base
                    ))
                })?;
                let std::net::IpAddr::V4(v4) = ip else {
                    return Err(crate::Error::NllParse(format!(
                        "pool '{}': only IPv4 pools are supported (got '{}')",
                        p.name, p.base
                    )));
                };
                if p.prefix > 32 || p.prefix < prefix {
                    return Err(crate::Error::NllParse(format!(
                        "pool '{}': allocation prefix /{} must be between the pool prefix /{prefix} and /32",
                        p.name, p.prefix
                    )));
                }
                // Mask to the network address so `pool p 10.0.0.5/8 /30`
                // allocates from 10.0.0.0.
                let base = if prefix == 0 {
                    0
                } else {
                    u32::from(v4) & (u32::MAX << (32 - prefix as u32))
                };
                // A /0 pool has 2^32 addresses, which does not fit u32;
                // saturate — no lab will exhaust it.
                let pool_size = 1u64
                    .checked_shl(32 - prefix as u32)
                    .map(|n| n.min(u32::MAX as u64) as u32)
                    .unwrap_or(u32::MAX);
                if ctx.pools.contains_key(&p.name) {
                    return Err(crate::Error::NllParse(format!(
                        "duplicate pool name '{}'",
                        p.name
                    )));
                }
                ctx.pools.insert(
                    p.name.clone(),
                    PoolState {
                        base,
                        pool_size,
                        alloc_prefix: p.prefix,
                        next_offset: 0,
                    },
                );
            }
            _ => {}
        }
    }

    // Inject lab auto-variables
    ctx.variables
        .insert("lab.name".into(), file.lab.name.clone());
    ctx.variables.insert(
        "lab.prefix".into(),
        file.lab
            .prefix
            .clone()
            .unwrap_or_else(|| file.lab.name.clone()),
    );

    // Pre-lowering validation
    validate_ast(file, &ctx)?;

    // Second pass: expand loops and collect all concrete statements
    let expanded = ctx.expand_statements(&file.statements)?;

    // Third pass: lower to Topology
    let mut topology = types::Topology::default();
    topology.lab = lower_lab(&file.lab)?;

    // Resolve imports before lowering statements
    if !file.imports.is_empty() {
        let base = base_dir.ok_or_else(|| {
            crate::Error::NllParse("import requires file-based parsing (use parse_file)".into())
        })?;
        resolve_imports(&file.imports, base, &mut topology, visited)?;
    }

    // Add profiles to topology (for validator cross-referencing)
    for (name, profile_def) in &ctx.profiles {
        topology
            .profiles
            .insert(name.clone(), lower_profile(profile_def));
    }

    for stmt in &expanded {
        match stmt {
            ast::Statement::Node(n) => lower_node(&mut topology, n, &mut ctx)?,
            ast::Statement::Link(l) => lower_link(&mut topology, l, &mut ctx)?,
            ast::Statement::Network(n) => lower_network(&mut topology, n, &ctx.variables)?,
            ast::Statement::Impair(i) => lower_impair(&mut topology, i),
            ast::Statement::Rate(r) => lower_rate(&mut topology, r),
            ast::Statement::Pattern(p) => expand_pattern(&mut topology, p, &mut ctx)?,
            ast::Statement::Validate(v) => {
                for a in &v.assertions {
                    match a {
                        ast::AssertionDef::Reach { from, to } => {
                            topology.assertions.push(types::Assertion::Reach {
                                from: from.clone(),
                                to: to.clone(),
                            });
                        }
                        ast::AssertionDef::NoReach { from, to } => {
                            topology.assertions.push(types::Assertion::NoReach {
                                from: from.clone(),
                                to: to.clone(),
                            });
                        }
                        ast::AssertionDef::TcpConnect {
                            from,
                            to,
                            port,
                            timeout,
                            retries,
                            interval,
                        } => {
                            topology.assertions.push(types::Assertion::TcpConnect {
                                from: from.clone(),
                                to: to.clone(),
                                port: *port,
                                timeout: timeout.clone(),
                                retries: *retries,
                                interval: interval.clone(),
                            });
                        }
                        ast::AssertionDef::LatencyUnder {
                            from,
                            to,
                            max,
                            samples,
                        } => {
                            topology.assertions.push(types::Assertion::LatencyUnder {
                                from: from.clone(),
                                to: to.clone(),
                                max: max.clone(),
                                samples: *samples,
                            });
                        }
                        ast::AssertionDef::RouteHas {
                            node,
                            destination,
                            via,
                            dev,
                        } => {
                            topology.assertions.push(types::Assertion::RouteHas {
                                node: node.clone(),
                                destination: destination.clone(),
                                via: via.clone(),
                                dev: dev.clone(),
                            });
                        }
                        ast::AssertionDef::DnsResolves {
                            from,
                            name,
                            expected_ip,
                        } => {
                            topology.assertions.push(types::Assertion::DnsResolves {
                                from: from.clone(),
                                name: name.clone(),
                                expected_ip: expected_ip.clone(),
                            });
                        }
                    }
                }
            }
            ast::Statement::Scenario(s) => {
                topology.scenarios.push(lower_scenario(s)?);
            }
            ast::Statement::Benchmark(b) => {
                topology.benchmarks.push(lower_benchmark(b)?);
            }
            // Handled in the first pass (profiles, defaults, pools,
            // params) or consumed by `expand_statements` (let / for /
            // if / site never reach this loop).
            ast::Statement::Profile(_)
            | ast::Statement::Let(_)
            | ast::Statement::For(_)
            | ast::Statement::If(_)
            | ast::Statement::Site(_)
            | ast::Statement::Defaults(_)
            | ast::Statement::Param(_)
            | ast::Statement::Pool(_) => {}
        }
    }

    // Post-lowering pass: resolve cross-references like ${router.eth0}
    resolve_cross_refs(&mut topology)?;
    warn_unresolved_refs(&topology);

    // Post-lowering pass: expand `translate` NAT rules using topology addresses
    expand_translate_rules(&mut topology);

    Ok(topology)
}

// ─── Import Resolution ───────────────────────────────────

fn resolve_imports(
    imports: &[ast::ImportDef],
    base_dir: &Path,
    topology: &mut types::Topology,
    visited: &mut HashSet<std::path::PathBuf>,
) -> Result<()> {
    for imp in imports {
        let import_path = base_dir.join(&imp.path);
        let canonical = std::fs::canonicalize(&import_path).map_err(|e| {
            crate::Error::NllParse(format!("cannot resolve import '{}': {e}", imp.path))
        })?;

        // Circular import detection: only flag if we're currently IN this file's
        // import chain (not if it was imported earlier with a different alias).
        // Allow re-importing the same file with different parameters.
        if visited.contains(&canonical) {
            return Err(crate::Error::NllParse(format!(
                "circular import detected: '{}'",
                imp.path
            )));
        }
        visited.insert(canonical.clone());

        // Parse and lower the imported file
        let content = std::fs::read_to_string(&import_path).map_err(|e| {
            crate::Error::NllParse(format!("cannot read import '{}': {e}", imp.path))
        })?;
        let import_name = import_path.display().to_string();
        let tokens = super::lexer::lex(&content)
            .map_err(|e| super::attach_source(e, &content, &import_name))?;
        let mut ast = super::parser::parse_tokens(&tokens, &content)
            .map_err(|e| super::attach_source(e, &content, &import_name))?;

        // Resolve parametric import: inject caller params, apply defaults from `param` stmts
        if !imp.params.is_empty()
            || ast
                .statements
                .iter()
                .any(|s| matches!(s, ast::Statement::Param(_)))
        {
            resolve_import_params(&imp.params, &mut ast)?;
        }

        let import_base = import_path.parent().unwrap_or(base_dir);
        let imported = lower_with_base_dir(&ast, Some(import_base), visited)
            .map_err(|e| super::attach_source(e, &content, &import_name))?;

        // Merge imported topology with alias prefix
        merge_import(topology, &imp.alias, imported);

        // Remove from visited to allow re-importing the same file with different params
        visited.remove(&canonical);
    }
    Ok(())
}

/// Resolve parametric import parameters.
///
/// Collects `param` declarations from the imported file, matches them against
/// caller-provided values, and injects the resolved values as `let` bindings
/// at the beginning of the imported file's statements.
fn resolve_import_params(caller_params: &[(String, String)], ast: &mut ast::File) -> Result<()> {
    // Collect param declarations
    let module_params: Vec<ast::ParamDef> = ast
        .statements
        .iter()
        .filter_map(|s| match s {
            ast::Statement::Param(p) => Some(p.clone()),
            _ => None,
        })
        .collect();

    // For each declared param, use caller value or default
    let mut let_stmts = Vec::new();
    for param in &module_params {
        let value = caller_params
            .iter()
            .find(|(k, _)| k == &param.name)
            .map(|(_, v)| v.clone())
            .or_else(|| param.default.clone())
            .ok_or_else(|| {
                crate::Error::NllParse(format!(
                    "required parameter '{}' not provided in import",
                    param.name
                ))
            })?;
        let_stmts.push(ast::Statement::Let(ast::LetDef {
            name: param.name.clone(),
            value,
        }));
    }

    // Warn about unknown caller params
    for (key, _) in caller_params {
        if !module_params.iter().any(|p| &p.name == key) {
            tracing::warn!("unknown parameter '{key}' passed to import");
        }
    }

    // Remove param statements and prepend let bindings
    ast.statements
        .retain(|s| !matches!(s, ast::Statement::Param(_)));
    let mut new_stmts = let_stmts;
    new_stmts.append(&mut ast.statements);
    ast.statements = new_stmts;

    Ok(())
}

fn merge_import(main: &mut types::Topology, alias: &str, imported: types::Topology) {
    // Merge nodes with prefixed names (use - separator for parser compatibility).
    // Profile references are prefixed alongside the profiles themselves so
    // the validator's dangling-profile check keeps matching.
    for (name, mut node) in imported.nodes {
        node.profiles = node
            .profiles
            .iter()
            .map(|p| format!("{alias}-{p}"))
            .collect();
        node.depends_on = node
            .depends_on
            .into_iter()
            .map(|d| format!("{alias}-{d}"))
            .collect();
        main.nodes.insert(format!("{alias}-{name}"), node);
    }

    // Merge links with prefixed endpoint references
    for mut link in imported.links {
        for ep in &mut link.endpoints {
            *ep = prefix_endpoint(alias, ep);
        }
        main.links.push(link);
    }

    // Merge networks with prefixed names and member references
    for (name, mut network) in imported.networks {
        for member in &mut network.members {
            *member = prefix_endpoint(alias, member);
        }
        // Prefix port keys
        let old_ports = std::mem::take(&mut network.ports);
        for (key, port) in old_ports {
            network.ports.insert(prefix_endpoint(alias, &key), port);
        }
        main.networks.insert(format!("{alias}-{name}"), network);
    }

    // Merge impairments with prefixed endpoint keys
    for (key, imp) in imported.impairments {
        main.impairments.insert(prefix_endpoint(alias, &key), imp);
    }

    // Merge rate limits with prefixed endpoint keys
    for (key, rl) in imported.rate_limits {
        main.rate_limits.insert(prefix_endpoint(alias, &key), rl);
    }

    // Merge profiles with prefixed names
    for (name, profile) in imported.profiles {
        main.profiles.insert(format!("{alias}-{name}"), profile);
    }

    // Assertions, scenarios and benchmarks travel with the module too
    // (they used to be dropped silently), with their node references
    // prefixed like everything else.
    let pn = |n: &str| format!("{alias}-{n}");
    for a in imported.assertions {
        main.assertions.push(prefix_lowered_assertion(a, &pn));
    }
    for mut sc in imported.scenarios {
        for step in &mut sc.steps {
            for action in &mut step.actions {
                match action {
                    types::ScenarioAction::Down(ep)
                    | types::ScenarioAction::Up(ep)
                    | types::ScenarioAction::Clear(ep) => *ep = prefix_endpoint(alias, ep),
                    types::ScenarioAction::Validate(list) => {
                        *list = std::mem::take(list)
                            .into_iter()
                            .map(|a| prefix_lowered_assertion(a, &pn))
                            .collect();
                    }
                    types::ScenarioAction::Exec { node, .. } => *node = pn(node),
                    types::ScenarioAction::Log(_) => {}
                }
            }
        }
        main.scenarios.push(sc);
    }
    for mut b in imported.benchmarks {
        for t in &mut b.tests {
            match t {
                types::BenchmarkTest::Iperf3 { from, to, .. }
                | types::BenchmarkTest::Ping { from, to, .. } => {
                    *from = pn(from);
                    *to = pn(to);
                }
            }
        }
        main.benchmarks.push(b);
    }
}

fn prefix_lowered_assertion(a: types::Assertion, pn: &dyn Fn(&str) -> String) -> types::Assertion {
    use types::Assertion as A;
    match a {
        A::Reach { from, to } => A::Reach {
            from: pn(&from),
            to: pn(&to),
        },
        A::NoReach { from, to } => A::NoReach {
            from: pn(&from),
            to: pn(&to),
        },
        A::TcpConnect {
            from,
            to,
            port,
            timeout,
            retries,
            interval,
        } => A::TcpConnect {
            from: pn(&from),
            to: pn(&to),
            port,
            timeout,
            retries,
            interval,
        },
        A::LatencyUnder {
            from,
            to,
            max,
            samples,
        } => A::LatencyUnder {
            from: pn(&from),
            to: pn(&to),
            max,
            samples,
        },
        A::RouteHas {
            node,
            destination,
            via,
            dev,
        } => A::RouteHas {
            node: pn(&node),
            destination,
            via,
            dev,
        },
        A::DnsResolves {
            from,
            name,
            expected_ip,
        } => A::DnsResolves {
            from: pn(&from),
            name,
            expected_ip,
        },
    }
}

fn prefix_endpoint(alias: &str, endpoint: &str) -> String {
    if let Some((node, iface)) = endpoint.split_once(':') {
        format!("{alias}-{node}:{iface}")
    } else {
        format!("{alias}-{endpoint}")
    }
}

// ─── Cross-Reference Resolution ─────────────────────────

/// Build a map of node:interface → IP address from all link definitions.
fn build_address_map(topology: &types::Topology) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for (ep_str, addr) in link.endpoints.iter().zip(addrs.iter()) {
                // Extract IP without prefix length
                let ip = addr.split('/').next().unwrap_or(addr);
                map.insert(ep_str.clone(), ip.to_string());
            }
        }
    }
    // Also collect explicit interface addresses
    for (node_name, node) in &topology.nodes {
        for (iface_name, iface_cfg) in &node.interfaces {
            if let Some(addr) = iface_cfg.addresses.first() {
                let ip = addr.split('/').next().unwrap_or(addr);
                let key = format!("{node_name}:{iface_name}");
                map.entry(key).or_insert_with(|| ip.to_string());
            }
        }
    }
    // …and bridge-network port addresses (keys are `node:iface`), so
    // `${host.eth0}` and `translate` work for members of a `network`.
    for network in topology.networks.values() {
        for (endpoint, port) in &network.ports {
            if let Some(addr) = port.addresses.first() {
                let ip = addr.split('/').next().unwrap_or(addr);
                map.entry(endpoint.clone())
                    .or_insert_with(|| ip.to_string());
            }
        }
    }
    map
}

// ─── NAT Translate Expansion ────────────────────────────

/// Expand `Translate` NAT rules into per-host DNAT rules by scanning
/// the topology for addresses in the destination range.
fn expand_translate_rules(topology: &mut types::Topology) {
    // Collect all assigned IPv4 addresses from links and node interfaces.
    let mut assigned: Vec<std::net::Ipv4Addr> = Vec::new();
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for addr in addrs {
                if let Some(ip) = addr.split('/').next()
                    && let Ok(v4) = ip.parse::<std::net::Ipv4Addr>()
                {
                    assigned.push(v4);
                }
            }
        }
    }
    for node in topology.nodes.values() {
        for iface_cfg in node.interfaces.values() {
            for addr in &iface_cfg.addresses {
                if let Some(ip) = addr.split('/').next()
                    && let Ok(v4) = ip.parse::<std::net::Ipv4Addr>()
                {
                    assigned.push(v4);
                }
            }
        }
    }

    // For each node with NAT rules, expand Translate rules.
    for node in topology.nodes.values_mut() {
        let nat = match &mut node.nat {
            Some(n) => n,
            None => continue,
        };
        let mut has_translate = false;
        for rule in &nat.rules {
            if rule.action == types::NatAction::Translate {
                has_translate = true;
                break;
            }
        }
        if !has_translate {
            continue;
        }

        let mut expanded = Vec::new();
        for rule in &nat.rules {
            if rule.action != types::NatAction::Translate {
                expanded.push(rule.clone());
                continue;
            }
            let src_cidr = match &rule.src {
                Some(s) => s.as_str(),
                None => continue,
            };
            let dst_cidr = match &rule.target {
                Some(s) => s.as_str(),
                None => continue,
            };
            let Some((src_net, src_prefix)) = parse_v4_cidr_pair(src_cidr) else {
                continue;
            };
            let Some((dst_net, dst_prefix)) = parse_v4_cidr_pair(dst_cidr) else {
                continue;
            };
            for &addr in &assigned {
                if in_prefix(addr, dst_net, dst_prefix) {
                    let mapped =
                        map_translate_address(addr, dst_net, dst_prefix, src_net, src_prefix);
                    expanded.push(types::NatRule {
                        action: types::NatAction::Dnat,
                        src: None,
                        dst: Some(format!("{mapped}/32")),
                        target: Some(addr.to_string()),
                        target_port: None,
                    });
                }
            }
        }
        nat.rules = expanded;
    }
}

/// Parse "A.B.C.D/N" into (Ipv4Addr, u8).
fn parse_v4_cidr_pair(s: &str) -> Option<(std::net::Ipv4Addr, u8)> {
    let (ip_str, prefix_str) = s.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip_str.parse().ok()?;
    let prefix: u8 = prefix_str.parse().ok()?;
    Some((ip, prefix))
}

/// Check whether `addr` is inside the network defined by `net/prefix`.
fn in_prefix(addr: std::net::Ipv4Addr, net: std::net::Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = !0u32 << (32 - prefix);
    (u32::from(addr) & mask) == (u32::from(net) & mask)
}

/// Map an address from the destination range to the source range,
/// preserving host bits. E.g., 172.100.1.18 with dst /16 → src /8
/// yields 144.0.1.18.
fn map_translate_address(
    addr: std::net::Ipv4Addr,
    _dst_net: std::net::Ipv4Addr,
    dst_prefix: u8,
    src_net: std::net::Ipv4Addr,
    src_prefix: u8,
) -> std::net::Ipv4Addr {
    let host_bits = u32::from(addr) & !(!0u32 << (32 - dst_prefix));
    let src_masked = u32::from(src_net) & (!0u32 << (32 - src_prefix));
    std::net::Ipv4Addr::from(src_masked | host_bits)
}

/// Replace `${node.iface}` references with resolved IP addresses.
fn resolve_ref(s: &str, addr_map: &BTreeMap<String, String>) -> Result<String> {
    let mut result = s.to_string();
    // Find all ${...} patterns that contain a dot (cross-references)
    let mut search_from = 0;
    while let Some(start) = result[search_from..].find("${") {
        let start = search_from + start;
        if let Some(end) = result[start..].find('}') {
            let end = start + end;
            let expr = &result[start + 2..end];
            // Only resolve dot-references (node.iface), not arithmetic
            if let Some(dot) = expr.find('.') {
                let node = &expr[..dot];
                let iface = &expr[dot + 1..];
                let key = format!("{node}:{iface}");
                if let Some(addr) = addr_map.get(&key) {
                    result.replace_range(start..=end, addr);
                    search_from = start + addr.len();
                    continue;
                }
            }
        }
        search_from = start + 2;
    }
    Ok(result)
}

/// Resolve cross-references in all topology string fields.
fn resolve_cross_refs(topology: &mut types::Topology) -> Result<()> {
    let addr_map = build_address_map(topology);
    if addr_map.is_empty() {
        return Ok(());
    }

    // Resolve references in node routes and firewall rules
    for node in topology.nodes.values_mut() {
        for route in node.routes.values_mut() {
            if let Some(via) = &mut route.via {
                *via = resolve_ref(via, &addr_map)?;
            }
        }
        if let Some(fw) = &mut node.firewall {
            for rule in &mut fw.rules {
                if let Some(match_expr) = &mut rule.match_expr {
                    *match_expr = resolve_ref(match_expr, &addr_map)?;
                }
            }
        }
    }

    Ok(())
}

/// Warn about unresolved cross-references remaining after lowering.
fn warn_unresolved_refs(topology: &types::Topology) {
    for (node_name, node) in &topology.nodes {
        for (dest, route) in &node.routes {
            if let Some(via) = &route.via
                && via.contains("${")
            {
                tracing::warn!("unresolved reference in route '{dest}' on '{node_name}': {via}");
            }
        }
        if let Some(fw) = &node.firewall {
            for rule in &fw.rules {
                if let Some(expr) = &rule.match_expr
                    && expr.contains("${")
                {
                    tracing::warn!(
                        "unresolved reference in firewall rule on '{node_name}': {expr}"
                    );
                }
            }
        }
    }
}

// ─── Context ──────────────────────────────────────────────

/// State for a named subnet pool.
struct PoolState {
    base: u32,        // base network address as u32
    pool_size: u32,   // total addresses in the pool (for exhaustion check)
    alloc_prefix: u8, // allocation prefix size (e.g., 30 for /30)
    next_offset: u32, // next allocation offset from base
}

impl PoolState {
    /// Allocate the next `/alloc_prefix` block, returned as a CIDR
    /// string, or an error when the pool is exhausted.
    fn allocate_subnet(&mut self, pool_name: &str) -> Result<String> {
        let subnet_size = 1u64 << (32 - self.alloc_prefix as u32);
        let next = self.next_offset as u64 + subnet_size;
        let exhausted = next > self.pool_size as u64
            || (self.base as u64 + self.next_offset as u64 + subnet_size) > (u32::MAX as u64 + 1);
        if exhausted {
            return Err(crate::Error::NllParse(format!(
                "pool '{pool_name}' exhausted: {} /{} blocks of {} addresses already allocated",
                self.next_offset as u64 / subnet_size,
                self.alloc_prefix,
                self.pool_size
            )));
        }
        let network = self.base + self.next_offset;
        self.next_offset = next as u32;
        let ip = std::net::Ipv4Addr::from(network);
        Ok(format!("{ip}/{}", self.alloc_prefix))
    }

    /// Allocate a single address from the pool (for /32 loopback, etc.)
    fn allocate(&mut self, pool_name: &str) -> Result<String> {
        self.allocate_subnet(pool_name)
    }
}

struct LowerCtx {
    profiles: BTreeMap<String, ast::ProfileDef>,
    variables: BTreeMap<String, String>,
    default_link_mtu: Option<u32>,
    default_impair: Option<ast::ImpairProps>,
    default_rate: Option<ast::RateProps>,
    pools: BTreeMap<String, PoolState>,
    link_profiles: BTreeMap<String, ast::DefaultsDef>,
}

impl LowerCtx {
    fn new() -> Self {
        Self {
            profiles: BTreeMap::new(),
            variables: BTreeMap::new(),
            default_link_mtu: None,
            link_profiles: BTreeMap::new(),
            default_impair: None,
            default_rate: None,
            pools: BTreeMap::new(),
        }
    }

    fn add_profile(&mut self, p: &ast::ProfileDef) {
        if self.profiles.contains_key(&p.name) {
            tracing::warn!(
                "duplicate profile name '{}' — later definition wins",
                p.name
            );
        }
        self.profiles.insert(p.name.clone(), p.clone());
    }

    fn add_variable(&mut self, l: &ast::LetDef) {
        self.variables.insert(l.name.clone(), l.value.clone());
    }

    fn expand_statements(&self, stmts: &[ast::Statement]) -> Result<Vec<ast::Statement>> {
        let mut vars = self.variables.clone();
        self.expand_into(stmts, &mut vars)
    }

    /// Expand `for` / `let` / `if` / `site` into a flat list of concrete,
    /// fully interpolated statements.
    ///
    /// Scoping: `let` bindings and loop variables are visible to the
    /// statements that follow them *inside the same block* and are
    /// restored to their previous value (or removed) when the block
    /// ends, so an outer `let i = 99` survives an inner `for i in …`
    /// and a `let` inside a loop body does not leak past the loop
    /// (issue #20). `site` blocks are expanded like any other block
    /// and every resulting statement — nodes, links, networks,
    /// impairments, assertions, scenarios, benchmarks, patterns — is
    /// name-prefixed with `<site>-` (issue #18).
    fn expand_into(
        &self,
        stmts: &[ast::Statement],
        vars: &mut BTreeMap<String, String>,
    ) -> Result<Vec<ast::Statement>> {
        let mut result = Vec::new();

        for stmt in stmts {
            match stmt {
                ast::Statement::For(f) => {
                    result.extend(self.expand_for(f, vars)?);
                }
                ast::Statement::Let(l) => {
                    let value = interpolate(&l.value, vars);
                    vars.insert(l.name.clone(), value);
                }
                ast::Statement::If(if_def) => {
                    let cond = interpolate(&if_def.condition, vars);
                    if eval_condition(&cond, vars) {
                        let saved = vars.clone();
                        let inner = self.expand_into(&if_def.body, vars)?;
                        *vars = saved;
                        result.extend(inner);
                    }
                }
                ast::Statement::Site(site) => {
                    let prefix = format!("{}-", interpolate(&site.name, vars));
                    let saved = vars.clone();
                    let inner = self.expand_into(&site.body, vars)?;
                    *vars = saved;
                    result.extend(inner.into_iter().map(|st| prefix_statement(st, &prefix)));
                }
                other => {
                    result.push(interpolate_statement(other, vars));
                }
            }
        }

        Ok(result)
    }

    fn expand_for(
        &self,
        for_loop: &ast::ForLoop,
        vars: &mut BTreeMap<String, String>,
    ) -> Result<Vec<ast::Statement>> {
        let values = range_values(&for_loop.range, &for_loop.var, vars)?;
        let len = values.len();
        let saved = vars.clone();
        let mut result = Vec::new();

        for (idx, value) in values.iter().enumerate() {
            // Each iteration starts from the enclosing scope so a `let`
            // from a previous iteration cannot bleed into the next one.
            *vars = saved.clone();
            vars.insert(for_loop.var.clone(), value.clone());
            vars.insert("loop.index".into(), idx.to_string());
            vars.insert("loop.first".into(), (idx == 0).to_string());
            vars.insert("loop.last".into(), (idx + 1 == len).to_string());
            result.extend(self.expand_into(&for_loop.body, vars)?);
        }

        *vars = saved;
        Ok(result)
    }
}

/// Upper bound on the iterations of one `for` range. A typo such as
/// `for i in 1..999999999` used to materialise the whole range as a
/// `Vec<String>` before any check ran (issue #21).
pub const MAX_LOOP_ITERATIONS: i64 = 100_000;

/// Materialise a `for` range, rejecting empty and oversized ranges.
/// `DynRange` bounds (`1..${count}`) are interpolated with `vars` first.
fn range_values(
    range: &ast::ForRange,
    var: &str,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    match range {
        ast::ForRange::DynRange { start, end } => {
            let bound = |raw: &str, which: &str| -> Result<i64> {
                let v = interpolate(raw, vars);
                v.trim().parse::<i64>().map_err(|_| {
                    crate::Error::NllParse(format!(
                        "for loop '{var}': {which} bound '{raw}' resolved to '{v}', which is not an integer"
                    ))
                })
            };
            let resolved = ast::ForRange::IntRange {
                start: bound(start, "start")?,
                end: bound(end, "end")?,
            };
            range_values(&resolved, var, vars)
        }
        ast::ForRange::IntRange { start, end } => {
            if end < start {
                return Err(crate::Error::NllParse(format!(
                    "for loop '{var}' has empty range {start}..{end}"
                )));
            }
            let n = (*end as i128) - (*start as i128) + 1;
            if n > MAX_LOOP_ITERATIONS as i128 {
                return Err(crate::Error::NllParse(format!(
                    "for loop '{var}' would iterate {n} times; the limit is {MAX_LOOP_ITERATIONS}"
                )));
            }
            Ok((*start..=*end).map(|i| i.to_string()).collect())
        }
        ast::ForRange::List(items) => {
            if items.is_empty() {
                return Err(crate::Error::NllParse(format!(
                    "for loop '{var}' has empty list"
                )));
            }
            Ok(items.clone())
        }
    }
}

// ─── Site prefixing ───────────────────────────────────────

/// `node:iface` → `<prefix>node:iface`; bare names are prefixed too.
fn prefix_ep(prefix: &str, ep: &str) -> String {
    match ep.split_once(':') {
        Some((node, iface)) => format!("{prefix}{node}:{iface}"),
        None => format!("{prefix}{ep}"),
    }
}

fn prefix_assertion(a: ast::AssertionDef, prefix: &str) -> ast::AssertionDef {
    use ast::AssertionDef as A;
    match a {
        A::Reach { from, to } => A::Reach {
            from: format!("{prefix}{from}"),
            to: format!("{prefix}{to}"),
        },
        A::NoReach { from, to } => A::NoReach {
            from: format!("{prefix}{from}"),
            to: format!("{prefix}{to}"),
        },
        A::TcpConnect {
            from,
            to,
            port,
            timeout,
            retries,
            interval,
        } => A::TcpConnect {
            from: format!("{prefix}{from}"),
            to: format!("{prefix}{to}"),
            port,
            timeout,
            retries,
            interval,
        },
        A::LatencyUnder {
            from,
            to,
            max,
            samples,
        } => A::LatencyUnder {
            from: format!("{prefix}{from}"),
            to: format!("{prefix}{to}"),
            max,
            samples,
        },
        A::RouteHas {
            node,
            destination,
            via,
            dev,
        } => A::RouteHas {
            node: format!("{prefix}{node}"),
            destination,
            via,
            dev,
        },
        A::DnsResolves {
            from,
            name,
            expected_ip,
        } => A::DnsResolves {
            from: format!("{prefix}{from}"),
            name,
            expected_ip,
        },
    }
}

/// Apply a `site` prefix to every name a statement introduces or
/// references. Statements that carry no topology names (profiles,
/// defaults, pools, params) pass through unchanged.
fn prefix_statement(st: ast::Statement, prefix: &str) -> ast::Statement {
    use ast::Statement as S;
    match st {
        S::Node(mut n) => {
            n.name = format!("{prefix}{}", n.name);
            n.depends_on = n
                .depends_on
                .into_iter()
                .map(|d| format!("{prefix}{d}"))
                .collect();
            S::Node(n)
        }
        S::Link(mut l) => {
            l.left_node = format!("{prefix}{}", l.left_node);
            l.right_node = format!("{prefix}{}", l.right_node);
            S::Link(l)
        }
        S::Network(mut n) => {
            n.name = format!("{prefix}{}", n.name);
            n.members = n.members.iter().map(|m| prefix_ep(prefix, m)).collect();
            for port in &mut n.ports {
                port.endpoint = prefix_ep(prefix, &port.endpoint);
            }
            for imp in &mut n.impairments {
                imp.src = prefix_ep(prefix, &imp.src);
                imp.dst = prefix_ep(prefix, &imp.dst);
            }
            for l in &mut n.loops {
                prefix_network_loop(prefix, l);
            }
            S::Network(n)
        }
        S::Impair(mut i) => {
            i.node = format!("{prefix}{}", i.node);
            S::Impair(i)
        }
        S::Rate(mut r) => {
            r.node = format!("{prefix}{}", r.node);
            S::Rate(r)
        }
        S::Pattern(mut p) => {
            p.name = format!("{prefix}{}", p.name);
            S::Pattern(p)
        }
        S::Validate(v) => S::Validate(ast::ValidateDef {
            assertions: v
                .assertions
                .into_iter()
                .map(|a| prefix_assertion(a, prefix))
                .collect(),
        }),
        S::Scenario(mut sc) => {
            for step in &mut sc.steps {
                step.actions = std::mem::take(&mut step.actions)
                    .into_iter()
                    .map(|a| match a {
                        ast::ScenarioActionDef::Down(ep) => {
                            ast::ScenarioActionDef::Down(prefix_ep(prefix, &ep))
                        }
                        ast::ScenarioActionDef::Up(ep) => {
                            ast::ScenarioActionDef::Up(prefix_ep(prefix, &ep))
                        }
                        ast::ScenarioActionDef::Clear(ep) => {
                            ast::ScenarioActionDef::Clear(prefix_ep(prefix, &ep))
                        }
                        ast::ScenarioActionDef::Validate(list) => ast::ScenarioActionDef::Validate(
                            list.into_iter()
                                .map(|a| prefix_assertion(a, prefix))
                                .collect(),
                        ),
                        ast::ScenarioActionDef::Exec { node, cmd } => {
                            ast::ScenarioActionDef::Exec {
                                node: format!("{prefix}{node}"),
                                cmd,
                            }
                        }
                        other => other,
                    })
                    .collect();
            }
            S::Scenario(sc)
        }
        S::Benchmark(mut b) => {
            for t in &mut b.tests {
                match t {
                    ast::BenchmarkTestDef::Iperf3 { from, to, .. }
                    | ast::BenchmarkTestDef::Ping { from, to, .. } => {
                        *from = format!("{prefix}{from}");
                        *to = format!("{prefix}{to}");
                    }
                }
            }
            S::Benchmark(b)
        }
        other => other,
    }
}

// ─── Interpolation ────────────────────────────────────────

/// Replace `${expr}` with its evaluated value.
///
/// Supports arithmetic (`${i + 1}`, `${(i - 1) * 2}`, `${i % 3}`),
/// ternary conditionals (`${env == "prod" ? "5ms" : "50ms"}`),
/// and simple variable lookup (`${var}`).
pub(crate) fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> String {
    // Run interpolation repeatedly until stable (handles nested ${leaf${i}})
    let mut current = template.to_string();
    for _ in 0..10 {
        let next = interpolate_once(&current, vars);
        if next == current {
            break;
        }
        current = next;
    }
    // Evaluate deferred function calls (@fn:name(args))
    resolve_functions(&current)
}

/// Resolve all `@fn:name(arg1, arg2, ...)` in a string.
fn resolve_functions(s: &str) -> String {
    if !s.contains("@fn:") {
        return s.to_string();
    }
    // Find @fn:name(args) patterns and evaluate them
    let mut result = s.to_string();
    // Iterate until no more @fn: patterns (handles nested calls)
    for _ in 0..10 {
        if !result.contains("@fn:") {
            break;
        }
        if let Some(start) = result.find("@fn:") {
            // Find the matching closing paren
            let after_fn = &result[start + 4..];
            if let Some(paren_start) = after_fn.find('(') {
                let name = &after_fn[..paren_start];
                let rest = &after_fn[paren_start + 1..];
                // Find matching closing paren (handle nesting)
                let mut depth = 1;
                let mut end = 0;
                for (i, ch) in rest.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let args_str = &rest[..end];
                // Split args by comma (respecting nested parens)
                let args = split_function_args(args_str);
                // Recursively resolve args first
                let resolved_args: Vec<String> =
                    args.iter().map(|a| resolve_functions(a.trim())).collect();

                match crate::ipfunc::eval_function(name, &resolved_args) {
                    Ok(value) => {
                        let full_match_end = start + 4 + paren_start + 1 + end + 1;
                        result =
                            format!("{}{}{}", &result[..start], value, &result[full_match_end..]);
                    }
                    Err(e) => {
                        tracing::warn!("function evaluation failed: {e}");
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }
    result
}

/// Split function arguments by comma, respecting nested parentheses.
fn split_function_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

fn interpolate_once(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut expr = String::new();
            let mut depth = 1;
            while let Some(&c) = chars.peek() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        chars.next();
                        break;
                    }
                }
                expr.push(c);
                chars.next();
            }

            // If expr contains nested ${}, recursively interpolate the inner part first
            let resolved_expr = if expr.contains("${") {
                interpolate_once(&expr, vars)
            } else {
                expr
            };
            let value = eval_expr(&resolved_expr, vars);
            result.push_str(&value);
        } else {
            result.push(ch);
        }
    }

    result
}

/// Evaluate an expression with support for:
/// - Arithmetic with precedence: `+`, `-`, `*`, `/`, `%`
/// - Compound expressions: `(i - 1) * 2 + 1`
/// - Ternary conditionals: `var == "value" ? true_val : false_val`
/// - Variable lookup: `var`
fn eval_expr(expr: &str, vars: &BTreeMap<String, String>) -> String {
    let expr = expr.trim();

    // Ternary conditional: `cond ? true_val : false_val`
    if let Some(result) = eval_ternary(expr, vars) {
        return result;
    }

    // Arithmetic expression
    let tokens = tokenize_arith(expr, vars);
    if !tokens.is_empty()
        && let Ok(val) = parse_arith_expr(&tokens, &mut 0)
    {
        return val.to_string();
    }

    // Simple variable lookup
    vars.get(expr)
        .cloned()
        .unwrap_or_else(|| format!("${{{expr}}}"))
}

/// Evaluate a ternary expression: `var == "lit" ? true_val : false_val`
fn eval_ternary(expr: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    let q = expr.find('?')?;
    let condition = expr[..q].trim();
    let rest = expr[q + 1..].trim();
    let colon = rest.find(':')?;
    let true_val = rest[..colon].trim();
    let false_val = rest[colon + 1..].trim();

    let result = if let Some((left, right)) = condition.split_once("!=") {
        resolve_var(left.trim(), vars) != resolve_var(right.trim(), vars)
    } else if let Some((left, right)) = condition.split_once("==") {
        resolve_var(left.trim(), vars) == resolve_var(right.trim(), vars)
    } else {
        return None;
    };

    let chosen = if result { true_val } else { false_val };
    Some(resolve_var(chosen, vars))
}

/// Evaluate a boolean condition. Supports:
/// - `==`, `!=` comparison
/// - `<`, `>`, `<=`, `>=` numeric comparison
/// - `&&`, `||` boolean operators
/// - Numeric literals and variable references
pub fn eval_condition(expr: &str, vars: &BTreeMap<String, String>) -> bool {
    let expr = expr.trim();

    // Handle || (lowest precedence)
    if let Some(pos) = find_operator(expr, "||") {
        let left = &expr[..pos];
        let right = &expr[pos + 2..];
        return eval_condition(left, vars) || eval_condition(right, vars);
    }

    // Handle &&
    if let Some(pos) = find_operator(expr, "&&") {
        let left = &expr[..pos];
        let right = &expr[pos + 2..];
        return eval_condition(left, vars) && eval_condition(right, vars);
    }

    // Handle comparison operators (order matters: check multi-char first)
    let comparisons: &[(&str, &str)] = &[
        ("!=", "ne"),
        ("==", "eq"),
        ("<=", "le"),
        (">=", "ge"),
        ("<", "lt"),
        (">", "gt"),
    ];
    for &(op, kind) in comparisons {
        if let Some((left, right)) = expr.split_once(op) {
            // Don't split on < when it's actually <=
            if op == "<" && right.starts_with('=') {
                continue;
            }
            if op == ">" && right.starts_with('=') {
                continue;
            }
            let l = resolve_var(left.trim(), vars);
            let r = resolve_var(right.trim(), vars);
            return match kind {
                "eq" => l == r,
                "ne" => l != r,
                "lt" | "le" | "gt" | "ge" => {
                    let li = l.parse::<i64>().unwrap_or(0);
                    let ri = r.parse::<i64>().unwrap_or(0);
                    match kind {
                        "lt" => li < ri,
                        "le" => li <= ri,
                        "gt" => li > ri,
                        "ge" => li >= ri,
                        _ => false,
                    }
                }
                _ => false,
            };
        }
    }

    // Truthiness: non-empty, non-zero, non-"false"
    let resolved = resolve_var(expr, vars);
    !resolved.is_empty() && resolved != "0" && resolved != "false"
}

/// Find operator position, skipping parenthesized expressions.
fn find_operator(expr: &str, op: &str) -> Option<usize> {
    let mut depth = 0;
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ if depth == 0
                && i + op_bytes.len() <= bytes.len()
                && &bytes[i..i + op_bytes.len()] == op_bytes =>
            {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

/// Resolve a value: strip quotes from string literals, look up variables.
fn resolve_var(s: &str, vars: &BTreeMap<String, String>) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return s[1..s.len() - 1].to_string();
    }
    vars.get(s).cloned().unwrap_or_else(|| s.to_string())
}

// ─── Arithmetic expression parser ────────────────────────

#[derive(Debug, Clone, Copy)]
enum ArithTok {
    Num(i64),
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    LParen,
    RParen,
}

/// Tokenize an arithmetic expression, resolving variables to numbers.
fn tokenize_arith(expr: &str, vars: &BTreeMap<String, String>) -> Vec<ArithTok> {
    let mut tokens = Vec::new();
    let mut chars = expr.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' => {
                chars.next();
            }
            '+' => {
                chars.next();
                tokens.push(ArithTok::Plus);
            }
            '-' => {
                chars.next();
                // Unary minus: after operator, open paren, or at start
                let is_unary = tokens.is_empty()
                    || matches!(
                        tokens.last(),
                        Some(
                            ArithTok::Plus
                                | ArithTok::Minus
                                | ArithTok::Mul
                                | ArithTok::Div
                                | ArithTok::Mod
                                | ArithTok::LParen
                        )
                    );
                if is_unary {
                    // Parse the number/variable and negate
                    let val = read_operand(&mut chars, vars);
                    if let Some(n) = val {
                        tokens.push(ArithTok::Num(-n));
                    } else {
                        return vec![];
                    } // can't parse → bail
                } else {
                    tokens.push(ArithTok::Minus);
                }
            }
            '*' => {
                chars.next();
                tokens.push(ArithTok::Mul);
            }
            '/' => {
                chars.next();
                tokens.push(ArithTok::Div);
            }
            '%' => {
                chars.next();
                tokens.push(ArithTok::Mod);
            }
            '(' => {
                chars.next();
                tokens.push(ArithTok::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(ArithTok::RParen);
            }
            '0'..='9' => {
                let mut num = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() {
                        num.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Ok(n) = num.parse::<i64>() {
                    tokens.push(ArithTok::Num(n));
                } else {
                    return vec![];
                }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(val) = vars.get(&name) {
                    if let Ok(n) = val.parse::<i64>() {
                        tokens.push(ArithTok::Num(n));
                    } else {
                        return vec![];
                    } // non-numeric variable → bail to string lookup
                } else {
                    return vec![]; // unknown variable → bail
                }
            }
            _ => return vec![], // unexpected char → bail
        }
    }
    tokens
}

/// Read a numeric operand (number or variable) from the char stream.
fn read_operand(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    vars: &BTreeMap<String, String>,
) -> Option<i64> {
    while let Some(&' ') = chars.peek() {
        chars.next();
    }
    if let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut num = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    num.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            num.parse().ok()
        } else if c.is_alphabetic() || c == '_' {
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '_' || c == '.' {
                    name.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            vars.get(&name)?.parse().ok()
        } else {
            None
        }
    } else {
        None
    }
}

/// Parse expression: handles `+` and `-` (lowest precedence).
fn parse_arith_expr(tokens: &[ArithTok], pos: &mut usize) -> std::result::Result<i64, ()> {
    let mut left = parse_arith_term(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithTok::Plus => {
                *pos += 1;
                left += parse_arith_term(tokens, pos)?;
            }
            ArithTok::Minus => {
                *pos += 1;
                left -= parse_arith_term(tokens, pos)?;
            }
            _ => break,
        }
    }
    Ok(left)
}

/// Parse term: handles `*`, `/`, `%` (higher precedence).
fn parse_arith_term(tokens: &[ArithTok], pos: &mut usize) -> std::result::Result<i64, ()> {
    let mut left = parse_arith_factor(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithTok::Mul => {
                *pos += 1;
                left *= parse_arith_factor(tokens, pos)?;
            }
            ArithTok::Div => {
                *pos += 1;
                let right = parse_arith_factor(tokens, pos)?;
                if right == 0 {
                    return Err(());
                }
                left /= right;
            }
            ArithTok::Mod => {
                *pos += 1;
                let right = parse_arith_factor(tokens, pos)?;
                if right == 0 {
                    return Err(());
                }
                left %= right;
            }
            _ => break,
        }
    }
    Ok(left)
}

/// Parse factor: number or parenthesized expression.
fn parse_arith_factor(tokens: &[ArithTok], pos: &mut usize) -> std::result::Result<i64, ()> {
    if *pos >= tokens.len() {
        return Err(());
    }
    match tokens[*pos] {
        ArithTok::Num(n) => {
            *pos += 1;
            Ok(n)
        }
        ArithTok::LParen => {
            *pos += 1;
            let val = parse_arith_expr(tokens, pos)?;
            if *pos < tokens.len() && matches!(tokens[*pos], ArithTok::RParen) {
                *pos += 1;
            }
            Ok(val)
        }
        _ => Err(()),
    }
}

/// Interpolate all string fields in a statement.
fn interpolate_statement(stmt: &ast::Statement, vars: &BTreeMap<String, String>) -> ast::Statement {
    match stmt {
        ast::Statement::Node(n) => ast::Statement::Node(interpolate_node(n, vars)),
        ast::Statement::Link(l) => ast::Statement::Link(interpolate_link(l, vars)),
        ast::Statement::Network(n) => ast::Statement::Network(interpolate_network(n, vars)),
        ast::Statement::Impair(i) => ast::Statement::Impair(interpolate_impair_def(i, vars)),
        ast::Statement::Rate(r) => ast::Statement::Rate(interpolate_rate_def(r, vars)),
        ast::Statement::Profile(p) => ast::Statement::Profile(p.clone()),
        ast::Statement::Defaults(d) => ast::Statement::Defaults(d.clone()),
        ast::Statement::Pool(p) => ast::Statement::Pool(p.clone()),
        ast::Statement::Pattern(p) => ast::Statement::Pattern(ast::PatternDef {
            kind: match &p.kind {
                ast::PatternKind::Star { hub } => ast::PatternKind::Star { hub: i(hub, vars) },
                other => other.clone(),
            },
            name: i(&p.name, vars),
            nodes: p.nodes.iter().map(|n| i(n, vars)).collect(),
            count: p.count,
            pool: io(&p.pool, vars),
            profile: io(&p.profile, vars),
        }),
        ast::Statement::Validate(v) => ast::Statement::Validate(ast::ValidateDef {
            assertions: v
                .assertions
                .iter()
                .map(|a| interpolate_assertion(a, vars))
                .collect(),
        }),
        ast::Statement::Scenario(sc) => ast::Statement::Scenario(ast::ScenarioDef {
            name: i(&sc.name, vars),
            steps: sc
                .steps
                .iter()
                .map(|step| ast::ScenarioStepDef {
                    time: i(&step.time, vars),
                    actions: step
                        .actions
                        .iter()
                        .map(|a| match a {
                            ast::ScenarioActionDef::Down(ep) => {
                                ast::ScenarioActionDef::Down(i(ep, vars))
                            }
                            ast::ScenarioActionDef::Up(ep) => {
                                ast::ScenarioActionDef::Up(i(ep, vars))
                            }
                            ast::ScenarioActionDef::Clear(ep) => {
                                ast::ScenarioActionDef::Clear(i(ep, vars))
                            }
                            ast::ScenarioActionDef::Validate(list) => {
                                ast::ScenarioActionDef::Validate(
                                    list.iter()
                                        .map(|a| interpolate_assertion(a, vars))
                                        .collect(),
                                )
                            }
                            ast::ScenarioActionDef::Exec { node, cmd } => {
                                ast::ScenarioActionDef::Exec {
                                    node: i(node, vars),
                                    cmd: cmd.iter().map(|c| i(c, vars)).collect(),
                                }
                            }
                            ast::ScenarioActionDef::Log(m) => {
                                ast::ScenarioActionDef::Log(i(m, vars))
                            }
                        })
                        .collect(),
                })
                .collect(),
        }),
        ast::Statement::Benchmark(b) => ast::Statement::Benchmark(ast::BenchmarkDef {
            name: i(&b.name, vars),
            tests: b
                .tests
                .iter()
                .map(|t| match t {
                    ast::BenchmarkTestDef::Iperf3 {
                        from,
                        to,
                        duration,
                        streams,
                        udp,
                        assertions,
                    } => ast::BenchmarkTestDef::Iperf3 {
                        from: i(from, vars),
                        to: i(to, vars),
                        duration: io(duration, vars),
                        streams: *streams,
                        udp: *udp,
                        assertions: assertions.clone(),
                    },
                    ast::BenchmarkTestDef::Ping {
                        from,
                        to,
                        count,
                        assertions,
                    } => ast::BenchmarkTestDef::Ping {
                        from: i(from, vars),
                        to: i(to, vars),
                        count: *count,
                        assertions: assertions.clone(),
                    },
                })
                .collect(),
        }),
        // Consumed by `expand_into` before interpolation; never reach
        // this function through that path.
        ast::Statement::Site(s) => ast::Statement::Site(s.clone()),
        ast::Statement::If(f) => ast::Statement::If(f.clone()),
        ast::Statement::Param(p) => ast::Statement::Param(p.clone()),
        ast::Statement::Let(l) => ast::Statement::Let(l.clone()),
        ast::Statement::For(f) => ast::Statement::For(f.clone()),
    }
}

fn i(s: &str, vars: &BTreeMap<String, String>) -> String {
    interpolate(s, vars)
}

fn interpolate_assertion(
    a: &ast::AssertionDef,
    vars: &BTreeMap<String, String>,
) -> ast::AssertionDef {
    use ast::AssertionDef as A;
    match a {
        A::Reach { from, to } => A::Reach {
            from: i(from, vars),
            to: i(to, vars),
        },
        A::NoReach { from, to } => A::NoReach {
            from: i(from, vars),
            to: i(to, vars),
        },
        A::TcpConnect {
            from,
            to,
            port,
            timeout,
            retries,
            interval,
        } => A::TcpConnect {
            from: i(from, vars),
            to: i(to, vars),
            port: *port,
            timeout: io(timeout, vars),
            retries: *retries,
            interval: io(interval, vars),
        },
        A::LatencyUnder {
            from,
            to,
            max,
            samples,
        } => A::LatencyUnder {
            from: i(from, vars),
            to: i(to, vars),
            max: i(max, vars),
            samples: *samples,
        },
        A::RouteHas {
            node,
            destination,
            via,
            dev,
        } => A::RouteHas {
            node: i(node, vars),
            destination: i(destination, vars),
            via: io(via, vars),
            dev: io(dev, vars),
        },
        A::DnsResolves {
            from,
            name,
            expected_ip,
        } => A::DnsResolves {
            from: i(from, vars),
            name: i(name, vars),
            expected_ip: i(expected_ip, vars),
        },
    }
}

fn io(s: &Option<String>, vars: &BTreeMap<String, String>) -> Option<String> {
    s.as_ref().map(|s| interpolate(s, vars))
}

fn interpolate_node(n: &ast::NodeDef, vars: &BTreeMap<String, String>) -> ast::NodeDef {
    ast::NodeDef {
        name: i(&n.name, vars),
        profiles: n.profiles.iter().map(|s| i(s, vars)).collect(),
        image: n.image.as_ref().map(|s| i(s, vars)),
        cmd: n
            .cmd
            .as_ref()
            .map(|c| c.iter().map(|s| i(s, vars)).collect()),
        env: n.env.iter().map(|s| i(s, vars)).collect(),
        volumes: n.volumes.iter().map(|s| i(s, vars)).collect(),
        cpu: io(&n.cpu, vars),
        memory: io(&n.memory, vars),
        privileged: n.privileged,
        cap_add: n.cap_add.iter().map(|s| i(s, vars)).collect(),
        cap_drop: n.cap_drop.iter().map(|s| i(s, vars)).collect(),
        entrypoint: io(&n.entrypoint, vars),
        hostname: io(&n.hostname, vars),
        workdir: io(&n.workdir, vars),
        labels: n.labels.iter().map(|s| i(s, vars)).collect(),
        pull: io(&n.pull, vars),
        container_exec: n.container_exec.iter().map(|s| i(s, vars)).collect(),
        healthcheck: io(&n.healthcheck, vars),
        healthcheck_interval: io(&n.healthcheck_interval, vars),
        healthcheck_timeout: io(&n.healthcheck_timeout, vars),
        startup_delay: io(&n.startup_delay, vars),
        env_file: io(&n.env_file, vars),
        configs: n
            .configs
            .iter()
            .map(|(h, c)| (i(h, vars), i(c, vars)))
            .collect(),
        overlay: io(&n.overlay, vars),
        depends_on: n.depends_on.iter().map(|s| i(s, vars)).collect(),
        props: n.props.iter().map(|p| interpolate_prop(p, vars)).collect(),
    }
}

fn interpolate_prop(p: &ast::NodeProp, vars: &BTreeMap<String, String>) -> ast::NodeProp {
    match p {
        ast::NodeProp::Forward(v) => ast::NodeProp::Forward(*v),
        ast::NodeProp::Sysctl(k, v) => ast::NodeProp::Sysctl(i(k, vars), i(v, vars)),
        ast::NodeProp::Lo(addr) => ast::NodeProp::Lo(i(addr, vars)),
        ast::NodeProp::Route(r) => ast::NodeProp::Route(interpolate_route(r, vars)),
        ast::NodeProp::Firewall(fw) => ast::NodeProp::Firewall(ast::FirewallDef {
            policy: i(&fw.policy, vars),
            rules: fw
                .rules
                .iter()
                .map(|r| ast::FirewallRuleDef {
                    match_expr: i(&r.match_expr, vars),
                    action: i(&r.action, vars),
                })
                .collect(),
        }),
        ast::NodeProp::Nat(nat) => ast::NodeProp::Nat(interpolate_nat(nat, vars)),
        ast::NodeProp::Vrf(v) => ast::NodeProp::Vrf(interpolate_vrf(v, vars)),
        ast::NodeProp::Wireguard(wg) => ast::NodeProp::Wireguard(interpolate_wg(wg, vars)),
        ast::NodeProp::Vxlan(vx) => ast::NodeProp::Vxlan(interpolate_vxlan(vx, vars)),
        ast::NodeProp::Dummy(d) => ast::NodeProp::Dummy(ast::DummyDef {
            name: i(&d.name, vars),
            addresses: d.addresses.iter().map(|s| i(s, vars)).collect(),
        }),
        ast::NodeProp::Macvlan(m) => ast::NodeProp::Macvlan(ast::MacvlanDef {
            name: i(&m.name, vars),
            parent: i(&m.parent, vars),
            mode: m.mode.as_ref().map(|s| i(s, vars)),
            addresses: m.addresses.iter().map(|s| i(s, vars)).collect(),
        }),
        ast::NodeProp::Ipvlan(iv) => ast::NodeProp::Ipvlan(ast::IpvlanDef {
            name: i(&iv.name, vars),
            parent: i(&iv.parent, vars),
            mode: iv.mode.as_ref().map(|s| i(s, vars)),
            addresses: iv.addresses.iter().map(|s| i(s, vars)).collect(),
        }),
        ast::NodeProp::Wifi(w) => ast::NodeProp::Wifi(ast::WifiDef {
            name: i(&w.name, vars),
            mode: w.mode.clone(),
            ssid: w.ssid.as_ref().map(|s| i(s, vars)),
            channel: w.channel,
            passphrase: io(&w.passphrase, vars),
            mesh_id: w.mesh_id.as_ref().map(|s| i(s, vars)),
            addresses: w.addresses.iter().map(|s| i(s, vars)).collect(),
        }),
        ast::NodeProp::Run(r) => ast::NodeProp::Run(ast::RunDef {
            cmd: r.cmd.iter().map(|s| i(s, vars)).collect(),
            background: r.background,
        }),
        // Outer variables are substituted now; the loop's own variable
        // is unknown here and stays as a literal `${var}` until
        // `expand_node_props` binds it.
        ast::NodeProp::ForLoop(f) => ast::NodeProp::ForLoop(ast::PropForLoop {
            var: f.var.clone(),
            range: match &f.range {
                ast::ForRange::List(items) => {
                    ast::ForRange::List(items.iter().map(|s| i(s, vars)).collect())
                }
                ast::ForRange::DynRange { start, end } => ast::ForRange::DynRange {
                    start: i(start, vars),
                    end: i(end, vars),
                },
                other => other.clone(),
            },
            body: f.body.iter().map(|p| interpolate_prop(p, vars)).collect(),
        }),
    }
}

fn interpolate_route(r: &ast::RouteDef, vars: &BTreeMap<String, String>) -> ast::RouteDef {
    ast::RouteDef {
        destination: i(&r.destination, vars),
        via: io(&r.via, vars),
        dev: io(&r.dev, vars),
        metric: r.metric,
    }
}

fn interpolate_vrf(v: &ast::VrfDef, vars: &BTreeMap<String, String>) -> ast::VrfDef {
    ast::VrfDef {
        name: i(&v.name, vars),
        table: v.table,
        interfaces: v.interfaces.iter().map(|s| i(s, vars)).collect(),
        routes: v
            .routes
            .iter()
            .map(|r| interpolate_route(r, vars))
            .collect(),
    }
}

fn interpolate_wg(wg: &ast::WireguardDef, vars: &BTreeMap<String, String>) -> ast::WireguardDef {
    ast::WireguardDef {
        name: i(&wg.name, vars),
        key: wg.key.clone(),
        listen_port: wg.listen_port,
        fwmark: wg.fwmark,
        addresses: wg.addresses.iter().map(|s| i(s, vars)).collect(),
        peers: wg.peers.iter().map(|s| i(s, vars)).collect(),
    }
}

fn interpolate_vxlan(vx: &ast::VxlanDef, vars: &BTreeMap<String, String>) -> ast::VxlanDef {
    ast::VxlanDef {
        name: i(&vx.name, vars),
        vni: vx.vni,
        local: io(&vx.local, vars),
        remote: io(&vx.remote, vars),
        port: vx.port,
        underlay: io(&vx.underlay, vars),
        addresses: vx.addresses.iter().map(|s| i(s, vars)).collect(),
    }
}

fn interpolate_link(l: &ast::LinkDef, vars: &BTreeMap<String, String>) -> ast::LinkDef {
    ast::LinkDef {
        left_node: i(&l.left_node, vars),
        left_iface: i(&l.left_iface, vars),
        right_node: i(&l.right_node, vars),
        right_iface: i(&l.right_iface, vars),
        left_addr: io(&l.left_addr, vars),
        right_addr: io(&l.right_addr, vars),
        subnet: io(&l.subnet, vars),
        pool: l.pool.clone(),
        mtu: l.mtu,
        impairment: l
            .impairment
            .as_ref()
            .map(|p| interpolate_impair_props(p, vars)),
        left_impair: l
            .left_impair
            .as_ref()
            .map(|p| interpolate_impair_props(p, vars)),
        right_impair: l
            .right_impair
            .as_ref()
            .map(|p| interpolate_impair_props(p, vars)),
        rate: l.rate.as_ref().map(|p| interpolate_rate_props(p, vars)),
        profile: l.profile.clone(),
    }
}

fn interpolate_impair_props(
    p: &ast::ImpairProps,
    vars: &BTreeMap<String, String>,
) -> ast::ImpairProps {
    ast::ImpairProps {
        delay: io(&p.delay, vars),
        jitter: io(&p.jitter, vars),
        loss: io(&p.loss, vars),
        rate: io(&p.rate, vars),
        corrupt: io(&p.corrupt, vars),
        reorder: io(&p.reorder, vars),
        duplicate: io(&p.duplicate, vars),
        delay_correlation: io(&p.delay_correlation, vars),
        loss_correlation: io(&p.loss_correlation, vars),
        limit: io(&p.limit, vars),
    }
}

fn interpolate_rate_props(p: &ast::RateProps, vars: &BTreeMap<String, String>) -> ast::RateProps {
    ast::RateProps {
        egress: io(&p.egress, vars),
        ingress: io(&p.ingress, vars),
        burst: io(&p.burst, vars),
    }
}

fn interpolate_network(n: &ast::NetworkDef, vars: &BTreeMap<String, String>) -> ast::NetworkDef {
    ast::NetworkDef {
        name: i(&n.name, vars),
        members: n.members.iter().map(|s| i(s, vars)).collect(),
        vlan_filtering: n.vlan_filtering,
        mtu: n.mtu,
        subnet: n.subnet.as_ref().map(|s| i(s, vars)),
        vlans: n.vlans.clone(),
        ports: n.ports.clone(),
        impairments: n
            .impairments
            .iter()
            .map(|imp| interpolate_network_impair(imp, vars))
            .collect(),
        loops: n
            .loops
            .iter()
            .map(|l| interpolate_network_loop(l, vars))
            .collect(),
    }
}

fn interpolate_network_impair(
    imp: &ast::NetworkImpairDef,
    vars: &BTreeMap<String, String>,
) -> ast::NetworkImpairDef {
    ast::NetworkImpairDef {
        src: i(&imp.src, vars),
        dst: i(&imp.dst, vars),
        props: interpolate_impair_props(&imp.props, vars),
        rate_cap: imp.rate_cap.as_ref().map(|s| i(s, vars)),
    }
}

/// Interpolate the outer scope into a network loop body; the loop's own
/// variable stays literal until `expand_network_impairs` binds it.
fn interpolate_network_loop(
    l: &ast::NetworkForLoop,
    vars: &BTreeMap<String, String>,
) -> ast::NetworkForLoop {
    ast::NetworkForLoop {
        var: l.var.clone(),
        range: l.range.clone(),
        impairments: l
            .impairments
            .iter()
            .map(|imp| interpolate_network_impair(imp, vars))
            .collect(),
        loops: l
            .loops
            .iter()
            .map(|inner| interpolate_network_loop(inner, vars))
            .collect(),
    }
}

fn prefix_network_loop(prefix: &str, l: &mut ast::NetworkForLoop) {
    for imp in &mut l.impairments {
        imp.src = prefix_ep(prefix, &imp.src);
        imp.dst = prefix_ep(prefix, &imp.dst);
    }
    for inner in &mut l.loops {
        prefix_network_loop(prefix, inner);
    }
}

/// Bind `var` for every value of the loop and interpolate the body
/// (recursing into nested loops). Shared by every loop kind that is not
/// a statement list.
fn for_each_value<T>(
    var: &str,
    range: &ast::ForRange,
    vars: &BTreeMap<String, String>,
    mut body: impl FnMut(&BTreeMap<String, String>) -> Result<Vec<T>>,
) -> Result<Vec<T>> {
    let values = range_values(range, var, vars)?;
    let len = values.len();
    let mut out = Vec::new();
    for (idx, value) in values.iter().enumerate() {
        let mut inner = vars.clone();
        inner.insert(var.to_string(), value.clone());
        inner.insert("loop.index".into(), idx.to_string());
        inner.insert("loop.first".into(), (idx == 0).to_string());
        inner.insert("loop.last".into(), (idx + 1 == len).to_string());
        out.extend(body(&inner)?);
    }
    Ok(out)
}

/// Flatten a network block's `impair` statements and `for` loops into
/// the final per-pair list.
fn expand_network_impairs(
    impairments: &[ast::NetworkImpairDef],
    loops: &[ast::NetworkForLoop],
    vars: &BTreeMap<String, String>,
) -> Result<Vec<ast::NetworkImpairDef>> {
    let mut out: Vec<ast::NetworkImpairDef> = impairments
        .iter()
        .map(|imp| interpolate_network_impair(imp, vars))
        .collect();
    for l in loops {
        out.extend(for_each_value(&l.var, &l.range, vars, |inner| {
            expand_network_impairs(&l.impairments, &l.loops, inner)
        })?);
    }
    Ok(out)
}

fn interpolate_nat(nat: &ast::NatDef, vars: &BTreeMap<String, String>) -> ast::NatDef {
    ast::NatDef {
        items: nat
            .items
            .iter()
            .map(|item| match item {
                ast::NatItem::Rule(r) => ast::NatItem::Rule(interpolate_nat_rule(r, vars)),
                ast::NatItem::For(f) => ast::NatItem::For(ast::NatForLoop {
                    var: f.var.clone(),
                    range: f.range.clone(),
                    body: interpolate_nat(&f.body, vars),
                }),
            })
            .collect(),
    }
}

fn interpolate_nat_rule(r: &ast::NatRuleDef, vars: &BTreeMap<String, String>) -> ast::NatRuleDef {
    ast::NatRuleDef {
        action: r.action.clone(),
        src: r.src.as_ref().map(|s| i(s, vars)),
        dst: r.dst.as_ref().map(|s| i(s, vars)),
        target: r.target.as_ref().map(|s| i(s, vars)),
        target_port: r.target_port,
    }
}

/// Flatten a `nat` block into rules, in source order, expanding `for`
/// loops with the full engine (arithmetic, `loop.*`, iteration cap).
fn expand_nat_rules(
    nat: &ast::NatDef,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<ast::NatRuleDef>> {
    let mut out = Vec::new();
    for item in &nat.items {
        match item {
            ast::NatItem::Rule(r) => out.push(interpolate_nat_rule(r, vars)),
            ast::NatItem::For(f) => out.extend(for_each_value(&f.var, &f.range, vars, |inner| {
                expand_nat_rules(&f.body, inner)
            })?),
        }
    }
    Ok(out)
}

/// `[for VAR in RANGE : template]` — a list expression expanded where it
/// is parsed. Same engine, metavariables and iteration cap as block
/// loops; bounds must be literal because a list has no enclosing scope
/// to resolve `${…}` against.
pub(crate) fn expand_list_for(
    var: &str,
    range: &ast::ForRange,
    template: &str,
) -> Result<Vec<String>> {
    if let ast::ForRange::DynRange { .. } = range {
        return Err(crate::Error::NllParse(format!(
            "for expression '{var}': bounds must be literal integers (`${{…}}` bounds are only \
             supported in block loops)"
        )));
    }
    for_each_value(var, range, &BTreeMap::new(), |inner| {
        Ok(vec![interpolate(template, inner)])
    })
}

fn interpolate_impair_def(imp: &ast::ImpairDef, vars: &BTreeMap<String, String>) -> ast::ImpairDef {
    ast::ImpairDef {
        node: i(&imp.node, vars),
        iface: i(&imp.iface, vars),
        props: interpolate_impair_props(&imp.props, vars),
    }
}

fn interpolate_rate_def(r: &ast::RateDef, vars: &BTreeMap<String, String>) -> ast::RateDef {
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
            // Check all profiles exist
            for profile in &n.profiles {
                if !ctx.profiles.contains_key(profile) {
                    errors.push(format!(
                        "node '{}' references undefined profile '{profile}'",
                        n.name
                    ));
                }
            }
        }
        ast::Statement::For(f) => {
            if let ast::ForRange::IntRange { start, end } = &f.range
                && start > end
            {
                errors.push(format!(
                    "for loop '{}' has empty range {}..{}",
                    f.var, start, end
                ));
            }
            if let ast::ForRange::List(items) = &f.range
                && items.is_empty()
            {
                errors.push(format!("for loop '{}' has empty list", f.var));
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

fn lower_lab(lab: &ast::LabDecl) -> Result<types::LabConfig> {
    let runtime = match lab.runtime.as_deref() {
        None => None,
        Some("auto") => Some(types::ContainerRuntime::Auto),
        Some("docker") => Some(types::ContainerRuntime::Docker),
        Some("podman") => Some(types::ContainerRuntime::Podman),
        Some(other) => {
            return Err(crate::Error::NllParse(format!(
                "unknown runtime '{other}' (expected auto, docker or podman)"
            )));
        }
    };
    let dns = match lab.dns.as_deref() {
        None | Some("off") => types::DnsMode::Off,
        Some("hosts") => types::DnsMode::Hosts,
        Some(other) => {
            return Err(crate::Error::NllParse(format!(
                "unknown dns mode '{other}' (expected off or hosts)"
            )));
        }
    };
    let routing = match lab.routing.as_deref() {
        None | Some("manual") => types::RoutingMode::Manual,
        Some("auto") => types::RoutingMode::Auto,
        Some(other) => {
            return Err(crate::Error::NllParse(format!(
                "unknown routing mode '{other}' (expected manual or auto)"
            )));
        }
    };
    Ok(types::LabConfig {
        name: lab.name.clone(),
        description: lab.description.clone(),
        prefix: lab.prefix.clone(),
        runtime,
        version: lab.version.clone(),
        author: lab.author.clone(),
        tags: lab.tags.clone(),
        mgmt_subnet: lab.mgmt.clone(),
        mgmt_host_reachable: lab.mgmt_host_reachable,
        dns,
        routing,
    })
}

fn lower_node(topo: &mut types::Topology, node: &ast::NodeDef, ctx: &mut LowerCtx) -> Result<()> {
    let mut n = types::Node {
        profiles: node.profiles.clone(),
        image: node.image.clone(),
        cmd: node.cmd.clone(),
        cpu: node.cpu.clone(),
        memory: node.memory.clone(),
        privileged: node.privileged,
        cap_add: node.cap_add.clone(),
        cap_drop: node.cap_drop.clone(),
        entrypoint: node.entrypoint.clone(),
        hostname: node.hostname.clone(),
        workdir: node.workdir.clone(),
        labels: node.labels.clone(),
        pull: node.pull.clone(),
        container_exec: node.container_exec.clone(),
        healthcheck: node.healthcheck.clone(),
        healthcheck_interval: node.healthcheck_interval.clone(),
        healthcheck_timeout: node.healthcheck_timeout.clone(),
        startup_delay: node.startup_delay.clone(),
        env_file: node.env_file.clone(),
        configs: node.configs.clone(),
        overlay: node.overlay.clone(),
        depends_on: node.depends_on.clone(),
        ..Default::default()
    };

    // Container env/volumes
    if !node.env.is_empty() {
        let map: BTreeMap<String, String> = node
            .env
            .iter()
            .filter_map(|s| {
                s.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        n.env = Some(map);
    }
    if !node.volumes.is_empty() {
        n.volumes = Some(node.volumes.clone());
    }

    // Apply profiles in order (later profiles override earlier ones)
    // Clone props to release the immutable borrow on ctx before mutating
    let profile_props: Vec<Vec<ast::NodeProp>> = node
        .profiles
        .iter()
        .filter_map(|name| ctx.profiles.get(name).map(|p| p.props.clone()))
        .collect();
    for props in &profile_props {
        apply_node_props(&mut n, props, ctx)?;
    }

    // Apply node's own properties (overrides profile)
    apply_node_props(&mut n, &node.props, ctx)?;

    if topo.nodes.contains_key(&node.name) {
        return Err(crate::Error::NllParse(format!(
            "duplicate node name '{}' — each node must have a unique name",
            node.name
        )));
    }
    topo.nodes.insert(node.name.clone(), n);
    Ok(())
}

/// Flatten `for` loops inside a node/profile block into a plain
/// property list, binding the loop variable (and `loop.index` /
/// `loop.first` / `loop.last`) for every iteration and recursing into
/// nested loops. Every property kind is supported — previously only
/// `route` and `nat` survived a loop (issue #18).
fn expand_node_props(
    props: &[ast::NodeProp],
    vars: &BTreeMap<String, String>,
) -> Result<Vec<ast::NodeProp>> {
    let mut out = Vec::with_capacity(props.len());
    for prop in props {
        match prop {
            ast::NodeProp::ForLoop(f) => {
                let values = range_values(&f.range, &f.var, vars)?;
                let len = values.len();
                for (idx, value) in values.iter().enumerate() {
                    let mut inner = vars.clone();
                    inner.insert(f.var.clone(), value.clone());
                    inner.insert("loop.index".into(), idx.to_string());
                    inner.insert("loop.first".into(), (idx == 0).to_string());
                    inner.insert("loop.last".into(), (idx + 1 == len).to_string());
                    let body: Vec<ast::NodeProp> =
                        f.body.iter().map(|q| interpolate_prop(q, &inner)).collect();
                    out.extend(expand_node_props(&body, &inner)?);
                }
            }
            other => out.push(other.clone()),
        }
    }
    Ok(out)
}

fn lower_nat_action(action: &str) -> Result<types::NatAction> {
    match action {
        "masquerade" => Ok(types::NatAction::Masquerade),
        "snat" => Ok(types::NatAction::Snat),
        "dnat" => Ok(types::NatAction::Dnat),
        "translate" => Ok(types::NatAction::Translate),
        other => Err(crate::Error::NllParse(format!(
            "unknown NAT action '{other}' (expected masquerade, snat, dnat or translate)"
        ))),
    }
}

fn apply_node_props(
    node: &mut types::Node,
    props: &[ast::NodeProp],
    ctx: &mut LowerCtx,
) -> Result<()> {
    let props = expand_node_props(props, &ctx.variables)?;
    for prop in &props {
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
                if let Some(pool_name) = addr.strip_prefix("pool:") {
                    // Allocate from pool
                    if let Some(pool) = ctx.pools.get_mut(pool_name) {
                        let allocated = pool.allocate(pool_name)?;
                        lo.addresses.push(allocated);
                    } else {
                        tracing::warn!("unknown pool '{pool_name}' for loopback");
                        lo.addresses.push(addr.clone());
                    }
                } else {
                    lo.addresses.push(addr.clone());
                }
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
                let vars = &ctx.variables;
                node.firewall = Some(types::FirewallConfig {
                    policy: Some(interpolate(&fw.policy, vars)),
                    rules: fw
                        .rules
                        .iter()
                        .map(|r| types::FirewallRule {
                            match_expr: Some(interpolate(&r.match_expr, vars)),
                            action: Some(interpolate(&r.action, vars)),
                        })
                        .collect(),
                });
            }
            ast::NodeProp::Nat(nat) => {
                let rules = expand_nat_rules(nat, &ctx.variables)?
                    .iter()
                    .map(|r| {
                        Ok(types::NatRule {
                            action: lower_nat_action(&r.action)?,
                            src: r.src.clone(),
                            dst: r.dst.clone(),
                            target: r.target.clone(),
                            target_port: r.target_port,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                // Several `nat` blocks (or loop iterations) accumulate
                // rules instead of the last one replacing the rest.
                match &mut node.nat {
                    Some(existing) => existing.rules.extend(rules),
                    None => node.nat = Some(types::NatConfig { rules }),
                }
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
                        fwmark: wg.fwmark,
                        addresses: wg.addresses.clone(),
                        peers: wg.peers.clone(),
                    },
                );
            }
            ast::NodeProp::Vxlan(vx) => {
                node.interfaces.insert(
                    vx.name.clone(),
                    types::InterfaceConfig {
                        kind: Some(types::InterfaceKind::Vxlan),
                        vni: Some(vx.vni),
                        local: vx.local.clone(),
                        remote: vx.remote.clone(),
                        port: vx.port,
                        underlay: vx.underlay.clone(),
                        addresses: vx.addresses.clone(),
                        ..Default::default()
                    },
                );
            }
            ast::NodeProp::Dummy(d) => {
                node.interfaces.insert(
                    d.name.clone(),
                    types::InterfaceConfig {
                        kind: Some(types::InterfaceKind::Dummy),
                        addresses: d.addresses.clone(),
                        ..Default::default()
                    },
                );
            }
            ast::NodeProp::Macvlan(m) => {
                node.macvlans.push(types::MacvlanConfig {
                    name: m.name.clone(),
                    parent: m.parent.clone(),
                    mode: match m.mode.as_deref() {
                        None | Some("bridge") => types::MacvlanMode::Bridge,
                        Some("private") => types::MacvlanMode::Private,
                        Some("vepa") => types::MacvlanMode::Vepa,
                        Some("passthru") => types::MacvlanMode::Passthru,
                        Some(other) => {
                            return Err(crate::Error::NllParse(format!(
                                "unknown macvlan mode '{other}' on '{}' (expected bridge, private, vepa or passthru)",
                                m.name
                            )));
                        }
                    },
                    addresses: m.addresses.clone(),
                });
            }
            ast::NodeProp::Ipvlan(iv) => {
                node.ipvlans.push(types::IpvlanConfig {
                    name: iv.name.clone(),
                    parent: iv.parent.clone(),
                    mode: match iv.mode.as_deref() {
                        None | Some("l3") => types::IpvlanMode::L3,
                        Some("l2") => types::IpvlanMode::L2,
                        Some("l3s") => types::IpvlanMode::L3S,
                        Some(other) => {
                            return Err(crate::Error::NllParse(format!(
                                "unknown ipvlan mode '{other}' on '{}' (expected l2, l3 or l3s)",
                                iv.name
                            )));
                        }
                    },
                    addresses: iv.addresses.clone(),
                });
            }
            ast::NodeProp::Wifi(w) => {
                node.wifi.push(types::WifiConfig {
                    name: w.name.clone(),
                    mode: match w.mode.as_str() {
                        "ap" => types::WifiMode::Ap,
                        "station" => types::WifiMode::Station,
                        "mesh" => types::WifiMode::Mesh,
                        other => {
                            return Err(crate::Error::NllParse(format!(
                                "unknown wifi mode '{other}' on '{}' (expected ap, station or mesh)",
                                w.name
                            )));
                        }
                    },
                    ssid: w.ssid.clone(),
                    channel: w.channel,
                    passphrase: w.passphrase.clone(),
                    mesh_id: w.mesh_id.clone(),
                    addresses: w.addresses.clone(),
                });
            }
            ast::NodeProp::Run(r) => {
                node.exec.push(types::ExecConfig {
                    cmd: r.cmd.clone(),
                    background: r.background,
                });
            }
            // Flattened by `expand_node_props` above.
            ast::NodeProp::ForLoop(_) => {}
        }
    }
    Ok(())
}

/// Expand a ForRange into a list of string values.
/// Split a subnet CIDR into two endpoint addresses.
///
/// - `/31`: `.0` and `.1` (RFC 3021 point-to-point)
/// - `/30` and larger: network+1 and network+2
fn split_subnet(cidr: &str) -> std::result::Result<[String; 2], ()> {
    let (ip_str, prefix_str) = cidr.rsplit_once('/').ok_or(())?;
    let prefix: u8 = prefix_str.parse().map_err(|_| ())?;
    if prefix >= 32 {
        return Err(());
    }
    let ip: std::net::Ipv4Addr = ip_str.parse().map_err(|_| ())?;
    let bits = u32::from(ip);
    if prefix == 31 {
        // RFC 3021: .0 and .1
        let base = bits & !(1u32);
        let a = std::net::Ipv4Addr::from(base);
        let b = std::net::Ipv4Addr::from(base + 1);
        Ok([format!("{a}/{prefix}"), format!("{b}/{prefix}")])
    } else {
        // Standard: network+1 and network+2
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let network = bits & mask;
        let a = std::net::Ipv4Addr::from(network.checked_add(1).ok_or(())?);
        let b = std::net::Ipv4Addr::from(network.checked_add(2).ok_or(())?);
        Ok([format!("{a}/{prefix}"), format!("{b}/{prefix}")])
    }
}

/// Allocate the next subnet from a pool and split it into the two
/// endpoint addresses of a point-to-point link.
///
/// Exhaustion (and address-space overflow) is an error, not a link
/// without addresses: `mesh`, `ring` and `star` all go through here
/// now — `ring`/`star` used to copy the arithmetic without the bounds
/// check and handed out subnets outside the pool (issue #19).
fn allocate_from_pool(pool: &mut PoolState, pool_name: &str) -> Result<[String; 2]> {
    let cidr = pool.allocate_subnet(pool_name)?;
    split_subnet(&cidr).map_err(|_| {
        crate::Error::NllParse(format!(
            "pool '{pool_name}': cannot split /{} into two endpoint addresses (use /31 or larger)",
            pool.alloc_prefix
        ))
    })
}

/// Expand a topology pattern (mesh, ring, star) into nodes and links.
/// A node generated by a pattern: carries the pattern's profile and,
/// like `node x : p`, the profile's properties (sysctls, firewall, …)
/// applied to it. Previously only the reference was recorded, so
/// `effective_sysctls` saw them but `render`/`diff` did not.
fn pattern_node(pattern: &ast::PatternDef, ctx: &mut LowerCtx) -> Result<types::Node> {
    let mut node = types::Node::default();
    if let Some(profile) = &pattern.profile {
        let props = ctx
            .profiles
            .get(profile)
            .map(|p| p.props.clone())
            .ok_or_else(|| {
                crate::Error::NllParse(format!(
                    "pattern '{}' references undefined profile '{profile}'",
                    pattern.name
                ))
            })?;
        apply_node_props(&mut node, &props, ctx)?;
        node.profiles = vec![profile.clone()];
    }
    Ok(node)
}

/// Addresses for one pattern link: allocated from the pattern's pool
/// when it names one, `None` otherwise.
fn pattern_addresses(pattern: &ast::PatternDef, ctx: &mut LowerCtx) -> Result<Option<[String; 2]>> {
    match &pattern.pool {
        None => Ok(None),
        Some(pool_name) => match ctx.pools.get_mut(pool_name.as_str()) {
            Some(pool) => allocate_from_pool(pool, pool_name).map(Some),
            None => Err(crate::Error::NllParse(format!(
                "pattern '{}' references undefined pool '{pool_name}'",
                pattern.name
            ))),
        },
    }
}

fn expand_pattern(
    topo: &mut types::Topology,
    pattern: &ast::PatternDef,
    ctx: &mut LowerCtx,
) -> Result<()> {
    match &pattern.kind {
        ast::PatternKind::Mesh => {
            // Generate nodes
            for name in &pattern.nodes {
                let node_name = format!("{}.{}", pattern.name, name);
                let node = pattern_node(pattern, ctx)?;
                topo.nodes.insert(node_name, node);
            }
            // Generate full-mesh links (all pairwise, i < j)
            for (i, a) in pattern.nodes.iter().enumerate() {
                for b in &pattern.nodes[i + 1..] {
                    let left = format!("{}.{}", pattern.name, a);
                    let right = format!("{}.{}", pattern.name, b);
                    let left_iface = format!("to-{b}");
                    let right_iface = format!("to-{a}");

                    let addresses = pattern_addresses(pattern, ctx)?;

                    topo.links.push(types::Link {
                        endpoints: [
                            format!("{left}:{left_iface}"),
                            format!("{right}:{right_iface}"),
                        ],
                        addresses,
                        mtu: ctx.default_link_mtu,
                    });
                }
            }
        }
        ast::PatternKind::Ring => {
            let n = pattern.count.unwrap_or(pattern.nodes.len() as i64);
            if !(0..=MAX_LOOP_ITERATIONS).contains(&n) {
                return Err(crate::Error::NllParse(format!(
                    "ring '{}': count {n} is out of range (0..={MAX_LOOP_ITERATIONS})",
                    pattern.name
                )));
            }
            let names: Vec<String> = if pattern.nodes.is_empty() {
                (1..=n).map(|i| format!("r{i}")).collect()
            } else {
                pattern.nodes.clone()
            };

            // Generate nodes
            for name in &names {
                let node_name = format!("{}.{}", pattern.name, name);
                let node = pattern_node(pattern, ctx)?;
                topo.nodes.insert(node_name, node);
            }

            // Generate ring links
            for i in 0..names.len() {
                let j = (i + 1) % names.len();
                let left = format!("{}.{}", pattern.name, names[i]);
                let right = format!("{}.{}", pattern.name, names[j]);

                let addresses = pattern_addresses(pattern, ctx)?;

                topo.links.push(types::Link {
                    endpoints: [format!("{left}:right"), format!("{right}:left")],
                    addresses,
                    mtu: ctx.default_link_mtu,
                });
            }
        }
        ast::PatternKind::Star { hub } => {
            // Generate hub node
            let hub_name = format!("{}.{}", pattern.name, hub);
            let hub_node = pattern_node(pattern, ctx)?;
            topo.nodes.insert(hub_name.clone(), hub_node);

            // Generate spoke nodes and links
            for (i, spoke) in pattern.nodes.iter().enumerate() {
                let spoke_name = format!("{}.{}", pattern.name, spoke);
                let spoke_node = pattern_node(pattern, ctx)?;
                topo.nodes.insert(spoke_name.clone(), spoke_node);

                let addresses = pattern_addresses(pattern, ctx)?;

                topo.links.push(types::Link {
                    endpoints: [format!("{hub_name}:eth{i}"), format!("{spoke_name}:eth0")],
                    addresses,
                    mtu: ctx.default_link_mtu,
                });
            }
        }
    }
    Ok(())
}

fn lower_link(topo: &mut types::Topology, link: &ast::LinkDef, ctx: &mut LowerCtx) -> Result<()> {
    let endpoints = [
        format!("{}:{}", link.left_node, link.left_iface),
        format!("{}:{}", link.right_node, link.right_iface),
    ];

    let addresses = match (&link.left_addr, &link.right_addr, &link.subnet, &link.pool) {
        (Some(l), Some(r), _, _) => Some([l.clone(), r.clone()]),
        (_, _, Some(subnet), _) => {
            // `auto/N` placeholders are substituted at deploy time by
            // the host-wide subnet pool; leave them alone here.
            if subnet.starts_with("auto") || subnet.contains("${") {
                None
            } else {
                Some(split_subnet(subnet).map_err(|_| {
                    crate::Error::NllParse(format!(
                        "link {} -- {}: cannot derive two endpoint addresses from subnet '{subnet}' (IPv4 /31 or larger required)",
                        endpoints[0], endpoints[1]
                    ))
                })?)
            }
        }
        (_, _, _, Some(pool_name)) => match ctx.pools.get_mut(pool_name.as_str()) {
            Some(pool) => Some(allocate_from_pool(pool, pool_name)?),
            None => {
                return Err(crate::Error::NllParse(format!(
                    "link {} -- {} references undefined pool '{pool_name}'",
                    endpoints[0], endpoints[1]
                )));
            }
        },
        _ => None,
    };

    // Resolve link profile defaults (profile < global defaults < per-link)
    let profile_defaults = link
        .profile
        .as_ref()
        .and_then(|name| ctx.link_profiles.get(name));

    let profile_mtu = profile_defaults.and_then(|d| d.mtu);
    let profile_impair = profile_defaults.and_then(|d| d.impair.as_ref());

    // Apply link defaults: per-link > profile > global
    let mtu = link.mtu.or(profile_mtu).or(ctx.default_link_mtu);

    topo.links.push(types::Link {
        endpoints,
        addresses,
        mtu,
    });

    // Lower symmetric impairment → both endpoints (per-link > profile > global)
    let effective_impair = link
        .impairment
        .as_ref()
        .or(profile_impair)
        .or(ctx.default_impair.as_ref());
    if let Some(imp) = effective_impair {
        let left_ep = format!("{}:{}", link.left_node, link.left_iface);
        let right_ep = format!("{}:{}", link.right_node, link.right_iface);
        topo.impairments.insert(left_ep, lower_impair_props(imp));
        topo.impairments.insert(right_ep, lower_impair_props(imp));
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
    Ok(())
}

/// Increment an IP address by a host number offset.
fn increment_ip(base: std::net::IpAddr, offset: u32) -> std::net::IpAddr {
    match base {
        std::net::IpAddr::V4(v4) => {
            let n = u32::from(v4) + offset;
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(n))
        }
        std::net::IpAddr::V6(v6) => {
            let n = u128::from(v6) + offset as u128;
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(n))
        }
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
        duplicate: props.duplicate.clone(),
        delay_correlation: props.delay_correlation.clone(),
        loss_correlation: props.loss_correlation.clone(),
        limit: props.limit.clone(),
    }
}

/// Resolve glob patterns in network member lists.
/// E.g., `*-black:fo` expands to `alpha-black:fo`, `bravo-black:fo`, etc.
fn resolve_glob_members(
    members: &[String],
    all_nodes: &BTreeMap<String, types::Node>,
) -> Vec<String> {
    let mut resolved = Vec::new();
    for member in members {
        if let Some((node_pattern, iface)) = member.split_once(':') {
            if node_pattern.contains('*') {
                // Glob: expand against all known node names
                for node_name in all_nodes.keys() {
                    if glob_matches(node_pattern, node_name) {
                        resolved.push(format!("{node_name}:{iface}"));
                    }
                }
            } else {
                resolved.push(member.clone());
            }
        } else if member.contains('*') {
            // Glob without interface — match bare node names
            for node_name in all_nodes.keys() {
                if glob_matches(member, node_name) {
                    resolved.push(node_name.clone());
                }
            }
        } else {
            resolved.push(member.clone());
        }
    }
    resolved
}

/// Simple glob matching: supports single `*` wildcard.
/// `*-black` matches `alpha-black`, `bravo-black`.
/// `hq-*` matches `hq-fw`, `hq-dcs`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() != 2 {
        return false; // only single * supported
    }
    name.starts_with(parts[0]) && name.ends_with(parts[1])
}

fn lower_network(
    topo: &mut types::Topology,
    net: &ast::NetworkDef,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    // Resolve glob patterns in member lists (e.g., "*-black:fo" → "alpha-black:fo")
    let resolved_members = resolve_glob_members(&net.members, &topo.nodes);

    let mut network = types::Network {
        kind: Some("bridge".to_string()),
        vlan_filtering: if net.vlan_filtering { Some(true) } else { None },
        mtu: net.mtu,
        members: resolved_members,
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
        // Canonical port key is the member endpoint `node:iface`; a bare
        // node name is resolved against the member list so every consumer
        // (deploy, dns, validator) can rely on one shape.
        let key = if port.endpoint.contains(':') {
            port.endpoint.clone()
        } else {
            let node = port.endpoint.as_str();
            let matches: Vec<&String> = network
                .members
                .iter()
                .filter(|m| m.split_once(':').map(|(n, _)| n) == Some(node))
                .collect();
            match matches.as_slice() {
                [one] => (*one).clone(),
                [] => {
                    return Err(crate::Error::NllParse(format!(
                        "network '{}': port '{node}' is not a member of this network",
                        net.name
                    )));
                }
                many => {
                    return Err(crate::Error::NllParse(format!(
                        "network '{}': node '{node}' has {} interfaces on this network ({}); use `port {node}:<iface>`",
                        net.name,
                        many.len(),
                        many.iter()
                            .map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        };
        if network.ports.contains_key(&key) {
            return Err(crate::Error::NllParse(format!(
                "network '{}': duplicate port block for '{key}'",
                net.name
            )));
        }
        network.ports.insert(
            key,
            types::PortConfig {
                interface: None,
                vlans: port.vlans.clone(),
                tagged: if port.tagged { Some(true) } else { None },
                pvid: port.pvid,
                untagged: if port.untagged { Some(true) } else { None },
                addresses: port.addresses.clone(),
            },
        );
    }

    for imp in expand_network_impairs(&net.impairments, &net.loops, vars)? {
        network.impairments.push(types::NetworkImpairment {
            src: imp.src.clone(),
            dst: imp.dst.clone(),
            impairment: lower_impair_props(&imp.props),
            rate_cap: imp.rate_cap.clone(),
        });
    }

    // Subnet auto-assignment: sequential host addresses for members
    // without an explicit `port … { address … }`. Port entries are keyed
    // by the member endpoint `node:iface` (issue #19). `auto/N` and
    // unresolved placeholders are left for deploy time.
    if let Some(subnet) = &net.subnet {
        network.subnet = Some(subnet.clone());
        if !subnet.starts_with("auto") && !subnet.contains("${") {
            let (base_ip, prefix) = crate::helpers::parse_cidr(subnet).map_err(|e| {
                crate::Error::NllParse(format!(
                    "network '{}': invalid subnet '{subnet}': {e}",
                    net.name
                ))
            })?;
            let base_ip = crate::helpers::network_address(base_ip, prefix);
            let host_bits = match base_ip {
                std::net::IpAddr::V4(_) => 32u32.saturating_sub(prefix as u32),
                std::net::IpAddr::V6(_) => 128u32.saturating_sub(prefix as u32),
            };
            // usable hosts: 2^bits - 2 (network + broadcast), capped so the
            // comparison below cannot overflow
            let max_host: u64 = if host_bits >= 63 {
                u64::MAX
            } else {
                (1u64 << host_bits).saturating_sub(2)
            };
            let mut host_num: u64 = 1;
            for member in &network.members {
                if network
                    .ports
                    .get(member)
                    .is_some_and(|p| !p.addresses.is_empty())
                {
                    continue;
                }
                if host_num > max_host {
                    return Err(crate::Error::NllParse(format!(
                        "network '{}': subnet {subnet} has only {max_host} usable host address(es) but {} members need one",
                        net.name,
                        network.members.len()
                    )));
                }
                let ip = increment_ip(base_ip, host_num as u32);
                let addr = format!("{ip}/{prefix}");
                network
                    .ports
                    .entry(member.clone())
                    .or_default()
                    .addresses
                    .push(addr);
                host_num += 1;
            }
        }
    }

    if topo.networks.contains_key(&net.name) {
        return Err(crate::Error::NllParse(format!(
            "duplicate network name '{}' — each network must have a unique name",
            net.name
        )));
    }
    topo.networks.insert(net.name.clone(), network);
    Ok(())
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

fn lower_benchmark(b: &ast::BenchmarkDef) -> Result<types::Benchmark> {
    let tests = b
        .tests
        .iter()
        .map(|t| match t {
            ast::BenchmarkTestDef::Iperf3 {
                from,
                to,
                duration,
                streams,
                udp,
                assertions,
            } => Ok(types::BenchmarkTest::Iperf3 {
                from: from.clone(),
                to: to.clone(),
                duration: duration.clone(),
                streams: *streams,
                udp: *udp,
                assertions: lower_benchmark_assertions(assertions)?,
            }),
            ast::BenchmarkTestDef::Ping {
                from,
                to,
                count,
                assertions,
            } => Ok(types::BenchmarkTest::Ping {
                from: from.clone(),
                to: to.clone(),
                count: *count,
                assertions: lower_benchmark_assertions(assertions)?,
            }),
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(types::Benchmark {
        name: b.name.clone(),
        tests,
    })
}

fn lower_benchmark_assertions(
    defs: &[ast::BenchmarkAssertionDef],
) -> Result<Vec<types::BenchmarkAssertion>> {
    defs.iter()
        .map(|a| {
            Ok(types::BenchmarkAssertion {
                metric: a.metric.clone(),
                op: match a.op.as_str() {
                    "above" | ">" => types::CompareOp::Gt,
                    "below" | "<" => types::CompareOp::Lt,
                    ">=" => types::CompareOp::Gte,
                    "<=" => types::CompareOp::Lte,
                    other => {
                        return Err(crate::Error::NllParse(format!(
                            "unknown benchmark comparison '{other}' for metric '{}' (expected above, below, >, <, >= or <=)",
                            a.metric
                        )));
                    }
                },
                value: a.value.clone(),
            })
        })
        .collect()
}

fn lower_scenario(s: &ast::ScenarioDef) -> Result<types::Scenario> {
    let mut steps = Vec::new();
    let mut cumulative_ms: u64 = 0;

    for step in &s.steps {
        let time_str = step.time.trim();
        let (is_relative, dur_str) = if let Some(stripped) = time_str.strip_prefix('+') {
            (true, stripped)
        } else {
            (false, time_str)
        };

        let dur = crate::helpers::parse_duration(dur_str).map_err(|_| {
            crate::error::Error::invalid_topology(format!(
                "invalid duration '{}' in scenario '{}'",
                step.time, s.name
            ))
        })?;
        let ms = dur.as_millis() as u64;

        let time_ms = if is_relative {
            cumulative_ms += ms;
            cumulative_ms
        } else {
            cumulative_ms = ms;
            ms
        };

        let mut actions = Vec::new();
        for action in &step.actions {
            actions.push(match action {
                ast::ScenarioActionDef::Down(ep) => types::ScenarioAction::Down(ep.clone()),
                ast::ScenarioActionDef::Up(ep) => types::ScenarioAction::Up(ep.clone()),
                ast::ScenarioActionDef::Clear(ep) => types::ScenarioAction::Clear(ep.clone()),
                ast::ScenarioActionDef::Log(msg) => types::ScenarioAction::Log(msg.clone()),
                ast::ScenarioActionDef::Exec { node, cmd } => types::ScenarioAction::Exec {
                    node: node.clone(),
                    cmd: cmd.clone(),
                },
                ast::ScenarioActionDef::Validate(assertions) => {
                    let mut typed = Vec::new();
                    for a in assertions {
                        typed.push(match a {
                            ast::AssertionDef::Reach { from, to } => types::Assertion::Reach {
                                from: from.clone(),
                                to: to.clone(),
                            },
                            ast::AssertionDef::NoReach { from, to } => types::Assertion::NoReach {
                                from: from.clone(),
                                to: to.clone(),
                            },
                            ast::AssertionDef::TcpConnect {
                                from,
                                to,
                                port,
                                timeout,
                                retries,
                                interval,
                            } => types::Assertion::TcpConnect {
                                from: from.clone(),
                                to: to.clone(),
                                port: *port,
                                timeout: timeout.clone(),
                                retries: *retries,
                                interval: interval.clone(),
                            },
                            ast::AssertionDef::LatencyUnder {
                                from,
                                to,
                                max,
                                samples,
                            } => types::Assertion::LatencyUnder {
                                from: from.clone(),
                                to: to.clone(),
                                max: max.clone(),
                                samples: *samples,
                            },
                            ast::AssertionDef::RouteHas {
                                node,
                                destination,
                                via,
                                dev,
                            } => types::Assertion::RouteHas {
                                node: node.clone(),
                                destination: destination.clone(),
                                via: via.clone(),
                                dev: dev.clone(),
                            },
                            ast::AssertionDef::DnsResolves {
                                from,
                                name,
                                expected_ip,
                            } => types::Assertion::DnsResolves {
                                from: from.clone(),
                                name: name.clone(),
                                expected_ip: expected_ip.clone(),
                            },
                        });
                    }
                    types::ScenarioAction::Validate(typed)
                }
            });
        }

        steps.push(types::ScenarioStep { time_ms, actions });
    }

    // Sort steps by time
    steps.sort_by_key(|s| s.time_ms);

    Ok(types::Scenario {
        name: s.name.clone(),
        steps,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::eval_condition;
    use crate::parser::nll;
    use crate::types;

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
        assert_eq!(topo.nodes["router"].sysctls["net.ipv4.ip_forward"], "1");
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
        assert_eq!(topo.impairments["host:eth0"].delay.as_deref(), Some("10ms"));
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
    fn test_multi_profile_inheritance() {
        let topo = parse_and_lower(
            r#"lab "t"
profile router { forward ipv4 }
profile monitored { sysctl "net.core.rmem_max" "16777216" }
node r1 : router, monitored"#,
        );
        // Gets forwarding from router profile
        assert_eq!(topo.nodes["r1"].sysctls["net.ipv4.ip_forward"], "1");
        // Gets sysctl from monitored profile
        assert_eq!(topo.nodes["r1"].sysctls["net.core.rmem_max"], "16777216");
    }

    #[test]
    fn test_multi_profile_override() {
        let topo = parse_and_lower(
            r#"lab "t"
profile base { sysctl "net.core.rmem_max" "1000" }
profile override { sysctl "net.core.rmem_max" "9999" }
node r1 : base, override"#,
        );
        // Later profile wins for conflicting keys
        assert_eq!(topo.nodes["r1"].sysctls["net.core.rmem_max"], "9999");
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
        let link = topo
            .links
            .iter()
            .find(|l| l.endpoints[0] == "spine1:eth1")
            .unwrap();
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
        assert_eq!(topo.impairments["a:e0"].delay.as_deref(), Some("30ms"));
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
        assert_eq!(topo.impairments["a:e0"].rate.as_deref(), Some("10mbit"));
        assert_eq!(topo.impairments["b:e0"].rate.as_deref(), Some("2mbit"));
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
    fn test_lower_dns_hosts() {
        let topo = parse_and_lower(r#"lab "t" { dns hosts }"#);
        assert_eq!(topo.lab.dns, types::DnsMode::Hosts);
    }

    #[test]
    fn test_lower_dns_off() {
        let topo = parse_and_lower(r#"lab "t" { dns off }"#);
        assert_eq!(topo.lab.dns, types::DnsMode::Off);
    }

    #[test]
    fn test_lower_dns_default() {
        let topo = parse_and_lower(r#"lab "t""#);
        assert_eq!(topo.lab.dns, types::DnsMode::Off);
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
        assert!(wg.fwmark.is_none(), "fwmark defaults to None when omitted");
        assert_eq!(wg.addresses, vec!["192.168.255.1/32"]);
        assert_eq!(wg.peers, vec!["gw-b"]);
    }

    /// Plan 159 follow-up — `fwmark <u32>` inside a `wireguard`
    /// block sets the routing mark applied to outbound tunnel
    /// packets. Threads through to nlink 0.19's
    /// `DeclaredWgDeviceBuilder::fwmark`.
    #[test]
    fn test_lower_wireguard_with_fwmark() {
        let topo = parse_and_lower(
            r#"lab "t"

node gw {
  wireguard wg0 {
    key auto
    listen 51820
    fwmark 100
    address 192.168.255.1/32
  }
}"#,
        );
        let wg = &topo.nodes["gw"].wireguard["wg0"];
        assert_eq!(wg.fwmark, Some(100));
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
        assert_eq!(iface.kind, Some(types::InterfaceKind::Vxlan));
        assert_eq!(iface.vni, Some(100));
        assert_eq!(iface.local.as_deref(), Some("10.0.0.1"));
        assert_eq!(iface.remote.as_deref(), Some("10.0.0.2"));
        assert_eq!(iface.port, Some(4789));
        assert_eq!(iface.addresses, vec!["192.168.100.1/24"]);
    }

    /// Plan 159 follow-up — `underlay` pins the VXLAN tunnel to a
    /// specific underlay device. nlink 0.19's
    /// `LinkBuilder::vxlan_underlay_dev` sets `IFLA_VXLAN_LINK`.
    #[test]
    fn test_lower_vxlan_with_underlay() {
        let topo = parse_and_lower(
            r#"lab "t"

node vtep1 {
  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    underlay eth0
  }
}"#,
        );
        let iface = &topo.nodes["vtep1"].interfaces["vxlan100"];
        assert_eq!(iface.kind, Some(types::InterfaceKind::Vxlan));
        assert_eq!(iface.underlay.as_deref(), Some("eth0"));
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
        // port keys are canonicalised to the member endpoint
        assert_eq!(net.ports["host1:eth0"].pvid, Some(100));
        assert_eq!(net.ports["host1:eth0"].untagged, Some(true));
    }

    fn lower_err(input: &str) -> String {
        match nll::parse(input) {
            Ok(_) => panic!("expected parsing/lowering to fail"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn port_block_accepts_node_iface_and_rejects_ambiguity() {
        let topo = parse_and_lower(
            r#"lab "t"
network n {
  members [a:eth0, a:eth1, b:eth0]
  port a:eth1 { pvid 7 }
}"#,
        );
        assert_eq!(topo.networks["n"].ports["a:eth1"].pvid, Some(7));
        let e = lower_err(
            r#"lab "t"
network n {
  members [a:eth0, a:eth1]
  port a { pvid 7 }
}"#,
        );
        assert!(e.contains("use `port a:<iface>`"), "{e}");
        let e = lower_err(
            r#"lab "t"
network n {
  members [a:eth0]
  port zz { pvid 7 }
}"#,
        );
        assert!(e.contains("not a member"), "{e}");
    }

    #[test]
    fn network_subnet_autoassign_respects_explicit_port_and_bounds() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.0.1.0/24
  port b { 10.0.1.200/24 }
}"#,
        );
        let net = &topo.networks["lan"];
        assert_eq!(net.ports["a:eth0"].addresses, vec!["10.0.1.1/24"]);
        assert_eq!(net.ports["b:eth0"].addresses, vec!["10.0.1.200/24"]);
        assert_eq!(net.ports["c:eth0"].addresses, vec!["10.0.1.2/24"]);
        assert_eq!(net.ports.len(), 3, "no second, differently-keyed entry");

        let e = lower_err(
            r#"lab "t"
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.0.1.0/30
}"#,
        );
        assert!(e.contains("only 2 usable host address"), "{e}");
        // base is masked to the network address
        let topo = parse_and_lower(
            r#"lab "t"
network lan { members [a:eth0]  subnet 10.0.1.5/24 }"#,
        );
        assert_eq!(
            topo.networks["lan"].ports["a:eth0"].addresses,
            vec!["10.0.1.1/24"]
        );
    }

    #[test]
    fn star_pool_exhaustion_is_an_error() {
        let e = lower_err(
            r#"lab "t"
pool access 10.0.0.0/24 /24
star campus { hub r  spokes [a, b]  pool access }"#,
        );
        assert!(e.contains("pool 'access' exhausted"), "{e}");
        // and ring/mesh/star all share the checked allocator
        let e = lower_err(
            r#"lab "t"
pool p 10.0.0.0/30 /30
ring r { count 3  pool p }"#,
        );
        assert!(e.contains("exhausted"), "{e}");
        let e = lower_err(
            r#"lab "t"
pool p 10.0.0.0/24 /8
node a"#,
        );
        assert!(e.contains("allocation prefix"), "{e}");
    }

    #[test]
    fn undefined_pool_is_an_error() {
        let e = lower_err(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { pool nope }"#,
        );
        assert!(e.contains("undefined pool 'nope'"), "{e}");
    }

    #[test]
    fn validate_and_scenario_inside_for_are_interpolated() {
        let topo = parse_and_lower(
            r#"lab "t"
node spine
for i in 1..2 {
  node leaf${i}
  validate { reach leaf${i} spine }
}
scenario "s" {
  at 1s { down leaf1:eth0  validate { no-reach leaf1 spine } }
}"#,
        );
        let names: Vec<String> = topo
            .assertions
            .iter()
            .map(|a| match a {
                crate::types::Assertion::Reach { from, to } => format!("{from}->{to}"),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(names, vec!["leaf1->spine", "leaf2->spine"]);
        assert!(!format!("{:?}", topo.assertions).contains("${"));
    }

    #[test]
    fn site_bodies_keep_every_statement_kind() {
        let topo = parse_and_lower(
            r#"lab "t"
site dc {
  for i in 1..2 { node r${i} }
  link r1:eth0 -- r2:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
  impair r1:eth0 delay 5ms
  validate { reach r1 r2 }
}"#,
        );
        assert!(topo.nodes.contains_key("dc-r1"), "{:?}", topo.nodes.keys());
        assert!(topo.nodes.contains_key("dc-r2"));
        assert_eq!(topo.links[0].endpoints, ["dc-r1:eth0", "dc-r2:eth0"]);
        assert!(topo.impairments.contains_key("dc-r1:eth0"));
        assert!(matches!(
            &topo.assertions[0],
            crate::types::Assertion::Reach { from, to } if from == "dc-r1" && to == "dc-r2"
        ));
    }

    #[test]
    fn if_bodies_keep_every_statement_kind_and_see_loop_vars() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
for i in 1..3 {
  if ${i} == 2 {
    node only${i}
    validate { reach only${i} a }
  }
}"#,
        );
        assert_eq!(topo.nodes.len(), 2);
        assert!(topo.nodes.contains_key("only2"));
        assert_eq!(topo.assertions.len(), 1);
    }

    #[test]
    fn node_level_for_handles_every_prop_and_accumulates_nat() {
        let topo = parse_and_lower(
            r#"lab "t"
node r {
  for i in 1..2 {
    sysctl "net.ipv4.conf.eth${i}.rp_filter" "0"
    nat { masquerade src 10.${i}.0.0/24 }
    dummy d${i} { address 192.0.2.1/32 }
  }
  nat { masquerade }
}"#,
        );
        let r = &topo.nodes["r"];
        assert_eq!(r.sysctls.len(), 2);
        assert!(r.sysctls.contains_key("net.ipv4.conf.eth2.rp_filter"));
        assert_eq!(r.nat.as_ref().unwrap().rules.len(), 3, "{:?}", r.nat);
        assert!(r.interfaces.contains_key("d1") && r.interfaces.contains_key("d2"));
    }

    #[test]
    fn loop_variables_are_scoped() {
        let topo = parse_and_lower(
            r#"lab "t"
let i = 99
for i in 1..2 {
  let inner = "x${i}"
  node n${i}
}
node after${i}
node leaked${inner}"#,
        );
        assert!(
            topo.nodes.contains_key("after99"),
            "{:?}",
            topo.nodes.keys()
        );
        assert!(
            topo.nodes.contains_key("leaked${inner}"),
            "inner let must not leak: {:?}",
            topo.nodes.keys()
        );
    }

    #[test]
    fn oversized_for_range_is_rejected() {
        let e = lower_err("lab \"t\"\nfor i in 1..999999999 { node n${i} }");
        assert!(e.contains("would iterate"), "{e}");
    }

    #[test]
    fn unknown_enum_strings_are_errors() {
        for (src, needle) in [
            (
                "lab \"t\" { dns bogus }\nnode a",
                "unknown dns mode 'bogus'",
            ),
            (
                "lab \"t\" { runtime \"containerd\" }\nnode a",
                "unknown runtime 'containerd'",
            ),
            (
                "lab \"t\" { routing bogus }\nnode a",
                "unknown routing mode 'bogus'",
            ),
            ("lab \"t\"\nnode a { nat { frobnicate } }", "NAT"),
            (
                "lab \"t\"\nnode a { macvlan m parent eth0 { mode weird } }",
                "macvlan mode",
            ),
        ] {
            let e = nll::parse(src)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(e.contains(needle), "{src}: {e}");
        }
    }

    #[test]
    fn param_without_default_is_always_an_error() {
        let e = lower_err("lab \"t\"\nparam count\nnode a");
        assert!(e.contains("required parameter 'count'"), "{e}");
    }

    #[test]
    fn test_lower_network_subnet() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.0.1.0/24
}"#,
        );
        let net = &topo.networks["lan"];
        assert_eq!(net.subnet.as_deref(), Some("10.0.1.0/24"));
        // Auto-assigned: a=.1, b=.2, c=.3
        assert_eq!(net.ports["a:eth0"].addresses, vec!["10.0.1.1/24"]);
        assert_eq!(net.ports["b:eth0"].addresses, vec!["10.0.1.2/24"]);
        assert_eq!(net.ports["c:eth0"].addresses, vec!["10.0.1.3/24"]);
    }

    #[test]
    fn test_lower_network_per_pair_impair() {
        let topo = parse_and_lower(
            r#"lab "t"
node hq
node alpha
node bravo
network radio {
  members [hq:fo, alpha:fo, bravo:fo]
  subnet 172.100.3.0/24
  impair hq -- alpha { delay 15ms loss 1% }
  impair hq -- bravo { delay 40ms rate-cap 100mbit }
}"#,
        );
        let net = &topo.networks["radio"];
        assert_eq!(net.impairments.len(), 2);

        let r0 = &net.impairments[0];
        assert_eq!(r0.src, "hq");
        assert_eq!(r0.dst, "alpha");
        assert_eq!(r0.impairment.delay.as_deref(), Some("15ms"));
        assert_eq!(r0.impairment.loss.as_deref(), Some("1%"));
        assert!(r0.rate_cap.is_none());

        let r1 = &net.impairments[1];
        assert_eq!(r1.dst, "bravo");
        assert_eq!(r1.rate_cap.as_deref(), Some("100mbit"));
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
    fn test_interpolation_no_spaces() {
        let topo = parse_and_lower(
            r#"lab "t"

for i in 1..3 {
  node n${i} { lo 10.0.0.${i*10}/32 }
}"#,
        );
        assert_eq!(topo.nodes.len(), 3);
        let lo1 = &topo.nodes["n1"].interfaces["lo"];
        assert_eq!(lo1.addresses, vec!["10.0.0.10/32"]);
        let lo3 = &topo.nodes["n3"].interfaces["lo"];
        assert_eq!(lo3.addresses, vec!["10.0.0.30/32"]);
    }

    #[test]
    fn test_duplicate_node_error() {
        let result = nll::parse(
            r#"lab "t"
node a
node a"#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("duplicate node"), "got: {err}");
    }

    #[test]
    fn test_undefined_profile_error() {
        let result = nll::parse(
            r#"lab "t"
node r1 : nonexistent"#,
        );
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
        assert_eq!(fw.rules.len(), 4);
    }

    #[test]
    fn test_example_vxlan() {
        let topo = parse_example("vxlan-overlay.nll");
        let vxlan = &topo.nodes["vtep1"].interfaces["vxlan100"];
        assert_eq!(vxlan.kind, Some(types::InterfaceKind::Vxlan));
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
        // Walk the top-level examples/ dir plus the cookbook/ subdir.
        // (imports/ is skipped — those files are designed to be
        // imported, not parsed standalone.)
        let dirs = [dir.clone(), dir.join("cookbook")];
        for d in &dirs {
            if !d.exists() {
                continue;
            }
            for entry in std::fs::read_dir(d).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) == Some("nll") {
                    let topo = crate::parser::parse_file(&path)
                        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
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
        }
        assert!(
            count >= 12,
            "expected at least 12 .nll examples, found {count}"
        );
    }

    // ─── Import tests ────────────────────────────────────

    #[test]
    fn test_import_basic() {
        let composed_path = examples_dir().join("imports/composed.nll");
        let topo = crate::parser::parse_file(&composed_path).unwrap();

        assert_eq!(topo.lab.name, "composed");
        // Imported nodes are prefixed with "dc-"
        assert!(topo.nodes.contains_key("dc-r1"), "missing dc.r1");
        assert!(topo.nodes.contains_key("dc-r2"), "missing dc.r2");
        // Local node is not prefixed
        assert!(topo.nodes.contains_key("host"), "missing host");
        // Total: 2 imported + 1 local
        assert_eq!(topo.nodes.len(), 3);

        // Imported link endpoints are prefixed
        let imported_link = topo
            .links
            .iter()
            .find(|l| l.endpoints[0].starts_with("dc-") && l.endpoints[1].starts_with("dc-"));
        assert!(imported_link.is_some(), "imported link not found");

        // Local link references the imported node
        let local_link = topo.links.iter().find(|l| {
            l.endpoints.iter().any(|e| e == "dc-r1:eth1")
                && l.endpoints.iter().any(|e| e == "host:eth0")
        });
        assert!(local_link.is_some(), "local→imported link not found");
    }

    #[test]
    fn test_import_circular_rejected() {
        // Create a temp file that imports itself
        let dir = std::env::temp_dir().join("nlink-lab-test-circular");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("self.nll");
        std::fs::write(
            &file,
            r#"import "self.nll" as me
lab "circular"
node a
"#,
        )
        .unwrap();

        let result = crate::parser::parse_file(&file);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("circular"),
            "expected circular import error, got: {err}"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_import_prefix_endpoint() {
        assert_eq!(super::prefix_endpoint("dc", "r1:eth0"), "dc-r1:eth0");
        assert_eq!(super::prefix_endpoint("wan", "pe1:wan0"), "wan-pe1:wan0");
        assert_eq!(super::prefix_endpoint("dc", "switch:br0"), "dc-switch:br0");
    }

    // ─── Expression engine tests ─────────────────────────

    #[test]
    fn test_modulo_operator() {
        let topo = parse_and_lower(
            r#"lab "t"
for i in 0..3 {
    node n${i} { lo 10.0.${i % 2}.${i}/32 }
}"#,
        );
        assert_eq!(
            topo.nodes["n0"].interfaces["lo"].addresses,
            vec!["10.0.0.0/32"]
        );
        assert_eq!(
            topo.nodes["n1"].interfaces["lo"].addresses,
            vec!["10.0.1.1/32"]
        );
        assert_eq!(
            topo.nodes["n2"].interfaces["lo"].addresses,
            vec!["10.0.0.2/32"]
        );
        assert_eq!(
            topo.nodes["n3"].interfaces["lo"].addresses,
            vec!["10.0.1.3/32"]
        );
    }

    #[test]
    fn test_compound_expression() {
        let topo = parse_and_lower(
            r#"lab "t"
for i in 1..3 {
    node n${i} { lo 10.0.0.${(i - 1) * 10 + 1}/32 }
}"#,
        );
        assert_eq!(
            topo.nodes["n1"].interfaces["lo"].addresses,
            vec!["10.0.0.1/32"]
        );
        assert_eq!(
            topo.nodes["n2"].interfaces["lo"].addresses,
            vec!["10.0.0.11/32"]
        );
        assert_eq!(
            topo.nodes["n3"].interfaces["lo"].addresses,
            vec!["10.0.0.21/32"]
        );
    }

    #[test]
    fn test_ternary_conditional() {
        let mut vars = BTreeMap::new();
        vars.insert("env".into(), "prod".into());
        assert_eq!(
            super::eval_expr(r#"env == "prod" ? 5ms : 50ms"#, &vars),
            "5ms"
        );
        assert_eq!(
            super::eval_expr(r#"env != "prod" ? 5ms : 50ms"#, &vars),
            "50ms"
        );

        vars.insert("env".into(), "dev".into());
        assert_eq!(
            super::eval_expr(r#"env == "prod" ? 5ms : 50ms"#, &vars),
            "50ms"
        );
    }

    #[test]
    fn test_ternary_with_variables() {
        let mut vars = BTreeMap::new();
        vars.insert("mode".into(), "fast".into());
        vars.insert("fast_delay".into(), "1ms".into());
        vars.insert("slow_delay".into(), "100ms".into());
        assert_eq!(
            super::eval_expr(r#"mode == "fast" ? fast_delay : slow_delay"#, &vars),
            "1ms"
        );
    }

    #[test]
    fn test_division_by_zero() {
        let vars = BTreeMap::new();
        // Division by zero returns the original expression
        assert_eq!(super::eval_expr("4 / 0", &vars), "${4 / 0}");
        assert_eq!(super::eval_expr("4 % 0", &vars), "${4 % 0}");
    }

    #[test]
    fn test_backward_compat_simple() {
        let mut vars = BTreeMap::new();
        vars.insert("i".into(), "3".into());
        // All existing expression forms still work
        assert_eq!(super::eval_expr("i", &vars), "3");
        assert_eq!(super::eval_expr("i + 1", &vars), "4");
        assert_eq!(super::eval_expr("i+1", &vars), "4");
        assert_eq!(super::eval_expr("i - 1", &vars), "2");
        assert_eq!(super::eval_expr("i * 2", &vars), "6");
        assert_eq!(super::eval_expr("i / 2", &vars), "1");
    }

    #[test]
    fn test_auto_variables_loop() {
        // loop.index is 0-based iteration index
        let topo = parse_and_lower(
            r#"lab "t"
for i in 1..3 {
    node n${i} { lo 10.0.${loop.index}.0/32 }
}"#,
        );
        assert_eq!(
            topo.nodes["n1"].interfaces["lo"].addresses,
            vec!["10.0.0.0/32"]
        ); // index 0
        assert_eq!(
            topo.nodes["n2"].interfaces["lo"].addresses,
            vec!["10.0.1.0/32"]
        ); // index 1
        assert_eq!(
            topo.nodes["n3"].interfaces["lo"].addresses,
            vec!["10.0.2.0/32"]
        ); // index 2
    }

    #[test]
    fn test_auto_variables_loop_first_last() {
        let mut vars = BTreeMap::new();
        // Simulate first iteration of for i in 1..3
        vars.insert("i".into(), "1".into());
        vars.insert("loop.first".into(), "true".into());
        vars.insert("loop.last".into(), "false".into());
        assert_eq!(
            super::eval_expr(r#"loop.first == "true" ? first : other"#, &vars),
            "first"
        );
        assert_eq!(
            super::eval_expr(r#"loop.last == "true" ? last : other"#, &vars),
            "other"
        );
    }

    #[test]
    fn test_auto_variables_lab() {
        let topo = parse_and_lower(
            r#"lab "mylab" { prefix "ml" }
node test"#,
        );
        // Lab variables are available during expansion but consumed.
        // Verify the lab name and prefix were set correctly.
        assert_eq!(topo.lab.name, "mylab");
        assert_eq!(topo.lab.prefix(), "ml");
    }

    #[test]
    fn test_block_comments() {
        let topo =
            parse_and_lower("lab \"t\"\nnode a\n/* this node is disabled\nnode b\n*/\nnode c");
        assert_eq!(topo.nodes.len(), 2);
        assert!(topo.nodes.contains_key("a"));
        assert!(topo.nodes.contains_key("c"));
        assert!(!topo.nodes.contains_key("b"));
    }

    #[test]
    fn test_nested_block_comments() {
        let topo =
            parse_and_lower("lab \"t\"\nnode a\n/* outer /* inner */ still commented */\nnode b");
        assert_eq!(topo.nodes.len(), 2);
        assert!(topo.nodes.contains_key("a"));
        assert!(topo.nodes.contains_key("b"));
    }

    // ─── Wave 2 tests ───────────────────────────────────

    #[test]
    fn test_subnet_auto_assign_slash30() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.0.0/30 }"#,
        );
        let link = &topo.links[0];
        let addrs = link.addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.0.1/30");
        assert_eq!(addrs[1], "10.0.0.2/30");
    }

    #[test]
    fn test_subnet_auto_assign_slash31() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.0.0/31 }"#,
        );
        let link = &topo.links[0];
        let addrs = link.addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.0.0/31");
        assert_eq!(addrs[1], "10.0.0.1/31");
    }

    #[test]
    fn test_subnet_auto_assign_slash24() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.1.0/24 }"#,
        );
        let link = &topo.links[0];
        let addrs = link.addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.1.1/24");
        assert_eq!(addrs[1], "10.0.1.2/24");
    }

    #[test]
    fn test_subnet_with_mtu() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.0.0/30 mtu 9000 }"#,
        );
        let link = &topo.links[0];
        assert!(link.addresses.is_some());
        assert_eq!(link.mtu, Some(9000));
    }

    #[test]
    fn test_explicit_addresses_still_work() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }"#,
        );
        let link = &topo.links[0];
        let addrs = link.addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.0.1/24");
        assert_eq!(addrs[1], "10.0.0.2/24");
    }

    #[test]
    fn test_list_iteration() {
        let topo = parse_and_lower(
            r#"lab "t"
for role in [web, api, db] {
    node ${role}
}"#,
        );
        assert_eq!(topo.nodes.len(), 3);
        assert!(topo.nodes.contains_key("web"));
        assert!(topo.nodes.contains_key("api"));
        assert!(topo.nodes.contains_key("db"));
    }

    #[test]
    fn test_list_iteration_with_properties() {
        let topo = parse_and_lower(
            r#"lab "t"
for name in [alpha, beta] {
    node ${name} { route default via 10.0.0.1 }
}"#,
        );
        assert_eq!(topo.nodes.len(), 2);
        assert!(topo.nodes["alpha"].routes.contains_key("default"));
        assert!(topo.nodes["beta"].routes.contains_key("default"));
    }

    #[test]
    fn test_integer_range_still_works() {
        let topo = parse_and_lower(
            r#"lab "t"
for i in 1..3 {
    node n${i}
}"#,
        );
        assert_eq!(topo.nodes.len(), 3);
        assert!(topo.nodes.contains_key("n1"));
        assert!(topo.nodes.contains_key("n2"));
        assert!(topo.nodes.contains_key("n3"));
    }

    #[test]
    fn test_defaults_link_mtu() {
        let topo = parse_and_lower(
            r#"lab "t"
defaults link { mtu 9000 }
node a
node b
node c
link a:eth0 -- b:eth0 { subnet 10.0.0.0/30 }
link b:eth0 -- c:eth0 { subnet 10.0.1.0/30 mtu 1500 }
"#,
        );
        // First link gets default MTU
        assert_eq!(topo.links[0].mtu, Some(9000));
        // Second link overrides
        assert_eq!(topo.links[1].mtu, Some(1500));
    }

    #[test]
    fn test_for_expression_in_peers() {
        let topo = parse_and_lower(
            r#"lab "t"
node hub {
    wireguard wg0 {
        key auto
        listen 51820
        address 10.0.0.1/32
        peers [for i in 1..3 : spoke${i}]
    }
}
for i in 1..3 {
    node spoke${i} {
        wireguard wg0 {
            key auto
            listen 51820
            address 10.0.0.${i + 1}/32
            peers [hub]
        }
    }
}
"#,
        );
        let hub_wg = &topo.nodes["hub"].wireguard["wg0"];
        assert_eq!(hub_wg.peers, vec!["spoke1", "spoke2", "spoke3"]);
    }

    #[test]
    fn test_defaults_impair() {
        let topo = parse_and_lower(
            r#"lab "t"
defaults impair { delay 5ms }
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.0.0/30 }
"#,
        );
        // Both endpoints should have the default impairment
        let ep = "a:eth0";
        assert!(topo.impairments.contains_key(ep));
        assert_eq!(topo.impairments[ep].delay.as_deref(), Some("5ms"));
    }

    // ─── Plan 094 tests ─────────────────────────────────

    #[test]
    fn test_cross_reference_route() {
        let topo = parse_and_lower(
            r#"lab "t"
node router
node host { route default via ${router.eth0} }
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        let route = &topo.nodes["host"].routes["default"];
        assert_eq!(route.via.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn test_cross_reference_forward() {
        // Cross-ref where the link appears BEFORE the route (forward reference)
        let topo = parse_and_lower(
            r#"lab "t"
link r1:eth0 -- h1:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node r1
node h1 { route default via ${r1.eth0} }
"#,
        );
        let route = &topo.nodes["h1"].routes["default"];
        assert_eq!(route.via.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn test_cross_reference_unresolved_stays() {
        // Reference to nonexistent node.iface stays as-is (not an error for flexibility)
        let topo = parse_and_lower(
            r#"lab "t"
node a { route default via ${nonexist.eth0} }
"#,
        );
        let route = &topo.nodes["a"].routes["default"];
        assert_eq!(route.via.as_deref(), Some("${nonexist.eth0}"));
    }

    #[test]
    fn test_cross_reference_with_subnet_auto() {
        let topo = parse_and_lower(
            r#"lab "t"
node r1
node h1 { route default via ${r1.eth0} }
link r1:eth0 -- h1:eth0 { subnet 10.0.0.0/30 }
"#,
        );
        let route = &topo.nodes["h1"].routes["default"];
        // Subnet auto-assigns .1 to left (r1:eth0)
        assert_eq!(route.via.as_deref(), Some("10.0.0.1"));
    }

    // ─── Container property tests ────────────────────

    #[test]
    fn test_lower_container_properties() {
        let topo = parse_and_lower(
            r#"lab "t"
node web image "nginx" {
    cpu 0.5
    memory "256m"
    hostname "web-01"
    workdir "/app"
    entrypoint "/bin/sh"
    privileged
    labels ["role=web"]
    pull always
    exec "echo setup"
    startup-delay 3s
    healthcheck "curl localhost"
}"#,
        );
        let n = &topo.nodes["web"];
        assert_eq!(n.cpu.as_deref(), Some("0.5"));
        assert_eq!(n.memory.as_deref(), Some("256m"));
        assert_eq!(n.hostname.as_deref(), Some("web-01"));
        assert_eq!(n.workdir.as_deref(), Some("/app"));
        assert_eq!(n.entrypoint.as_deref(), Some("/bin/sh"));
        assert!(n.privileged);
        assert_eq!(n.labels, vec!["role=web"]);
        assert_eq!(n.pull.as_deref(), Some("always"));
        assert_eq!(n.container_exec, vec!["echo setup"]);
        assert_eq!(n.startup_delay.as_deref(), Some("3s"));
        assert_eq!(n.healthcheck.as_deref(), Some("curl localhost"));
    }

    #[test]
    fn test_lower_container_depends_on() {
        let topo = parse_and_lower(
            r#"lab "t"
node db image "postgres"
node app image "myapp" {
    depends-on [db]
}"#,
        );
        assert_eq!(topo.nodes["app"].depends_on, vec!["db"]);
        assert!(topo.nodes["db"].depends_on.is_empty());
    }

    #[test]
    fn test_lower_container_config_overlay() {
        let topo = parse_and_lower(
            r#"lab "t"
node router image "frr" {
    config "a.conf" "/etc/a.conf"
    config "b.conf" "/etc/b.conf"
    overlay "configs/router/"
    env-file "router.env"
}"#,
        );
        let n = &topo.nodes["router"];
        assert_eq!(n.configs.len(), 2);
        assert_eq!(
            n.configs[0],
            ("a.conf".to_string(), "/etc/a.conf".to_string())
        );
        assert_eq!(n.overlay.as_deref(), Some("configs/router/"));
        assert_eq!(n.env_file.as_deref(), Some("router.env"));
    }

    #[test]
    fn test_nested_interpolation() {
        let mut vars = BTreeMap::new();
        vars.insert("i".into(), "2".into());
        vars.insert("leaf2".into(), "resolved".into());
        // ${leaf${i}} → first pass resolves ${i} → ${leaf2} → second pass → "resolved"
        assert_eq!(super::interpolate("${leaf${i}}", &vars), "resolved");
    }

    #[test]
    fn test_adjacent_interpolation_in_topology() {
        let topo = parse_and_lower(
            r#"lab "t"
let base = "node"
for i in 1..2 {
    node ${base}${i}
}"#,
        );
        assert!(topo.nodes.contains_key("node1"));
        assert!(topo.nodes.contains_key("node2"));
    }

    // ─── Plan 098 tests ──────────────────────────────

    #[test]
    fn test_subnet_pool_allocation() {
        let topo = parse_and_lower(
            r#"lab "t"
pool fabric 10.0.0.0/24 /30
node a
node b
node c
node d
link a:eth0 -- b:eth0 { pool fabric }
link c:eth0 -- d:eth0 { pool fabric }
"#,
        );
        // First allocation: 10.0.0.0/30 → .1 and .2
        let l1 = &topo.links[0];
        let a1 = l1.addresses.as_ref().unwrap();
        assert_eq!(a1[0], "10.0.0.1/30");
        assert_eq!(a1[1], "10.0.0.2/30");

        // Second allocation: 10.0.0.4/30 → .5 and .6
        let l2 = &topo.links[1];
        let a2 = l2.addresses.as_ref().unwrap();
        assert_eq!(a2[0], "10.0.0.5/30");
        assert_eq!(a2[1], "10.0.0.6/30");
    }

    #[test]
    fn test_pool_with_slash31() {
        let topo = parse_and_lower(
            r#"lab "t"
pool p2p 10.0.0.0/24 /31
node a
node b
link a:eth0 -- b:eth0 { pool p2p }
"#,
        );
        let addrs = topo.links[0].addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.0.0/31");
        assert_eq!(addrs[1], "10.0.0.1/31");
    }

    #[test]
    fn test_validate_block_parse() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { subnet 10.0.0.0/30 }
validate {
    reach a b
    no-reach b a
}
"#,
        );
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.assertions.len(), 2);
        assert!(
            matches!(&topo.assertions[0], types::Assertion::Reach { from, to } if from == "a" && to == "b")
        );
        assert!(
            matches!(&topo.assertions[1], types::Assertion::NoReach { from, to } if from == "b" && to == "a")
        );
    }

    #[test]
    fn test_lower_macvlan() {
        let topo = parse_and_lower(
            r#"lab "t"
node gw {
  macvlan eth0 parent "enp3s0" mode private {
    192.168.1.100/24
  }
}"#,
        );
        assert_eq!(topo.nodes["gw"].macvlans.len(), 1);
        let mv = &topo.nodes["gw"].macvlans[0];
        assert_eq!(mv.name, "eth0");
        assert_eq!(mv.parent, "enp3s0");
        assert_eq!(mv.mode, types::MacvlanMode::Private);
        assert_eq!(mv.addresses, vec!["192.168.1.100/24"]);
    }

    #[test]
    fn test_lower_ipvlan() {
        let topo = parse_and_lower(
            r#"lab "t"
node r {
  ipvlan eth0 parent "enp3s0" mode l2
}"#,
        );
        assert_eq!(topo.nodes["r"].ipvlans.len(), 1);
        let iv = &topo.nodes["r"].ipvlans[0];
        assert_eq!(iv.name, "eth0");
        assert_eq!(iv.parent, "enp3s0");
        assert_eq!(iv.mode, types::IpvlanMode::L2);
    }

    #[test]
    fn test_ip_function_subnet() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
let net = subnet("10.0.0.0/16", 24, 18)
link a:eth0 -- b:eth0 { host(${net}, 1)/24 -- host(${net}, 2)/24 }
"#,
        );
        let addrs = topo.links[0].addresses.as_ref().unwrap();
        assert_eq!(addrs[0], "10.0.18.1/24");
        assert_eq!(addrs[1], "10.0.18.2/24");
    }

    #[test]
    fn test_ip_function_nested() {
        let topo = parse_and_lower(
            r#"lab "t"
node a { route default via host(subnet("10.0.0.0/8", 16, 2), 1) }
"#,
        );
        let route = &topo.nodes["a"].routes["default"];
        assert_eq!(route.via.as_deref(), Some("10.2.0.1"));
    }

    #[test]
    fn test_ip_function_in_let() {
        let topo = parse_and_lower(
            r#"lab "t"
let base = subnet("10.0.0.0/8", 16, 5)
let lan = subnet(${base}, 24, 1)
node a { route default via host(${lan}, 254) }
"#,
        );
        let route = &topo.nodes["a"].routes["default"];
        assert_eq!(route.via.as_deref(), Some("10.5.1.254"));
    }

    #[test]
    fn test_site_grouping() {
        let topo = parse_and_lower(
            r#"lab "t"
profile router { forward ipv4 }

site hq {
  node dc1 { route default via 10.2.1.3 }
  node dcs : router
  link dc1:eth0 -- dcs:eth0 { 10.2.1.1/24 -- 10.2.1.3/24 }
}

site alpha {
  node cc { route default via 10.18.1.3 }
  node red : router
  link cc:eth0 -- red:eth0 { 10.18.1.1/24 -- 10.18.1.3/24 }
}

# Cross-site link (uses prefixed names)
link hq-dcs:eth1 -- alpha-red:eth1 { 172.100.1.2/24 -- 172.100.1.18/24 }
"#,
        );
        // Nodes should be prefixed with site name
        assert!(topo.nodes.contains_key("hq-dc1"), "expected hq-dc1");
        assert!(topo.nodes.contains_key("hq-dcs"), "expected hq-dcs");
        assert!(topo.nodes.contains_key("alpha-cc"), "expected alpha-cc");
        assert!(topo.nodes.contains_key("alpha-red"), "expected alpha-red");
        // Links should reference prefixed names
        assert_eq!(topo.links.len(), 3);
        assert_eq!(topo.links[0].endpoints[0], "hq-dc1:eth0");
        assert_eq!(topo.links[0].endpoints[1], "hq-dcs:eth0");
    }

    #[test]
    fn test_link_profile() {
        let topo = parse_and_lower(
            r#"lab "t"
defaults radio { delay 15ms jitter 10ms loss 2% }
node a
node b
node c
link a:eth0 -- b:eth0 : radio { 10.0.0.1/24 -- 10.0.0.2/24 }
link a:eth1 -- c:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 delay 5ms }
"#,
        );
        // Link with :radio profile should get radio's impairment
        assert!(
            topo.impairments.contains_key("a:eth0"),
            "a:eth0 should have impairment from radio profile"
        );
        assert_eq!(topo.impairments["a:eth0"].delay.as_deref(), Some("15ms"));
        // Link without profile gets its own impairment
        assert_eq!(topo.impairments["a:eth1"].delay.as_deref(), Some("5ms"));
    }

    #[test]
    fn test_if_true() {
        let topo = parse_and_lower(
            r#"lab "t"
let mode = 1
if ${mode} == 1 {
  node a
}
"#,
        );
        assert!(topo.nodes.contains_key("a"));
    }

    #[test]
    fn test_if_false() {
        let topo = parse_and_lower(
            r#"lab "t"
let mode = 0
if ${mode} == 1 {
  node a
}
"#,
        );
        assert!(!topo.nodes.contains_key("a"));
    }

    #[test]
    fn test_if_numeric_comparison() {
        let topo = parse_and_lower(
            r#"lab "t"
let count = 5
if ${count} > 3 {
  node big
}
if ${count} < 3 {
  node small
}
"#,
        );
        assert!(topo.nodes.contains_key("big"));
        assert!(!topo.nodes.contains_key("small"));
    }

    #[test]
    fn test_eval_condition_basic() {
        let vars: BTreeMap<String, String> = [("x".into(), "5".into())].into_iter().collect();
        assert!(eval_condition("5 == 5", &vars));
        assert!(!eval_condition("5 == 6", &vars));
        assert!(eval_condition("5 != 6", &vars));
        assert!(eval_condition("5 > 3", &vars));
        assert!(eval_condition("5 <= 5", &vars));
        assert!(!eval_condition("5 < 5", &vars));
    }

    #[test]
    fn test_eval_condition_boolean() {
        let vars: BTreeMap<String, String> = BTreeMap::new();
        assert!(eval_condition("1 == 1 && 2 == 2", &vars));
        assert!(!eval_condition("1 == 1 && 2 == 3", &vars));
        assert!(eval_condition("1 == 2 || 2 == 2", &vars));
        assert!(!eval_condition("1 == 2 || 3 == 4", &vars));
    }

    #[test]
    fn test_lo_pool_allocation() {
        let topo = parse_and_lower(
            r#"lab "t"
pool loopbacks 10.255.0.0/24 /32
node r1 { lo pool loopbacks }
node r2 { lo pool loopbacks }
node r3 { lo pool loopbacks }
"#,
        );
        let r1_lo = &topo.nodes["r1"].interfaces["lo"];
        let r2_lo = &topo.nodes["r2"].interfaces["lo"];
        let r3_lo = &topo.nodes["r3"].interfaces["lo"];
        assert_eq!(r1_lo.addresses[0], "10.255.0.0/32");
        assert_eq!(r2_lo.addresses[0], "10.255.0.1/32");
        assert_eq!(r3_lo.addresses[0], "10.255.0.2/32");
    }

    #[test]
    fn test_for_inside_node_routes() {
        let topo = parse_and_lower(
            r#"lab "t"
node dcs {
  forward ipv4
  for asset in [18, 19] {
    route 144.0.1.${asset}/32 via 10.2.2.2
  }
}
"#,
        );
        let routes = &topo.nodes["dcs"].routes;
        let keys: Vec<_> = routes.keys().collect();
        assert_eq!(routes.len(), 2, "routes: {:?}", keys);
        assert!(routes.contains_key("144.0.1.18/32"), "routes: {:?}", keys);
        assert!(routes.contains_key("144.0.1.19/32"));
        assert_eq!(routes["144.0.1.18/32"].via.as_deref(), Some("10.2.2.2"));
    }

    #[test]
    fn test_network_for_expands_with_arithmetic_and_outer_scope() {
        let topo = parse_and_lower(
            r#"lab "t"
let n = 4
for i in 0..3 { node n${i} }
network ring {
  members [n0:rf, n1:rf, n2:rf, n3:rf]
  for i in 0..${n - 1} {
    impair n${i} -- n${(i + 1) % n} { delay 50ms }
    for j in [x] { impair n${i} -- ${j}${loop.first} { loss 1% } }
  }
}
"#,
        );
        let net = &topo.networks["ring"];
        let pairs: Vec<(String, String, Option<String>)> = net
            .impairments
            .iter()
            .map(|i| (i.src.clone(), i.dst.clone(), i.impairment.delay.clone()))
            .collect();
        assert!(
            pairs.contains(&("n0".into(), "n1".into(), Some("50ms".into()))),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("n3".into(), "n0".into(), Some("50ms".into()))),
            "{pairs:?}"
        );
        // nested loop: loop.first refers to the inner loop
        assert!(
            net.impairments
                .iter()
                .any(|i| i.src == "n2" && i.dst == "xtrue"),
            "{pairs:?}"
        );
        assert_eq!(net.impairments.len(), 8);
    }

    #[test]
    fn test_network_for_respects_iteration_cap() {
        let err = crate::parser::parse(
            r#"lab "t"
node a
network big { members [a:eth0]  for i in 1..1000000 { impair a -- a { delay 1ms } } }
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("limit is"), "{err}");
    }

    #[test]
    fn test_nat_for_with_dyn_bounds_and_metavars() {
        let topo = parse_and_lower(
            r#"lab "t"
let count = 2
node fw {
  nat {
    for i in 1..${count} {
      snat src 144.0.${i}.0/24 to 172.100.${i * 10}.1
    }
    masquerade src 10.0.0.0/8
  }
}
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        assert_eq!(nat.rules.len(), 3, "{:?}", nat.rules);
        assert_eq!(nat.rules[0].src.as_deref(), Some("144.0.1.0/24"));
        assert_eq!(nat.rules[0].target.as_deref(), Some("172.100.10.1"));
        assert_eq!(nat.rules[1].target.as_deref(), Some("172.100.20.1"));
        assert_eq!(
            nat.rules[2].action,
            types::NatAction::Masquerade,
            "source order kept"
        );
    }

    #[test]
    fn test_list_for_expression_uses_the_shared_engine() {
        let topo = parse_and_lower(
            r#"lab "t"
node hub { vrf red table 10 { interfaces [for i in 1..2 : eth${i * 10}] } }
"#,
        );
        assert_eq!(
            topo.nodes["hub"].vrfs["red"].interfaces,
            vec!["eth10".to_string(), "eth20".to_string()]
        );
        let err = crate::parser::parse(
            r#"lab "t"
node hub { vrf red table 10 { interfaces [for i in 1..${n} : eth${i}] } }
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be literal"), "{err}");
    }

    #[test]
    fn test_netem_extras_lower_and_render() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms delay-correlation 25%
  loss 1% loss-correlation 10%
  duplicate 0.5% limit 500
}
"#,
        );
        let imp = &topo.impairments["a:eth0"];
        assert_eq!(imp.delay_correlation.as_deref(), Some("25%"));
        assert_eq!(imp.loss_correlation.as_deref(), Some("10%"));
        assert_eq!(imp.duplicate.as_deref(), Some("0.5%"));
        assert_eq!(imp.limit.as_deref(), Some("500"));
        let rendered = crate::render::try_render(&topo).unwrap();
        assert!(
            rendered
                .contains("duplicate 0.5% delay-correlation 25% loss-correlation 10% limit 500"),
            "{rendered}"
        );
        let back = crate::parser::parse(&rendered).unwrap();
        assert_eq!(back.impairments["a:eth0"], *imp);
        assert!(crate::deploy::plan::qdisc::build_netem(imp).is_ok());
        assert!(!topo.validate().has_errors());
        let bad = parse_and_lower(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  limit 0 }\n",
        );
        assert!(
            bad.validate()
                .errors()
                .any(|e| e.rule == "invalid-impairment-value")
        );
    }

    #[test]
    fn test_for_inside_nat() {
        let topo = parse_and_lower(
            r#"lab "t"
node fw {
  forward ipv4
  nat {
    masquerade src 10.0.0.0/16
    for asset in [18, 19] {
      dnat dst 144.0.1.0/24 to 10.0.0.${asset}
    }
  }
}
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        assert_eq!(nat.rules.len(), 3, "rules: {:?}", nat.rules); // masquerade + 2 dnat
        assert_eq!(nat.rules[0].action, types::NatAction::Masquerade);
        assert_eq!(nat.rules[1].target.as_deref(), Some("10.0.0.18"));
        assert_eq!(nat.rules[2].target.as_deref(), Some("10.0.0.19"));
    }

    #[test]
    fn test_route_group() {
        let topo = parse_and_lower(
            r#"lab "t"
node r {
  route [10.0.0.0/8, 10.1.0.0/8, 10.2.0.0/8] via 192.168.1.1
  route default via 192.168.1.1
}
"#,
        );
        let routes = &topo.nodes["r"].routes;
        assert_eq!(routes.len(), 4); // 3 from list + 1 default
        assert_eq!(routes["10.0.0.0/8"].via.as_deref(), Some("192.168.1.1"));
        assert_eq!(routes["10.1.0.0/8"].via.as_deref(), Some("192.168.1.1"));
        assert_eq!(routes["10.2.0.0/8"].via.as_deref(), Some("192.168.1.1"));
        assert_eq!(routes["default"].via.as_deref(), Some("192.168.1.1"));
    }

    #[test]
    fn test_lower_wifi() {
        let topo = parse_and_lower(
            r#"lab "t"
node ap {
  wifi wlan0 mode ap {
    ssid "testnet"
    channel 6
    wpa2 "secret"
    10.0.0.1/24
  }
}
node sta {
  wifi wlan0 mode station {
    ssid "testnet"
    wpa2 "secret"
  }
}
"#,
        );
        assert_eq!(topo.nodes["ap"].wifi.len(), 1);
        let ap_wifi = &topo.nodes["ap"].wifi[0];
        assert_eq!(ap_wifi.name, "wlan0");
        assert_eq!(ap_wifi.mode, types::WifiMode::Ap);
        assert_eq!(ap_wifi.ssid.as_deref(), Some("testnet"));
        assert_eq!(ap_wifi.channel, Some(6));
        assert_eq!(ap_wifi.passphrase.as_deref(), Some("secret"));
        assert_eq!(ap_wifi.addresses, vec!["10.0.0.1/24"]);

        assert_eq!(topo.nodes["sta"].wifi.len(), 1);
        let sta_wifi = &topo.nodes["sta"].wifi[0];
        assert_eq!(sta_wifi.mode, types::WifiMode::Station);
    }

    #[test]
    fn test_glob_member_resolution() {
        let topo = parse_and_lower(
            r#"lab "t"
node gw
node site1-router
node site2-router
node site3-host

network wan {
  members [gw:wan, *-router:wan]
  subnet 172.16.0.0/24
}
"#,
        );
        let net = &topo.networks["wan"];
        // Glob *-router:wan should match site1-router and site2-router
        assert!(
            net.members.contains(&"site1-router:wan".to_string()),
            "members: {:?}",
            net.members
        );
        assert!(
            net.members.contains(&"site2-router:wan".to_string()),
            "members: {:?}",
            net.members
        );
        // site3-host should NOT match *-router
        assert!(
            !net.members.contains(&"site3-host:wan".to_string()),
            "site3-host should not match *-router"
        );
        // gw is literal, should be present
        assert!(net.members.contains(&"gw:wan".to_string()));
    }

    #[test]
    fn test_glob_member_with_subnet() {
        let topo = parse_and_lower(
            r#"lab "t"
node gw
node s1-router
node s2-router

network wan {
  members [gw:wan, *-router:wan]
  subnet 10.0.0.0/24
}
"#,
        );
        let net = &topo.networks["wan"];
        // All 3 members should have auto-assigned addresses
        assert_eq!(
            net.ports.len(),
            3,
            "ports: {:?}",
            net.ports.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_nat_dnat_lower() {
        let topo = parse_and_lower(
            r#"lab "t"
node fw {
  nat {
    masquerade src 10.0.0.0/16
    dnat dst 192.168.1.0/24 to 10.0.1.100
    snat src 10.0.0.0/8 to 203.0.113.1
  }
}
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        assert_eq!(nat.rules.len(), 3);
        assert_eq!(nat.rules[0].action, types::NatAction::Masquerade);
        assert_eq!(nat.rules[1].action, types::NatAction::Dnat);
        assert_eq!(nat.rules[1].target.as_deref(), Some("10.0.1.100"));
        assert_eq!(nat.rules[2].action, types::NatAction::Snat);
        assert_eq!(nat.rules[2].target.as_deref(), Some("203.0.113.1"));
    }

    #[test]
    fn test_shell_style_run() {
        let topo = parse_and_lower(
            r#"lab "t"
node server {
  run "echo hello world"
}
"#,
        );
        let exec = &topo.nodes["server"].exec[0];
        assert_eq!(exec.cmd, vec!["sh", "-c", "echo hello world"]);
    }

    #[test]
    fn test_lower_benchmark() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

benchmark "perf" {
  ping a b {
    count 10
    assert avg below 50ms
    assert loss below 5%
  }
}
"#,
        );
        assert_eq!(topo.benchmarks.len(), 1);
        let b = &topo.benchmarks[0];
        assert_eq!(b.name, "perf");
        assert_eq!(b.tests.len(), 1);
        match &b.tests[0] {
            types::BenchmarkTest::Ping {
                from,
                to,
                count,
                assertions,
            } => {
                assert_eq!(from, "a");
                assert_eq!(to, "b");
                assert_eq!(*count, Some(10));
                assert_eq!(assertions.len(), 2);
                assert_eq!(assertions[0].metric, "avg");
                assert_eq!(assertions[0].op, types::CompareOp::Lt);
                assert_eq!(assertions[0].value, "50ms");
            }
            _ => panic!("expected Ping"),
        }
    }

    #[test]
    fn test_lower_scenario() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

scenario "test" {
  at 0s {
    log "start"
    validate { reach a b }
  }
  at 2s {
    down a:eth0
  }
  at 4s {
    validate { no-reach a b }
  }
  at 6s {
    up a:eth0
  }
}
"#,
        );
        assert_eq!(topo.scenarios.len(), 1);
        let s = &topo.scenarios[0];
        assert_eq!(s.name, "test");
        assert_eq!(s.steps.len(), 4);
        assert_eq!(s.steps[0].time_ms, 0);
        assert_eq!(s.steps[1].time_ms, 2000);
        assert_eq!(s.steps[2].time_ms, 4000);
        assert_eq!(s.steps[3].time_ms, 6000);
        assert_eq!(s.steps[0].actions.len(), 2); // log + validate
        assert!(matches!(
            &s.steps[1].actions[0],
            types::ScenarioAction::Down(ep) if ep == "a:eth0"
        ));
        assert!(matches!(
            &s.steps[3].actions[0],
            types::ScenarioAction::Up(ep) if ep == "a:eth0"
        ));
    }

    #[test]
    fn test_lower_scenario_relative_time() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

scenario "rel" {
  at 0s { log "start" }
  at +5s { log "five" }
  at +3s { log "eight" }
}
"#,
        );
        let s = &topo.scenarios[0];
        assert_eq!(s.steps[0].time_ms, 0);
        assert_eq!(s.steps[1].time_ms, 5000);
        assert_eq!(s.steps[2].time_ms, 8000);
    }

    #[test]
    fn test_validate_rich_assertions() {
        let topo = parse_and_lower(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
validate {
    reach a b
    tcp-connect a b 80
    tcp-connect a b 443 timeout 5s
    latency-under a b 50ms
    latency-under a b 100ms samples 10
    route-has a default via 10.0.0.1
    route-has b 10.0.0.0/24 dev eth0
    dns-resolves a "b" "10.0.0.2"
}
"#,
        );
        assert_eq!(topo.assertions.len(), 8);
        assert!(matches!(
            &topo.assertions[0],
            types::Assertion::Reach { .. }
        ));
        assert!(matches!(
            &topo.assertions[1],
            types::Assertion::TcpConnect {
                port: 80,
                timeout: None,
                ..
            }
        ));
        assert!(
            matches!(&topo.assertions[2], types::Assertion::TcpConnect { port: 443, timeout: Some(t), .. } if t == "5s")
        );
        assert!(
            matches!(&topo.assertions[3], types::Assertion::LatencyUnder { max, samples: None, .. } if max == "50ms")
        );
        assert!(matches!(
            &topo.assertions[4],
            types::Assertion::LatencyUnder {
                samples: Some(10),
                ..
            }
        ));
        assert!(
            matches!(&topo.assertions[5], types::Assertion::RouteHas { node, via: Some(v), .. } if node == "a" && v == "10.0.0.1")
        );
        assert!(
            matches!(&topo.assertions[6], types::Assertion::RouteHas { node, dev: Some(d), .. } if node == "b" && d == "eth0")
        );
        assert!(
            matches!(&topo.assertions[7], types::Assertion::DnsResolves { name, expected_ip, .. } if name == "b" && expected_ip == "10.0.0.2")
        );
    }

    #[test]
    fn test_mesh_pattern() {
        let topo = parse_and_lower(
            r#"lab "t"
pool p 10.0.0.0/24 /30
mesh cluster {
    node [a, b, c]
    pool p
}"#,
        );
        // 3 nodes: cluster.a, cluster.b, cluster.c
        assert_eq!(topo.nodes.len(), 3);
        assert!(topo.nodes.contains_key("cluster.a"));
        assert!(topo.nodes.contains_key("cluster.b"));
        assert!(topo.nodes.contains_key("cluster.c"));
        // 3 links (C(3,2) = 3 pairwise)
        assert_eq!(topo.links.len(), 3);
        // All links have auto-allocated addresses
        for link in &topo.links {
            assert!(link.addresses.is_some());
        }
    }

    #[test]
    fn test_ring_pattern() {
        let topo = parse_and_lower(
            r#"lab "t"
ring backbone {
    count 4
}"#,
        );
        // 4 nodes: backbone.r1..r4
        assert_eq!(topo.nodes.len(), 4);
        // 4 links (ring)
        assert_eq!(topo.links.len(), 4);
    }

    #[test]
    fn test_star_pattern() {
        let topo = parse_and_lower(
            r#"lab "t"
star net {
    hub center
    spokes [s1, s2, s3]
}"#,
        );
        // 4 nodes: net.center + net.s1, net.s2, net.s3
        assert_eq!(topo.nodes.len(), 4);
        assert!(topo.nodes.contains_key("net.center"));
        assert!(topo.nodes.contains_key("net.s1"));
        // 3 links (hub to each spoke)
        assert_eq!(topo.links.len(), 3);
    }

    #[test]
    fn test_pool_mixed_with_explicit() {
        let topo = parse_and_lower(
            r#"lab "t"
pool auto 10.0.0.0/24 /30
node a
node b
node c
link a:eth0 -- b:eth0 { pool auto }
link b:eth1 -- c:eth0 { 192.168.0.1/24 -- 192.168.0.2/24 }
"#,
        );
        // First link from pool
        let a1 = topo.links[0].addresses.as_ref().unwrap();
        assert_eq!(a1[0], "10.0.0.1/30");
        // Second link explicit
        let a2 = topo.links[1].addresses.as_ref().unwrap();
        assert_eq!(a2[0], "192.168.0.1/24");
    }

    #[test]
    fn test_lower_translate_basic() {
        let topo = parse_and_lower(
            r#"lab "t"
profile router { forward ipv4 }
node fw : router {
  nat {
    translate 144.0.0.0/8 to 172.100.0.0/16
  }
}
node a
node b
link fw:eth0 -- a:eth0 { 172.100.1.2/24 -- 172.100.1.10/24 }
link fw:eth1 -- b:eth0 { 172.100.2.2/24 -- 172.100.2.20/24 }
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        // Translate should have been expanded to DNAT rules for addresses in 172.100.x.x
        assert!(!nat.rules.is_empty());
        for rule in &nat.rules {
            assert_eq!(rule.action, types::NatAction::Dnat);
            // Each generated rule should have dst in 144.0.x.x/32 and target in 172.100.x.x
            let dst = rule.dst.as_ref().unwrap();
            assert!(
                dst.starts_with("144.0."),
                "dst should be in source range: {dst}"
            );
            assert!(dst.ends_with("/32"));
            let target = rule.target.as_ref().unwrap();
            assert!(
                target.starts_with("172.100."),
                "target should be in dst range: {target}"
            );
        }
        // Verify specific mappings: 172.100.1.10 → 144.0.1.10, 172.100.2.20 → 144.0.2.20
        let dsts: Vec<&str> = nat
            .rules
            .iter()
            .map(|r| r.dst.as_deref().unwrap())
            .collect();
        assert!(dsts.contains(&"144.0.1.10/32"));
        assert!(dsts.contains(&"144.0.2.20/32"));
    }

    #[test]
    fn test_lower_translate_sparse() {
        // Only addresses actually in the topology should generate rules
        let topo = parse_and_lower(
            r#"lab "t"
node fw {
  nat {
    translate 10.0.0.0/8 to 192.168.0.0/16
  }
}
node a
link fw:eth0 -- a:eth0 { 192.168.1.1/24 -- 192.168.1.2/24 }
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        // Only addresses in 192.168.x.x that exist in links should produce rules
        // The fw's own address 192.168.1.1 and a's address 192.168.1.2
        let targets: Vec<&str> = nat
            .rules
            .iter()
            .map(|r| r.target.as_deref().unwrap())
            .collect();
        assert!(targets.contains(&"192.168.1.1"));
        assert!(targets.contains(&"192.168.1.2"));
        assert_eq!(nat.rules.len(), 2);
    }

    #[test]
    fn test_lower_translate_no_matches() {
        let topo = parse_and_lower(
            r#"lab "t"
node fw {
  nat {
    translate 10.0.0.0/8 to 192.168.0.0/16
  }
}
node a
link fw:eth0 -- a:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 }
"#,
        );
        let nat = topo.nodes["fw"].nat.as_ref().unwrap();
        // No addresses in 192.168.x.x range → no rules generated
        assert_eq!(nat.rules.len(), 0);
    }

    #[test]
    fn test_map_translate_address() {
        use std::net::Ipv4Addr;
        // 172.100.1.18 with dst=/16 → src=/8 should yield 144.0.1.18
        let result = super::map_translate_address(
            Ipv4Addr::new(172, 100, 1, 18),
            Ipv4Addr::new(172, 100, 0, 0),
            16,
            Ipv4Addr::new(144, 0, 0, 0),
            8,
        );
        assert_eq!(result, Ipv4Addr::new(144, 0, 1, 18));
    }

    #[test]
    fn test_map_translate_address_same_prefix() {
        use std::net::Ipv4Addr;
        // Same prefix length: 10.1.2.3 with /16 → /16 yields 192.168.2.3
        let result = super::map_translate_address(
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(10, 1, 0, 0),
            16,
            Ipv4Addr::new(192, 168, 0, 0),
            16,
        );
        assert_eq!(result, Ipv4Addr::new(192, 168, 2, 3));
    }

    #[test]
    fn test_in_prefix() {
        use std::net::Ipv4Addr;
        assert!(super::in_prefix(
            Ipv4Addr::new(172, 100, 1, 18),
            Ipv4Addr::new(172, 100, 0, 0),
            16
        ));
        assert!(!super::in_prefix(
            Ipv4Addr::new(10, 0, 1, 1),
            Ipv4Addr::new(172, 100, 0, 0),
            16
        ));
        assert!(super::in_prefix(
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(10, 0, 0, 0),
            8
        ));
    }
}
