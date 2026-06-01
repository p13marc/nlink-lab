# Plans

Implementation plans for nlink-lab.

## Active plans

### 159 arc — adopt nlink 0.19

Workspace dep bumped to `nlink = "0.19"` (commit pending). The
0.19 release closed 14/16 numbered items + 4/9 wishlist items
+ all 6 doc suggestions from `nlink-feedback.md`. The 159 arc
adopts the new APIs that 0.19 unlocked. See
[`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
for the per-item closeout.

| Plan | Title | Effort | Priority | Status |
|------|-------|--------|----------|--------|
| [159](159-nlink-0.19-adoption.md) | Umbrella — what 0.19 unlocks; ship order | — | — | proposed |
| [159a](159a-declarative-vrf-wg-vxlan.md) | Declarative VRF + WireGuard + VXLAN (closes 158e Slice 4) | M | P1 | proposed |
| [159b](159b-watch-route-events.md) | `nlink-lab watch` covering RTNETLINK + nftables (supersedes 158d) | M | P2 | proposed |
| [159c](159c-facade-stack-adoption.md) | `facade::Stack` adoption — single per-namespace apply | S–M | P2 | proposed (blocked on 159a) |
| [159d](159d-serde-layered-diff.md) | `serde` derive on `LayeredDiff`; drop `layered_summary` string fallback | S | P2 | proposed |
| [159e](159e-confdiff-apply-inherent.md) | `ConfigDiff::apply` inherent + `del_*_if_exists` adoption | XS | P3 | proposed |
| [159f](159f-chain-walk-refactor.md) | `Error::chain_walk` refactor of `ext_ack`/`errno`/`ext_ack_offset` accessors | XS | P3 | proposed |

Recommended ship order: **159a** (biggest leverage, unblocks 159c)
→ **159f** (XS cleanup) → **159d** (schema bump, ship early for
deprecation lead time) → **159e** (janitor) → **159c**
(architectural cleanup, needs 159a's `WireguardConfig`) → **159b**
(net-new feature, can ship whenever there's demand).

### 158g — blocked on upstream

The one remaining 158-arc plan that is neither shipped nor
superseded:

| Plan | Title | Effort | Priority | Status |
|------|-------|--------|----------|--------|
| [158g](158g-rate-limit-reconcile.md) | Adopt `RateLimiter::reconcile` (small upstream + swap) | S | P2 | ⏳ blocked — `PerHostLimiter::reconcile` ships in 0.19, but the per-iface `RateLimiter` that nlink-lab uses has only `apply`/`remove`. Awaiting upstream parity. |

### 158 arc — shipped (no longer in this directory)

Plans 158, 158a, 158b, 158c, 158d, 158e, 158f shipped through
the 0.18 adoption pass and have been removed per the convention
below. Highlights:

- **158a** — nftables reconcile via `NftablesConfig` + atomic
  `apply` (commit `792a588`)
- **158b** — typed `Error::source` chain + `ext_ack` accessor
  (commit `22887bd`); cosmetic refactor of the accessor onto
  `chain_walk` now lives in [159f](159f-chain-walk-refactor.md)
- **158c** — `From<AddrParseError>`/`From<ParseIntError>` +
  `default_route()` adoption (commit `3af7e7b`)
- **158d** — push-driven nftables event tail; **superseded by
  [159b](159b-watch-route-events.md)** which covers both
  nftables AND RTNETLINK families
- **158e** — declarative RTNETLINK deploy via `NetworkConfig`;
  Slices 1+2+3 shipped (commits `4098328`, `5ae58a8`,
  `ffb0e5b`); **Slice 4 reopened as
  [159a](159a-declarative-vrf-wg-vxlan.md)** once 0.19 lifted
  the upstream gaps for VRF/WG/VXLAN
- **158f** — `LayeredDiff` rendering via upstream `Display`
  (commits `4115099`, `4581be3`); typed-JSON follow-up in
  [159d](159d-serde-layered-diff.md)

Full per-commit record in `CHANGELOG.md`. Per-item closeout of
the upstream feedback that shaped the arc in
[`nlink-feedback.md`](../../nlink-feedback.md) and
[`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md).

## Completed

Plans 050–158 (excluding 158g, still active) have been
implemented and their plan files removed.
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
