# Plan 094: NLL v2 — Composition & Safety

**Priority:** Medium
**Effort:** 4-5 days
**Depends on:** Plan 093 (needs ForRange enum, expression engine for cross-refs)
**Target:** `crates/nlink-lab/src/parser/nll/`, `crates/nlink-lab/src/`

## Summary

Deep composition features that transform NLL from a flat topology language
into a proper module system. Firewall improvements, multi-profile inheritance,
parametric imports, and cross-references between nodes.

These features require more careful design than plan 093 — profile merge
semantics, two-pass lowering for cross-references, and parameter scoping for
imports all have subtle edge cases.

## Breaking Changes

**NodeDef.profile** changes from `Option<String>` to `profiles: Vec<String>`.
Internal AST change only — single-profile syntax `node r1 : router` still works.

**ImportDef** gains `params: Vec<(String, String)>`. Bare imports still work.

---

## Phase 1: Firewall `src`/`dst` Matching (day 1)

### Problem

`parse_match_expr()` at parser.rs:694-785 only supports `ct`, `tcp`/`udp`
`dport`/`sport`, `icmp`, `icmpv6`, `mark`. Can't filter by IP.

### Syntax

```nll
firewall policy drop {
    accept tcp dport 443
    accept tcp dport 80 src 10.0.0.0/8
    drop src 192.168.0.0/16
    accept dst 10.0.0.1
    accept src fd00::/64 tcp dport 22   # IPv6
}
```

### Generated nftables

| NLL | nftables |
|-----|---------|
| `src 10.0.0.0/8` | `ip saddr 10.0.0.0/8` |
| `dst 10.0.0.1` | `ip daddr 10.0.0.1` |
| `src fd00::/64` | `ip6 saddr fd00::/64` |

Auto-detect `ip` vs `ip6` from address format (`:` present → IPv6).

### Implementation

**Lexer**: Add tokens:

```rust
#[token("src")] Src,
#[token("dst")] Dst,
```

**Parser** (`parse_match_expr()`): After existing match types, check for
`src`/`dst` tokens. They can appear before or after protocol matches.
Scan for them in a loop alongside existing match parsing:

```rust
// Inside parse_match_expr(), extend the token dispatch:
Token::Src => {
    let addr = parse_cidr_or_name(tokens, pos)?;
    let family = if addr.contains(':') { "ip6" } else { "ip" };
    parts.push(format!("{family} saddr {addr}"));
}
Token::Dst => {
    let addr = parse_cidr_or_name(tokens, pos)?;
    let family = if addr.contains(':') { "ip6" } else { "ip" };
    parts.push(format!("{family} daddr {addr}"));
}
```

Assemble parts in nftables-canonical order: `saddr`, `daddr`, `protocol`,
`dport`/`sport`.

**Files**: `lexer.rs`, `parser.rs`

### Tasks

- [ ] Add `Src`, `Dst` tokens to lexer
- [ ] Add `src`/`dst` to `token_as_ident()` for backward compat as identifiers
- [ ] Extend `parse_match_expr()` with src/dst handling
- [ ] Auto-detect IPv4/IPv6 from address format
- [ ] Assemble nftables expression in canonical order
- [ ] Tests: src only, dst only, src+protocol, IPv6, combined src+dst

## Phase 2: Multi-Profile Inheritance (day 1-2)

### Problem

Nodes inherit from one profile. Can't compose orthogonal concerns:

```nll
profile router { forward ipv4 }
profile monitored { sysctl "net.core.rmem_max" "16777216" }
# Can't do: node r1 : router, monitored
```

### Syntax

```nll
node r1 : router, monitored
```

### Merge semantics (left-to-right, later wins on conflict)

| Property | Merge rule |
|----------|-----------|
| `forward` | Last wins |
| `sysctl` | Per-key: later profile overrides matching keys |
| `firewall` | Last wins (entire block replaced) |
| `route` | Per-destination: later profile overrides matching dests |

Node-level properties always override all profiles.

### Implementation

**AST** (`ast.rs`):

```rust
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,  // was: profile: Option<String>
    pub image: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub props: Vec<NodeProp>,
}
```

**Parser** (`parse_node()` at parser.rs:370): After `:`, parse comma-separated
profile names instead of single name:

```rust
let profiles = if eat_opt(tokens, pos, &Token::Colon) {
    let mut profiles = vec![parse_name(tokens, pos)?];
    while eat_opt(tokens, pos, &Token::Comma) {
        profiles.push(parse_name(tokens, pos)?);
    }
    profiles
} else {
    vec![]
};
```

**Lowering** (`lower.rs`): Replace single profile lookup with merge loop:

```rust
fn apply_profiles(
    node: &mut types::Node,
    profiles: &[String],
    profile_defs: &HashMap<String, types::Profile>,
) -> Result<()> {
    for name in profiles {
        let profile = profile_defs.get(name).ok_or_else(|| {
            Error::NllParse(format!("undefined profile '{name}'"))
        })?;
        // Merge sysctls (later wins per-key)
        for (k, v) in &profile.sysctls { node.sysctls.insert(k.clone(), v.clone()); }
        // Merge forward (last wins)
        if profile.forward_ipv4 { node.forward_ipv4 = true; }
        if profile.forward_ipv6 { node.forward_ipv6 = true; }
        // Merge firewall (last wins)
        if profile.firewall.is_some() { node.firewall = profile.firewall.clone(); }
        // Merge routes (per-dest override)
        for (dest, route) in &profile.routes { node.routes.insert(dest.clone(), route.clone()); }
    }
    Ok(())
}
```

Update `interpolate_statement()` to handle `Vec<String>` profiles instead of
`Option<String>`.

**Files**: `ast.rs`, `parser.rs`, `lower.rs`

### Tasks

- [ ] Change `NodeDef.profile` to `profiles: Vec<String>`
- [ ] Update `parse_node()` for comma-separated profiles
- [ ] Implement `apply_profiles()` with merge semantics
- [ ] Update `interpolate_statement()` for new NodeDef shape
- [ ] Update all pattern matches on `NodeDef` across the codebase
- [ ] Tests: single (backward compat), two profiles, three profiles, sysctl
      override, firewall override, route merge, undefined profile error

## Phase 3: Parametric Imports (day 2-3)

### Problem

Imports are static. Can't parameterize reusable modules:

```nll
import "spine-leaf.nll" as dc  # hardcoded spine/leaf counts
```

### Syntax

Module declares parameters:
```nll
# spine-leaf.nll
param spines default 2
param leaves default 4

for s in 1..${spines} { node spine${s} : router }
for l in 1..${leaves} { node leaf${l} : router }
```

Consumer passes values:
```nll
import "spine-leaf.nll" as dc(spines=4, leaves=8)
```

### Implementation

**Lexer**: Add tokens:

```rust
#[token("param")] Param,
#[token("(")] LParen,
#[token(")")] RParen,
```

**AST** (`ast.rs`):

```rust
pub struct ImportDef {
    pub path: String,
    pub alias: String,
    pub params: Vec<(String, String)>,  // NEW
}

pub struct ParamDef {
    pub name: String,
    pub default: Option<String>,
}

// Add to Statement enum:
Param(ParamDef),
```

**Parser**:

Extend `parse_import()` — after alias, optionally parse `(key=val, ...)`:

```rust
let params = if eat_opt(tokens, pos, &Token::LParen) {
    let mut params = Vec::new();
    loop {
        if check(tokens, *pos, &Token::RParen) { break; }
        let key = expect_ident(tokens, pos)?;
        expect(tokens, pos, &Token::Eq)?;
        let value = parse_value(tokens, pos)?;
        params.push((key, value));
        eat_opt(tokens, pos, &Token::Comma);
    }
    expect(tokens, pos, &Token::RParen)?;
    params
} else { vec![] };
```

Add `parse_param()`:

```rust
fn parse_param(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ParamDef> {
    expect(tokens, pos, &Token::Param)?;
    let name = expect_ident(tokens, pos)?;
    let default = if eat_opt(tokens, pos, &Token::Default) {
        Some(parse_value(tokens, pos)?)
    } else { None };
    Ok(ast::ParamDef { name, default })
}
```

**Lowering** (`lower.rs`, `resolve_imports()` at line 83-117):

When processing an imported file:
1. Collect `Param` statements from the imported AST
2. For each param, use caller's value or default or error if missing
3. Inject as variables before expanding the imported file's statements

```rust
fn resolve_import_params(
    caller_params: &[(String, String)],
    module_params: &[ast::ParamDef],
    vars: &mut HashMap<String, String>,
) -> Result<()> {
    for p in module_params {
        let val = caller_params.iter()
            .find(|(k, _)| k == &p.name)
            .map(|(_, v)| v.clone())
            .or_else(|| p.default.clone())
            .ok_or_else(|| Error::NllParse(
                format!("required param '{}' not provided", p.name)
            ))?;
        vars.insert(p.name.clone(), val);
    }
    // Warn on unexpected params from caller
    for (k, _) in caller_params {
        if !module_params.iter().any(|p| p.name == *k) {
            tracing::warn!("unknown param '{k}' passed to import");
        }
    }
    Ok(())
}
```

**Files**: `lexer.rs`, `ast.rs`, `parser.rs`, `lower.rs`

### Tasks

- [ ] Add `Param`, `LParen`, `RParen` tokens to lexer
- [ ] Add `ParamDef` to AST, extend `ImportDef` with `params`
- [ ] Add `Statement::Param` variant
- [ ] Parse parametric imports `(key=value, ...)`
- [ ] Parse `param name default value` declarations
- [ ] Implement `resolve_import_params()` in lowering
- [ ] Error on missing required params, warn on unknown params
- [ ] Tests: basic params, defaults, missing required, unknown param warning,
      nested parametric imports, param used in for-loop range

## Phase 4: Cross-References Between Nodes (day 3-5)

### Problem

Addresses duplicated between links and routes. Change one, must update all:

```nll
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via 10.0.0.1 }  # must match link address
```

### Syntax

```nll
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via ${router.eth0} }  # resolves to 10.0.0.1
```

`${node.iface}` resolves to the IP address (without prefix) assigned to that
endpoint in any link definition.

### Design decisions

1. **Scope**: Cross-refs work in routes, firewall rules, sysctl values,
   impairment endpoints. NOT in link address fields (would be circular).

2. **Resolution**: IP only (no prefix). `${router.eth0}` → `"10.0.0.1"`.

3. **Ambiguity**: If multiple addresses on same interface, use the first.

4. **Forward references**: Allowed. `node host { route ... via ${router.eth0} }`
   can appear before the link that defines router:eth0's address.

### Implementation

This requires **two-pass lowering**:

**Pass 1** (existing): Parse → expand loops/variables → lower links/addresses.

**Pass 2** (new): Build address map → resolve `${node.iface}` references in
nodes, impairments, etc.

```rust
struct AddressMap(HashMap<String, HashMap<String, String>>);
// node_name → iface_name → ip_address (without prefix)

impl AddressMap {
    fn collect(topology: &Topology) -> Self {
        let mut map = HashMap::new();
        for link in &topology.links {
            if let Some(addrs) = &link.addresses {
                for (ep_str, addr) in link.endpoints.iter().zip(addrs.iter()) {
                    if let Some(ep) = EndpointRef::parse(ep_str) {
                        let ip = addr.split('/').next().unwrap_or(addr);
                        map.entry(ep.node.clone())
                            .or_insert_with(HashMap::new)
                            .insert(ep.iface.clone(), ip.to_string());
                    }
                }
            }
        }
        // Also collect explicit interface addresses
        for (node_name, node) in &topology.nodes {
            for (iface_name, iface_cfg) in &node.interfaces {
                if let Some(addr) = iface_cfg.addresses.first() {
                    let ip = addr.split('/').next().unwrap_or(addr);
                    map.entry(node_name.clone())
                        .or_insert_with(HashMap::new)
                        .entry(iface_name.clone())
                        .or_insert_with(|| ip.to_string());
                }
            }
        }
        Self(map)
    }

    fn resolve(&self, node: &str, iface: &str) -> Option<&str> {
        self.0.get(node)?.get(iface).map(|s| s.as_str())
    }
}
```

**Integration**: After the first lowering pass produces a `Topology`, scan all
string fields for unresolved `${node.iface}` patterns and resolve them:

```rust
fn resolve_cross_refs(topology: &mut Topology) -> Result<()> {
    let addr_map = AddressMap::collect(topology);
    // Walk all nodes and resolve references in routes, firewall, etc.
    for (_, node) in &mut topology.nodes {
        for (_, route) in &mut node.routes {
            if let Some(via) = &mut route.via {
                *via = resolve_refs(via, &addr_map)?;
            }
        }
        if let Some(fw) = &mut node.firewall {
            for rule in &mut fw.rules {
                rule.match_expr = resolve_refs(&rule.match_expr, &addr_map)?;
            }
        }
    }
    Ok(())
}

fn resolve_refs(s: &str, addr_map: &AddressMap) -> Result<String> {
    // Find ${node.iface} patterns and resolve
    let mut result = s.to_string();
    let re = regex::Regex::new(r"\$\{(\w+)\.(\w+)\}").unwrap();
    for cap in re.captures_iter(s) {
        let node = &cap[1];
        let iface = &cap[2];
        let addr = addr_map.resolve(node, iface).ok_or_else(|| {
            Error::NllParse(format!("unresolved reference ${{{node}.{iface}}}"))
        })?;
        result = result.replace(&cap[0], addr);
    }
    Ok(result)
}
```

**Call site**: In the main `lower()` function, after building the Topology:

```rust
pub fn lower(ast: &ast::File) -> Result<Topology> {
    // ... existing lowering ...
    let mut topology = /* ... */;
    resolve_cross_refs(&mut topology)?;
    Ok(topology)
}
```

**Files**: `lower.rs` (add AddressMap, resolve_cross_refs, modify lower())

### Tasks

- [ ] Implement `AddressMap` with `collect()` and `resolve()`
- [ ] Implement `resolve_cross_refs()` post-processing pass
- [ ] Resolve in routes, firewall rules, sysctl values
- [ ] Handle forward references (link appears after route that references it)
- [ ] Error with helpful message on unresolved refs
- [ ] Don't resolve inside link address fields (prevent circular refs)
- [ ] Tests: basic cross-ref, forward reference, undefined ref error,
      interaction with loops, interaction with imports, interaction with
      subnet auto-assign (plan 093)

---

## Progress

### Phase 1: Firewall src/dst
- [ ] Tokens
- [ ] Parser
- [ ] IPv4/IPv6 detection
- [ ] Tests

### Phase 2: Multi-Profile Inheritance
- [ ] AST change
- [ ] Parser
- [ ] Merge logic
- [ ] Tests

### Phase 3: Parametric Imports
- [ ] Tokens + AST
- [ ] Parser
- [ ] Parameter resolution
- [ ] Tests

### Phase 4: Cross-References
- [ ] AddressMap
- [ ] resolve_cross_refs()
- [ ] Integration into lowering
- [ ] Tests
