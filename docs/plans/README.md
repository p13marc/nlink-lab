# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [095](095-container-core.md) | Container core: resource limits, capabilities, properties | High | 2-3 days |
| [096](096-container-lifecycle.md) | Container lifecycle: health checks, config injection, dependencies | Medium | 3-4 days |

### Recommended Order

1. **095 — Container Core** — resource limits + capabilities (breaking: removes --privileged default)
2. **096 — Container Lifecycle** — depends on 095 for property plumbing pattern

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 070 (topoviewer GUI), 071 (Zenoh backend & metrics),
072 (lab templates), 080 (bug fixes & safety), 081 (code quality),
082 (NLL completeness), 083 (validator hardening), 084 (CLI UX),
085 (test coverage), 086 (feature flags), 087 (topology composition
& hot-reload), 088 (remove TOML), 090 (hardening),
091 (user documentation), 092 (structured errors),
093 (NLL v2 language & ergonomics), and 094 (NLL v2 composition)
have been implemented and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
