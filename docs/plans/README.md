# Plans

Implementation plans for nlink-lab.

## Active Plans

A coordinated arc — Plan 150 is the foundation; the others slot in
once the docs scaffolding is in place. Each plan can ship as one
or more independent PRs.

| Plan | Title | Effort | Priority |
|------|-------|--------|----------|
| [150](150-documentation-overhaul.md) | Documentation overhaul (README rewrite, CLI reference, cookbook, comparison, architecture) | Large (5–7d, 4 phases) | P0 |
| [151](151-killer-examples.md) | Killer examples — "what containerlab can't do, in 30 seconds" (4 polished writeups) | Medium (3–4d) | P1 |
| [152](152-apply-reconcile.md) | Complete `apply` reconcile path leveraging nlink 0.15.1 `PerPeerImpairer::reconcile()` | Medium-Large (4–6d) | P1 |
| [153](153-export-import.md) | `export` / `import` — `.nlz` lab archive for repros and sharing | Small (1.5–2d) | P2 |
| [154](154-lab-test-macro-polish.md) | Polish + promote `#[lab_test]` proc macro for library-first testing | Small (1.5d) | P2 |

**Recommended ship order:** 150 (Phase A: README) → 151 Example A
(satellite mesh) → 150 Phases B–C → 152 Phase A → 154 → rest. This
maximizes the compounding clarity: each piece makes the next one
easier to motivate.

## Completed

Plans 050–149 have been implemented and their plan files removed:

- 050–104: Core features, parser, CLI, containers, polish
- 105–119: DNS, macvlan/ipvlan, rich assertions, scenario DSL,
  CI integration, integration tests, benchmarks, Wi-Fi emulation,
  context-sensitive keywords, NAT, network subnet, shell-style run,
  route groups, link profiles, site grouping
- 120–127: IP computation functions, for-inside-blocks,
  site improvements, auto-addressing, conditional logic,
  auto-routing, fleet for_each imports, glob member patterns
- 128: Per-pair impairment matrix on shared networks
  (`impair A -- B { … }` inside `network { }`). Implementation lives
  on top of `nlink::netlink::impair::PerPeerImpairer` (shipped in
  nlink 0.15.1 in response to our spec); deploy step 14b builds one
  HTB+netem+flower tree per source interface.
- 129–149: NAT translate, editor/IDE support, mgmt bridge,
  spawn/wait-for/exec CLI, asymmetric impairments, healthcheck,
  partition/heal, IP discovery, CLI parameters, process capture,
  tcp-connect retry, network addresses, deploy suffix, validate
  show-ips, documentation gaps, external feedback triage +
  nlink 0.13.0 / 0.15.1 upgrades.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
