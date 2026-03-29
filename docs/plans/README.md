# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [091](091-user-documentation.md) | User guide, testing guide, troubleshooting, man page | High | 3-4 days |
| [092](092-structured-errors.md) | Specific error variants, phase context, fix unsafe unwraps | High | 2-3 days |

### Recommended Order

1. **092 — Structured Errors** — fix unsafe unwraps first (safety), then improve error variants
2. **091 — User Documentation** — user guide, testing guide, troubleshooting

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 070 (topoviewer GUI), 071 (Zenoh backend & metrics),
072 (lab templates), 080 (bug fixes & safety), 081 (code quality),
082 (NLL completeness), 083 (validator hardening), 084 (CLI UX),
085 (test coverage), 086 (feature flags), 087 (topology composition
& hot-reload), 088 (remove TOML), and 090 (hardening) have been
implemented and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
