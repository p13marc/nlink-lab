# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [099](099-production-readiness.md) | CI pipeline, crates.io packaging, state locking | High | 2-3 days |
| [100](100-validate-and-errors.md) | Validate execution, error migration, man page, NLL spec update | Medium | 2-3 days |
| [101](101-nll-syntax-cleanup.md) | Breaking syntax changes: explicit subnet, container block, firewall, nested interpolation | Low | 3-5 days |

### Recommended Order

1. **099 — Production Readiness** — CI must exist before breaking changes
2. **100 — Validate & Errors** — wire the validate block, clean up errors
3. **101 — Syntax Cleanup** — breaking changes last, after CI catches regressions

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 070 (topoviewer GUI), 071 (Zenoh backend & metrics),
072 (lab templates), 080 (bug fixes & safety), 081 (code quality),
082 (NLL completeness), 083 (validator hardening), 084 (CLI UX),
085 (test coverage), 086 (feature flags), 087 (topology composition
& hot-reload), 088 (remove TOML), 090 (hardening),
091 (user documentation), 092 (structured errors),
093 (NLL v2 language & ergonomics), 094 (NLL v2 composition),
095 (container core), 096 (container lifecycle), 097 (parser hardening),
and 098 (NLL patterns) have been implemented and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
