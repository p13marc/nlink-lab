# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Effort | Status |
|------|-------------|--------|--------|
| [125](125-auto-routing.md) | Auto-routing from topology graph | Medium | **Implemented** |
| [126](126-fleet-imports.md) | Fleet `for_each` imports | Small | **Implemented** |
| [127](127-glob-members.md) | Glob patterns in network member lists | Medium | Ready (P2) |
| [128](128-network-impairment-matrix.md) | Per-pair impairment on shared networks | Medium-Large | Draft (P2) |
| [129](129-nat-translate.md) | NAT `translate` shorthand | Small | Draft (P3) |

### Recommended execution order

```
125 (auto-routing) ─── highest impact, P0
  └── eliminates ~30 lines of manual routes
126 (fleet imports) ── independent, quick win
127 (glob members) ── independent, medium
128 (impairment matrix) ── new capability, needs TC research
129 (NAT translate) ── nice-to-have, low priority
```

## Completed

Plans 050–124 have been implemented and their plan files removed:

- 050–104: Core features, parser, CLI, containers, polish
- 105–119: DNS, macvlan/ipvlan, rich assertions, scenario DSL,
  CI integration, integration tests, benchmarks, Wi-Fi emulation,
  context-sensitive keywords, NAT, network subnet, shell-style run,
  route groups, link profiles, site grouping
- 120–124: IP computation functions, for-inside-blocks,
  site improvements, auto-addressing, conditional logic

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
| [../NLL_DEEP_ANALYSIS_V2.md](../NLL_DEEP_ANALYSIS_V2.md) | Declarative DSL analysis |
