# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [080](080-bugfixes-and-safety.md) | Bug fixes, panic risks, state safety | High | 1-2 days |
| [081](081-code-quality.md) | Type safety, error stratification, builder validation | High | 2-3 days |
| [082](082-nll-completeness.md) | NLL missing features (image/cmd, ICMP, interpolation) | Medium | 3-4 days |
| [083](083-validator-and-deploy.md) | New validation rules, deployer hardening | Medium | 2-3 days |
| [084](084-cli-ux.md) | Shell completions, --json, --dry-run, export, diff | Medium | 3-4 days |
| [085](085-test-coverage.md) | Integration tests for advanced features, lifecycle, stress | Medium | 2-3 days |
| [086](086-feature-flags-and-publishing.md) | Cargo feature flags, crates.io preparation | Medium | 2-3 days |
| [087](087-topology-composition.md) | NLL imports, hot-reload / apply | Low | 5-7 days |
| [071](071-live-metrics-dashboard.md) | Zenoh backend daemon, metrics collector, CLI dashboard | Low | 5-7 days |
| [070](070-topoviewer.md) | Native topology visualizer (Iced GUI, Zenoh client) | Low | 5-7 days |

### Recommended Order

1. **080 — Bug fixes & safety** — fix remaining bugs (FD validation, PID ownership)
2. **081 — Code quality** — type safety and error improvements
3. **082 — NLL completeness** — image/cmd lowering, ICMP firewall, interpolation
4. **083 — Validator & deploy** — new rules, hardening
5. **084 — CLI UX** — completions, --json, export, diff
6. **085 — Test coverage** — verify advanced features actually work
7. **086 — Feature flags** — prepare for publishing
8. **087 — Composition** — imports and hot-reload (power user feature)
9. **071 — Zenoh daemon & metrics** — privileged backend + shared types + CLI metrics (must come before 070)
10. **070 — TopoViewer GUI** — unprivileged Iced GUI, connects to backend via Zenoh

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 072 (lab templates), and 088 (remove TOML format) have been
implemented and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
