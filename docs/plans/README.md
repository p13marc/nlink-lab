# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Effort | Status |
|------|-------------|--------|--------|
| [128](128-network-impairment-matrix.md) | Per-pair impairment on shared networks | Medium-Large | Blocked (needs nlink TC filter API) |
| [129](129-nat-translate.md) | NAT `translate` shorthand | Small | Done |
| [130](130-editor-ide-support.md) | Editor/IDE support (tree-sitter + VS Code + Neovim/Helix) | Large | Done |

## Completed

Plans 050–127 have been implemented and their plan files removed:

- 050–104: Core features, parser, CLI, containers, polish
- 105–119: DNS, macvlan/ipvlan, rich assertions, scenario DSL,
  CI integration, integration tests, benchmarks, Wi-Fi emulation,
  context-sensitive keywords, NAT, network subnet, shell-style run,
  route groups, link profiles, site grouping
- 120–124: IP computation functions, for-inside-blocks,
  site improvements, auto-addressing, conditional logic
- 125–127: Auto-routing, fleet for_each imports, glob member patterns

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
