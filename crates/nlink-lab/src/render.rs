//! Render a [`Topology`] back to NLL syntax.
//!
//! Used by `nlink-lab render` to show the fully-expanded topology after
//! loops, variables, and imports have been resolved.
//!
//! The output is meant to round-trip: `parse(render(parse(src)))` must
//! yield the same `Topology` as `parse(src)`. Every emitted token goes
//! through a small set of helpers (`nll_string`, `nll_ident`, `nll_name`,
//! `lit_*`) that mirror the lexer's literal grammar, so a value that the
//! lexer would mis-tokenise is quoted where the parser accepts a string,
//! and rejected with an error where it does not. The renderer never
//! emits NLL it knows the parser will reject, and it never silently drops
//! a field: anything that has no NLL spelling is reported as an error via
//! [`try_render`].
//!
//! Output is deterministic — every map is emitted in sorted key order.

use std::collections::HashMap;
use std::fmt::Write;
use std::net::IpAddr;

use crate::error::Error;
use crate::types::{
    Assertion, BenchmarkTest, CompareOp, ContainerRuntime, DnsMode, FirewallConfig, Impairment,
    InterfaceKind, IpvlanMode, LabConfig, Link, MacvlanMode, NatAction, Network, Node, Profile,
    RouteConfig, RoutingMode, ScenarioAction, Topology, WifiMode,
};

type Result<T> = std::result::Result<T, Error>;

/// Render a topology as valid NLL syntax.
///
/// Returns an error when the topology contains a value that has no NLL
/// spelling — a string with an embedded `"` (the lexer's string rule is
/// `"[^"]*"`, so it cannot be escaped), a node name the name grammar
/// cannot express, a firewall match expression outside the `match_expr`
/// grammar, or an interface kind (bond/vlan) the language has no block
/// for. Prefer this over [`render`], which panics on the same input.
pub fn try_render(topology: &Topology) -> Result<String> {
    let mut out = String::new();

    render_lab(&mut out, &topology.lab)?;
    render_profiles(&mut out, topology)?;
    render_nodes(&mut out, topology)?;
    render_links(&mut out, topology)?;
    render_networks(&mut out, topology)?;
    render_impairments(&mut out, topology)?;
    render_rate_limits(&mut out, topology)?;
    render_assertions(&mut out, topology)?;
    render_scenarios(&mut out, topology)?;
    render_benchmarks(&mut out, topology)?;

    Ok(out)
}

/// Render a topology as valid NLL syntax.
///
/// Infallible wrapper around [`try_render`] kept for existing callers.
///
/// # Panics
///
/// Panics if the topology cannot be represented in NLL (see
/// [`try_render`] for the cases). Topologies produced by the NLL parser
/// never hit this; only programmatically built ones can. New code should
/// call [`try_render`] and handle the error.
pub fn render(topology: &Topology) -> String {
    try_render(topology).unwrap_or_else(|e| panic!("topology cannot be rendered as NLL: {e}"))
}

// ─────────────────────────────────────────────────
// Lexical helpers
// ─────────────────────────────────────────────────

/// Keywords the lexer turns into dedicated tokens. None of these lex as
/// `Ident`, so they are rejected wherever the parser calls `parse_value`
/// (which matches `Token::Ident` only).
const KEYWORDS: &[&str] = &[
    "import",
    "as",
    "lab",
    "node",
    "profile",
    "link",
    "network",
    "defaults",
    "param",
    "pool",
    "validate",
    "scenario",
    "benchmark",
    "mesh",
    "ring",
    "star",
    "for",
    "in",
    "let",
    "impair",
    "rate",
];

/// Keywords the parser's `token_as_ident` accepts as identifiers anyway.
const KEYWORDS_AS_IDENT: &[&str] = &[
    "import", "as", "defaults", "pool", "validate", "mesh", "ring", "star", "rate",
];

fn unrepresentable(what: &str, value: &str, why: &str) -> Error {
    Error::invalid_topology(format!("cannot render {what} {value:?} as NLL: {why}"))
}

/// `"…"` — the lexer's string rule is `"[^"]*"`, so a `"` cannot appear
/// inside a string at all.
fn nll_string(s: &str) -> Result<String> {
    if s.contains('"') {
        return Err(unrepresentable(
            "string",
            s,
            "NLL strings cannot contain a double quote",
        ));
    }
    Ok(format!("\"{s}\""))
}

/// Shape of the lexer's `Ident` token: `[a-zA-Z_][a-zA-Z0-9_-]*`.
fn is_ident_shape(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A bare identifier accepted by `expect_ident` (Ident token, or one of
/// the keywords `token_as_ident` tolerates).
fn nll_ident(s: &str) -> Result<&str> {
    if !is_ident_shape(s) {
        return Err(unrepresentable(
            "identifier",
            s,
            "identifiers must match [a-zA-Z_][a-zA-Z0-9_-]*",
        ));
    }
    if KEYWORDS.contains(&s) && !KEYWORDS_AS_IDENT.contains(&s) {
        return Err(unrepresentable(
            "identifier",
            s,
            "it is a reserved NLL keyword",
        ));
    }
    Ok(s)
}

/// A name accepted by `parse_name`: dot-separated segments, each either an
/// identifier (optionally starting with `*` for globs) or a bare integer.
/// Segments must be adjacent (no whitespace), which is trivially true for
/// a single string.
fn nll_name(s: &str) -> Result<&str> {
    let bad = |why: &str| unrepresentable("name", s, why);
    if s.is_empty() {
        return Err(bad("empty name"));
    }
    for (i, seg) in s.split('.').enumerate() {
        if seg.is_empty() {
            return Err(bad("empty segment between dots"));
        }
        let all_digits = seg.chars().all(|c| c.is_ascii_digit());
        if all_digits {
            if i == 0 {
                return Err(bad("names cannot start with a digit"));
            }
            continue;
        }
        let ident_like = seg.strip_prefix('*').unwrap_or(seg);
        let ok = if ident_like.is_empty() {
            true
        } else if seg.starts_with('*') {
            // `*-black`: after the glob star the lexer sees `-` and an ident.
            ident_like
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        } else {
            is_ident_shape(seg)
        };
        if !ok {
            return Err(bad(
                "segments must match [a-zA-Z_*][a-zA-Z0-9_-]* or be an integer",
            ));
        }
        if i == 0 && KEYWORDS.contains(&seg) && !KEYWORDS_AS_IDENT.contains(&seg) {
            return Err(bad("it starts with a reserved NLL keyword"));
        }
    }
    Ok(s)
}

/// `node:iface` endpoint.
fn nll_endpoint(s: &str) -> Result<String> {
    let (node, iface) = s
        .split_once(':')
        .ok_or_else(|| unrepresentable("endpoint", s, "expected node:interface"))?;
    Ok(format!("{}:{}", nll_name(node)?, nll_name(iface)?))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `[0-9]+(\.[0-9]+)?`
fn is_number(s: &str) -> bool {
    match s.split_once('.') {
        Some((a, b)) => is_digits(a) && is_digits(b),
        None => is_digits(s),
    }
}

/// `\+?[0-9]+(\.[0-9]+)?(ms|us|ns|s)`
fn is_duration(s: &str) -> bool {
    let s = s.strip_prefix('+').unwrap_or(s);
    ["ms", "us", "ns", "s"]
        .iter()
        .any(|suf| s.strip_suffix(suf).is_some_and(is_number))
}

/// `[0-9]+(mbit|kbit|gbit|bit|mbyte|kbyte|gbyte|byte|[mgtp])` — note: no
/// decimal part, unlike durations.
fn is_rate(s: &str) -> bool {
    [
        "mbit", "kbit", "gbit", "bit", "mbyte", "kbyte", "gbyte", "byte", "m", "g", "t", "p",
    ]
    .iter()
    .any(|suf| s.strip_suffix(suf).is_some_and(is_digits))
}

/// `[0-9]+(\.[0-9]+)?%`
fn is_percent(s: &str) -> bool {
    s.strip_suffix('%').is_some_and(is_number)
}

/// IPv4 address, optionally with `/prefix`.
fn is_ipv4(s: &str) -> bool {
    let (ip, prefix) = match s.split_once('/') {
        Some((ip, p)) => (ip, Some(p)),
        None => (s, None),
    };
    let octets: Vec<&str> = ip.split('.').collect();
    octets.len() == 4 && octets.iter().all(|o| is_digits(o)) && prefix.is_none_or(is_digits)
}

/// IPv6 address as the lexer sees it: must contain `::`, the part before
/// it must be hex and contain a digit (or be empty), the rest hex/`:`/`.`,
/// optionally `/prefix`.
fn is_ipv6(s: &str) -> bool {
    let (ip, prefix) = match s.split_once('/') {
        Some((ip, p)) => (ip, Some(p)),
        None => (s, None),
    };
    let Some((head, tail)) = ip.split_once("::") else {
        return false;
    };
    let head_ok = head.is_empty()
        || (head.chars().all(|c| c.is_ascii_hexdigit())
            && head.chars().any(|c| c.is_ascii_digit()));
    let tail_ok = tail
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    head_ok && tail_ok && prefix.is_none_or(is_digits)
}

/// Something `parse_cidr_or_name` accepts: an IP/CIDR literal or the
/// `auto[/N]` placeholder. Strings are not accepted in these positions.
fn nll_addr(s: &str) -> Result<&str> {
    let auto_ok = s == "auto" || s.strip_prefix("auto/").is_some_and(is_digits);
    if is_ipv4(s) || is_ipv6(s) || auto_ok {
        Ok(s)
    } else {
        Err(unrepresentable(
            "address",
            s,
            "expected an IPv4/IPv6 address or CIDR (IPv6 must contain `::`)",
        ))
    }
}

/// A bare identifier as accepted by `parse_value` (`Token::Ident` only).
fn is_plain_ident(s: &str) -> bool {
    is_ident_shape(s) && !KEYWORDS.contains(&s)
}

/// Value in a `parse_value` position: bare when it lexes as a single
/// literal token, quoted otherwise.
fn lit_value(s: &str) -> Result<String> {
    if is_plain_ident(s)
        || is_number(s)
        || is_duration(s)
        || is_rate(s)
        || is_percent(s)
        || is_ipv4(s)
        || is_ipv6(s)
    {
        Ok(s.to_string())
    } else {
        nll_string(s)
    }
}

/// Value in an `expect_duration_or_value` position (Duration | Ident |
/// String).
fn lit_duration(s: &str) -> Result<String> {
    if is_duration(s) || is_plain_ident(s) {
        Ok(s.to_string())
    } else {
        nll_string(s)
    }
}

/// Value in an `expect_rate_or_value` position (Rate | Ident | String).
fn lit_rate(s: &str) -> Result<String> {
    if is_rate(s) || is_plain_ident(s) {
        Ok(s.to_string())
    } else {
        nll_string(s)
    }
}

/// Value in an `expect_percent_or_value` position (Percent | Ident |
/// String).
fn lit_percent(s: &str) -> Result<String> {
    if is_percent(s) || is_plain_ident(s) {
        Ok(s.to_string())
    } else {
        nll_string(s)
    }
}

fn ident_list(items: &[String]) -> Result<String> {
    let parts: Vec<&str> = items.iter().map(|s| nll_ident(s)).collect::<Result<_>>()?;
    Ok(format!("[{}]", parts.join(", ")))
}

fn string_list(items: &[String]) -> Result<String> {
    let parts: Vec<String> = items.iter().map(|s| nll_string(s)).collect::<Result<_>>()?;
    Ok(format!("[{}]", parts.join(", ")))
}

fn sorted_keys<V>(map: &HashMap<String, V>) -> Vec<&String> {
    let mut keys: Vec<_> = map.keys().collect();
    keys.sort();
    keys
}

// ─────────────────────────────────────────────────
// Lab
// ─────────────────────────────────────────────────

fn render_lab(out: &mut String, lab: &LabConfig) -> Result<()> {
    let mut body = String::new();
    if let Some(desc) = &lab.description {
        writeln!(body, "  description {}", nll_string(desc)?).unwrap();
    }
    if let Some(prefix) = &lab.prefix {
        writeln!(body, "  prefix {}", nll_string(prefix)?).unwrap();
    }
    if let Some(rt) = &lab.runtime {
        let rt = match rt {
            ContainerRuntime::Auto => "auto",
            ContainerRuntime::Docker => "docker",
            ContainerRuntime::Podman => "podman",
        };
        writeln!(body, "  runtime \"{rt}\"").unwrap();
    }
    if let Some(version) = &lab.version {
        writeln!(body, "  version {}", nll_string(version)?).unwrap();
    }
    if let Some(author) = &lab.author {
        writeln!(body, "  author {}", nll_string(author)?).unwrap();
    }
    if !lab.tags.is_empty() {
        writeln!(body, "  tags {}", ident_list(&lab.tags)?).unwrap();
    }
    if let Some(mgmt) = &lab.mgmt_subnet {
        write!(body, "  mgmt {}", nll_addr(mgmt)?).unwrap();
        if lab.mgmt_host_reachable {
            body.push_str(" host-reachable");
        }
        body.push('\n');
    }
    match lab.dns {
        DnsMode::Off => {}
        DnsMode::Hosts => body.push_str("  dns hosts\n"),
    }
    match lab.routing {
        RoutingMode::Manual => {}
        RoutingMode::Auto => body.push_str("  routing auto\n"),
    }

    write!(out, "lab {}", nll_string(&lab.name)?).unwrap();
    if body.is_empty() {
        out.push('\n');
    } else {
        out.push_str(" {\n");
        out.push_str(&body);
        out.push_str("}\n");
    }
    out.push('\n');
    Ok(())
}

// ─────────────────────────────────────────────────
// Sysctls / firewall / NAT (shared by profiles and nodes)
// ─────────────────────────────────────────────────

const IPV4_FORWARD: &str = "net.ipv4.ip_forward";
const IPV6_FORWARD: &str = "net.ipv6.conf.all.forwarding";

/// Emit `forward ipv4` / `forward ipv6` sugar where the sysctl matches the
/// lowering of that sugar exactly (`=1`), and `sysctl "k" "v"` otherwise.
fn render_sysctls(out: &mut String, indent: &str, sysctls: &HashMap<String, String>) -> Result<()> {
    let is_sugar = |k: &str| sysctls.get(k).map(String::as_str) == Some("1");
    if is_sugar(IPV4_FORWARD) {
        writeln!(out, "{indent}forward ipv4").unwrap();
    }
    if is_sugar(IPV6_FORWARD) {
        writeln!(out, "{indent}forward ipv6").unwrap();
    }
    for k in sorted_keys(sysctls) {
        if (k == IPV4_FORWARD || k == IPV6_FORWARD) && is_sugar(k) {
            continue;
        }
        writeln!(
            out,
            "{indent}sysctl {} {}",
            nll_string(k)?,
            nll_string(&sysctls[k])?
        )
        .unwrap();
    }
    Ok(())
}

/// Translate an nftables match expression (as produced by the parser's
/// `parse_match_expr`) back into NLL `match_expr` syntax.
fn firewall_match_to_nll(expr: &str) -> Result<String> {
    let bad = |why: &str| unrepresentable("firewall match", expr, why);
    let toks: Vec<&str> = expr.split_whitespace().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let rest = &toks[i..];
        match rest {
            ["ip" | "ip6", dir @ ("saddr" | "daddr"), addr, ..] => {
                let kw = if *dir == "saddr" { "src" } else { "dst" };
                parts.push(format!("{kw} {}", nll_addr(addr)?));
                i += 3;
            }
            ["ct", "state", states, ..] => {
                for st in states.split(',') {
                    nll_name(st)?;
                }
                parts.push(format!("ct {states}"));
                i += 3;
            }
            [proto @ ("tcp" | "udp"), dir @ ("dport" | "sport"), port, ..] if is_digits(port) => {
                parts.push(format!("{proto} {dir} {port}"));
                i += 3;
            }
            [proto @ ("icmp" | "icmpv6"), "type", ty, ..] if is_digits(ty) => {
                parts.push(format!("{proto} {ty}"));
                i += 3;
            }
            ["mark", mark, ..] if is_digits(mark) => {
                parts.push(format!("mark {mark}"));
                i += 2;
            }
            _ => {
                return Err(bad(
                    "only `ip[6] saddr/daddr`, `ct state`, `tcp/udp dport/sport`, \
                     `icmp[v6] type` and `mark` have NLL syntax",
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(bad("a firewall rule needs at least one match"));
    }
    Ok(parts.join(" "))
}

fn render_firewall(out: &mut String, indent: &str, fw: &FirewallConfig) -> Result<()> {
    let policy = fw.policy.as_deref().unwrap_or("accept");
    writeln!(out, "{indent}firewall policy {} {{", nll_ident(policy)?).unwrap();
    for rule in &fw.rules {
        let action = rule.action.as_deref().unwrap_or("accept");
        if !matches!(action, "accept" | "drop" | "reject") {
            return Err(unrepresentable(
                "firewall action",
                action,
                "expected accept, drop or reject",
            ));
        }
        let expr = rule.match_expr.as_deref().unwrap_or("");
        writeln!(out, "{indent}  {action} {}", firewall_match_to_nll(expr)?).unwrap();
    }
    writeln!(out, "{indent}}}").unwrap();
    Ok(())
}

fn render_nat(out: &mut String, indent: &str, nat: &crate::types::NatConfig) -> Result<()> {
    writeln!(out, "{indent}nat {{").unwrap();
    for rule in &nat.rules {
        let need = |field: Option<&String>, what: &str| -> Result<String> {
            field
                .map(|s| nll_addr(s).map(str::to_string))
                .unwrap_or_else(|| {
                    Err(unrepresentable(
                        "nat rule",
                        &format!("{:?}", rule.action),
                        &format!("missing required {what}"),
                    ))
                })
        };
        write!(out, "{indent}  ").unwrap();
        match rule.action {
            NatAction::Masquerade => {
                out.push_str("masquerade");
                if let Some(src) = &rule.src {
                    write!(out, " src {}", nll_addr(src)?).unwrap();
                }
            }
            NatAction::Dnat => {
                out.push_str("dnat");
                if let Some(dst) = &rule.dst {
                    write!(out, " dst {}", nll_addr(dst)?).unwrap();
                }
                write!(out, " to {}", need(rule.target.as_ref(), "target")?).unwrap();
                if let Some(port) = rule.target_port {
                    write!(out, ":{port}").unwrap();
                }
            }
            NatAction::Snat => {
                out.push_str("snat");
                if let Some(src) = &rule.src {
                    write!(out, " src {}", nll_addr(src)?).unwrap();
                }
                write!(out, " to {}", need(rule.target.as_ref(), "target")?).unwrap();
            }
            NatAction::Translate => {
                write!(
                    out,
                    "translate {} to {}",
                    need(rule.src.as_ref(), "source range")?,
                    need(rule.target.as_ref(), "target range")?
                )
                .unwrap();
            }
        }
        out.push('\n');
    }
    writeln!(out, "{indent}}}").unwrap();
    Ok(())
}

fn render_route(out: &mut String, indent: &str, dest: &str, route: &RouteConfig) -> Result<()> {
    let dest = if dest == "default" {
        dest
    } else {
        nll_addr(dest)?
    };
    write!(out, "{indent}route {dest}").unwrap();
    if let Some(via) = &route.via {
        write!(out, " via {}", nll_addr(via)?).unwrap();
    }
    if let Some(dev) = &route.dev {
        write!(out, " dev {}", nll_name(dev)?).unwrap();
    }
    if let Some(metric) = route.metric {
        write!(out, " metric {metric}").unwrap();
    }
    out.push('\n');
    Ok(())
}

fn render_routes(
    out: &mut String,
    indent: &str,
    routes: &HashMap<String, RouteConfig>,
) -> Result<()> {
    for dest in sorted_keys(routes) {
        render_route(out, indent, dest, &routes[dest])?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────
// Profiles
// ─────────────────────────────────────────────────

fn render_profiles(out: &mut String, topo: &Topology) -> Result<()> {
    for name in sorted_keys(&topo.profiles) {
        render_profile(out, name, &topo.profiles[name])?;
    }
    if !topo.profiles.is_empty() {
        out.push('\n');
    }
    Ok(())
}

fn render_profile(out: &mut String, name: &str, profile: &Profile) -> Result<()> {
    writeln!(out, "profile {} {{", nll_ident(name)?).unwrap();
    render_sysctls(out, "  ", &profile.sysctls)?;
    if let Some(fw) = &profile.firewall {
        render_firewall(out, "  ", fw)?;
    }
    out.push_str("}\n");
    Ok(())
}

// ─────────────────────────────────────────────────
// Nodes
// ─────────────────────────────────────────────────

fn render_nodes(out: &mut String, topo: &Topology) -> Result<()> {
    for name in sorted_keys(&topo.nodes) {
        render_node(out, name, &topo.nodes[name])?;
    }
    if !topo.nodes.is_empty() {
        out.push('\n');
    }
    Ok(())
}

fn render_node(out: &mut String, name: &str, node: &Node) -> Result<()> {
    write!(out, "node {}", nll_name(name)?).unwrap();
    if let Some(profile) = &node.profile {
        write!(out, " : {}", nll_name(profile)?).unwrap();
    }
    if let Some(image) = &node.image {
        write!(out, " image {}", nll_string(image)?).unwrap();
    }

    let mut body = String::new();
    render_node_body(&mut body, node)?;

    if body.is_empty() {
        out.push('\n');
    } else {
        out.push_str(" {\n");
        out.push_str(&body);
        out.push_str("}\n");
    }
    Ok(())
}

fn render_node_body(out: &mut String, node: &Node) -> Result<()> {
    // ── Container properties ──
    if let Some(cmd) = &node.cmd {
        writeln!(out, "  cmd {}", string_list(cmd)?).unwrap();
    }
    if let Some(env) = &node.env {
        let mut entries: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        entries.sort();
        writeln!(out, "  env {}", string_list(&entries)?).unwrap();
    }
    if let Some(volumes) = &node.volumes {
        writeln!(out, "  volumes {}", string_list(volumes)?).unwrap();
    }
    if let Some(cpu) = &node.cpu {
        writeln!(out, "  cpu {}", lit_value(cpu)?).unwrap();
    }
    if let Some(mem) = &node.memory {
        writeln!(out, "  memory {}", lit_value(mem)?).unwrap();
    }
    if node.privileged {
        out.push_str("  privileged\n");
    }
    if !node.cap_add.is_empty() {
        writeln!(out, "  cap-add {}", ident_list(&node.cap_add)?).unwrap();
    }
    if !node.cap_drop.is_empty() {
        writeln!(out, "  cap-drop {}", ident_list(&node.cap_drop)?).unwrap();
    }
    if let Some(ep) = &node.entrypoint {
        writeln!(out, "  entrypoint {}", nll_string(ep)?).unwrap();
    }
    if let Some(h) = &node.hostname {
        writeln!(out, "  hostname {}", nll_string(h)?).unwrap();
    }
    if let Some(w) = &node.workdir {
        writeln!(out, "  workdir {}", nll_string(w)?).unwrap();
    }
    if !node.labels.is_empty() {
        writeln!(out, "  labels {}", string_list(&node.labels)?).unwrap();
    }
    if let Some(p) = &node.pull {
        writeln!(out, "  pull {}", lit_value(p)?).unwrap();
    }
    for cmd in &node.container_exec {
        writeln!(out, "  exec {}", nll_string(cmd)?).unwrap();
    }
    if let Some(hc) = &node.healthcheck {
        write!(out, "  healthcheck {}", nll_string(hc)?).unwrap();
        if node.healthcheck_interval.is_some() || node.healthcheck_timeout.is_some() {
            out.push_str(" {");
            if let Some(iv) = &node.healthcheck_interval {
                write!(out, " interval {}", lit_value(iv)?).unwrap();
            }
            if let Some(to) = &node.healthcheck_timeout {
                write!(out, " timeout {}", lit_value(to)?).unwrap();
            }
            out.push_str(" }");
        }
        out.push('\n');
    } else {
        // Standalone forms are accepted without a `healthcheck` command
        // (integration-testing.nll style is command + standalone; keep
        // the values either way).
        if let Some(iv) = &node.healthcheck_interval {
            writeln!(out, "  healthcheck-interval {}", lit_value(iv)?).unwrap();
        }
        if let Some(to) = &node.healthcheck_timeout {
            writeln!(out, "  healthcheck-timeout {}", lit_value(to)?).unwrap();
        }
    }
    if let Some(d) = &node.startup_delay {
        writeln!(out, "  startup-delay {}", lit_value(d)?).unwrap();
    }
    if let Some(ef) = &node.env_file {
        writeln!(out, "  env-file {}", nll_string(ef)?).unwrap();
    }
    for (h, c) in &node.configs {
        writeln!(out, "  config {} {}", nll_string(h)?, nll_string(c)?).unwrap();
    }
    if let Some(o) = &node.overlay {
        writeln!(out, "  overlay {}", nll_string(o)?).unwrap();
    }
    if !node.depends_on.is_empty() {
        writeln!(out, "  depends-on {}", ident_list(&node.depends_on)?).unwrap();
    }

    // ── Namespace properties ──
    render_sysctls(out, "  ", &node.sysctls)?;

    for iface_name in sorted_keys(&node.interfaces) {
        render_interface(out, iface_name, &node.interfaces[iface_name])?;
    }

    render_routes(out, "  ", &node.routes)?;

    if let Some(fw) = &node.firewall {
        render_firewall(out, "  ", fw)?;
    }
    if let Some(nat) = &node.nat {
        render_nat(out, "  ", nat)?;
    }

    for vrf_name in sorted_keys(&node.vrfs) {
        let vrf = &node.vrfs[vrf_name];
        writeln!(out, "  vrf {} table {} {{", nll_ident(vrf_name)?, vrf.table).unwrap();
        if !vrf.interfaces.is_empty() {
            writeln!(out, "    interfaces {}", ident_list(&vrf.interfaces)?).unwrap();
        }
        render_routes(out, "    ", &vrf.routes)?;
        out.push_str("  }\n");
    }

    for wg_name in sorted_keys(&node.wireguard) {
        let wg = &node.wireguard[wg_name];
        writeln!(out, "  wireguard {} {{", nll_ident(wg_name)?).unwrap();
        if let Some(key) = &wg.private_key {
            writeln!(out, "    key {}", lit_value(key)?).unwrap();
        }
        if let Some(port) = wg.listen_port {
            writeln!(out, "    listen {port}").unwrap();
        }
        if let Some(mark) = wg.fwmark {
            writeln!(out, "    fwmark {mark}").unwrap();
        }
        for addr in &wg.addresses {
            writeln!(out, "    address {}", nll_addr(addr)?).unwrap();
        }
        if !wg.peers.is_empty() {
            writeln!(out, "    peers {}", ident_list(&wg.peers)?).unwrap();
        }
        out.push_str("  }\n");
    }

    for mv in &node.macvlans {
        let mode = match mv.mode {
            MacvlanMode::Bridge => "bridge",
            MacvlanMode::Private => "private",
            MacvlanMode::Vepa => "vepa",
            MacvlanMode::Passthru => "passthru",
        };
        write!(
            out,
            "  macvlan {} parent {} mode {mode}",
            nll_ident(&mv.name)?,
            lit_value(&mv.parent)?
        )
        .unwrap();
        render_addr_block(out, &mv.addresses)?;
    }
    for iv in &node.ipvlans {
        let mode = match iv.mode {
            IpvlanMode::L2 => "l2",
            IpvlanMode::L3 => "l3",
            IpvlanMode::L3S => "l3s",
        };
        write!(
            out,
            "  ipvlan {} parent {} mode {mode}",
            nll_ident(&iv.name)?,
            lit_value(&iv.parent)?
        )
        .unwrap();
        render_addr_block(out, &iv.addresses)?;
    }
    for w in &node.wifi {
        let mode = match w.mode {
            WifiMode::Ap => "ap",
            WifiMode::Station => "station",
            WifiMode::Mesh => "mesh",
        };
        write!(out, "  wifi {} mode {mode}", nll_ident(&w.name)?).unwrap();
        let has_props = w.ssid.is_some()
            || w.channel.is_some()
            || w.passphrase.is_some()
            || w.mesh_id.is_some()
            || !w.addresses.is_empty();
        if has_props {
            out.push_str(" {\n");
            if let Some(ssid) = &w.ssid {
                writeln!(out, "    ssid {}", nll_string(ssid)?).unwrap();
            }
            if let Some(ch) = w.channel {
                writeln!(out, "    channel {ch}").unwrap();
            }
            if let Some(pass) = &w.passphrase {
                writeln!(out, "    wpa2 {}", nll_string(pass)?).unwrap();
            }
            if let Some(mid) = &w.mesh_id {
                writeln!(out, "    mesh-id {}", nll_string(mid)?).unwrap();
            }
            for addr in &w.addresses {
                writeln!(out, "    address {}", nll_addr(addr)?).unwrap();
            }
            out.push_str("  }\n");
        } else {
            out.push('\n');
        }
    }

    for exec in &node.exec {
        out.push_str("  run");
        if exec.background {
            out.push_str(" background");
        }
        match exec.cmd.as_slice() {
            // `run "cmd"` lowers to ["sh", "-c", "cmd"]; emit the sugar back.
            [sh, dash_c, script] if sh == "sh" && dash_c == "-c" => {
                write!(out, " {}", nll_string(script)?).unwrap();
            }
            cmd => write!(out, " {}", string_list(cmd)?).unwrap(),
        }
        out.push('\n');
    }
    Ok(())
}

/// `{ addr… }` block for macvlan/ipvlan, or a bare newline when empty.
fn render_addr_block(out: &mut String, addresses: &[String]) -> Result<()> {
    if addresses.is_empty() {
        out.push('\n');
        return Ok(());
    }
    out.push_str(" {\n");
    for addr in addresses {
        writeln!(out, "    address {}", nll_addr(addr)?).unwrap();
    }
    out.push_str("  }\n");
    Ok(())
}

fn render_interface(
    out: &mut String,
    name: &str,
    iface: &crate::types::InterfaceConfig,
) -> Result<()> {
    let bad = |why: &str| unrepresentable("interface", name, why);
    let is_lo = name == "lo" || iface.kind == Some(InterfaceKind::Loopback);
    let kind = if is_lo {
        if name != "lo" {
            return Err(bad("loopback interfaces must be named `lo`"));
        }
        InterfaceKind::Loopback
    } else {
        match &iface.kind {
            Some(k) => k.clone(),
            None => return Err(bad("an explicit interface needs a kind (dummy/vxlan)")),
        }
    };
    // Fields with no NLL spelling on any interface block.
    if iface.mtu.is_some() {
        return Err(bad("per-interface `mtu` has no NLL syntax"));
    }
    match kind {
        InterfaceKind::Loopback => {
            if iface.vni.is_some()
                || iface.local.is_some()
                || iface.remote.is_some()
                || iface.port.is_some()
                || iface.underlay.is_some()
                || iface.parent.is_some()
                || !iface.members.is_empty()
            {
                return Err(bad("`lo` only carries addresses in NLL"));
            }
            for addr in &iface.addresses {
                writeln!(out, "  lo {}", nll_addr(addr)?).unwrap();
            }
        }
        InterfaceKind::Dummy => {
            if iface.vni.is_some()
                || iface.local.is_some()
                || iface.remote.is_some()
                || iface.port.is_some()
                || iface.underlay.is_some()
                || iface.parent.is_some()
                || !iface.members.is_empty()
            {
                return Err(bad("`dummy` only carries addresses in NLL"));
            }
            write!(out, "  dummy {}", nll_ident(name)?).unwrap();
            if iface.addresses.is_empty() {
                out.push('\n');
            } else {
                out.push_str(" {\n");
                for addr in &iface.addresses {
                    writeln!(out, "    address {}", nll_addr(addr)?).unwrap();
                }
                out.push_str("  }\n");
            }
        }
        InterfaceKind::Vxlan => {
            if iface.parent.is_some() || !iface.members.is_empty() {
                return Err(bad("`vxlan` has no parent/members syntax"));
            }
            writeln!(out, "  vxlan {} {{", nll_ident(name)?).unwrap();
            if let Some(vni) = iface.vni {
                writeln!(out, "    vni {vni}").unwrap();
            }
            if let Some(local) = &iface.local {
                writeln!(out, "    local {}", nll_addr(local)?).unwrap();
            }
            if let Some(remote) = &iface.remote {
                writeln!(out, "    remote {}", nll_addr(remote)?).unwrap();
            }
            if let Some(port) = iface.port {
                writeln!(out, "    port {port}").unwrap();
            }
            if let Some(underlay) = &iface.underlay {
                writeln!(out, "    underlay {}", nll_ident(underlay)?).unwrap();
            }
            for addr in &iface.addresses {
                writeln!(out, "    address {}", nll_addr(addr)?).unwrap();
            }
            out.push_str("  }\n");
        }
        InterfaceKind::Bond => return Err(bad("bond interfaces have no NLL syntax")),
        InterfaceKind::Vlan => return Err(bad("vlan sub-interfaces have no NLL syntax")),
    }
    Ok(())
}

// ─────────────────────────────────────────────────
// Links
// ─────────────────────────────────────────────────

fn render_links(out: &mut String, topo: &Topology) -> Result<()> {
    for link in &topo.links {
        render_link(out, link)?;
    }
    if !topo.links.is_empty() {
        out.push('\n');
    }
    Ok(())
}

fn render_link(out: &mut String, link: &Link) -> Result<()> {
    write!(
        out,
        "link {} -- {}",
        nll_endpoint(&link.endpoints[0])?,
        nll_endpoint(&link.endpoints[1])?
    )
    .unwrap();

    let mut props: Vec<String> = Vec::new();
    if let Some(addrs) = &link.addresses {
        props.push(format!(
            "{} -- {}",
            nll_addr(&addrs[0])?,
            nll_addr(&addrs[1])?
        ));
    }
    if let Some(mtu) = link.mtu {
        props.push(format!("mtu {mtu}"));
    }
    if !props.is_empty() {
        write!(out, " {{ {} }}", props.join(" ")).unwrap();
    }
    out.push('\n');
    Ok(())
}

// ─────────────────────────────────────────────────
// Networks
// ─────────────────────────────────────────────────

fn render_networks(out: &mut String, topo: &Topology) -> Result<()> {
    for name in sorted_keys(&topo.networks) {
        render_network(out, name, &topo.networks[name])?;
    }
    if !topo.networks.is_empty() {
        out.push('\n');
    }
    Ok(())
}

fn render_network(out: &mut String, name: &str, net: &Network) -> Result<()> {
    writeln!(out, "network {} {{", nll_ident(name)?).unwrap();
    if !net.members.is_empty() {
        let members: Vec<String> = net
            .members
            .iter()
            .map(|m| nll_endpoint(m))
            .collect::<Result<_>>()?;
        writeln!(out, "  members [{}]", members.join(", ")).unwrap();
    }
    if net.vlan_filtering == Some(true) {
        out.push_str("  vlan-filtering\n");
    }
    if let Some(subnet) = &net.subnet {
        writeln!(out, "  subnet {}", nll_addr(subnet)?).unwrap();
    }
    if let Some(mtu) = net.mtu {
        writeln!(out, "  mtu {mtu}").unwrap();
    }
    let mut vlan_ids: Vec<&u16> = net.vlans.keys().collect();
    vlan_ids.sort();
    for id in vlan_ids {
        write!(out, "  vlan {id}").unwrap();
        if let Some(vname) = &net.vlans[id].name {
            write!(out, " {}", nll_string(vname)?).unwrap();
        }
        out.push('\n');
    }

    // Ports come in two flavours after lowering: explicit `port NODE { … }`
    // blocks keyed by bare node name, and subnet auto-assignment keyed by
    // the full `node:iface` member string. The latter is regenerated by
    // the parser from `subnet` + member order, so it must not be emitted
    // (and cannot be: `port` takes a name, not an endpoint).
    let auto = auto_assigned_ports(net);
    for key in sorted_keys(&net.ports) {
        let port = &net.ports[key];
        if key.contains(':') {
            let expected = auto.get(key);
            let matches = expected.is_some_and(|addr| {
                port.addresses.len() == 1
                    && &port.addresses[0] == addr
                    && port.vlans.is_empty()
                    && port.tagged.is_none()
                    && port.pvid.is_none()
                    && port.untagged.is_none()
                    && port.interface.is_none()
            });
            if matches {
                continue;
            }
            return Err(unrepresentable(
                "network port",
                key,
                "`port` takes a node name; endpoint-keyed ports are only produced by \
                 `subnet` auto-assignment and this one does not match it",
            ));
        }
        if port.interface.is_some() {
            return Err(unrepresentable(
                "network port",
                key,
                "`interface` has no NLL syntax",
            ));
        }
        write!(out, "  port {} {{", nll_name(key)?).unwrap();
        for addr in &port.addresses {
            if !is_ipv4(addr) || !addr.contains('/') {
                return Err(unrepresentable(
                    "port address",
                    addr,
                    "port blocks accept IPv4 CIDRs only",
                ));
            }
            write!(out, " {addr}").unwrap();
        }
        if let Some(pvid) = port.pvid {
            write!(out, " pvid {pvid}").unwrap();
        }
        if !port.vlans.is_empty() {
            let ids: Vec<String> = port.vlans.iter().map(u16::to_string).collect();
            write!(out, " vlans [{}]", ids.join(", ")).unwrap();
        }
        if port.tagged == Some(true) {
            out.push_str(" tagged");
        }
        if port.untagged == Some(true) {
            out.push_str(" untagged");
        }
        out.push_str(" }\n");
    }

    for imp in &net.impairments {
        write!(
            out,
            "  impair {} -- {} {{",
            nll_name(&imp.src)?,
            nll_name(&imp.dst)?
        )
        .unwrap();
        let props = impairment_props(&imp.impairment)?;
        if !props.is_empty() {
            write!(out, " {props}").unwrap();
        }
        if let Some(rc) = &imp.rate_cap {
            write!(out, " rate-cap {}", lit_rate(rc)?).unwrap();
        }
        out.push_str(" }\n");
    }
    out.push_str("}\n");
    Ok(())
}

/// Reproduce the parser's `subnet` auto-assignment: member `i` gets
/// `base + i + 1` with the subnet's prefix length.
fn auto_assigned_ports(net: &Network) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(subnet) = &net.subnet else {
        return map;
    };
    let Some((base, prefix)) = subnet.split_once('/') else {
        return map;
    };
    let Ok(base) = base.parse::<IpAddr>() else {
        return map;
    };
    if !is_digits(prefix) {
        return map;
    }
    for (i, member) in net.members.iter().enumerate() {
        let ip = match base {
            IpAddr::V4(v4) => IpAddr::V4((u32::from(v4).wrapping_add(i as u32 + 1)).into()),
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6).wrapping_add(i as u128 + 1)).into()),
        };
        map.insert(member.clone(), format!("{ip}/{prefix}"));
    }
    map
}

// ─────────────────────────────────────────────────
// Impairments / rate limits
// ─────────────────────────────────────────────────

/// `delay … jitter … loss … rate … corrupt … reorder …` on one line.
///
/// Always a single line: the link-block parser re-enters
/// `parse_impair_props` per line and the last line wins, so splitting
/// properties across lines would lose all but the last.
fn impairment_props(imp: &Impairment) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = &imp.delay {
        parts.push(format!("delay {}", lit_duration(d)?));
    }
    if let Some(j) = &imp.jitter {
        parts.push(format!("jitter {}", lit_duration(j)?));
    }
    if let Some(l) = &imp.loss {
        parts.push(format!("loss {}", lit_percent(l)?));
    }
    if let Some(r) = &imp.rate {
        parts.push(format!("rate {}", lit_rate(r)?));
    }
    if let Some(c) = &imp.corrupt {
        parts.push(format!("corrupt {}", lit_percent(c)?));
    }
    if let Some(r) = &imp.reorder {
        parts.push(format!("reorder {}", lit_percent(r)?));
    }
    Ok(parts.join(" "))
}

fn render_impairments(out: &mut String, topo: &Topology) -> Result<()> {
    for endpoint in sorted_keys(&topo.impairments) {
        write!(out, "impair {}", nll_endpoint(endpoint)?).unwrap();
        let props = impairment_props(&topo.impairments[endpoint])?;
        if !props.is_empty() {
            write!(out, " {props}").unwrap();
        }
        out.push('\n');
    }
    if !topo.impairments.is_empty() {
        out.push('\n');
    }
    Ok(())
}

fn render_rate_limits(out: &mut String, topo: &Topology) -> Result<()> {
    for endpoint in sorted_keys(&topo.rate_limits) {
        let rl = &topo.rate_limits[endpoint];
        write!(out, "rate {}", nll_endpoint(endpoint)?).unwrap();
        if let Some(e) = &rl.egress {
            write!(out, " egress {}", lit_value(e)?).unwrap();
        }
        if let Some(i) = &rl.ingress {
            write!(out, " ingress {}", lit_value(i)?).unwrap();
        }
        if let Some(b) = &rl.burst {
            write!(out, " burst {}", lit_value(b)?).unwrap();
        }
        out.push('\n');
    }
    if !topo.rate_limits.is_empty() {
        out.push('\n');
    }
    Ok(())
}

// ─────────────────────────────────────────────────
// Assertions / scenarios / benchmarks
// ─────────────────────────────────────────────────

fn render_assertion(out: &mut String, indent: &str, a: &Assertion) -> Result<()> {
    write!(out, "{indent}").unwrap();
    match a {
        Assertion::Reach { from, to } => {
            writeln!(out, "reach {} {}", nll_ident(from)?, nll_ident(to)?).unwrap();
        }
        Assertion::NoReach { from, to } => {
            writeln!(out, "no-reach {} {}", nll_ident(from)?, nll_ident(to)?).unwrap();
        }
        Assertion::TcpConnect {
            from,
            to,
            port,
            timeout,
            retries,
            interval,
        } => {
            write!(
                out,
                "tcp-connect {} {} {port}",
                nll_ident(from)?,
                nll_ident(to)?
            )
            .unwrap();
            if let Some(t) = timeout {
                write!(out, " timeout {}", lit_duration(t)?).unwrap();
            }
            if let Some(r) = retries {
                write!(out, " retries {r}").unwrap();
            }
            if let Some(i) = interval {
                write!(out, " interval {}", lit_duration(i)?).unwrap();
            }
            out.push('\n');
        }
        Assertion::LatencyUnder {
            from,
            to,
            max,
            samples,
        } => {
            write!(
                out,
                "latency-under {} {} {}",
                nll_ident(from)?,
                nll_ident(to)?,
                lit_duration(max)?
            )
            .unwrap();
            if let Some(s) = samples {
                write!(out, " samples {s}").unwrap();
            }
            out.push('\n');
        }
        Assertion::RouteHas {
            node,
            destination,
            via,
            dev,
        } => {
            write!(
                out,
                "route-has {} {}",
                nll_ident(node)?,
                lit_value(destination)?
            )
            .unwrap();
            if let Some(v) = via {
                write!(out, " via {}", lit_value(v)?).unwrap();
            }
            if let Some(d) = dev {
                write!(out, " dev {}", nll_ident(d)?).unwrap();
            }
            out.push('\n');
        }
        Assertion::DnsResolves {
            from,
            name,
            expected_ip,
        } => {
            writeln!(
                out,
                "dns-resolves {} {} {}",
                nll_ident(from)?,
                lit_value(name)?,
                lit_value(expected_ip)?
            )
            .unwrap();
        }
    }
    Ok(())
}

fn render_assertion_block(out: &mut String, indent: &str, assertions: &[Assertion]) -> Result<()> {
    writeln!(out, "{indent}validate {{").unwrap();
    let inner = format!("{indent}  ");
    for a in assertions {
        render_assertion(out, &inner, a)?;
    }
    writeln!(out, "{indent}}}").unwrap();
    Ok(())
}

fn render_assertions(out: &mut String, topo: &Topology) -> Result<()> {
    if topo.assertions.is_empty() {
        return Ok(());
    }
    render_assertion_block(out, "", &topo.assertions)?;
    out.push('\n');
    Ok(())
}

fn render_scenarios(out: &mut String, topo: &Topology) -> Result<()> {
    for scenario in &topo.scenarios {
        writeln!(out, "scenario {} {{", nll_string(&scenario.name)?).unwrap();
        for step in &scenario.steps {
            if step.time_ms % 1000 == 0 {
                writeln!(out, "  at {}s {{", step.time_ms / 1000).unwrap();
            } else {
                writeln!(out, "  at {}ms {{", step.time_ms).unwrap();
            }
            for action in &step.actions {
                match action {
                    ScenarioAction::Down(ep) => {
                        writeln!(out, "    down {}", nll_endpoint(ep)?).unwrap();
                    }
                    ScenarioAction::Up(ep) => {
                        writeln!(out, "    up {}", nll_endpoint(ep)?).unwrap();
                    }
                    ScenarioAction::Clear(ep) => {
                        writeln!(out, "    clear {}", nll_endpoint(ep)?).unwrap();
                    }
                    ScenarioAction::Log(msg) => {
                        writeln!(out, "    log {}", nll_string(msg)?).unwrap();
                    }
                    ScenarioAction::Exec { node, cmd } => {
                        write!(out, "    exec {}", nll_ident(node)?).unwrap();
                        for c in cmd {
                            write!(out, " {}", nll_string(c)?).unwrap();
                        }
                        out.push('\n');
                    }
                    ScenarioAction::Validate(assertions) => {
                        render_assertion_block(out, "    ", assertions)?;
                    }
                }
            }
            out.push_str("  }\n");
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

fn render_benchmark_assertions(
    out: &mut String,
    assertions: &[crate::types::BenchmarkAssertion],
) -> Result<()> {
    for a in assertions {
        // `above`/`below` are the documented spellings; `>=`/`<=` are the
        // spellings `lower.rs` maps to `Gte`/`Lte`.
        let op = match a.op {
            CompareOp::Gt => "above",
            CompareOp::Lt => "below",
            CompareOp::Gte => ">=",
            CompareOp::Lte => "<=",
        };
        writeln!(
            out,
            "    assert {} {op} {}",
            nll_ident(&a.metric)?,
            lit_value(&a.value)?
        )
        .unwrap();
    }
    Ok(())
}

fn render_benchmarks(out: &mut String, topo: &Topology) -> Result<()> {
    for benchmark in &topo.benchmarks {
        writeln!(out, "benchmark {} {{", nll_string(&benchmark.name)?).unwrap();
        for test in &benchmark.tests {
            match test {
                BenchmarkTest::Ping {
                    from,
                    to,
                    count,
                    assertions,
                } => {
                    writeln!(out, "  ping {} {} {{", nll_ident(from)?, nll_ident(to)?).unwrap();
                    if let Some(c) = count {
                        writeln!(out, "    count {c}").unwrap();
                    }
                    render_benchmark_assertions(out, assertions)?;
                    out.push_str("  }\n");
                }
                BenchmarkTest::Iperf3 {
                    from,
                    to,
                    duration,
                    streams,
                    udp,
                    assertions,
                } => {
                    writeln!(out, "  iperf3 {} {} {{", nll_ident(from)?, nll_ident(to)?).unwrap();
                    if let Some(d) = duration {
                        writeln!(out, "    duration {}", lit_duration(d)?).unwrap();
                    }
                    if let Some(s) = streams {
                        writeln!(out, "    streams {s}").unwrap();
                    }
                    if *udp {
                        out.push_str("    udp\n");
                    }
                    render_benchmark_assertions(out, assertions)?;
                    out.push_str("  }\n");
                }
            }
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    use crate::types::{
        BenchmarkAssertion, ExecConfig, InterfaceConfig, NatConfig, NatRule, Network, PortConfig,
        RateLimit,
    };

    /// parse → render → parse, asserting structural equality via JSON.
    fn roundtrip(input: &str) -> (Topology, String, Topology) {
        let topo = parser::parse(input).unwrap();
        let rendered = try_render(&topo).unwrap();
        let topo2 = parser::parse(&rendered)
            .unwrap_or_else(|e| panic!("re-parse failed: {e}\n--- rendered ---\n{rendered}"));
        let a = serde_json::to_value(&topo).unwrap();
        let b = serde_json::to_value(&topo2).unwrap();
        assert_eq!(a, b, "round-trip mismatch\n--- rendered ---\n{rendered}");
        (topo, rendered, topo2)
    }

    #[test]
    fn test_render_roundtrip() {
        let (topo, _, topo2) = roundtrip(
            r#"lab "test"

node a
node b { route default via 10.0.0.1 }
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        );
        assert_eq!(topo2.nodes.len(), topo.nodes.len());
        assert_eq!(topo2.links.len(), topo.links.len());
        assert_eq!(topo2.lab.name, "test");
    }

    #[test]
    fn test_render_with_metadata() {
        let (_, rendered, _) = roundtrip(
            r#"lab "mylab" {
  description "Test lab"
  version "1.0"
  author "Test Author"
  tags [networking, test]
}

node a
"#,
        );
        assert!(rendered.contains("version \"1.0\""));
        assert!(rendered.contains("author \"Test Author\""));
        assert!(rendered.contains("tags [networking, test]"));
    }

    #[test]
    fn test_render_lab_runtime_and_routing() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "mylab" {
  runtime "podman"
  routing auto
  mgmt 172.20.0.0/24 host-reachable
}

node a
"#,
        );
        assert!(rendered.contains("runtime \"podman\""), "{rendered}");
        assert!(rendered.contains("routing auto"), "{rendered}");
        assert!(
            rendered.contains("mgmt 172.20.0.0/24 host-reachable"),
            "{rendered}"
        );
        assert_eq!(topo2.lab.runtime, Some(ContainerRuntime::Podman));
        assert_eq!(topo2.lab.routing, RoutingMode::Auto);
    }

    #[test]
    fn test_render_dns_hosts() {
        let (_, rendered, reparsed) = roundtrip(
            r#"lab "mylab" {
  dns hosts
}

node a
"#,
        );
        assert!(rendered.contains("dns hosts"), "rendered: {rendered}");
        assert_eq!(reparsed.lab.dns, DnsMode::Hosts);
    }

    #[test]
    fn test_render_dns_off_omitted() {
        let (_, rendered, _) = roundtrip(
            r#"lab "simple"

node a
"#,
        );
        assert!(
            !rendered.contains("dns"),
            "dns off should not be rendered: {rendered}"
        );
    }

    #[test]
    fn test_render_node_sysctls_and_forward_sugar() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node r {
  forward ipv4
  forward ipv6
  sysctl "net.core.rmem_max" "4194304"
}
node h { sysctl "net.ipv4.ip_forward" "0" }
"#,
        );
        assert!(rendered.contains("  forward ipv4\n"), "{rendered}");
        assert!(rendered.contains("  forward ipv6\n"), "{rendered}");
        assert!(
            rendered.contains("sysctl \"net.core.rmem_max\" \"4194304\""),
            "{rendered}"
        );
        // ip_forward=0 is not the sugar — it must be kept as an explicit sysctl.
        assert!(
            rendered.contains("sysctl \"net.ipv4.ip_forward\" \"0\""),
            "{rendered}"
        );
        assert_eq!(
            topo2.nodes["h"]
                .sysctls
                .get("net.ipv4.ip_forward")
                .map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn test_render_profile_firewall() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

profile web {
  forward ipv4
  firewall policy drop {
    accept ct established,related
    accept tcp dport 80
    accept tcp dport 22 src 10.0.1.0/24
    drop icmp 8
  }
}
node a : web
"#,
        );
        // The parser normalises `src`/`dst` to the front of the match.
        assert!(
            rendered.contains("accept src 10.0.1.0/24 tcp dport 22"),
            "{rendered}"
        );
        let fw = topo2.profiles["web"].firewall.as_ref().unwrap();
        assert_eq!(fw.rules.len(), 4);
    }

    #[test]
    fn test_render_firewall_empty_rules_has_braces() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        let mut n = Node::default();
        n.firewall = Some(FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![],
        });
        topo.nodes.insert("a".into(), n);
        let rendered = try_render(&topo).unwrap();
        assert!(rendered.contains("firewall policy drop {"), "{rendered}");
        let topo2 = parser::parse(&rendered).unwrap();
        assert_eq!(
            topo2.nodes["a"]
                .firewall
                .as_ref()
                .unwrap()
                .policy
                .as_deref(),
            Some("drop")
        );
    }

    #[test]
    fn test_render_firewall_unknown_match_is_error() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        let mut n = Node::default();
        n.firewall = Some(FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("meta l4proto sctp".into()),
                action: Some("accept".into()),
            }],
        });
        topo.nodes.insert("a".into(), n);
        let err = try_render(&topo).unwrap_err();
        assert!(err.to_string().contains("firewall match"), "{err}");
    }

    #[test]
    fn test_render_interfaces_vxlan_dummy_lo() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node vtep {
  lo 10.255.0.1/32
  lo 10.255.0.2/32
  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    port 4789
    underlay eth0
    address 192.168.100.1/24
  }
  dummy dum0 { address 10.99.0.1/32 }
  dummy dum1
}
"#,
        );
        assert!(rendered.contains("vxlan vxlan100 {"), "{rendered}");
        assert!(rendered.contains("underlay eth0"), "{rendered}");
        assert!(rendered.contains("dummy dum0 {"), "{rendered}");
        assert!(rendered.contains("dummy dum1\n"), "{rendered}");
        let n = &topo2.nodes["vtep"];
        assert_eq!(n.interfaces["vxlan100"].kind, Some(InterfaceKind::Vxlan));
        assert_eq!(n.interfaces["lo"].addresses.len(), 2);
    }

    #[test]
    fn test_render_bond_is_error() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        let mut n = Node::default();
        n.interfaces.insert(
            "bond0".into(),
            InterfaceConfig {
                kind: Some(InterfaceKind::Bond),
                members: vec!["eth0".into(), "eth1".into()],
                ..Default::default()
            },
        );
        topo.nodes.insert("a".into(), n);
        let err = try_render(&topo).unwrap_err();
        assert!(err.to_string().contains("bond"), "{err}");
    }

    #[test]
    fn test_render_vrf_wireguard_run() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node pe {
  forward ipv4
  vrf red table 10 {
    interfaces [eth1, wg-red]
    route default dev eth1
    route 10.10.0.0/16 via 10.10.0.1 metric 100
  }
  wireguard wg-red {
    key auto
    listen 51820
    fwmark 51820
    address 192.168.255.1/32
    peers [pe2]
  }
  route 192.168.2.0/24 dev wg-red metric 5
  run background ["iperf3", "-s"]
  run "ip link set eth0 txqueuelen 10000"
  run ["echo", "hi"] background
}
node pe2
"#,
        );
        assert!(rendered.contains("vrf red table 10 {"), "{rendered}");
        assert!(rendered.contains("metric 100"), "{rendered}");
        assert!(rendered.contains("wireguard wg-red {"), "{rendered}");
        assert!(rendered.contains("fwmark 51820"), "{rendered}");
        assert!(
            rendered.contains("run background [\"iperf3\", \"-s\"]"),
            "{rendered}"
        );
        assert!(
            rendered.contains("run \"ip link set eth0 txqueuelen 10000\""),
            "{rendered}"
        );
        let n = &topo2.nodes["pe"];
        assert_eq!(n.vrfs["red"].routes["10.10.0.0/16"].metric, Some(100));
        assert_eq!(n.exec.len(), 3);
        assert_eq!(n.exec[0].cmd, vec!["iperf3", "-s"]);
        assert!(n.exec[2].background);
    }

    #[test]
    fn test_render_run_shell_sugar_only_for_sh_c() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        let mut n = Node::default();
        n.exec.push(ExecConfig {
            cmd: vec!["sh".into(), "-c".into(), "echo hi".into()],
            background: false,
        });
        n.exec.push(ExecConfig {
            cmd: vec!["sh".into(), "-c".into()],
            background: true,
        });
        topo.nodes.insert("a".into(), n);
        let rendered = try_render(&topo).unwrap();
        assert!(rendered.contains("run \"echo hi\""), "{rendered}");
        assert!(
            rendered.contains("run background [\"sh\", \"-c\"]"),
            "{rendered}"
        );
        let topo2 = parser::parse(&rendered).unwrap();
        assert_eq!(
            serde_json::to_value(&topo).unwrap(),
            serde_json::to_value(&topo2).unwrap()
        );
    }

    #[test]
    fn test_render_container_props() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t" { runtime "docker" }

node app image "python:3-slim" {
  entrypoint "/bin/sh"
  cmd ["-c", "python -m http.server 8080"]
  env ["POSTGRES_PASSWORD=secret", "A=b=c"]
  volumes ["/host:/container", "/x:/y"]
  workdir "/app"
  cpu 1
  memory 512m
  privileged
  cap-add [NET_ADMIN, NET_RAW]
  cap-drop [SYS_ADMIN]
  hostname "app-host"
  labels ["nlink.role=app", "nlink.tier=backend"]
  pull missing
  exec "pip install x"
  exec "echo done"
  healthcheck "curl -f localhost" { interval 5s timeout 3s }
  startup-delay 3s
  env-file "configs/app.env"
  config "./nginx.conf" "/etc/nginx/nginx.conf"
  overlay "./overlay"
  depends-on [db]
}
node db image "postgres:16" {
  healthcheck-interval 500ms
  healthcheck-timeout 10s
}
"#,
        );
        assert!(rendered.contains("cap-drop [SYS_ADMIN]"), "{rendered}");
        assert!(
            rendered.contains("volumes [\"/host:/container\", \"/x:/y\"]"),
            "{rendered}"
        );
        assert!(
            rendered.contains("env [\"A=b=c\", \"POSTGRES_PASSWORD=secret\"]"),
            "{rendered}"
        );
        assert!(
            rendered.contains("healthcheck-interval 500ms"),
            "{rendered}"
        );
        let app = &topo2.nodes["app"];
        assert_eq!(app.env.as_ref().unwrap()["A"], "b=c");
        assert_eq!(app.cap_drop, vec!["SYS_ADMIN"]);
    }

    #[test]
    fn test_render_network_full() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node host1
node host2
node host3

network fabric {
  vlan-filtering
  mtu 9000
  members [host1:eth0, host2:eth0, host3:eth0]
  vlan 100 "sales"
  vlan 200
  port host1 { pvid 100 untagged }
  port host2 { vlans [100, 200] tagged }
  port host3 { 10.0.0.3/24 pvid 200 }
}

network radio {
  members [host1:rf, host2:rf]
  subnet 172.100.3.0/24
  impair host1 -- host2 { delay 15ms jitter 5ms loss 1% rate-cap 10mbit }
  impair host2 -- host1 { delay 40ms loss 5% }
}
"#,
        );
        assert!(rendered.contains("vlan-filtering"), "{rendered}");
        assert!(rendered.contains("mtu 9000"), "{rendered}");
        assert!(rendered.contains("vlan 100 \"sales\""), "{rendered}");
        assert!(rendered.contains("vlan 200\n"), "{rendered}");
        assert!(
            rendered.contains("port host2 { vlans [100, 200] tagged }"),
            "{rendered}"
        );
        assert!(rendered.contains("subnet 172.100.3.0/24"), "{rendered}");
        // Auto-assigned addresses are regenerated from `subnet`, never emitted.
        assert!(!rendered.contains("port host1:rf"), "{rendered}");
        let fabric = &topo2.networks["fabric"];
        assert_eq!(fabric.vlan_filtering, Some(true));
        assert_eq!(fabric.vlans[&100].name.as_deref(), Some("sales"));
        assert_eq!(fabric.ports["host2"].vlans, vec![100, 200]);
        let radio = &topo2.networks["radio"];
        assert_eq!(
            radio.ports["host2:rf"].addresses,
            vec!["172.100.3.2/24".to_string()]
        );
        assert_eq!(radio.impairments[0].rate_cap.as_deref(), Some("10mbit"));
    }

    #[test]
    fn test_render_network_endpoint_port_not_from_subnet_is_error() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        topo.nodes.insert("a".into(), Node::default());
        let mut net = Network {
            members: vec!["a:eth0".into()],
            ..Default::default()
        };
        net.ports.insert(
            "a:eth0".into(),
            PortConfig {
                addresses: vec!["10.0.0.1/24".into()],
                ..Default::default()
            },
        );
        topo.networks.insert("lan".into(), net);
        let err = try_render(&topo).unwrap_err();
        assert!(err.to_string().contains("network port"), "{err}");
    }

    #[test]
    fn test_render_rate_limit_burst_and_impairments() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node a
node b
link a:eth0 -- b:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  mtu 1400
  -> delay 5ms
  <- delay 100ms jitter 20ms loss 0.5% rate 10mbit corrupt 0.01% reorder 1%
  rate egress 100mbit ingress 50mbit burst 10mbit
}
"#,
        );
        assert!(rendered.contains("burst 10mbit"), "{rendered}");
        assert!(rendered.contains("mtu 1400"), "{rendered}");
        assert!(
            rendered.contains(
                "impair b:eth0 delay 100ms jitter 20ms loss 0.5% rate 10mbit corrupt 0.01% reorder 1%"
            ),
            "{rendered}"
        );
        assert_eq!(
            topo2.rate_limits["a:eth0"],
            RateLimit {
                egress: Some("100mbit".into()),
                ingress: Some("50mbit".into()),
                burst: Some("10mbit".into()),
            }
        );
    }

    #[test]
    fn test_render_assertions_all_kinds() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t" { dns hosts }

node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

validate {
  reach a b
  no-reach b a
  tcp-connect a b 8080 timeout 3s retries 10 interval 1s
  latency-under a b 10ms samples 5
  route-has a default via 10.0.0.2 dev eth0
  route-has a 10.0.0.0/24
  dns-resolves a "b" "10.0.0.2"
}

scenario "s" {
  at 0s {
    log "start"
    exec a "ping" "-c" "1" "b"
    validate {
      tcp-connect a b 80
      latency-under a b 10ms
      route-has a default
      dns-resolves a b 10.0.0.2
    }
  }
  at 1500ms {
    down a:eth0
    clear a:eth0
    up a:eth0
  }
}
"#,
        );
        assert!(
            rendered.contains("tcp-connect a b 8080 timeout 3s retries 10 interval 1s"),
            "{rendered}"
        );
        assert!(rendered.contains("at 1500ms {"), "{rendered}");
        assert!(
            rendered.contains("      latency-under a b 10ms\n"),
            "{rendered}"
        );
        assert_eq!(topo2.assertions.len(), 7);
        let ScenarioAction::Validate(inner) = &topo2.scenarios[0].steps[0].actions[2] else {
            panic!("expected validate action");
        };
        assert_eq!(inner.len(), 4);
    }

    #[test]
    fn test_render_benchmarks() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node a
node b

benchmark "perf" {
  ping a b {
    count 10
    assert avg below 50ms
    assert loss below 5%
  }
  iperf3 a b {
    duration 10s
    streams 4
    udp
    assert bandwidth above 900mbit
  }
}
"#,
        );
        assert!(
            rendered.contains("assert bandwidth above 900mbit"),
            "{rendered}"
        );
        assert!(rendered.contains("    udp\n"), "{rendered}");
        assert_eq!(topo2.benchmarks[0].tests.len(), 2);
    }

    #[test]
    fn test_render_benchmark_gte_lte_spelling() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        topo.benchmarks.push(crate::types::Benchmark {
            name: "b".into(),
            tests: vec![BenchmarkTest::Ping {
                from: "a".into(),
                to: "b".into(),
                count: None,
                assertions: vec![
                    BenchmarkAssertion {
                        metric: "avg".into(),
                        op: CompareOp::Gte,
                        value: "1ms".into(),
                    },
                    BenchmarkAssertion {
                        metric: "avg".into(),
                        op: CompareOp::Lte,
                        value: "9ms".into(),
                    },
                ],
            }],
        });
        let rendered = try_render(&topo).unwrap();
        // Distinct from `above`/`below` — the spellings lower.rs maps to Gte/Lte.
        assert!(rendered.contains("assert avg >= 1ms"), "{rendered}");
        assert!(rendered.contains("assert avg <= 9ms"), "{rendered}");
    }

    #[test]
    fn test_render_nat_all_actions() {
        let (_, rendered, _) = roundtrip(
            r#"lab "t"

node fw {
  nat {
    masquerade src 10.0.0.0/16
    masquerade
    dnat dst 203.0.113.0/24 to 10.0.1.2:8080
    dnat to 10.0.1.3
    snat src 10.0.0.0/8 to 203.0.113.1
  }
}
"#,
        );
        assert!(
            rendered.contains("dnat dst 203.0.113.0/24 to 10.0.1.2:8080"),
            "{rendered}"
        );
        assert!(
            rendered.contains("snat src 10.0.0.0/8 to 203.0.113.1"),
            "{rendered}"
        );
    }

    #[test]
    fn test_render_nat_translate() {
        let mut topo = Topology::default();
        topo.lab.name = "t".into();
        let mut fw = Node::default();
        fw.nat = Some(NatConfig {
            rules: vec![NatRule {
                action: NatAction::Translate,
                src: Some("144.0.0.0/8".into()),
                dst: None,
                target: Some("172.100.0.0/16".into()),
                target_port: None,
            }],
        });
        topo.nodes.insert("fw".into(), fw);
        let rendered = render(&topo);
        assert!(
            rendered.contains("translate 144.0.0.0/8 to 172.100.0.0/16"),
            "translate should render: {rendered}"
        );
    }

    #[test]
    fn test_render_macvlan_ipvlan_wifi() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

node gw {
  macvlan eth0 parent "enp3s0" mode bridge { 192.168.1.100/24 }
  ipvlan eth1 parent "enp3s0" mode l3 { 192.168.1.101/24 }
  wifi wlan0 mode ap {
    ssid "labnet"
    channel 6
    wpa2 "testpassword"
    10.0.0.1/24
  }
  wifi wlan1 mode mesh { mesh-id "m" }
}
"#,
        );
        assert!(
            rendered.contains("ipvlan eth1 parent enp3s0 mode l3 {"),
            "{rendered}"
        );
        assert_eq!(
            topo2.nodes["gw"].ipvlans[0].addresses,
            vec!["192.168.1.101/24"]
        );
    }

    #[test]
    fn test_render_quote_in_string_is_error() {
        let mut topo = Topology::default();
        topo.lab.name = "has \"quote\"".into();
        let err = try_render(&topo).unwrap_err();
        assert!(err.to_string().contains("double quote"), "{err}");
    }

    #[test]
    fn test_render_ipv6_link() {
        let (_, rendered, _) = roundtrip(
            r#"lab "t"

node r { forward ipv6 }
node h { route default via fd00::1 }
link r:eth0 -- h:eth0 { fd00::1/64 -- fd00::2/64 }
"#,
        );
        assert!(rendered.contains("fd00::1/64 -- fd00::2/64"), "{rendered}");
    }

    #[test]
    fn test_render_pattern_names_with_dots() {
        let (_, rendered, topo2) = roundtrip(
            r#"lab "t"

pool links 10.0.0.0/24 /30
mesh cluster {
  node [n1, n2, n3]
  pool links
}
"#,
        );
        assert!(rendered.contains("node cluster.n1"), "{rendered}");
        assert_eq!(topo2.links.len(), 3);
    }

    #[test]
    fn test_render_output_is_sorted_and_deterministic() {
        let topo = parser::parse(
            r#"lab "t"
node zeta
node alpha
node mid
"#,
        )
        .unwrap();
        let a = try_render(&topo).unwrap();
        let b = try_render(&topo).unwrap();
        assert_eq!(a, b);
        let za = a.find("node zeta").unwrap();
        let al = a.find("node alpha").unwrap();
        assert!(al < za, "{a}");
    }

    #[test]
    fn test_lexical_helpers() {
        assert!(is_duration("10ms"));
        assert!(is_duration("0.5s"));
        assert!(is_duration("+2s"));
        assert!(!is_duration("10"));
        assert!(is_rate("100mbit"));
        assert!(is_rate("512m"));
        assert!(!is_rate("1.5mbit"));
        assert!(is_percent("0.1%"));
        assert!(is_ipv4("10.0.0.1"));
        assert!(is_ipv4("10.0.0.0/24"));
        assert!(!is_ipv4("10.0.0"));
        assert!(is_ipv6("fd00::1/64"));
        assert!(is_ipv6("::1"));
        assert!(!is_ipv6("2001:db8:0:0:0:0:0:1"));
        assert_eq!(lit_value("hello world").unwrap(), "\"hello world\"");
        assert_eq!(lit_value("0.5").unwrap(), "0.5");
        assert_eq!(lit_value("rate").unwrap(), "\"rate\"");
        assert!(nll_ident("rate").is_ok());
        assert!(nll_ident("node").is_err());
        assert!(nll_ident("dc1.r1").is_err());
        assert!(nll_name("dc1.r1").is_ok());
        assert!(nll_name("*-black").is_ok());
        assert!(nll_name("1abc").is_err());
        assert!(nll_name("node").is_err());
        assert!(nll_name("has space").is_err());
        assert!(nll_addr("auto/24").is_ok());
        assert!(nll_addr("gateway").is_err());
    }
}
