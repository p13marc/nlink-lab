# Plans

Implementation plans for nlink-lab.

## Completed

### Phase 2: Core Lab Engine (65 tests)

| Component | Files | Status |
|-----------|-------|--------|
| Topology types + Serialize | `types.rs` | Done |
| TOML parser | `parser/toml.rs` | Done |
| Builder DSL | `builder.rs` | Done |
| Value helpers | `helpers.rs` | Done |
| Validator (14 rules) | `validator.rs` | Done |
| Deployer (steps 3-18) | `deploy.rs` | Done |
| RunningLab | `running.rs` | Done |
| State persistence | `state.rs` | Done |
| CLI (8 commands) | `bins/lab/src/main.rs` | Done |

### Plan 060: NLL Parser (68 tests)

| Component | Files | Status |
|-----------|-------|--------|
| Lexer (logos) | `parser/nll/lexer.rs` | Done |
| AST types | `parser/nll/ast.rs` | Done |
| Parser | `parser/nll/parser.rs` | Done |
| Lowering (AST → Topology) | `parser/nll/lower.rs` | Done |
| Format dispatch | `parser/mod.rs` | Done |
| miette error diagnostics | `error.rs` | Done |
| NLL example files (9) | `examples/*.nll` | Done |
| Equivalence tests (6) | `parser/nll/lower.rs` | Done |

### Plan 052: Ecosystem (12 integration tests)

| Component | Files | Status |
|-----------|-------|--------|
| Example topologies (9 TOML + 9 NLL) | `examples/` | Done |
| `#[lab_test]` proc macro | `crates/nlink-lab-macros/` | Done |
| Integration tests (12) | `crates/nlink-lab/tests/integration.rs` | Done |
| README.md | `README.md` | Done |

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [050](050-advanced-interface-types.md) | VRF, WireGuard, bond, VLAN, bridge VLAN ports | Medium | 2-3 days |
| [051](051-phase3-advanced-features.md) | Runtime impairment CLI, diagnostics, capture, graph, process mgr | Medium | 3-5 days |

### Recommended Order

1. **050 — VRF + WireGuard** — unlock the more complex topology examples
2. **051 — CLI commands** — impair, diagnose, capture (polish)

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
