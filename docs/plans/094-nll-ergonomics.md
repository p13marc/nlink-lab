# Plan 094: NLL Syntax Ergonomics

**Priority:** High
**Effort:** 3-4 days
**Depends on:** None (can be done in parallel with 093)
**Target:** `crates/nlink-lab/src/parser/nll/`, `bins/lab/`

## Summary

Reduce NLL boilerplate through subnet auto-assignment, list iteration in
for-loops, default blocks, for-expressions, lab metadata, and a `render`
command. These changes target the most repetitive patterns in real topologies.

## Breaking Changes

**Subnet auto-assignment** changes the meaning of a single CIDR on a link.
Currently, a link body requires exactly two CIDRs separated by `--`:

```nll
link a:e0 -- b:e0 { 10.0.0.1/24 -- 10.0.0.2/24 }
```

After this change, a single CIDR is treated as a subnet with auto-assigned
endpoints. No existing topology uses a single CIDR (the parser requires the
`--` separator), so this is backward-compatible in practice.

## Phase 1: Subnet Auto-Assignment (day 1)

### Problem

Every point-to-point link requires manually specifying both endpoint addresses.
For /30 subnets (standard in datacenter fabrics), the second address is always
derivable from the first.

```nll
# Current — 4 fabric links, each with redundant addressing
link s1:e1 -- l1:e1 { 10.0.11.1/30 -- 10.0.11.2/30  mtu 9000 }
link s1:e2 -- l2:e1 { 10.0.12.1/30 -- 10.0.12.2/30  mtu 9000 }
link s2:e1 -- l1:e2 { 10.0.21.1/30 -- 10.0.21.2/30  mtu 9000 }
link s2:e2 -- l2:e2 { 10.0.22.1/30 -- 10.0.22.2/30  mtu 9000 }
```

### Change

Allow a single network-address CIDR on a link. Auto-assign `.1` to left
endpoint, `.2` to right endpoint.

```nll
# After — subnet auto-assignment
link s1:e1 -- l1:e1 { 10.0.11.0/30  mtu 9000 }
link s1:e2 -- l2:e1 { 10.0.12.0/30  mtu 9000 }
```

### Address assignment rules

| Prefix | Left address | Right address | RFC |
|--------|-------------|---------------|-----|
| /30 | network + 1 | network + 2 | Standard (RFC 1878) |
| /31 | network + 0 | network + 1 | RFC 3021 (point-to-point) |
| /24 or larger | network + 1 | network + 2 | Common convention |
| /32 | Error | Error | Can't split |

### Implementation

**Parser** (`parser.rs`, `parse_link()` around line 1020-1028):

Currently the parser expects two CIDRs with `DashDash` between them. Change to:
1. Parse first CIDR
2. Check if next token is `DashDash`
   - **Yes**: parse second CIDR (existing behavior)
   - **No**: store single CIDR as `subnet` field

**AST** (`ast.rs`, `LinkDef`):

Add a `subnet` alternative to the existing address fields:

```rust
pub struct LinkDef {
    // ... existing fields ...
    pub left_addr: Option<String>,
    pub right_addr: Option<String>,
    pub subnet: Option<String>,  // NEW: auto-assign from this subnet
}
```

**Lowering** (`lower.rs`):

In `lower_link()`, when `subnet` is set, compute left/right addresses:

```rust
fn split_subnet(cidr: &str) -> Result<(String, String)> {
    let (ip, prefix) = parse_cidr(cidr)?;
    match prefix {
        32 => Err("cannot auto-assign /32 subnet"),
        31 => Ok((format!("{}/{prefix}", ip), format!("{}/{prefix}", next_ip(ip)))),
        _ => Ok((format!("{}/{prefix}", next_ip(ip)), format!("{}/{prefix}", next_ip(next_ip(ip))))),
    }
}
```

### Files

- `ast.rs` — add `subnet: Option<String>` to `LinkDef`
- `parser.rs` — modify `parse_link()` to handle single CIDR
- `lower.rs` — add `split_subnet()`, apply in `lower_link()`
- `helpers.rs` — add `next_ip()` helper (increment IPv4 by 1)

### Tasks

- [ ] Add `subnet` field to `LinkDef` AST
- [ ] Modify `parse_link()` to detect single vs. paired CIDR
- [ ] Implement `split_subnet()` in lowering
- [ ] Implement `next_ip()` helper for IPv4 increment
- [ ] Handle IPv6 subnets (same logic, 128-bit arithmetic)
- [ ] Add tests: /30, /31, /24, /32 (error), IPv6, mixed with MTU/impairments
- [ ] Add validation: error if subnet is network address with /32

## Phase 2: List Iteration in For-Loops (day 1-2)

### Problem

For-loops only support integer ranges. Common patterns need named iteration:

```nll
# Current — forced to use integer indices
for i in 1..3 {
    node server${i}
}

# Desired — iterate over meaningful names
for role in [web, api, db] {
    node ${role} { route default via 10.0.0.1 }
}
```

### Change

Extend `for` syntax to accept bracketed identifier/string lists:

```
for_stmt = "for" IDENT "in" ( range | list ) block
range    = INT ".." INT
list     = "[" value ("," value)* "]"
value    = IDENT | STRING | INT | CIDR | DURATION | RATE | INTERP
```

### Implementation

**AST** (`ast.rs`):

```rust
pub enum ForRange {
    IntRange { start: i64, end: i64 },
    List(Vec<String>),
}

pub struct ForLoop {
    pub var: String,
    pub range: ForRange,
    pub body: Vec<Statement>,
}
```

**Parser** (`parser.rs`, `parse_for()` at line 1289):

After parsing `for var in`, check next token:
- `Token::Int` → parse integer range (existing behavior)
- `Token::LBracket` → parse list: consume values until `]`

```rust
let range = if check(tokens, *pos, &Token::LBracket) {
    eat(tokens, pos, &Token::LBracket)?;
    let mut items = Vec::new();
    loop {
        if check(tokens, *pos, &Token::RBracket) { break; }
        items.push(parse_value(tokens, pos)?);
        if !eat_opt(tokens, pos, &Token::Comma) { break; }
    }
    expect(tokens, pos, &Token::RBracket)?;
    ForRange::List(items)
} else {
    let start = expect_int(tokens, pos)?;
    expect(tokens, pos, &Token::DotDot)?;
    let end = expect_int(tokens, pos)?;
    ForRange::IntRange { start, end }
};
```

**Lowering** (`lower.rs`, `expand_for()`):

```rust
fn expand_for(&self, for_loop: &ast::ForLoop, vars: &mut HashMap<String, String>) -> Result<Vec<Statement>> {
    let values: Vec<String> = match &for_loop.range {
        ForRange::IntRange { start, end } => (*start..=*end).map(|i| i.to_string()).collect(),
        ForRange::List(items) => items.clone(),
    };
    let mut result = Vec::new();
    for value in &values {
        vars.insert(for_loop.var.clone(), value.clone());
        // ... expand body (existing logic) ...
    }
    vars.remove(&for_loop.var);
    Ok(result)
}
```

### Files

- `ast.rs` — change `ForLoop` to use `ForRange` enum
- `parser.rs` — extend `parse_for()` with list parsing
- `lower.rs` — update `expand_for()` to handle both range types

### Tasks

- [ ] Define `ForRange` enum in AST
- [ ] Update `ForLoop` struct (breaking: `start`/`end` fields → `range` field)
- [ ] Extend `parse_for()` with `[...]` list parsing
- [ ] Update `expand_for()` for list iteration
- [ ] Handle interpolation inside list values: `[host${i}, ...]`
- [ ] Add tests: simple list, single item, mixed types, empty list (error)
- [ ] Update `interpolate_statement()` to handle the new ForLoop structure

## Phase 3: Default Blocks (day 2)

### Problem

Common properties like MTU, impairments, or rate limits must be repeated on
every link or node. No way to set defaults.

```nll
# Current — mtu 9000 on every fabric link
link s1:e1 -- l1:e1 { 10.0.1.0/30 mtu 9000 }
link s1:e2 -- l2:e1 { 10.0.2.0/30 mtu 9000 }
link s2:e1 -- l1:e2 { 10.0.3.0/30 mtu 9000 }
link s2:e2 -- l2:e2 { 10.0.4.0/30 mtu 9000 }
```

### Change

Add a `defaults` block that applies properties to all subsequent links or
impairments. Per-link values override defaults.

```nll
defaults link { mtu 9000 }
defaults impair { delay 5ms }

link s1:e1 -- l1:e1 { 10.0.1.0/30 }            # gets mtu 9000
link s1:e2 -- l2:e1 { 10.0.2.0/30 mtu 1500 }    # overrides to 1500
```

### Supported defaults

| Target | Properties |
|--------|-----------|
| `defaults link` | `mtu` |
| `defaults impair` | `delay`, `jitter`, `loss`, `rate`, `corrupt`, `reorder` |
| `defaults rate` | `egress`, `ingress`, `burst` |

### Implementation

**Lexer**: Add `Defaults` token.

**AST** (`ast.rs`):

```rust
pub enum DefaultsKind { Link, Impair, Rate }

pub struct DefaultsDef {
    pub kind: DefaultsKind,
    pub link_mtu: Option<u32>,
    pub impair: Option<ImpairProps>,
    pub rate: Option<RateProps>,
}

// Add to Statement enum:
Defaults(DefaultsDef),
```

**Parser**: Parse `defaults link { ... }` / `defaults impair { ... }` as new
top-level statements.

**Lowering**: Track active defaults. When lowering links/impairments, merge
defaults with per-statement values (per-statement wins on conflict).

### Files

- `lexer.rs` — add `Defaults` token
- `ast.rs` — add `DefaultsDef` and `DefaultsKind`
- `parser.rs` — add `parse_defaults()`, register in `parse_statement()`
- `lower.rs` — track defaults in `LowerCtx`, apply during lowering

### Tasks

- [ ] Add `Defaults` token to lexer
- [ ] Add `DefaultsDef` to AST
- [ ] Implement `parse_defaults()`
- [ ] Implement defaults merging in lowering
- [ ] Add tests: link MTU default, impair defaults, override, multiple defaults blocks

## Phase 4: For-Expressions (day 2-3)

### Problem

Network member lists and other collection fields require manual enumeration.
Can't use loops to generate list values.

```nll
# Current — must list every member manually
network mgmt {
    members [r1:mgmt0, r2:mgmt0, r3:mgmt0, r4:mgmt0]
}
```

### Change

Allow `for` as a value-producing expression inside `[...]` brackets:

```nll
network mgmt {
    members [for i in 1..4 : r${i}:mgmt0]
}
```

### Grammar

```
list_expr = "[" (for_expr | value_list) "]"
for_expr  = "for" IDENT "in" range ":" template
value_list = value ("," value)*
```

### Implementation

**Parser**: In `parse_members()` and similar list-parsing functions, check if
the first token after `[` is `For`. If so, parse as for-expression:

```rust
fn parse_list_expr(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<String>> {
    expect(tokens, pos, &Token::LBracket)?;
    if check(tokens, *pos, &Token::For) {
        // For-expression: [for var in start..end : template]
        eat(tokens, pos, &Token::For)?;
        let var = expect_ident(tokens, pos)?;
        expect(tokens, pos, &Token::In)?;
        let start = expect_int(tokens, pos)?;
        expect(tokens, pos, &Token::DotDot)?;
        let end = expect_int(tokens, pos)?;
        expect(tokens, pos, &Token::Colon)?;
        let template = parse_name(tokens, pos)?;
        expect(tokens, pos, &Token::RBracket)?;
        // Expand at parse time or store as AST node for lowering
        Ok((start..=end).map(|i| template.replace(&format!("${{{var}}}"), &i.to_string())).collect())
    } else {
        // Regular comma-separated list (existing behavior)
        // ...
    }
}
```

**Scope**: Apply to `members [...]`, `peers [...]`, `interfaces [...]`,
`vlans [...]` — any bracketed list in NLL.

### Files

- `parser.rs` — add for-expression support in list parsing functions
- `lower.rs` — expand for-expressions during lowering (if deferred to AST)

### Tasks

- [ ] Implement for-expression parsing in `parse_list_expr()`
- [ ] Apply to all bracketed list contexts (members, peers, interfaces, vlans)
- [ ] Handle nested interpolation in template: `r${i}:eth${i}`
- [ ] Add tests: network members, wireguard peers, VRF interfaces

## Phase 5: Lab Metadata (day 3)

### Problem

Lab files have no structured metadata beyond name, description, and prefix.
Useful for topology sharing, catalogs, and tooling integration.

### Change

Extend the `lab` block with optional metadata fields:

```nll
lab "datacenter" {
    description "Production datacenter simulation"
    version "2.1.0"
    author "Network Team"
    tags [datacenter, spine-leaf, bgp]
}
```

### Implementation

**AST** (`ast.rs`, `LabDecl`):

```rust
pub struct LabDecl {
    pub name: String,
    pub description: Option<String>,
    pub prefix: Option<String>,
    pub runtime: Option<String>,
    pub version: Option<String>,     // NEW
    pub author: Option<String>,      // NEW
    pub tags: Vec<String>,           // NEW
}
```

**Lexer**: Add `Version`, `Author`, `Tags` tokens.

**Types** (`types.rs`, `LabConfig`): Add matching fields.

**Parser**: Extend `parse_lab_block()` to accept new fields.

### Files

- `lexer.rs` — add `Version`, `Author`, `Tags` tokens
- `ast.rs` — extend `LabDecl`
- `parser.rs` — extend `parse_lab_block()`
- `lower.rs` — map new fields in `lower_lab()`
- `types.rs` — extend `LabConfig`

### Tasks

- [ ] Add tokens
- [ ] Extend AST, types, parser, lowering
- [ ] Add tests
- [ ] Make all new fields optional (backward compatible)

## Phase 6: Render Command (day 3-4)

### Problem

When using loops, variables, and imports, it's hard to verify what the expanded
topology looks like. Users need a way to inspect the fully-resolved result.

### Change

Add `nlink-lab render <topology.nll>` CLI command that parses, expands, and
pretty-prints the resolved topology.

```bash
$ nlink-lab render examples/spine-leaf.nll
lab "spine-leaf"

node spine1 { forward ipv4; lo 10.255.0.1/32 }
node spine2 { forward ipv4; lo 10.255.0.2/32 }
node leaf1 { forward ipv4; route default via 10.0.11.1 }
# ... fully expanded, no loops or variables
```

### Implementation

1. Parse → lower to `Topology` (existing pipeline)
2. Serialize `Topology` back to NLL syntax (new `render` module)
3. Print to stdout

The renderer walks `Topology` fields and emits NLL syntax:

```rust
pub fn render(topology: &Topology) -> String {
    let mut out = String::new();
    // Lab block
    writeln!(out, "lab \"{}\"", topology.lab.name);
    // Profiles
    for (name, profile) in &topology.profiles { ... }
    // Nodes
    for (name, node) in &topology.nodes { ... }
    // Links
    for link in &topology.links { ... }
    // etc.
    out
}
```

### Files

- `crates/nlink-lab/src/render.rs` — new module
- `crates/nlink-lab/src/lib.rs` — add `pub mod render;`
- `bins/lab/src/main.rs` — add `Render` subcommand

### Tasks

- [ ] Implement `render()` function that produces valid NLL from a `Topology`
- [ ] Add `Render` CLI subcommand
- [ ] Handle all topology features: profiles, nodes, links, networks, impairments,
      rate limits, firewall, VRF, WireGuard, VXLAN, containers
- [ ] Add tests: render(parse(nll)) should produce equivalent topology
- [ ] Support `--json` flag for JSON output alternative

## Progress

### Phase 1: Subnet Auto-Assignment
- [ ] AST field
- [ ] Parser change
- [ ] Lowering + split_subnet()
- [ ] Tests

### Phase 2: List Iteration
- [ ] ForRange enum
- [ ] Parser extension
- [ ] Lowering update
- [ ] Tests

### Phase 3: Default Blocks
- [ ] Token + AST
- [ ] Parser
- [ ] Defaults merging
- [ ] Tests

### Phase 4: For-Expressions
- [ ] List expression parsing
- [ ] Apply to all list contexts
- [ ] Tests

### Phase 5: Lab Metadata
- [ ] Tokens + AST + types
- [ ] Parser + lowering
- [ ] Tests

### Phase 6: Render Command
- [ ] render module
- [ ] CLI subcommand
- [ ] Tests
