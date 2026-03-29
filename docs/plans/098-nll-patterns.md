# Plan 098: NLL Patterns — Topology Generators, Subnet Pools, Assertions

**Priority:** Medium
**Effort:** 4-5 days
**Depends on:** Plan 097 (needs float literal for subnet pool sizes)
**Target:** `crates/nlink-lab/src/`

## Summary

Add three high-impact features from the Deep Review that eliminate the most
boilerplate and error-prone patterns: topology pattern generators, named
subnet pools, and reachability assertions. These are genuine innovations
that no competing tool offers.

---

## Phase 1: Named Subnet Pools (day 1-2)

### Problem

Manual address planning is the most error-prone part of topology design.
Users must track which subnets are used, avoid overlaps, and maintain
consistency across hundreds of links. Currently:

```nll
link s1:e1 -- l1:e1 { 10.0.1.0/30 }
link s1:e2 -- l2:e1 { 10.0.2.0/30 }
link s1:e3 -- l3:e1 { 10.0.3.0/30 }
# ... must manually track which /30s are used
```

### Syntax

```nll
pool fabric 10.0.0.0/16 /30
pool access 10.1.0.0/16 /24
pool mgmt 172.16.0.0/16 /24

link s1:e1 -- l1:e1 { pool fabric }   # auto: 10.0.0.0/30 → .1/.2
link s1:e2 -- l2:e1 { pool fabric }   # auto: 10.0.0.4/30 → .5/.6
link l1:e3 -- h1:e0 { pool access }   # auto: 10.1.0.0/24 → .1/.2
```

### Allocation rules

- Subnets are allocated sequentially from the pool's base address
- Each allocation increments by the pool's prefix size
- `/30`: 4 addresses per allocation (network + 2 hosts + broadcast)
- `/31`: 2 addresses per allocation (RFC 3021)
- `/24`: 256 addresses per allocation
- Endpoint addresses use the same split_subnet() logic (`.1` and `.2`)
- Duplicate detection: error if pool exhausted

### Implementation

**Lexer**: Add `Pool` token.

**AST**: Add `PoolDef` struct and `Statement::Pool`:

```rust
pub struct PoolDef {
    pub name: String,
    pub base: String,     // e.g., "10.0.0.0/16"
    pub prefix: u8,       // allocation size, e.g., 30
}
```

Add `pool: Option<String>` to `LinkDef` (alongside subnet/left_addr/right_addr).

**Parser**: Add `parse_pool()` as top-level statement. In `parse_link()`,
check for `pool <name>` in the link block.

**Lowering**: Track pool state in `LowerCtx`:

```rust
struct PoolState {
    base: Ipv4Addr,
    mask: u32,        // base network mask from the pool's CIDR
    prefix: u8,       // allocation prefix size
    next_offset: u32, // next allocation offset
}

struct LowerCtx {
    // ... existing fields ...
    pools: HashMap<String, PoolState>,
}
```

When lowering a link with `pool`, call:

```rust
fn allocate_subnet(pool: &mut PoolState) -> Result<[String; 2]> {
    let subnet_size = 1u32 << (32 - pool.prefix);
    let network = u32::from(pool.base) + pool.next_offset;
    pool.next_offset += subnet_size;

    // Check pool exhaustion
    let pool_size = 1u32 << (32 - pool_prefix_from_mask(pool.mask));
    if pool.next_offset > pool_size {
        return Err("pool exhausted");
    }

    split_subnet_from_u32(network, pool.prefix)
}
```

**Validator**: Add rules:
- `pool-name-unique`: pool names must be unique
- `pool-base-valid`: base must be a valid CIDR
- `pool-prefix-valid`: allocation prefix must be larger than base prefix

### Tasks

- [ ] Add `Pool` token to lexer
- [ ] Add `PoolDef` struct and `Statement::Pool` to AST
- [ ] Add `pool: Option<String>` to LinkDef AST
- [ ] Implement `parse_pool()` top-level parser
- [ ] Extend `parse_link()` to accept `pool <name>` in link block
- [ ] Add `PoolState` and allocation logic to `LowerCtx`
- [ ] Implement `allocate_subnet()` with exhaustion check
- [ ] Add validation rules
- [ ] Tests: basic allocation, sequential allocation, pool exhaustion, mixed pool + explicit
- [ ] Add `pool` support to render module
- [ ] Create example: `examples/subnet-pools.nll`

## Phase 2: Topology Patterns (day 2-4)

### Problem

Common topologies (spine-leaf, ring, full-mesh, star) require boilerplate
loops, address planning, and link enumeration. The datacenter-fabric.nll
example is 60+ lines for a standard Clos fabric.

### Syntax

```nll
# Spine-leaf fabric — generates everything
spine-leaf fabric {
    spines 4
    leaves 8
    fabric-pool 10.0.0.0/16 /31
    loopback-pool 10.255.0.0/16 /32
}

# Full mesh — generates all pairwise links
mesh cluster {
    nodes [n1, n2, n3, n4]
    pool 10.0.0.0/16 /30
}

# Ring — generates ring topology
ring backbone {
    count 6
    pool 10.0.0.0/16 /31
}

# Star — generates hub-and-spoke
star access {
    hub router
    spokes [h1, h2, h3, h4]
    pool 10.0.0.0/16 /24
}
```

### Implementation strategy

Topology patterns are **syntactic sugar** that expand to regular NLL
statements during lowering. They don't create new types in the Topology
struct — they generate nodes and links.

**AST**: Add `Statement::Pattern(PatternDef)`:

```rust
pub enum PatternKind {
    SpineLeaf,
    Mesh,
    Ring,
    Star,
}

pub struct PatternDef {
    pub kind: PatternKind,
    pub name: String,
    pub props: HashMap<String, String>,
    pub node_list: Vec<String>,  // for mesh/star
}
```

**Lowering**: Each pattern expands to nodes + links:

```rust
fn expand_spine_leaf(pattern: &PatternDef) -> Vec<Statement> {
    let spines = pattern.props["spines"].parse().unwrap();
    let leaves = pattern.props["leaves"].parse().unwrap();
    let mut stmts = Vec::new();

    // Generate spine nodes
    for s in 1..=spines {
        stmts.push(Statement::Node(NodeDef {
            name: format!("{}.spine{s}", pattern.name),
            profiles: vec!["router".into()],
            ..Default::default()
        }));
    }

    // Generate leaf nodes
    for l in 1..=leaves {
        stmts.push(Statement::Node(NodeDef {
            name: format!("{}.leaf{l}", pattern.name),
            profiles: vec!["router".into()],
            ..Default::default()
        }));
    }

    // Generate fabric links (every spine connects to every leaf)
    for s in 1..=spines {
        for l in 1..=leaves {
            stmts.push(Statement::Link(LinkDef {
                left_node: format!("{}.spine{s}", pattern.name),
                left_iface: format!("eth{l}"),
                right_node: format!("{}.leaf{l}", pattern.name),
                right_iface: format!("up{s}"),
                pool: pattern.props.get("fabric-pool").cloned(),
                ..Default::default()
            }));
        }
    }

    stmts
}
```

Similarly for `expand_mesh()`, `expand_ring()`, `expand_star()`.

### Pattern properties

**spine-leaf**:
| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `spines` | int | yes | Number of spine nodes |
| `leaves` | int | yes | Number of leaf nodes |
| `fabric-pool` | pool-ref | no | Pool for fabric links |
| `loopback-pool` | pool-ref | no | Pool for loopback addresses |
| `profile` | ident | no | Profile for generated nodes (default: "router") |

**mesh**:
| Property | Type | Required |
|----------|------|----------|
| `nodes` | list | yes |
| `pool` | pool-ref | no |

**ring**:
| Property | Type | Required |
|----------|------|----------|
| `count` | int | yes |
| `pool` | pool-ref | no |

**star**:
| Property | Type | Required |
|----------|------|----------|
| `hub` | ident | yes |
| `spokes` | list | yes |
| `pool` | pool-ref | no |

### Tasks

- [ ] Add `SpineLeaf`, `Mesh`, `Ring`, `Star` tokens to lexer
- [ ] Add `PatternDef` and `PatternKind` to AST
- [ ] Add `Statement::Pattern` variant
- [ ] Implement `parse_pattern()` in parser
- [ ] Implement `expand_spine_leaf()`, `expand_mesh()`, `expand_ring()`, `expand_star()`
- [ ] Integrate pool allocation for pattern links
- [ ] Add pattern support to render module
- [ ] Tests for each pattern type
- [ ] Create examples: `examples/pattern-spineleaf.nll`, `examples/pattern-mesh.nll`

## Phase 3: Reachability Assertions (day 4-5)

### Problem

Users must write integration tests to verify connectivity. Common patterns
like "host1 can ping host2" or "firewall blocks host3" are repeated across
every test. These could be declared in the topology itself.

### Syntax

```nll
validate {
    reach host1 host2           # host1 can ping host2 (ICMP)
    no-reach host1 host3        # firewall blocks host1 → host3
    reach host1 host2 tcp 80    # TCP connectivity on port 80
    dns host1 "example.com"     # DNS resolution works
}
```

### Deployment behavior

After deploy Step 17 (validation), run declared assertions:

```
17b. Run validate block assertions
  For each assertion:
    reach A B       → exec "ping -c1 -W2 <B_ip>" in A's namespace
    no-reach A B    → exec "ping -c1 -W2 <B_ip>" in A's namespace (expect failure)
    reach A B tcp P → exec "nc -z -w2 <B_ip> P" in A's namespace
```

Results are reported as pass/fail. On failure, deployment continues but
returns a warning (not an error — the topology is valid, just the
connectivity check failed).

### Implementation

**Lexer**: Add `Validate`, `Reach`, `NoReach` tokens.

**AST**: Add `ValidateDef` and `AssertionDef`:

```rust
pub struct ValidateDef {
    pub assertions: Vec<AssertionDef>,
}

pub enum AssertionDef {
    Reach { from: String, to: String, proto: Option<String>, port: Option<u16> },
    NoReach { from: String, to: String },
}
```

Add `Statement::Validate(ValidateDef)`.

**Types**: Add `assertions: Vec<Assertion>` to `Topology`.

**Deploy**: After Step 17, iterate assertions and exec ping/nc in source
node's namespace. Report results via `tracing::info!`/`tracing::warn!`.

**CLI**: Add `--skip-validate` flag to `deploy` command.

### Tasks

- [ ] Add tokens to lexer
- [ ] Add AST types
- [ ] Implement `parse_validate()` block parser
- [ ] Add `assertions` field to Topology types
- [ ] Implement assertion execution in deploy.rs (post Step 17)
- [ ] Use cross-reference address map to resolve target IPs
- [ ] Add `--skip-validate` flag to deploy command
- [ ] Tests: reach assertion parse, no-reach parse, assertion with protocol
- [ ] Create example: `examples/validated.nll`

## Progress

### Phase 1: Subnet Pools
- [ ] Token + AST
- [ ] Parser (pool statement + link pool reference)
- [ ] PoolState + allocation logic
- [ ] Validation rules
- [ ] Tests + example

### Phase 2: Topology Patterns
- [ ] Tokens + AST
- [ ] Parser
- [ ] expand_spine_leaf()
- [ ] expand_mesh()
- [ ] expand_ring()
- [ ] expand_star()
- [ ] Tests + examples

### Phase 3: Reachability Assertions
- [ ] Tokens + AST
- [ ] Parser
- [ ] Deploy integration
- [ ] CLI flag
- [ ] Tests + example
