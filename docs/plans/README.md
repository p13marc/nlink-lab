# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [085](085-test-coverage.md) | Integration tests, new examples | Medium | 2-3 days |
| [086](086-feature-flags-and-publishing.md) | Cargo feature flags, crates.io preparation | Medium | 2-3 days |
| [087](087-topology-composition.md) | Hot-reload / apply command | Low | 3-5 days |
| [071](071-live-metrics-dashboard.md) | Zenoh backend daemon, metrics collector, CLI dashboard | Low | 5-7 days |
| [070](070-topoviewer.md) | Native topology visualizer (Iced GUI, Zenoh client) | Low | 5-7 days |

### Recommended Order

1. **085 — Test coverage** — integration tests for advanced features
2. **086 — Feature flags** — Cargo features and crates.io prep
3. **087 — Hot-reload** — apply command for live topology updates
4. **071 — Zenoh daemon & metrics** — privileged backend (must come before 070)
5. **070 — TopoViewer GUI** — unprivileged Iced GUI via Zenoh

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 072 (lab templates), 080 (bug fixes & safety),
081 (code quality), 082 (NLL completeness), 083 (validator hardening),
084 (CLI UX), and 088 (remove TOML) have been implemented and their plan
files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
