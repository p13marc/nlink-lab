# Plans

Implementation plans for nlink-lab.

## Active plans

The Plan 158 arc — adopt nlink 0.16/0.17/0.18. Each sub-plan
ships as its own PR; the workspace `nlink = "0.18"` bump is
already in. With breaking-compat freedom granted, the arc
expanded from 4 PRs to 7.

| Plan | Title | Effort | Priority | Status |
|------|-------|--------|----------|--------|
| [158](158-nlink-0.16-0.17-adoption.md) | Umbrella — what 0.16/0.17/0.18 give us; ship order | — | — | ongoing |
| [158a](158a-nftables-reconcile.md) | Per-rule nftables reconcile via `NftablesConfig` + atomic `apply()` | M | P1 | ✅ shipped (`792a588`) |
| [158e](158e-network-config-adoption.md) | Declarative RTNETLINK deploy via `NetworkConfig`; collapses many deploy steps | L | P1 | ✅ Slices 1+2+3 shipped (`4098328` `5ae58a8` `ffb0e5b`); Vxlan still imperative (upstream gap) |
| [158b](158b-error-ext-ack.md) | Typed `Error::source` chain + `ext_ack()` accessor (BC-break) | M | P2 | ✅ shipped (`22887bd`) |
| [158f](158f-display-driven-diff.md) | `LayeredDiff` using upstream `Display for *Diff` | S | P2 | ✅ Phase 1 + Phase 2 shipped (`4115099` + `4581be3`) |
| [158g](158g-rate-limit-reconcile.md) | Adopt `RateLimiter::reconcile` (small upstream + swap) | S | P2 | ⏳ blocked on upstream `RateLimiter::reconcile` PR |
| [158c](158c-from-parse-error.md) | Parse-error ergonomics + `default_route()` adoption | S | P3 | ✅ shipped (`3af7e7b`) |
| [158d](158d-watch-nft-events.md) | `nlink-lab watch <lab>` — push-driven nftables event tail | M | P3 | ⏳ unimplemented — power-user feature, "ship if asked" per umbrella plan |

Recommended ship order: **A + E in one bundle** (declarative
deploy shape) → **B** (typed Error chain) → **F** (Display
shape uses B's `ext_ack` accessor) → **G** (independent, ships
when upstream `RateLimiter::reconcile` lands) → **C** (janitor)
→ **D** (power-user, last). See the umbrella plan for the
rationale.

After the arc lands, nlink-lab's deploy has zero "delete-
then-rebuild" reconcile paths — every resource layer is
incremental.

## Completed

Plans 050–157 have been implemented and their plan files removed.
Authoritative ship-record is `CHANGELOG.md` at the repo root.

Highlights, in rough chronological order:

- **050–104** — Core features, parser, CLI, containers, polish.
- **105–119** — DNS, macvlan/ipvlan, rich assertions, scenario DSL,
  CI integration, integration tests, benchmarks, Wi-Fi emulation,
  context-sensitive keywords, NAT, network subnet, shell-style
  run, route groups, link profiles, site grouping.
- **120–127** — IP computation functions, for-inside-blocks,
  site improvements, auto-addressing, conditional logic,
  auto-routing, fleet for_each imports, glob member patterns.
- **128** — Per-pair impairment matrix on shared networks
  (`impair A -- B { … }` inside `network { }`). Implementation
  lives on top of `nlink::netlink::impair::PerPeerImpairer`
  (shipped in nlink 0.15.1); deploy step 14b builds one
  HTB+netem+flower tree per source interface.
- **129–148** — NAT translate, editor/IDE support, mgmt bridge,
  spawn/wait-for/exec CLI, asymmetric impairments, healthcheck,
  partition/heal, IP discovery, CLI parameters, process capture,
  tcp-connect retry, network addresses, deploy suffix, validate
  show-ips, documentation gaps.
- **149** — External feedback triage round 1 + nlink 0.13.0
  upgrade (`shell` nsenter, `np{hash8}` peer naming, `destroy
  --orphans`, `status --scan`, streaming exec, `logs --pid
  --follow`).
- **150–154** — Documentation overhaul, killer examples,
  `apply` reconcile, `.nlz` lab archive, `#[lab_test]` macro
  polish (shipped in `0.2.0`).
- **155** — Round-3 harness feedback (capture flush, `--env`,
  `--alive-only`, doc sweep, `--wait-log`) — `0.2.0`.
- **156 (round-4)** — Partition cycles, `exec --timeout`,
  `impair --show --json` — `0.3.0` / `0.3.1`.
- **156 (eliminate tcpdump runtime dep)** + **156a (netring
  upstream proposal)** — Typed BPF filter builder; default
  capture path no longer shells out to `tcpdump`. netring 0.11.0
  ships `BpfFilter::builder()` upstream. `0.5.0`.
- **157** — Round-5 wishlist: `proc-stat`, capture rotation,
  `--wait-port`, `--wait-fd-stable`, `subnet auto/N`,
  `--dedupe-loopback`, `host_pid`, parallel-deploy `/etc/hosts`
  flock, ARCHITECTURE namespace section, `HARNESS_GUIDE.md`.
  `0.4.0` / `0.4.1`.

Deliberately scoped out (and remaining so): vendor NOS support,
multi-host clustering, web UI.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines for new plans |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
| `../../CHANGELOG.md` | Authoritative ship record |
