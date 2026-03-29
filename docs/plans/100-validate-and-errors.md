# Plan 100: Validate Execution, Error Migration, Man Page

**Priority:** Medium
**Effort:** 2-3 days
**Depends on:** None
**Target:** `crates/nlink-lab/src/`, `bins/lab/`

## Summary

Wire the validate block to actually execute connectivity checks at deploy
time, migrate remaining `deploy_failed()` calls to specific error variants,
generate a man page from clap, and update the NLL spec.

---

## Phase 1: Validate Block Execution (day 1)

### Current state

The `validate { reach a b }` block is parsed to AST (`ValidateDef`,
`AssertionDef`) and lowered — but the assertions are not stored in the
`Topology` struct and not executed during deployment.

### Implementation

**Types** (`types.rs`): Add assertions field to `Topology`:

```rust
pub struct Topology {
    pub lab: LabConfig,
    pub nodes: HashMap<String, Node>,
    pub links: Vec<Link>,
    // ... existing fields ...
    pub assertions: Vec<Assertion>,
}

pub enum Assertion {
    Reach { from: String, to: String },
    NoReach { from: String, to: String },
}
```

**Lowering** (`lower.rs`): Store validate assertions:

```rust
ast::Statement::Validate(v) => {
    for a in &v.assertions {
        match a {
            ast::AssertionDef::Reach { from, to } => {
                topology.assertions.push(types::Assertion::Reach {
                    from: from.clone(), to: to.clone(),
                });
            }
            ast::AssertionDef::NoReach { from, to } => {
                topology.assertions.push(types::Assertion::NoReach {
                    from: from.clone(), to: to.clone(),
                });
            }
        }
    }
}
```

**Deploy** (`deploy.rs`): After Step 17 (existing validation checks),
add Step 17b:

```rust
// Step 17b: Run validate block assertions
if !topology.assertions.is_empty() {
    tracing::info!("step 17b: running validate assertions");
    let addr_map = build_address_map_from_topology(topology);
    for assertion in &topology.assertions {
        match assertion {
            Assertion::Reach { from, to } => {
                // Find target IP from address map
                let target_ip = find_any_ip(topology, to);
                if let Some(ip) = target_ip {
                    let result = running.exec(from, "ping", &["-c1", "-W2", &ip]);
                    match result {
                        Ok(out) if out.exit_code == 0 => {
                            tracing::info!("PASS: {from} can reach {to}");
                        }
                        _ => {
                            tracing::warn!("FAIL: {from} cannot reach {to}");
                        }
                    }
                }
            }
            Assertion::NoReach { from, to } => {
                let target_ip = find_any_ip(topology, to);
                if let Some(ip) = target_ip {
                    let result = running.exec(from, "ping", &["-c1", "-W2", &ip]);
                    match result {
                        Ok(out) if out.exit_code != 0 => {
                            tracing::info!("PASS: {from} cannot reach {to} (expected)");
                        }
                        _ => {
                            tracing::warn!("FAIL: {from} CAN reach {to} (should be blocked)");
                        }
                    }
                }
            }
        }
    }
}
```

**CLI**: Add `--skip-validate` flag to `deploy` command.

### Tasks

- [ ] Add `Assertion` enum and `assertions: Vec<Assertion>` to Topology types
- [ ] Store assertions during lowering
- [ ] Implement assertion execution in deploy.rs (post Step 17)
- [ ] Use address map to resolve target IPs for ping
- [ ] Add `--skip-validate` flag to deploy CLI
- [ ] Tests: parse + lower + verify assertions stored
- [ ] Integration test: deploy topology with validate block

## Phase 2: Error Migration (day 1-2)

### Current state

~30 `Error::deploy_failed(format!(...))` calls remain in deploy.rs.
Plan 092 added specific variants (`Namespace`, `NetlinkOp`, `Route`,
`Firewall`, `Container`) but only migrated namespace operations.

### Migration targets

Replace `Error::deploy_failed()` calls with specific variants by category:

| Category | Pattern | Target variant | ~Count |
|----------|---------|---------------|--------|
| `connection for 'X'` | Connection error | `NetlinkOp` | 10 |
| `failed to create veth` | Link creation | `NetlinkOp` | 3 |
| `failed to add address` | Address config | `NetlinkOp` | 3 |
| `failed to set link up` | Interface up | `NetlinkOp` | 3 |
| `failed to add route` | Route addition | `Route` | 2 |
| `nft` | Firewall | `Firewall` | 2 |
| `container` | Container ops | `Container` | 5 |
| Remaining generic | Misc | Keep `DeployFailed` | ~5 |

### Tasks

- [ ] Migrate connection errors → `NetlinkOp`
- [ ] Migrate link creation errors → `NetlinkOp`
- [ ] Migrate address errors → `NetlinkOp`
- [ ] Migrate route errors → `Route`
- [ ] Migrate firewall errors → `Firewall`
- [ ] Migrate container errors → `Container`
- [ ] Verify remaining DeployFailed calls are truly generic
- [ ] Run tests

## Phase 3: Man Page Generation (day 2)

### Implementation

Add `clap_mangen` as a build dependency and generate the man page:

**Option A: Build script** (`bins/lab/build.rs`):
```rust
fn main() {
    let cmd = <Cli as clap::CommandFactory>::command();
    let man = clap_mangen::Man::new(cmd);
    let mut out = std::fs::File::create("nlink-lab.1").unwrap();
    man.render(&mut out).unwrap();
}
```

**Option B: Xtask command** (separate binary that generates docs):
```bash
cargo run -p xtask -- man    # generates nlink-lab.1
```

Option A is simpler. The man page is generated at build time and
can be installed with `install -m 644 nlink-lab.1 /usr/share/man/man1/`.

### Tasks

- [ ] Add `clap_mangen` to build-dependencies
- [ ] Create build.rs or xtask for man page generation
- [ ] Generate `nlink-lab.1`
- [ ] Add install instructions to README

## Phase 4: NLL Spec Update (day 2-3)

### Current state

`docs/NLL_DSL_DESIGN.md` grammar section is missing:
- Subnet pools (`pool` statement)
- Topology patterns (`mesh`, `ring`, `star`)
- Validate blocks (`validate { reach ... }`)
- Healthcheck timing tokens (`interval`, `timeout`, `retries`)
- Container lifecycle properties
- For-expressions in lists
- Cross-references
- Block comments
- Lab metadata (version, author, tags)

### Tasks

- [ ] Add pool syntax to grammar
- [ ] Add pattern syntax to grammar
- [ ] Add validate block to grammar
- [ ] Add healthcheck block syntax (interval/timeout/retries)
- [ ] Add container properties to node grammar
- [ ] Add for-expressions to list grammar
- [ ] Add cross-reference resolution documentation
- [ ] Add block comment syntax (fix "line comments only" claim)
- [ ] Add lab metadata fields (version, author, tags)
- [ ] Add examples for each new feature

## Progress

### Phase 1: Validate Execution
- [x] Types + lowering (Assertion enum, stored in Topology)
- [ ] Deploy execution (ping/nc at deploy time)
- [ ] CLI flag (--skip-validate)
- [x] Tests (assertions stored correctly)

### Phase 2: Error Migration
- [ ] NetlinkOp migrations
- [ ] Route migrations
- [ ] Firewall migrations
- [ ] Container migrations

### Phase 3: Man Page
- [ ] clap_mangen setup
- [ ] Generate man page

### Phase 4: NLL Spec
- [ ] Grammar updates
- [ ] New examples
- [ ] Fix outdated claims
