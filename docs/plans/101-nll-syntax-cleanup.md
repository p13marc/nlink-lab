# Plan 101: NLL Syntax Cleanup — Breaking Changes

**Priority:** Low
**Effort:** 3-5 days
**Depends on:** Plan 099 (CI must be in place before breaking changes)
**Target:** `crates/nlink-lab/src/parser/nll/`

## Summary

The breaking syntax changes from the Deep Review: explicit `subnet` keyword
to eliminate auto-assign ambiguity, container block grouping for cleaner
NodeDef, streamlined firewall syntax, nested interpolation support,
pool exhaustion detection, and render output modes.

These are all quality-of-life improvements that make the language more
consistent. They should only be done after CI is in place (plan 099) so
regressions are caught.

---

## Phase 1: Explicit `subnet` Keyword (day 1)

### Breaking change

Currently a single CIDR on a link silently auto-assigns:
```nll
link a:e0 -- b:e0 { 10.0.0.0/30 }    # ambiguous: subnet or address?
```

Change to require explicit `subnet` keyword:
```nll
link a:e0 -- b:e0 { subnet 10.0.0.0/30 }    # clear intent
```

A bare CIDR without `--` or `subnet` becomes a parse error.

### Implementation

**Lexer**: `Subnet` token already exists? No — add it.

**Parser** (`parse_link()`): Change the single-CIDR branch:
```rust
// Before:
} else {
    link.subnet = Some(first_addr);  // silent auto-assign
}

// After:
} else {
    return Err(err(tokens, *pos, format!(
        "single address on link — use 'subnet {first_addr}' for auto-assign \
         or 'addr1 -- addr2' for explicit"
    )));
}
```

Add new match arm for `Token::Subnet`:
```rust
Some(Token::Subnet) => {
    *pos += 1;
    link.subnet = Some(parse_cidr_or_name(tokens, pos)?);
}
```

**Migration**: Update all examples that use single-CIDR syntax.

### Tasks

- [ ] Add `Subnet` token to lexer
- [ ] Add `subnet` to `token_as_ident()`
- [ ] Add `Token::Subnet` match arm in `parse_link()`
- [ ] Remove silent single-CIDR auto-assign
- [ ] Error message suggests correct syntax
- [ ] Update all 27 example files
- [ ] Update all tests
- [ ] Update documentation

## Phase 2: Container Block Syntax (day 2)

### Breaking change (internal refactor, syntax unchanged)

Restructure `NodeDef` from 26 flat fields to grouped:

```rust
// Before: 26 fields
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,
    pub image: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    // ... 20 more container fields ...
    pub props: Vec<NodeProp>,
}

// After: 4 fields
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,
    pub container: Option<ContainerDef>,
    pub props: Vec<NodeProp>,
}

pub struct ContainerDef {
    pub image: String,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub entrypoint: Option<String>,
    pub hostname: Option<String>,
    pub workdir: Option<String>,
    pub labels: Vec<String>,
    pub pull: Option<String>,
    pub container_exec: Vec<String>,
    pub healthcheck: Option<String>,
    pub healthcheck_interval: Option<String>,
    pub healthcheck_timeout: Option<String>,
    pub startup_delay: Option<String>,
    pub env_file: Option<String>,
    pub configs: Vec<(String, String)>,
    pub overlay: Option<String>,
    pub depends_on: Vec<String>,
}
```

NLL syntax doesn't change — this is purely an internal refactor. The
parser populates `ContainerDef` when `image` is present.

### Tasks

- [ ] Create `ContainerDef` struct in AST
- [ ] Restructure `NodeDef` to use `Option<ContainerDef>`
- [ ] Update parser to populate ContainerDef
- [ ] Update interpolate_node() for new structure
- [ ] Update lower_node() for new structure
- [ ] Update render_node() for new structure
- [ ] Update validator for new structure
- [ ] Run all tests

## Phase 3: Pool Exhaustion Detection (day 2)

### Problem

Pool allocation doesn't check if the pool is exhausted. A pool of
`10.0.0.0/24` with `/30` allocations can provide 64 subnets. The 65th
allocation silently wraps around or produces invalid addresses.

### Implementation

In `lower_link()` pool allocation:
```rust
if pool.next_offset >= pool.pool_size {
    return Err(Error::NllParse(format!(
        "pool '{}' exhausted — allocated {} subnets of /{} from {}",
        pool_name, pool.next_offset / subnet_size, pool.alloc_prefix,
        Ipv4Addr::from(pool.base)
    )));
}
```

### Tasks

- [ ] Add exhaustion check in pool allocation
- [ ] Use the currently-unused `pool_size` field
- [ ] Remove `#[allow(dead_code)]` from pool_size
- [ ] Tests: allocate until exhaustion, verify error

## Phase 4: Render Output Modes (day 3)

### Problem

`nlink-lab render` only outputs NLL and JSON. Users want DOT graph
and ASCII diagram output.

### Syntax

```bash
nlink-lab render topology.nll             # flat NLL (default)
nlink-lab render topology.nll --json      # JSON
nlink-lab render topology.nll --dot       # DOT graph (for graphviz)
nlink-lab render topology.nll --ascii     # ASCII art diagram
```

### Implementation

**DOT output**: Already partially exists as `topology_to_dot()` in
`main.rs` (used by `graph` command). Extract into a shared module
and reuse from `render`.

**ASCII output**: Simple text diagram showing nodes and links:
```
spine1 ─── leaf1 ─── server1
  │          │
spine2 ─── leaf2 ─── server2
```

Use a basic force-directed layout (similar to topoviewer's
`LayoutEngine` but text-based).

### Tasks

- [ ] Add `--dot` flag to render command
- [ ] Extract `topology_to_dot()` into shared module
- [ ] Add `--ascii` flag to render command
- [ ] Implement simple text-based topology diagram
- [ ] Tests for each output mode

## Phase 5: Nested Interpolation (day 3-4)

### Problem

`${leaf${i}.eth0}` doesn't work — the lexer grabs `${leaf${i}` as
one token. This prevents dynamic cross-references inside loops.

### Implementation

**Option A: Two-phase interpolation**

Run variable substitution (`${i}` → `1`) first, then cross-reference
resolution (`${leaf1.eth0}` → IP). This already works if the lexer
properly handles nested `${}`.

The fix is in the lexer regex. Change:
```rust
// Before: greedy, stops at first }
#[regex(r"\$\{[^}]+\}")]

// After: handle nested braces
#[regex(r"\$\{[^{}]*(\$\{[^}]*\}[^{}]*)*\}")]
```

Actually, the cleanest fix is in `interpolate()` (lower.rs) — run
interpolation recursively until no more `${` remain:

```rust
fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    // Iterate until no more substitutions happen
    for _ in 0..10 { // max depth to prevent infinite loops
        let prev = result.clone();
        result = interpolate_once(&result, vars);
        if result == prev { break; }
    }
    result
}
```

### Tasks

- [ ] Implement recursive interpolation in `interpolate()`
- [ ] Add depth limit to prevent infinite loops
- [ ] Test: `${leaf${i}.eth0}` resolves correctly
- [ ] Test: `${${var}}` resolves correctly
- [ ] Test: depth limit prevents infinite recursion

## Phase 6: Streamlined Firewall Syntax (day 4-5)

### Breaking change

```nll
# Before
firewall policy drop {
    accept ct established,related
    accept tcp dport 80 src 10.0.0.0/8
    accept icmp 8
}

# After
firewall drop {
    allow established
    allow tcp:80 from 10.0.0.0/8
    allow icmp
}
```

Changes:
- `policy` keyword dropped (redundant)
- `accept` → `allow`, `drop` → `deny` (more intuitive)
- `tcp dport 80` → `tcp:80` (standard port notation)
- `ct established,related` → `established` (shorthand)
- `src` → `from`, `dst` → `to` (more readable)
- `icmp` without type number (allow all ICMP)

This is the most disruptive change and should be done last.

### Tasks

- [ ] Add `Allow`, `Deny`, `From`, `To`, `Established` tokens
- [ ] Implement `tcp:80` port notation parsing
- [ ] Add `established` shorthand for `ct state established,related`
- [ ] Allow bare `icmp` without type
- [ ] Drop `policy` keyword requirement
- [ ] Update all examples and tests
- [ ] Update documentation and NLL spec

## Progress

### Phase 1: Explicit subnet
- [ ] Token + parser
- [ ] Remove silent auto-assign
- [ ] Update examples + tests

### Phase 2: Container block refactor
- [ ] ContainerDef struct
- [ ] Restructure NodeDef
- [ ] Update all consumers

### Phase 3: Pool exhaustion
- [x] Exhaustion check (allocate_from_pool helper)
- [x] Remove dead_code allow
- [x] Tests

### Phase 4: Render modes
- [x] --dot (reuses topology_to_dot)
- [ ] --ascii
- [ ] Tests

### Phase 5: Nested interpolation
- [x] Recursive interpolation (interpolate_once + multi-pass)
- [x] Depth limit (max 10 passes)
- [x] Tests (${leaf${i}}, adjacent ${base}${i})

### Phase 6: Firewall syntax
- [ ] New tokens
- [ ] Port notation
- [ ] Shorthands
- [ ] Update everything
