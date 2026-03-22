# Plans

Implementation plans for nlink-lab.

## Active Plans — Phase 2: Core Lab Engine

| Plan | Description | Status | Effort |
|------|-------------|--------|--------|
| [040](040-nlink-lab-topology-types.md) | Topology types, TOML parser, builder DSL | ~80% done | 3-4 days |
| [041](041-nlink-lab-validator.md) | Topology validation rules | Not started | 2-3 days |
| [042](042-nlink-lab-deployer.md) | Deployer, RunningLab, state management | Not started | 5-7 days |
| [043](043-nlink-lab-cli.md) | CLI binary | ~30% (skeleton) | 2-3 days |

### Implementation Order

1. **040** (finish) — Add `Serialize` derives, builder DSL, remaining parser tests
2. **041** — Validator (blocks deployer: deploy calls `validate().bail()` first)
3. **042** — Deployer (the core). Start with MVP: namespaces + veths + addresses + routes + netem. Defer bridges, firewall, advanced interface types.
4. **043** — Wire CLI commands to library (should be fast once 042 is done)

### MVP Scope (first deployable lab)

The minimum for a working `nlink-lab deploy` + `exec` + `destroy`:

- **040:** Types + parser (done), `Serialize` derives (needed for state)
- **041:** Error-level validation rules only (warnings can come later)
- **042 MVP subset:**
  - Steps 3, 5, 9, 10, 11, 12, 14, 16, 18 (namespaces, veths, addresses, up, sysctls, routes, netem, spawn, state)
  - `RunningLab::exec()`, `destroy()`
  - State save/load/list/remove
  - Rollback on failure
- **043:** `deploy`, `destroy`, `exec`, `validate`, `status` commands

Deferred to post-MVP:
- Builder DSL (040)
- Bridge networks, VLANs, nftables, VRF, WireGuard, VXLAN (042)
- Warning-level validation rules (041)
- `--force` flags, colored output, detailed status (043)

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
