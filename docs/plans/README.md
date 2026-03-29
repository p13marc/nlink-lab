# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [102](102-cli-quality.md) | CLI quality: diagnose JSON, exec validation, destroy detail, status table, shell, verbose | High | 2-3 days |
| [103](103-container-cli.md) | Container CLI: containers, logs, pull, stats, restart commands | Medium | 2-3 days |
| [104](104-polish.md) | Polish: management network, colored output, inspect command, deploy timing, man page | Low | 3-4 days |

### Recommended Order

1. **102 — CLI Quality** — must-fix issues + should-fix UX improvements + shell command
2. **103 — Container CLI** — depends on 102 for `container_for()` API
3. **104 — Polish** — management network, colors, inspect, man page

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
098 (NLL patterns), 099 (production readiness), 100 (validate & errors),
and 101 (NLL syntax cleanup) have been implemented and their plan files
removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
