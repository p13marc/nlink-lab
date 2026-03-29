# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Priority | Effort |
|------|-------------|----------|--------|
| [093](093-nll-expressions.md) | Expression engine: modulo, compound exprs, conditionals, auto-vars, block comments | High | 2-3 days |
| [094](094-nll-ergonomics.md) | Syntax ergonomics: subnet auto-assign, list iteration, defaults, for-exprs, render | High | 3-4 days |
| [095](095-nll-composition.md) | Modules & composition: firewall src/dst, multi-profile, parametric imports, cross-refs | Medium | 4-5 days |

### Recommended Order

1. **093 — Expression Engine** — foundation for conditionals used by 094 and 095
2. **094 — Syntax Ergonomics** — biggest boilerplate reduction, can parallel with 093
3. **095 — Modules & Composition** — depends on 093 for conditionals, heaviest lift

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
