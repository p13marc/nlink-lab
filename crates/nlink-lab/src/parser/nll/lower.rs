//! Lowering pass: AST → Topology.
//!
//! Expands `for` loops, substitutes `let` variables, resolves profiles,
//! and maps AST nodes to the [`crate::types::Topology`] struct.

use std::collections::{HashMap, HashSet};
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

fn lower_with_base_dir(
    file: &ast::File,
    base_dir: Option<&Path>,
    visited: &mut HashSet<std::path::PathBuf>,
) -> Result<types::Topology> {
    let mut ctx = LowerCtx::new();

    // First pass: collect profiles, variables, and defaults
    for stmt in &file.statements {
        match stmt {
            ast::Statement::Profile(p) => ctx.add_profile(p),
            ast::Statement::Let(l) => ctx.add_variable(l),
            ast::Statement::Defaults(d) => {
                match d.kind {
                    ast::DefaultsKind::Link => ctx.default_link_mtu = d.mtu,
                    ast::DefaultsKind::Impair => ctx.default_impair = d.impair.clone(),
                    ast::DefaultsKind::Rate => ctx.default_rate = d.rate.clone(),
                }
            }
            ast::Statement::Pool(p) => {
                if let Ok((ip, prefix)) = crate::helpers::parse_cidr(&p.base)
                    && let std::net::IpAddr::V4(v4) = ip {
                        let base = u32::from(v4);
                        let pool_size = 1u32.checked_shl(32 - prefix as u32).unwrap_or(0);
                        ctx.pools.insert(p.name.clone(), PoolState {
                            base,
                            pool_size,
                            alloc_prefix: p.prefix,
                            next_offset: 0,
                        });
                    }
            }
            _ => {}
        }
    }

    // Inject lab auto-variables
    ctx.variables.insert("lab.name".into(), file.lab.name.clone());
    ctx.variables.insert(
        "lab.prefix".into(),
        file.lab.prefix.clone().unwrap_or_else(|| file.lab.name.clone()),
    );

    // Pre-lowering validation
    validate_ast(file, &ctx)?;

    // Second pass: expand loops and collect all concrete statements
    let expanded = ctx.expand_statements(&file.statements)?;

    // Third pass: lower to Topology
    let mut topology = types::Topology::default();
    topology.lab = lower_lab(&file.lab);

    // Resolve imports before lowering statements
    if !file.imports.is_empty() {
        let base = base_dir.ok_or_else(|| {
            crate::Error::NllParse("import requires file-based parsing (use parse_file)".into())
        })?;
        resolve_imports(&file.imports, base, &mut topology, visited)?;
    }

    // Add profiles to topology (for validator cross-referencing)
    for (name, profile_def) in &ctx.profiles {
        topology.profiles.insert(name.clone(), lower_profile(profile_def));
    }

    for stmt in &expanded {
        match stmt {
            ast::Statement::Node(n) => lower_node(&mut topology, n, &ctx)?,
            ast::Statement::Link(l) => lower_link(&mut topology, l, &mut ctx),
            ast::Statement::Network(n) => lower_network(&mut topology, n)?,
            ast::Statement::Impair(i) => lower_impair(&mut topology, i),
            ast::Statement::Rate(r) => lower_rate(&mut topology, r),
            ast::Statement::Pattern(p) => expand_pattern(&mut topology, p, &mut ctx),
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
                    }
                }
            }
            ast::Statement::Profile(_) | ast::Statement::Let(_)
            | ast::Statement::For(_) | ast::Statement::Defaults(_)
            | ast::Statement::Param(_) | ast::Statement::Pool(_) => {}
        }
    }

    // Post-lowering pass: resolve cross-references like ${router.eth0}
    resolve_cross_refs(&mut topology)?;
    warn_unresolved_refs(&topology);

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

        // Circular import detection
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
        let tokens = super::lexer::lex(&content)?;
        let mut ast = super::parser::parse_tokens(&tokens, &content)?;

        // Resolve parametric import: inject caller params, apply defaults from `param` stmts
        if !imp.params.is_empty() || ast.statements.iter().any(|s| matches!(s, ast::Statement::Param(_))) {
            resolve_import_params(&imp.params, &mut ast)?;
        }

        let import_base = import_path.parent().unwrap_or(base_dir);
        let imported = lower_with_base_dir(&ast, Some(import_base), visited)?;

        // Merge imported topology with alias prefix
        merge_import(topology, &imp.alias, imported);
    }
    Ok(())
}

/// Resolve parametric import parameters.
///
/// Collects `param` declarations from the imported file, matches them against
/// caller-provided values, and injects the resolved values as `let` bindings
/// at the beginning of the imported file's statements.
fn resolve_import_params(
    caller_params: &[(String, String)],
    ast: &mut ast::File,
) -> Result<()> {
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
    ast.statements.retain(|s| !matches!(s, ast::Statement::Param(_)));
    let mut new_stmts = let_stmts;
    new_stmts.append(&mut ast.statements);
    ast.statements = new_stmts;

    Ok(())
}

fn merge_import(main: &mut types::Topology, alias: &str, imported: types::Topology) {
    // Merge nodes with prefixed names
    for (name, node) in imported.nodes {
        main.nodes.insert(format!("{alias}.{name}"), node);
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
        main.networks.insert(format!("{alias}.{name}"), network);
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
        main.profiles.insert(format!("{alias}.{name}"), profile);
    }
}

fn prefix_endpoint(alias: &str, endpoint: &str) -> String {
    if let Some((node, iface)) = endpoint.split_once(':') {
        format!("{alias}.{node}:{iface}")
    } else {
        format!("{alias}.{endpoint}")
    }
}

// ─── Cross-Reference Resolution ─────────────────────────

/// Build a map of node:interface → IP address from all link definitions.
fn build_address_map(topology: &types::Topology) -> HashMap<String, String> {
    let mut map = HashMap::new();
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
    map
}

/// Replace `${node.iface}` references with resolved IP addresses.
fn resolve_ref(s: &str, addr_map: &HashMap<String, String>) -> Result<String> {
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
                && via.contains("${") {
                    tracing::warn!(
                        "unresolved reference in route '{dest}' on '{node_name}': {via}"
                    );
                }
        }
        if let Some(fw) = &node.firewall {
            for rule in &fw.rules {
                if let Some(expr) = &rule.match_expr
                    && expr.contains("${") {
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
    base: u32,         // base network address as u32
    pool_size: u32,    // total addresses in the pool (for exhaustion check)
    alloc_prefix: u8,  // allocation prefix size (e.g., 30 for /30)
    next_offset: u32,  // next allocation offset from base
}

struct LowerCtx {
    profiles: HashMap<String, ast::ProfileDef>,
    variables: HashMap<String, String>,
    default_link_mtu: Option<u32>,
    default_impair: Option<ast::ImpairProps>,
    default_rate: Option<ast::RateProps>,
    pools: HashMap<String, PoolState>,
}

impl LowerCtx {
    fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            variables: HashMap::new(),
            default_link_mtu: None,
            default_impair: None,
            default_rate: None,
            pools: HashMap::new(),
        }
    }

    fn add_profile(&mut self, p: &ast::ProfileDef) {
        if self.profiles.contains_key(&p.name) {
            tracing::warn!("duplicate profile name '{}' — later definition wins", p.name);
        }
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

        let values: Vec<String> = match &for_loop.range {
            ast::ForRange::IntRange { start, end } => {
                (*start..=*end).map(|i| i.to_string()).collect()
            }
            ast::ForRange::List(items) => items.clone(),
        };
        let len = values.len();

        for (idx, value) in values.iter().enumerate() {
            vars.insert(for_loop.var.clone(), value.clone());
            vars.insert("loop.index".into(), idx.to_string());
            vars.insert("loop.first".into(), (idx == 0).to_string());
            vars.insert("loop.last".into(), (idx == len - 1).to_string());

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
        vars.remove("loop.index");
        vars.remove("loop.first");
        vars.remove("loop.last");
        Ok(result)
    }
}

// ─── Interpolation ────────────────────────────────────────

/// Replace `${expr}` with its evaluated value.
///
/// Supports arithmetic (`${i + 1}`, `${(i - 1) * 2}`, `${i % 3}`),
/// ternary conditionals (`${env == "prod" ? "5ms" : "50ms"}`),
/// and simple variable lookup (`${var}`).
fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    // Run interpolation repeatedly until stable (handles nested ${leaf${i}})
    let mut current = template.to_string();
    for _ in 0..10 {
        let next = interpolate_once(&current, vars);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

fn interpolate_once(template: &str, vars: &HashMap<String, String>) -> String {
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
fn eval_expr(expr: &str, vars: &HashMap<String, String>) -> String {
    let expr = expr.trim();

    // Ternary conditional: `cond ? true_val : false_val`
    if let Some(result) = eval_ternary(expr, vars) {
        return result;
    }

    // Arithmetic expression
    let tokens = tokenize_arith(expr, vars);
    if !tokens.is_empty()
        && let Ok(val) = parse_arith_expr(&tokens, &mut 0) {
            return val.to_string();
        }

    // Simple variable lookup
    vars.get(expr)
        .cloned()
        .unwrap_or_else(|| format!("${{{expr}}}"))
}

/// Evaluate a ternary expression: `var == "lit" ? true_val : false_val`
fn eval_ternary(expr: &str, vars: &HashMap<String, String>) -> Option<String> {
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

/// Resolve a value: strip quotes from string literals, look up variables.
fn resolve_var(s: &str, vars: &HashMap<String, String>) -> String {
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
fn tokenize_arith(expr: &str, vars: &HashMap<String, String>) -> Vec<ArithTok> {
    let mut tokens = Vec::new();
    let mut chars = expr.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' => { chars.next(); }
            '+' => { chars.next(); tokens.push(ArithTok::Plus); }
            '-' => {
                chars.next();
                // Unary minus: after operator, open paren, or at start
                let is_unary = tokens.is_empty()
                    || matches!(tokens.last(), Some(ArithTok::Plus | ArithTok::Minus
                        | ArithTok::Mul | ArithTok::Div | ArithTok::Mod | ArithTok::LParen));
                if is_unary {
                    // Parse the number/variable and negate
                    let val = read_operand(&mut chars, vars);
                    if let Some(n) = val { tokens.push(ArithTok::Num(-n)); }
                    else { return vec![]; } // can't parse → bail
                } else {
                    tokens.push(ArithTok::Minus);
                }
            }
            '*' => { chars.next(); tokens.push(ArithTok::Mul); }
            '/' => { chars.next(); tokens.push(ArithTok::Div); }
            '%' => { chars.next(); tokens.push(ArithTok::Mod); }
            '(' => { chars.next(); tokens.push(ArithTok::LParen); }
            ')' => { chars.next(); tokens.push(ArithTok::RParen); }
            '0'..='9' => {
                let mut num = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() { num.push(c); chars.next(); } else { break; }
                }
                if let Ok(n) = num.parse::<i64>() { tokens.push(ArithTok::Num(n)); }
                else { return vec![]; }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                        name.push(c); chars.next();
                    } else { break; }
                }
                if let Some(val) = vars.get(&name) {
                    if let Ok(n) = val.parse::<i64>() { tokens.push(ArithTok::Num(n)); }
                    else { return vec![]; } // non-numeric variable → bail to string lookup
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
fn read_operand(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, vars: &HashMap<String, String>) -> Option<i64> {
    while let Some(&' ') = chars.peek() { chars.next(); }
    if let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut num = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() { num.push(c); chars.next(); } else { break; }
            }
            num.parse().ok()
        } else if c.is_alphabetic() || c == '_' {
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '_' || c == '.' { name.push(c); chars.next(); } else { break; }
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
            ArithTok::Plus => { *pos += 1; left += parse_arith_term(tokens, pos)?; }
            ArithTok::Minus => { *pos += 1; left -= parse_arith_term(tokens, pos)?; }
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
            ArithTok::Mul => { *pos += 1; left *= parse_arith_factor(tokens, pos)?; }
            ArithTok::Div => {
                *pos += 1;
                let right = parse_arith_factor(tokens, pos)?;
                if right == 0 { return Err(()); }
                left /= right;
            }
            ArithTok::Mod => {
                *pos += 1;
                let right = parse_arith_factor(tokens, pos)?;
                if right == 0 { return Err(()); }
                left %= right;
            }
            _ => break,
        }
    }
    Ok(left)
}

/// Parse factor: number or parenthesized expression.
fn parse_arith_factor(tokens: &[ArithTok], pos: &mut usize) -> std::result::Result<i64, ()> {
    if *pos >= tokens.len() { return Err(()); }
    match tokens[*pos] {
        ArithTok::Num(n) => { *pos += 1; Ok(n) }
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
        ast::Statement::Defaults(d) => ast::Statement::Defaults(d.clone()),
        ast::Statement::Pool(p) => ast::Statement::Pool(p.clone()),
        ast::Statement::Pattern(p) => ast::Statement::Pattern(p.clone()),
        ast::Statement::Validate(v) => ast::Statement::Validate(v.clone()),
        ast::Statement::Param(p) => ast::Statement::Param(p.clone()),
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
        profiles: n.profiles.iter().map(|s| i(s, vars)).collect(),
        image: n.image.as_ref().map(|s| i(s, vars)),
        cmd: n.cmd.clone(),
        env: n.env.iter().map(|s| i(s, vars)).collect(),
        volumes: n.volumes.iter().map(|s| i(s, vars)).collect(),
        cpu: io(&n.cpu, vars),
        memory: io(&n.memory, vars),
        privileged: n.privileged,
        cap_add: n.cap_add.clone(),
        cap_drop: n.cap_drop.clone(),
        entrypoint: io(&n.entrypoint, vars),
        hostname: io(&n.hostname, vars),
        workdir: io(&n.workdir, vars),
        labels: n.labels.iter().map(|s| i(s, vars)).collect(),
        pull: n.pull.clone(),
        container_exec: n.container_exec.iter().map(|s| i(s, vars)).collect(),
        healthcheck: io(&n.healthcheck, vars),
        healthcheck_interval: n.healthcheck_interval.clone(),
        healthcheck_timeout: n.healthcheck_timeout.clone(),
        startup_delay: n.startup_delay.clone(),
        env_file: io(&n.env_file, vars),
        configs: n.configs.iter().map(|(h, c)| (i(h, vars), i(c, vars))).collect(),
        overlay: io(&n.overlay, vars),
        depends_on: n.depends_on.iter().map(|s| i(s, vars)).collect(),
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
        subnet: io(&l.subnet, vars),
        pool: l.pool.clone(),
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
                && start > end {
                    errors.push(format!(
                        "for loop '{}' has empty range {}..{}",
                        f.var, start, end
                    ));
                }
            if let ast::ForRange::List(items) = &f.range
                && items.is_empty() {
                    errors.push(format!(
                        "for loop '{}' has empty list",
                        f.var
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
        version: lab.version.clone(),
        author: lab.author.clone(),
        tags: lab.tags.clone(),
        mgmt_subnet: lab.mgmt.clone(),
    }
}

fn lower_node(
    topo: &mut types::Topology,
    node: &ast::NodeDef,
    ctx: &LowerCtx,
) -> Result<()> {
    let mut n = types::Node {
        profile: node.profiles.first().cloned(),
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

    // Apply profiles in order (later profiles override earlier ones)
    for profile_name in &node.profiles {
        if let Some(profile) = ctx.profiles.get(profile_name) {
            apply_node_props(&mut n, &profile.props);
        }
    }

    // Apply node's own properties (overrides profile)
    apply_node_props(&mut n, &node.props);

    if topo.nodes.contains_key(&node.name) {
        return Err(crate::Error::NllParse(format!(
            "duplicate node name '{}' — each node must have a unique name",
            node.name
        )));
    }
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
                        kind: Some(types::InterfaceKind::Vxlan),
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
                        kind: Some(types::InterfaceKind::Dummy),
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
        let mask = !((1u32 << (32 - prefix)) - 1);
        let network = bits & mask;
        let a = std::net::Ipv4Addr::from(network + 1);
        let b = std::net::Ipv4Addr::from(network + 2);
        Ok([format!("{a}/{prefix}"), format!("{b}/{prefix}")])
    }
}

/// Allocate a subnet from a pool, returning the two endpoint addresses.
fn allocate_from_pool(pool: &mut PoolState, pool_name: &str) -> Option<[String; 2]> {
    let subnet_size = 1u32.checked_shl(32 - pool.alloc_prefix as u32).unwrap_or(0);
    if pool.next_offset + subnet_size > pool.pool_size {
        tracing::error!("pool '{pool_name}' exhausted");
        return None;
    }
    let network = pool.base + pool.next_offset;
    pool.next_offset += subnet_size;
    let cidr = format!("{}/{}", std::net::Ipv4Addr::from(network), pool.alloc_prefix);
    split_subnet(&cidr).ok()
}

/// Expand a topology pattern (mesh, ring, star) into nodes and links.
fn expand_pattern(topo: &mut types::Topology, pattern: &ast::PatternDef, ctx: &mut LowerCtx) {
    match &pattern.kind {
        ast::PatternKind::Mesh => {
            // Generate nodes
            for name in &pattern.nodes {
                let node_name = format!("{}.{}", pattern.name, name);
                let mut node = types::Node::default();
                node.profile = pattern.profile.clone();
                topo.nodes.insert(node_name, node);
            }
            // Generate full-mesh links (all pairwise, i < j)
            for (i, a) in pattern.nodes.iter().enumerate() {
                for b in &pattern.nodes[i + 1..] {
                    let left = format!("{}.{}", pattern.name, a);
                    let right = format!("{}.{}", pattern.name, b);
                    let left_iface = format!("to-{b}");
                    let right_iface = format!("to-{a}");

                    let addresses = if let Some(pool_name) = &pattern.pool {
                        if let Some(pool) = ctx.pools.get_mut(pool_name.as_str()) {
                            allocate_from_pool(pool, pool_name)
                        } else { None }
                    } else { None };

                    topo.links.push(types::Link {
                        endpoints: [format!("{left}:{left_iface}"), format!("{right}:{right_iface}")],
                        addresses,
                        mtu: ctx.default_link_mtu,
                    });
                }
            }
        }
        ast::PatternKind::Ring => {
            let n = pattern.count.unwrap_or(pattern.nodes.len() as i64) as usize;
            let names: Vec<String> = if pattern.nodes.is_empty() {
                (1..=n).map(|i| format!("r{i}")).collect()
            } else {
                pattern.nodes.clone()
            };

            // Generate nodes
            for name in &names {
                let node_name = format!("{}.{}", pattern.name, name);
                let mut node = types::Node::default();
                node.profile = pattern.profile.clone();
                topo.nodes.insert(node_name, node);
            }

            // Generate ring links
            for i in 0..names.len() {
                let j = (i + 1) % names.len();
                let left = format!("{}.{}", pattern.name, names[i]);
                let right = format!("{}.{}", pattern.name, names[j]);

                let addresses = if let Some(pool_name) = &pattern.pool {
                    if let Some(pool) = ctx.pools.get_mut(pool_name.as_str()) {
                        let subnet_size = 1u32.checked_shl(32 - pool.alloc_prefix as u32).unwrap_or(0);
                        let network = pool.base + pool.next_offset;
                        pool.next_offset += subnet_size;
                        let cidr = format!("{}/{}", std::net::Ipv4Addr::from(network), pool.alloc_prefix);
                        split_subnet(&cidr).ok()
                    } else { None }
                } else { None };

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
            let mut hub_node = types::Node::default();
            hub_node.profile = pattern.profile.clone();
            topo.nodes.insert(hub_name.clone(), hub_node);

            // Generate spoke nodes and links
            for (i, spoke) in pattern.nodes.iter().enumerate() {
                let spoke_name = format!("{}.{}", pattern.name, spoke);
                let mut spoke_node = types::Node::default();
                spoke_node.profile = pattern.profile.clone();
                topo.nodes.insert(spoke_name.clone(), spoke_node);

                let addresses = if let Some(pool_name) = &pattern.pool {
                    if let Some(pool) = ctx.pools.get_mut(pool_name.as_str()) {
                        let subnet_size = 1u32.checked_shl(32 - pool.alloc_prefix as u32).unwrap_or(0);
                        let network = pool.base + pool.next_offset;
                        pool.next_offset += subnet_size;
                        let cidr = format!("{}/{}", std::net::Ipv4Addr::from(network), pool.alloc_prefix);
                        split_subnet(&cidr).ok()
                    } else { None }
                } else { None };

                topo.links.push(types::Link {
                    endpoints: [format!("{hub_name}:eth{i}"), format!("{spoke_name}:eth0")],
                    addresses,
                    mtu: ctx.default_link_mtu,
                });
            }
        }
    }
}

fn lower_link(topo: &mut types::Topology, link: &ast::LinkDef, ctx: &mut LowerCtx) {
    let endpoints = [
        format!("{}:{}", link.left_node, link.left_iface),
        format!("{}:{}", link.right_node, link.right_iface),
    ];

    let addresses = match (&link.left_addr, &link.right_addr, &link.subnet, &link.pool) {
        (Some(l), Some(r), _, _) => Some([l.clone(), r.clone()]),
        (_, _, Some(subnet), _) => split_subnet(subnet).ok(),
        (_, _, _, Some(pool_name)) => {
            if let Some(pool) = ctx.pools.get_mut(pool_name.as_str()) {
                allocate_from_pool(pool, pool_name)
            } else {
                tracing::warn!("undefined pool '{pool_name}'");
                None
            }
        }
        _ => None,
    };

    // Apply link defaults (per-link values override defaults)
    let mtu = link.mtu.or(ctx.default_link_mtu);

    topo.links.push(types::Link {
        endpoints,
        addresses,
        mtu,
    });

    // Lower symmetric impairment → both endpoints (fall back to defaults)
    let effective_impair = link.impairment.as_ref().or(ctx.default_impair.as_ref());
    if let Some(imp) = effective_impair {
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

fn lower_network(topo: &mut types::Topology, net: &ast::NetworkDef) -> Result<()> {
    let mut network = types::Network {
        kind: Some("bridge".to_string()),  // Network kind stays as String
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
        assert_eq!(iface.kind, Some(types::InterfaceKind::Vxlan));
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

    // ─── Import tests ────────────────────────────────────

    #[test]
    fn test_import_basic() {
        let composed_path = examples_dir().join("imports/composed.nll");
        let topo = crate::parser::parse_file(&composed_path).unwrap();

        assert_eq!(topo.lab.name, "composed");
        // Imported nodes are prefixed with "dc."
        assert!(topo.nodes.contains_key("dc.r1"), "missing dc.r1");
        assert!(topo.nodes.contains_key("dc.r2"), "missing dc.r2");
        // Local node is not prefixed
        assert!(topo.nodes.contains_key("host"), "missing host");
        // Total: 2 imported + 1 local
        assert_eq!(topo.nodes.len(), 3);

        // Imported link endpoints are prefixed
        let imported_link = topo.links.iter().find(|l| {
            l.endpoints[0].starts_with("dc.") && l.endpoints[1].starts_with("dc.")
        });
        assert!(imported_link.is_some(), "imported link not found");

        // Local link references the imported node
        let local_link = topo.links.iter().find(|l| {
            l.endpoints.iter().any(|e| e == "dc.r1:eth1")
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
        assert_eq!(super::prefix_endpoint("dc", "r1:eth0"), "dc.r1:eth0");
        assert_eq!(super::prefix_endpoint("wan", "pe1:wan0"), "wan.pe1:wan0");
        assert_eq!(super::prefix_endpoint("dc", "switch:br0"), "dc.switch:br0");
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
        assert_eq!(topo.nodes["n0"].interfaces["lo"].addresses, vec!["10.0.0.0/32"]);
        assert_eq!(topo.nodes["n1"].interfaces["lo"].addresses, vec!["10.0.1.1/32"]);
        assert_eq!(topo.nodes["n2"].interfaces["lo"].addresses, vec!["10.0.0.2/32"]);
        assert_eq!(topo.nodes["n3"].interfaces["lo"].addresses, vec!["10.0.1.3/32"]);
    }

    #[test]
    fn test_compound_expression() {
        let topo = parse_and_lower(
            r#"lab "t"
for i in 1..3 {
    node n${i} { lo 10.0.0.${(i - 1) * 10 + 1}/32 }
}"#,
        );
        assert_eq!(topo.nodes["n1"].interfaces["lo"].addresses, vec!["10.0.0.1/32"]);
        assert_eq!(topo.nodes["n2"].interfaces["lo"].addresses, vec!["10.0.0.11/32"]);
        assert_eq!(topo.nodes["n3"].interfaces["lo"].addresses, vec!["10.0.0.21/32"]);
    }

    #[test]
    fn test_ternary_conditional() {
        let mut vars = HashMap::new();
        vars.insert("env".into(), "prod".into());
        assert_eq!(super::eval_expr(r#"env == "prod" ? 5ms : 50ms"#, &vars), "5ms");
        assert_eq!(super::eval_expr(r#"env != "prod" ? 5ms : 50ms"#, &vars), "50ms");

        vars.insert("env".into(), "dev".into());
        assert_eq!(super::eval_expr(r#"env == "prod" ? 5ms : 50ms"#, &vars), "50ms");
    }

    #[test]
    fn test_ternary_with_variables() {
        let mut vars = HashMap::new();
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
        let vars = HashMap::new();
        // Division by zero returns the original expression
        assert_eq!(super::eval_expr("4 / 0", &vars), "${4 / 0}");
        assert_eq!(super::eval_expr("4 % 0", &vars), "${4 % 0}");
    }

    #[test]
    fn test_backward_compat_simple() {
        let mut vars = HashMap::new();
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
        assert_eq!(topo.nodes["n1"].interfaces["lo"].addresses, vec!["10.0.0.0/32"]); // index 0
        assert_eq!(topo.nodes["n2"].interfaces["lo"].addresses, vec!["10.0.1.0/32"]); // index 1
        assert_eq!(topo.nodes["n3"].interfaces["lo"].addresses, vec!["10.0.2.0/32"]); // index 2
    }

    #[test]
    fn test_auto_variables_loop_first_last() {
        let mut vars = HashMap::new();
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
        let topo = parse_and_lower(
            "lab \"t\"\nnode a\n/* this node is disabled\nnode b\n*/\nnode c",
        );
        assert_eq!(topo.nodes.len(), 2);
        assert!(topo.nodes.contains_key("a"));
        assert!(topo.nodes.contains_key("c"));
        assert!(!topo.nodes.contains_key("b"));
    }

    #[test]
    fn test_nested_block_comments() {
        let topo = parse_and_lower(
            "lab \"t\"\nnode a\n/* outer /* inner */ still commented */\nnode b",
        );
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
        assert_eq!(n.configs[0], ("a.conf".to_string(), "/etc/a.conf".to_string()));
        assert_eq!(n.overlay.as_deref(), Some("configs/router/"));
        assert_eq!(n.env_file.as_deref(), Some("router.env"));
    }

    #[test]
    fn test_nested_interpolation() {
        let mut vars = HashMap::new();
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
        assert!(matches!(&topo.assertions[0], types::Assertion::Reach { from, to } if from == "a" && to == "b"));
        assert!(matches!(&topo.assertions[1], types::Assertion::NoReach { from, to } if from == "b" && to == "a"));
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
}
