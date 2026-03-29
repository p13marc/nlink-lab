# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [093](093-nll-v2.md) | NLL v2 Language & Ergonomics: expressions, syntax sugar, render | High | 5-7 days |
| [094](094-nll-composition.md) | NLL v2 Composition & Safety: firewall, multi-profile, modules, cross-refs | Medium | 4-5 days |

### Recommended Order

1. **093 — NLL v2 Language** — expressions + syntax sugar (foundation for 094)
2. **094 — NLL v2 Composition** — depends on 093 for ForRange enum and expression engine

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 070 (topoviewer GUI), 071 (Zenoh backend & metrics),
072 (lab templates), 080 (bug fixes & safety), 081 (code quality),
082 (NLL completeness), 083 (validator hardening), 084 (CLI UX),
085 (test coverage), 086 (feature flags), 087 (topology composition
& hot-reload), 088 (remove TOML), 090 (hardening),
091 (user documentation), and 092 (structured errors) have been
implemented and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
