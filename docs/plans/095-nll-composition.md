# Plan 095: NLL Modules & Composition

**Priority:** Medium
**Effort:** 4-5 days
**Depends on:** Plan 093 (expressions — conditionals needed for cross-references)
**Target:** `crates/nlink-lab/src/parser/nll/`, `crates/nlink-lab/src/`

## Summary

Transform NLL's static import system into a proper module system with
parametric imports, multi-profile inheritance, cross-references between nodes,
and firewall source/destination matching. These features remove the deepest
sources of duplication and enable true topology composition.

## Breaking Changes

### ForLoop struct change (from plan 094)

Plan 094 changes `ForLoop` from `{ var, start, end, body }` to
`{ var, range: ForRange, body }`. Plan 095 depends on this.

### Profile inheritance syntax

Currently `node r1 : router` applies one profile. Multi-profile syntax
changes this to `node r1 : router, monitored`. The parser currently
treats everything after `:` as a single profile name. This is a
**backward-compatible extension** — single profile still works.

### Import syntax

Currently `import "file.nll" as alias`. Parametric form adds optional
parenthesized parameters: `import "file.nll" as alias(key=value)`.
The bare form still works — **backward compatible**.

## Phase 1: Firewall `src`/`dst` Matching (day 1)

### Problem

The firewall match expression parser (`parse_match_expr()` at parser.rs:694-785)
only supports `ct`, `tcp`/`udp` `dport`/`sport`, `icmp`, `icmpv6`, and `mark`.
Can't filter by source or destination IP.

### Change

Add `src` and `dst` keywords that accept CIDR or IP address arguments.

```nll
node server {
    firewall policy drop {
        accept tcp dport 443
        accept tcp dport 80 src 10.0.0.0/8
        drop src 192.168.0.0/16
        accept dst 10.0.0.1
    }
}
```

### Generated nftables expressions

| NLL | nftables |
|-----|---------|
| `src 10.0.0.0/8` | `ip saddr 10.0.0.0/8` |
| `dst 10.0.0.1` | `ip daddr 10.0.0.1` |
| `src fd00::/64` | `ip6 saddr fd00::/64` |
| `tcp dport 80 src 10.0.0.0/8` | `ip saddr 10.0.0.0/8 tcp dport 80` |

### Implementation

**Lexer** (`lexer.rs`): Add `Src` and `Dst` tokens.

```rust
#[token("src")]
Src,
#[token("dst")]
Dst,
```

**Parser** (`parser.rs`, `parse_match_expr()`):

Add `src`/`dst` handling after existing match types. These can appear as
standalone matches or combined with protocol matches:

```rust
// After existing match types, check for src/dst
if eat_opt(tokens, pos, &Token::Src) {
    let addr = parse_cidr_or_name(tokens, pos)?;
    let family = if addr.contains(':') { "ip6" } else { "ip" };
    expr.push_str(&format!("{family} saddr {addr} "));
}
if eat_opt(tokens, pos, &Token::Dst) {
    let addr = parse_cidr_or_name(tokens, pos)?;
    let family = if addr.contains(':') { "ip6" } else { "ip" };
    expr.push_str(&format!("{family} daddr {addr} "));
}
```

**Key design decision**: `src`/`dst` can appear before OR after protocol
matches. The generated nftables expression puts `saddr`/`daddr` first
(nftables convention).

### Files

- `lexer.rs` — add `Src`, `Dst` tokens
- `parser.rs` — extend `parse_match_expr()`

### Tasks

- [ ] Add `Src`, `Dst` tokens to lexer
- [ ] Extend `parse_match_expr()` with src/dst handling
- [ ] Handle IPv4 and IPv6 addresses (auto-detect `ip` vs `ip6`)
- [ ] Support combined matches: `tcp dport 80 src 10.0.0.0/8`
- [ ] Add tests: src only, dst only, src+protocol, IPv6 src
- [ ] Update `token_as_ident()` so `src`/`dst` can still be used as identifiers

## Phase 2: Multi-Profile Inheritance (day 1-2)

### Problem

Nodes can inherit from only one profile. Common patterns need composition:

```nll
# Current — must duplicate sysctls across profiles
profile router { forward ipv4 }
profile monitored { sysctl "net.core.rmem_max" "16777216" }

node r1 : router     # can't also apply "monitored"
```

### Change

Allow comma-separated profile list after `:`.

```nll
node r1 : router, monitored   # merges both profiles
```

### Merge semantics

Profiles are applied left-to-right. Later profiles override earlier ones
for conflicting keys:

| Property | Merge rule |
|----------|-----------|
| `forward` | Last wins |
| `sysctl` | Per-key override (later wins) |
| `firewall` | Last wins (entire block) |
| `route` | Per-destination override |

### Implementation

**AST** (`ast.rs`, `NodeDef`):

```rust
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,  // CHANGED from profile: Option<String>
    // ...
}
```

**Parser** (`parser.rs`, `parse_node()`):

After parsing `:`, parse comma-separated profile names:

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

**Lowering** (`lower.rs`):

In `lower_node()`, merge all profiles in order:

```rust
fn merge_profiles(profiles: &[String], profile_defs: &HashMap<String, Profile>) -> Node {
    let mut merged = Node::default();
    for name in profiles {
        let profile = &profile_defs[name];
        // Merge sysctls (later wins per-key)
        for (k, v) in &profile.sysctls {
            merged.sysctls.insert(k.clone(), v.clone());
        }
        // Merge forward (last wins)
        if profile.forward.is_some() { merged.forward = profile.forward; }
        // Merge firewall (last wins)
        if profile.firewall.is_some() { merged.firewall = profile.firewall.clone(); }
        // Merge routes (per-dest override)
        for (dest, route) in &profile.routes {
            merged.routes.insert(dest.clone(), route.clone());
        }
    }
    merged
}
```

### Files

- `ast.rs` — change `NodeDef.profile: Option<String>` to `profiles: Vec<String>`
- `parser.rs` — parse comma-separated profiles
- `lower.rs` — implement profile merging
- Update `interpolate_statement()` for new `NodeDef` shape

### Tasks

- [ ] Change AST from single profile to profile list
- [ ] Update parser for comma-separated profiles
- [ ] Implement profile merge logic in lowering
- [ ] Update all references to `node.profile` → `node.profiles`
- [ ] Add tests: single profile (backward compat), two profiles, three profiles,
      sysctl override, firewall override, route merge
- [ ] Error on undefined profile names

## Phase 3: Parametric Imports (day 2-3)

### Problem

Imports are static — can't pass parameters. A reusable "spine-leaf" module
must hardcode spine/leaf counts.

```nll
# Current — imported file has fixed topology
import "spine-leaf.nll" as dc
```

### Change

Allow parameter passing to imported files:

```nll
# Module declares parameters with defaults
# spine-leaf.nll:
param spines default 2
param leaves default 4

for s in 1..${spines} { node spine${s} : router }
for l in 1..${leaves} { node leaf${l} : router }

# Consumer passes values:
import "spine-leaf.nll" as dc(spines=4, leaves=8)
```

### Implementation

**Lexer**: Add `Param` token.

**AST** (`ast.rs`):

```rust
pub struct ImportDef {
    pub path: String,
    pub alias: String,
    pub params: Vec<(String, String)>,  // NEW: key=value pairs
}

// New top-level statement for module parameter declaration
pub struct ParamDef {
    pub name: String,
    pub default: Option<String>,
}

// Add to Statement enum:
Param(ParamDef),
```

**Parser** (`parser.rs`):

Extend `parse_import()` to accept `(key=value, ...)` after alias:

```rust
let params = if eat_opt(tokens, pos, &Token::LParen) {
    let mut params = Vec::new();
    loop {
        if check(tokens, *pos, &Token::RParen) { break; }
        let key = expect_ident(tokens, pos)?;
        expect(tokens, pos, &Token::Eq)?;
        let value = parse_value(tokens, pos)?;
        params.push((key, value));
        if !eat_opt(tokens, pos, &Token::Comma) { break; }
    }
    expect(tokens, pos, &Token::RParen)?;
    params
} else {
    vec![]
};
```

Add `parse_param()` for the `param` statement:

```rust
fn parse_param(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ParamDef> {
    expect(tokens, pos, &Token::Param)?;
    let name = expect_ident(tokens, pos)?;
    let default = if eat_opt(tokens, pos, &Token::Default) {
        Some(parse_value(tokens, pos)?)
    } else {
        None
    };
    Ok(ast::ParamDef { name, default })
}
```

**Lowering** (`lower.rs`, `resolve_imports()`):

When lowering an imported file:
1. Collect `param` declarations from the imported AST
2. Match against import parameters (caller values override defaults)
3. Inject as variables before expanding the imported file

```rust
fn resolve_import_params(
    import_params: &[(String, String)],  // from ImportDef
    module_params: &[ParamDef],           // from imported file
    vars: &mut HashMap<String, String>,
) -> Result<()> {
    for param in module_params {
        let value = import_params.iter()
            .find(|(k, _)| k == &param.name)
            .map(|(_, v)| v.clone())
            .or(param.default.clone())
            .ok_or_else(|| Error::NllParse(format!(
                "required parameter '{}' not provided", param.name
            )))?;
        vars.insert(param.name.clone(), value);
    }
    Ok(())
}
```

**Lexer**: Need `LParen` and `RParen` tokens for `(` and `)`.

### Files

- `lexer.rs` — add `Param`, `LParen`, `RParen` tokens
- `ast.rs` — extend `ImportDef`, add `ParamDef`, add `Statement::Param`
- `parser.rs` — extend `parse_import()`, add `parse_param()`
- `lower.rs` — implement `resolve_import_params()` in `resolve_imports()`

### Tasks

- [ ] Add new tokens to lexer
- [ ] Extend ImportDef AST with params
- [ ] Add ParamDef AST and Statement::Param
- [ ] Parse parametric imports `(key=value, ...)`
- [ ] Parse `param name default value` declarations
- [ ] Implement parameter resolution in lowering
- [ ] Error on missing required parameters (no default)
- [ ] Add tests: basic parametric import, default values, missing required param,
      multiple params, nested parametric imports

## Phase 4: Cross-References Between Nodes (day 3-5)

### Problem

Addresses are duplicated between link definitions and route targets. Change one
and you must manually update all references.

```nll
# 10.0.0.1 appears in link AND in host's route — must stay in sync
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via 10.0.0.1 }
```

### Change

Allow referencing a node's interface address with `${node.iface}` syntax:

```nll
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via ${router.eth0} }
```

The reference `${router.eth0}` resolves to the IP address (without prefix)
assigned to `router:eth0` in any link definition.

### Design decisions

1. **Resolution timing**: Two-pass lowering. First pass collects all address
   assignments from links. Second pass resolves `${node.iface}` references.

2. **What gets resolved**: Only the IP address part (without prefix length).
   `${router.eth0}` → `10.0.0.1` (not `10.0.0.1/24`).

3. **Ambiguity**: If a node has multiple addresses on one interface (unlikely
   for links, possible for explicit interfaces), use the first one.

4. **Scope**: Cross-references work in routes, firewall rules, and impairment
   endpoints. Not in link addresses (would be circular).

### Implementation

**Lowering** (`lower.rs`):

This is the most complex change. The current lowering is single-pass.
Add a pre-pass that collects an address map:

```rust
struct AddressMap {
    // node_name -> iface_name -> ip_address (without prefix)
    map: HashMap<String, HashMap<String, String>>,
}

impl AddressMap {
    fn collect(topology: &Topology) -> Self {
        let mut map = HashMap::new();
        for link in &topology.links {
            if let Some(addresses) = &link.addresses {
                for (ep_str, addr) in link.endpoints.iter().zip(addresses.iter()) {
                    if let Some(ep) = EndpointRef::parse(ep_str) {
                        let ip = addr.split('/').next().unwrap_or(addr);
                        map.entry(ep.node.clone())
                            .or_insert_with(HashMap::new)
                            .insert(ep.iface.clone(), ip.to_string());
                    }
                }
            }
        }
        Self { map }
    }

    fn resolve(&self, node: &str, iface: &str) -> Option<&str> {
        self.map.get(node)?.get(iface).map(|s| s.as_str())
    }
}
```

Integrate into `interpolate()`:

```rust
fn interpolate(s: &str, vars: &HashMap<String, String>, addr_map: &AddressMap) -> String {
    // ... existing interpolation ...
    // For expressions containing '.', try address resolution first:
    if let Some(dot) = expr.find('.') {
        let node = &expr[..dot];
        let iface = &expr[dot + 1..];
        if let Some(addr) = addr_map.resolve(node, iface) {
            return addr.to_string();
        }
    }
    // Fall through to variable resolution
    eval_expr(expr, vars)
}
```

**Order of operations**:
1. Parse → AST
2. Expand loops and variables → flat AST
3. Lower to Topology (first pass — links get addresses)
4. Build AddressMap from Topology
5. Resolve cross-references in routes, firewall, etc. (second pass)

### Files

- `lower.rs` — add `AddressMap`, two-pass lowering
- Modify `interpolate()` to accept address map

### Tasks

- [ ] Implement `AddressMap` struct with `collect()` and `resolve()`
- [ ] Restructure lowering to two passes
- [ ] Integrate address resolution into interpolation
- [ ] Handle forward references (route references link that appears later in file)
- [ ] Error on unresolvable references with helpful message
- [ ] Add tests: basic cross-ref, forward reference, undefined reference error,
      interaction with loops, interaction with imports
- [ ] Document that cross-refs don't work inside link address fields (circular)

## Progress

### Phase 1: Firewall src/dst
- [ ] Tokens
- [ ] Parser extension
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
- [ ] Two-pass lowering
- [ ] Interpolation integration
- [ ] Tests
