# Plans

Implementation plans for nlink-lab.

## Active Plans

A coordinated arc — Plan 150 is the foundation; the others slot in
once the docs scaffolding is in place. Each plan can ship as one
or more independent PRs.

| Plan | Title | Status |
|------|-------|--------|
| [150](150-documentation-overhaul.md) | Documentation overhaul (README rewrite, CLI reference, cookbook, comparison, architecture) | ✅ Phases A+B+C shipped — Phase D (long-tail polish) open |
| [151](151-killer-examples.md) | Killer examples — "what containerlab can't do, in 30 seconds" (4 polished writeups) | ✅ Example A (satellite mesh) shipped — B/C/D pending |
| [152](152-apply-reconcile.md) | Complete `apply` reconcile path leveraging nlink 0.15.1 `PerPeerImpairer::reconcile()` | ✅ Phases A + B/1 (routes) + B/2 (sysctls) + C (`--check` + `--json`) shipped — nftables / NAT / rate-limits remain |
| [153](153-export-import.md) | `export` / `import` — `.nlz` lab archive for repros and sharing | ✅ Module + CLI + cookbook + 3 CLI pages shipped |
| [154](154-lab-test-macro-polish.md) | Polish + promote `#[lab_test]` proc macro for library-first testing | ✅ `set { … }`, `timeout = N`, louder non-root skip + cookbook recipe shipped — `capture = true` pending |

**Recently shipped (in ship order):**

1. **Plan 150 Phase A** (`4439869`): README rewrite leading with the
   wedge — 622 → 96 lines, hero example (3-line per-pair impair),
   second hero (`#[lab_test]`), short comparison, doc-link audit.
2. **Plan 151 Example A** (`d2682da`): 12-node Iridium-style
   satellite mesh + parser extension to support `for` in network
   blocks (with arithmetic and modulo).
3. **Plan 150 Phase B** (`5e643bd`, `26d1f10`, `c7339a9`,
   `99da188`, `218b0e9`, `fb0181e`): cookbook + CLI scaffolds,
   8 hand-crafted CLI pages (deploy/destroy/validate/exec/spawn/
   apply/capture/status), 9 cookbook recipes covering networking
   primitives (VRF, WG, macvlan, nftables, VLAN trunk) and
   application/CI patterns (iperf3, healthcheck, parametric
   imports, CI sweep).
4. **Plan 150 Phase C** (`842242b`, `9092cdb`): COMPARISON.md
   (honest vs containerlab + capability matrix + side-by-side +
   honest limitations) and ARCHITECTURE.md (contributor on-ramp
   with worked end-to-end example).
5. **Plan 152 Phase A** (`8b4afc5`): `PerPeerImpairer::reconcile()`
   wired into `apply_diff`. Editing per-pair impair rules now
   reconciles in-place with zero packet loss.
6. **Plan 154** (`fe9d3d8`, `76ec514`): `#[lab_test]` cookbook
   recipe + `set { … }` + `timeout = SECS` macro args + louder
   skip-on-non-root.

**Still open:**

- Plan 150 Phase D — long-tail CLI pages (~22 stubs), TROUBLESHOOTING
  expansion, USER_GUIDE walkthrough restructure, doc-CI gates.
- Plan 151 Examples B/C/D — VRF+WG+nftables WAN, scenario partition
  showcase, full Rust integration test writeup.
- Plan 152 nftables / NAT / rate-limits reconcile (Phase B residual).
- Plan 154 capture-on-failure (`capture = true`).

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
