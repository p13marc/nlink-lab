# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Effort | Status |
|------|-------------|--------|--------|
| [120](120-ip-functions.md) | IP computation functions (`subnet`, `host`) | Medium | Ready (P0) |
| [121](121-for-inside-blocks.md) | `for` loops inside blocks (nat, firewall, node) | Medium | Ready (P0) |
| [122](122-site-improvements.md) | Site improvements (networks + cross-refs) | Small-Medium | Ready (P1) |
| [123](123-auto-addressing.md) | Extended auto-addressing (loopback pools) | Small | Ready (P2) |
| [124](124-conditional-logic.md) | Conditional logic (`if` blocks, `for ... if`) | Medium | Draft (P3) |

### Recommended execution order

```
120 (IP functions) ─── unblocks everything, P0
  ├── 121 (for inside blocks) ─── second highest impact
  │    └── loop-generated NAT/links become possible
  └── parametric import templates become possible

122 (site improvements) ─── independent, P1
123 (auto-addressing) ─── independent, P2
124 (conditional logic) ─── independent, P3
```

## Completed

Plans 050–104 (core features, parser, CLI, containers, polish) and
Plans 105–119 (DNS, macvlan/ipvlan, rich assertions, scenario DSL,
CI integration, integration tests, benchmarks, Wi-Fi emulation,
context-sensitive keywords, NAT, network subnet, shell-style run,
route groups, link profiles, site grouping) have been implemented
and their plan files removed.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
| [../NLL_PAIN_POINTS_REPORT.md](../NLL_PAIN_POINTS_REPORT.md) | DSL pain points analysis |
