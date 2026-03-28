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

### Plan 050: Advanced Interface Types

| Component | Files | Status |
|-----------|-------|--------|
| VRF, WireGuard, bond, VLAN | `deploy.rs` | Done |
| Bridge VLAN ports | `deploy.rs` | Done |
| Integration tests (3) | `tests/integration.rs` | Done |

### Plan 051: Phase 3 — Advanced Features

| Component | Files | Status |
|-----------|-------|--------|
| Runtime impairment CLI | `running.rs`, `main.rs` | Done |
| Diagnostics | `running.rs` | Done |
| Packet capture | `main.rs` | Done |
| DOT graph | `main.rs` | Done |
| Process manager | `running.rs` | Done |

### Plan 052: Ecosystem (15 integration tests)

| Component | Files | Status |
|-----------|-------|--------|
| Example topologies (9 TOML + 9 NLL) | `examples/` | Done |
| `#[lab_test]` proc macro | `crates/nlink-lab-macros/` | Done |
| Integration tests (15) | `tests/integration.rs` | Done |
| README.md | `README.md` | Done |

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

### Plan 072: Lab Templates

| Component | Files | Status |
|-----------|-------|--------|
| 12 built-in templates | `bins/lab/src/main.rs` | Done |
| `nlink-lab init` command | `bins/lab/src/main.rs` | Done |

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [088](088-remove-toml-format.md) | Close NLL gaps, remove TOML topology format | **High** | 3-4 days |
| [080](080-bugfixes-and-safety.md) | Bug fixes, panic risks, state safety | Critical | 1-2 days |
| [081](081-code-quality.md) | Type safety, error stratification, builder validation | High | 2-3 days |
| [082](082-nll-completeness.md) | NLL missing features (image/cmd, ICMP, interpolation) | Medium | 3-4 days |
| [083](083-validator-and-deploy.md) | New validation rules, deployer hardening | Medium | 2-3 days |
| [084](084-cli-ux.md) | Shell completions, --json, --dry-run, export, diff | Medium | 3-4 days |
| [085](085-test-coverage.md) | Integration tests for advanced features, lifecycle, stress | Medium | 2-3 days |
| [086](086-feature-flags-and-publishing.md) | Cargo feature flags, crates.io preparation | Medium | 2-3 days |
| [087](087-topology-composition.md) | NLL imports, hot-reload / apply | Low | 5-7 days |
| [070](070-topoviewer.md) | Native topology visualizer (Iced GUI) | Low | 5-7 days |
| [071](071-live-metrics-dashboard.md) | Metrics collector + CLI dashboard | Low | 3-4 days |

### Recommended Order

1. **088 — Remove TOML format** — close NLL gaps then drop TOML topology parsing
2. **080 — Bug fixes & safety** — fix known bugs and panic risks
3. **081 — Code quality** — type safety and error improvements
4. **082 — NLL completeness** — image/cmd lowering, ICMP firewall, interpolation
5. **083 — Validator & deploy** — new rules, hardening
6. **084 — CLI UX** — completions, --json, export, diff
7. **085 — Test coverage** — verify advanced features actually work
8. **086 — Feature flags** — prepare for publishing
9. **087 — Composition** — imports and hot-reload (power user feature)
10. **070/071 — GUI & metrics** — visualization layer

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
| [../DEEP_ANALYSIS_REPORT.md](../DEEP_ANALYSIS_REPORT.md) | Deep codebase analysis (2026-03-28) |
