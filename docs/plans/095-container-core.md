# Plan 095: Container Core — Resources, Capabilities, Properties

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `crates/nlink-lab/src/`

## Summary

Upgrade container node support with resource limits, fine-grained capabilities
(replacing blanket `--privileged`), and 6 missing property plumbing items
(entrypoint, hostname, workdir, labels, pull policy, exec). All changes
follow the same pattern: add token → AST field → parser arm → types field →
container flag → validation rule.

## Breaking Changes

**Default capabilities**: Containers switch from `--privileged` to
`--cap-add=NET_ADMIN --cap-add=NET_RAW`. Nodes needing full privileges
must explicitly say `privileged`. This is the right default for network
labs — most containers only need network capabilities.

---

## Phase 1: Resource Limits (day 1)

### Problem

Can't constrain container CPU/memory. Large labs exhaust host resources.

### Syntax

```nll
node router image "frr:latest" {
    cpu 1.5
    memory 512m
}

# In a profile
profile heavy { cpu 2; memory 1g }
node big-router : heavy { image "frr:latest" }
```

### Implementation

**Lexer** (`lexer.rs`): `Cpu` token already exists? No — need to add it.
`Memory` would conflict with nothing.

```rust
#[token("cpu")]
Cpu,
// memory — reuse existing token or add new one
// Actually "memory" is not a token yet. Use as ident parsed in context.
```

Actually, `cpu` and `memory` are best parsed as identifiers in context
(inside node blocks) rather than keywords, to avoid reserving common words.
But since we have `token_as_ident()` for backward compat, adding tokens
is safe.

**AST** (`ast.rs`): Add to `NodeDef`:

```rust
pub struct NodeDef {
    // ... existing fields ...
    pub cpu: Option<String>,
    pub memory: Option<String>,
}
```

**Parser** (`parser.rs`, `parse_node()` block at line 457): Add match arms:

```rust
Some(Token::Cpu) => {
    *pos += 1;
    cpu = Some(parse_value(tokens, pos)?);
}
Some(Token::Memory) => {
    *pos += 1;
    memory = Some(parse_value(tokens, pos)?);
}
```

**Types** (`types.rs`, `Node`): Add fields:

```rust
pub cpu: Option<String>,
pub memory: Option<String>,
```

**Container** (`container.rs`, `CreateOpts`): Add fields, pass to CLI:

```rust
pub struct CreateOpts {
    pub cmd: Option<Vec<String>>,
    pub env: HashMap<String, String>,
    pub volumes: Vec<String>,
    pub cpu: Option<String>,      // NEW
    pub memory: Option<String>,   // NEW
}
```

In `create()`, add to args vector:

```rust
if let Some(cpu) = &opts.cpu {
    args.push("--cpus".to_string());
    args.push(cpu.clone());
}
if let Some(memory) = &opts.memory {
    args.push("--memory".to_string());
    args.push(memory.clone());
}
```

**Lowering** (`lower.rs`, `lower_node()`): Map fields.

**Deploy** (`deploy.rs`, CreateOpts construction): Pass fields.

**Validator**: Add `container-requires-image` check for cpu/memory.

### Tasks

- [ ] Add `Cpu`, `Memory` tokens to lexer + `token_as_ident()`
- [ ] Add `cpu`, `memory` to NodeDef AST
- [ ] Add parser match arms in `parse_node()` block
- [ ] Add fields to `Node` types
- [ ] Add fields to `CreateOpts`, pass `--cpus`/`--memory` in `create()`
- [ ] Update `lower_node()` to map fields
- [ ] Update `deploy()` CreateOpts construction
- [ ] Update `interpolate_node()` for new fields
- [ ] Add validation: cpu/memory require image
- [ ] Tests: parse, lower, validation

## Phase 2: Capabilities (day 1-2)

### Problem

All containers run with `--privileged`. This grants full host access.
Most lab nodes only need `NET_ADMIN` (ip/tc/nftables) and `NET_RAW` (ping).

### Syntax

```nll
# Default: NET_ADMIN + NET_RAW (no explicit config needed)
node host image "alpine"

# Add extra capabilities
node router image "frr:latest" {
    cap-add [NET_ADMIN, NET_RAW, SYS_PTRACE]
}

# Full privileges (opt-in)
node debugger image "ubuntu" {
    privileged
}
```

### Breaking change

`container.rs` `create()` currently hardcodes `--privileged` (line 137).
Change to:

```rust
if opts.privileged {
    args.push("--privileged".to_string());
} else {
    for cap in &opts.cap_add {
        args.push("--cap-add".to_string());
        args.push(cap.clone());
    }
    for cap in &opts.cap_drop {
        args.push("--cap-drop".to_string());
        args.push(cap.clone());
    }
    if opts.cap_add.is_empty() {
        // Default capabilities for network labs
        args.push("--cap-add=NET_ADMIN".to_string());
        args.push("--cap-add=NET_RAW".to_string());
    }
}
```

### Implementation

**Lexer**: Add `Privileged`, `CapAdd`, `CapDrop` tokens.

**AST** (`NodeDef`): Add fields:

```rust
pub privileged: bool,
pub cap_add: Vec<String>,
pub cap_drop: Vec<String>,
```

**Parser**: Add match arms in `parse_node()`:

```rust
Some(Token::Privileged) => { *pos += 1; privileged = true; }
Some(Token::CapAdd) => { *pos += 1; cap_add = parse_ident_list(tokens, pos)?; }
Some(Token::CapDrop) => { *pos += 1; cap_drop = parse_ident_list(tokens, pos)?; }
```

**Types** (`Node`): Add `privileged`, `cap_add`, `cap_drop`.

**Container** (`CreateOpts`): Add fields, implement logic above.

**Validator**: `privileged` requires image. `cap-add`/`cap-drop` require image.

### Tasks

- [ ] Add tokens to lexer + `token_as_ident()`
- [ ] Add fields to NodeDef AST
- [ ] Add parser match arms
- [ ] Add fields to Node types
- [ ] Update CreateOpts and `create()` — remove hardcoded `--privileged`
- [ ] Default to `NET_ADMIN + NET_RAW` when no caps specified
- [ ] Update lowering, interpolation, deploy
- [ ] Add validation rules
- [ ] Tests: default caps, explicit caps, privileged, cap-drop
- [ ] Update container.nll example

## Phase 3: Simple Property Additions (day 2)

Six properties that each follow the same pattern: token → AST → parser →
types → container flag. Batch them together.

### 3a. Entrypoint

```nll
node app image "python:3" {
    entrypoint "/bin/bash"
    cmd ["-c", "python app.py"]
}
```

Flag: `--entrypoint <value>`

### 3b. Hostname

```nll
node router image "frr" {
    hostname "core-router-01"
}
```

Flag: `--hostname <value>`. Default: node name.

### 3c. Working Directory

```nll
node app image "node:18" {
    workdir "/app"
}
```

Flag: `--workdir <value>`

### 3d. Labels

```nll
node router image "frr" {
    labels ["nlink.role=router", "nlink.tier=core"]
}
```

Flag: `--label <key=value>` for each.

### 3e. Pull Policy

```nll
node router image "frr:latest" {
    pull always
}
```

Values: `always` (force pull), `never` (fail if not local), `missing` (default).

Implementation: Control `ensure_image()` behavior — skip for `never`,
always pull for `always`, check-then-pull for `missing`.

### 3f. Post-Deploy Exec

```nll
node router image "frr:latest" {
    exec "apk add iperf3"
    exec "sysctl -w net.core.somaxconn=4096"
}
```

One-shot commands executed after container start, before link setup.
Distinct from `run` (which creates tracked background processes).

Implementation: Add `exec: Vec<Vec<String>>` to NodeDef/Node.
In deploy, after container creation, iterate and call `runtime.exec()`.

### Implementation (all 6)

**Lexer**: Add tokens: `Entrypoint`, `Hostname`, `Workdir`, `Labels`, `Pull`, `Exec`.
Note: `Exec` may conflict — currently no token for it. Check.

**AST** (`NodeDef`): Add fields for each.

**Types** (`Node`): Add matching fields.

**Container** (`CreateOpts`): Add fields, pass flags.

**Deploy**: Handle exec commands post-creation, pass other fields to CreateOpts.

### Tasks

- [ ] Add 6 tokens to lexer
- [ ] Add 6 fields to NodeDef AST
- [ ] Add 6 parser match arms in `parse_node()`
- [ ] Add 6 fields to Node types
- [ ] Update CreateOpts with entrypoint, hostname, workdir, labels
- [ ] Implement pull policy logic in `ensure_image()`/deploy
- [ ] Implement exec commands in deploy (post-creation)
- [ ] Update lowering, interpolation
- [ ] Add validation: all require image
- [ ] Tests for each property
- [ ] Create new container-advanced.nll example

## Progress

### Phase 1: Resource Limits
- [ ] Tokens + AST
- [ ] Parser
- [ ] Types + Container
- [ ] Lowering + Deploy
- [ ] Validation + Tests

### Phase 2: Capabilities
- [ ] Tokens + AST
- [ ] Parser
- [ ] Remove --privileged, add cap logic
- [ ] Default NET_ADMIN + NET_RAW
- [ ] Validation + Tests

### Phase 3: Simple Properties
- [ ] Entrypoint
- [ ] Hostname
- [ ] Workdir
- [ ] Labels
- [ ] Pull policy
- [ ] Post-deploy exec
- [ ] Tests + example
