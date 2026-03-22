# Plans

Implementation plans for nlink-lab.

## Completed — Phase 2: Core Lab Engine

Phase 2 is done. All core functionality is implemented and tested (65 tests).

| Component | Files | Status |
|-----------|-------|--------|
| Topology types + Serialize | `types.rs` | Done |
| TOML parser | `parser.rs` | Done |
| Builder DSL | `builder.rs` | Done |
| Value helpers | `helpers.rs` | Done |
| Validator (14 rules) | `validator.rs` | Done |
| Deployer (steps 3-18) | `deploy.rs` | Done |
| RunningLab | `running.rs` | Done |
| State persistence | `state.rs` | Done |
| CLI (5 commands) | `bins/lab/src/main.rs` | Done |

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [050](050-advanced-interface-types.md) | VRF, WireGuard, bond, VLAN, bridge VLAN ports | Medium | 2-3 days |
| [051](051-phase3-advanced-features.md) | Runtime impairment CLI, diagnostics, capture, graph, process mgr | Medium | 3-5 days |
| [052](052-phase4-ecosystem.md) | Example topologies, `#[lab_test]` macro, integration tests, docs | High | 5-7 days |

### Recommended Order

1. **052 — Examples + integration tests** — prove the deployer works end-to-end, most impactful
2. **050 — VRF + WireGuard** — unlock the more complex topology examples
3. **051 — CLI commands** — impair, diagnose, capture (polish)
4. **052 — Test macro + docs** — ecosystem maturity

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
