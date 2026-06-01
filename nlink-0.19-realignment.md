# nlink 0.19 realignment

**Date:** 2026-05-31
**nlink version:** `0.19.0` (released 2026-05-30, source at
`/home/mpardo/git/rip`).
**Prior report:** [`nlink-feedback.md`](./nlink-feedback.md)
(2026-05-30, written against nlink 0.18).
**Audience:** nlink-lab maintainer (me, future-me) + the nlink
maintainer for confirmation.

---

## TL;DR

nlink 0.19 closes **14 of the 16 numbered feedback items** from
`nlink-feedback.md`, **4 of the 9 wishlist items**, and **all 6
documentation suggestions**. It also ships a stack of net-new
features I did not ask for — full declarative `WireguardConfig`,
a `facade` layer, `Stack` orchestrator, resync-aware
combinators, multipath route parsing, and a long tail of silent
correctness fixes (TC filter `tcm_info` packing, IPv6 NAT
register drop, XFRM struct sizes, `MessageIter` infinite loop,
GENL request-lock concurrency, and more) that nlink-lab
transitively benefits from without code changes.

**Adoption status in this commit:**

| Action | Status |
|---|---|
| Bump workspace `nlink = "0.18"` → `"0.19"` | ✅ |
| `cargo build --workspace --all-features` clean | ✅ |
| `cargo test -p nlink-lab --lib` — 422/422 pass | ✅ |
| `cargo clippy --workspace --all-features --all-targets -- -D warnings` | ✅ |
| Adapt to `Error::from_errno*` `.abs()` normalization (2 test asserts) | ✅ |
| Silence new `#[must_use]` on `ReconcileReport` | ✅ |

**No other code change was required to bump.** The breaking
changes that landed in 0.19 either (a) targeted call sites
nlink-lab does not use (`ApplyOptions::with_purge`,
`Hook::Ingress` split, `NatExpr.addr`, `Connection<P>::events()`
async, `subscribe` `&mut`→`&`) or (b) were source-compatible
through the builder patterns nlink-lab already uses.

**What now unlocks future nlink-lab work:**

- **Plan 158d (RTNETLINK side of `watch`) is now feasible** —
  `Connection<Route>::subscribe_all_with_resync` + `ResyncStreamExt`
  ship in 0.19 (Plan 191 + Plan 195 upstream).
- **Plan 158e Slice 4 (VRF + WG declarative) is unblocked** —
  `LinkBuilder::vrf` ships (closes feedback #13 half);
  `WireguardConfig` is a full declarative GENL model with
  `diff`/`apply`/`apply_reconcile` (the OTHER half of #13).
- **VXLAN declarative gap closed** —
  `LinkBuilder::vxlan_local`/`_port`/`_underlay_dev` ship
  (closes #10), so the final Slice can move VXLAN out of the
  imperative step 6 path.
- **`apply_reconcile` parity for NetworkConfig** ships — we can
  collapse the apply path to a single round-trip diff.
- **`serde` derive on diffs** ships — we can drop the
  `layered_summary: String` fallback in
  `docs/json-schemas/layered-diff.schema.json` and serialize
  the typed diff directly.
- **`facade::Stack`** orchestrates RTNETLINK + nftables +
  WireGuard as one call — a 3-line replacement for the per-node
  apply orchestration in `deploy.rs`.

These are follow-up plans, not part of this realignment commit.
This commit only bumps the dep and adapts to the two test
assertion flips. Adoption work is tracked separately so the
review cost stays bounded.

---

## Feedback closeout

### Numbered items — 14 / 16 shipped

Sourced from `nlink-feedback.md` (2026-05-30) and cross-checked
against `/home/mpardo/git/rip/CHANGELOG.md` for the `[0.19.0]`
section.

| # | Item | 0.19 disposition | Upstream tag |
|---|------|------------------|--------------|
| 1 | VLAN parent ifindex race | **Closed.** Topo-sort defensive fix in `NetworkConfig::apply`. Maintainer's audit also concluded the race I hypothesized (sysfs cache) was wrong — `resolve_interface` is netlink-based, so the original symptom may have been a different bug. Topo-sort ships anyway as belt-and-suspenders + dependency-aware ordering for siblings. | Plan 186 §3c |
| 2 | Declared-order iteration | **Closed.** Same fix as #1 — `NetworkConfig::apply` now stable-sorts `links_to_add` parents-before-children. | Plan 186 §3c |
| 3 | `from_errno*` sign convention | **Closed via normalization** (option (c) from my report). `from_errno_ext_ack(1, …).errno()` is now `Some(1)`, not `Some(-1)`. Required flipping two assertions in `crates/nlink-lab/src/error.rs` tests (this commit). | Plan 187 §2.1 |
| 4 | `Box<nlink::Error>` source downcast trap | **Closed.** New `Error::chain_walk` + `root_cause` + `contexts` iterator transparently unwraps `Box<nlink::Error>` in the source chain. Rustdoc on `Error::Kernel` documents the trap + points at `chain_walk` as the escape hatch. nlink-lab's existing `downcast_ref`-loop accessors in `error.rs` still work because we don't box the source on our variants — but we *could* refactor onto `chain_walk` for clarity. Not done in this commit. | Plan 187 §2.2 |
| 5 | `ConfigDiff::apply` inherent method | **Closed.** Mirrors `NftablesDiff::apply` shape. Eliminates the re-diff cost in `compute_layered_diff` (Plan 158f Phase 2). Adoption deferred. | Plan 188 §2.1 |
| 6 | `apply_diff` re-export from `config` mod | **Closed via #5** — the inherent `.apply()` is the canonical path now. | Plan 188 §2.1 |
| 7 | `ApplyOptions` builder methods | **Closed.** Now `ApplyOptions::default().with_continue_on_error(true).with_dry_run(false)` — matches the `ReconcileOptions` shape I suggested. Struct-literal init is gone (breaking). nlink-lab doesn't construct `ApplyOptions` directly (we use `default()` via the implicit path in `diff.apply(&conn, ApplyOptions::default())` style — and we don't even do that, we let the upstream wrapper own it). | Plan 188 §2.2 |
| 8 | `RouteBuilder::new("default")` | **Closed.** `RouteBuilder::default_v4()` / `default_v6()` ship (exactly the form I suggested in W5). nlink-lab already adopted `Ipv4Route::default_route()` from 0.17 — could switch to the builder form for consistency. Cosmetic. | Plan 188 §2.3 |
| 9 | `Serialize` derive on diffs | **Closed.** `serde` feature gates `Serialize`/`Deserialize` on `ConfigDiff` / `NftablesDiff` / `LinkChanges` / etc. Lets us replace the `layered_summary: String` fallback in the JSON schema with the typed diff. Adoption deferred. | Plan 189 |
| 10 | VXLAN missing `.local()` / `.port()` / `.underlay_dev()` | **Closed.** All three ship on `LinkBuilder`. Unblocks moving VXLAN from step 6 (imperative) to step 11c (declarative) in `deploy.rs`. | Plan 190 §2.1 |
| 11 | Bond options gap-fill | **Closed.** 5 new setters (`ad_select`, `lacp_rate`, `downdelay`, `updelay`, `resend_igmp`). | Plan 190 §8 |
| 12 | VLAN `protocol` (802.1Q vs 802.1ad) | **Closed.** `LinkBuilder::vlan_protocol`. | Plan 190 §2.2 |
| 13 | `LinkBuilder::vrf` + `LinkBuilder::wireguard` | **Both closed** — VRF as a link kind (`LinkBuilder::vrf`); WireGuard as a *better* design: full declarative `WireguardConfig` (Plan 196) with `diff`/`apply`/`apply_reconcile`. The declarative GENL model is what I should have asked for; a single `LinkBuilder::wireguard` would have been the wrong shape. | Plan 190 §2.3 + Plan 196 |
| 14 | Declarative nft sets | **Deferred.** No `DeclaredTableBuilder::set()` in 0.19. nlink-lab doesn't use sets yet — no urgency. | — |
| 15 | `Connection<Route>` event subscription | **Closed.** `subscribe_all_with_resync` + `into_events_with_resync` ship for Route. Unblocks Plan 158d RTNETLINK side. **Note:** the post-cycle audit (Finding B) flipped `events()` / `into_events_with_resync()` to `async fn` and the `subscribe()` family from `&mut self` to `&self` — minor migration when we wire 158d. | Plan 191 + post-cycle Findings A & B |
| 16 | `NetworkConfig::apply_reconcile` parity | **Closed.** Mirrors `NftablesConfig::apply_reconcile`. Adoption deferred. | Plan 188 §2.4 |

### Wishlist — 4 / 9 shipped

| # | Item | 0.19 disposition |
|---|------|------------------|
| W1 | Dump-cache invalidation hook | **N/A** — maintainer's audit found `resolve_interface` is already netlink-based; W1's premise was wrong. The topo-sort (closing #1/#2) is the better fix anyway. |
| W2 | RTNETLINK Route events | **Closed** (closes #15). |
| W3 | Declarative VRF + WireGuard | **Closed** (closes #13). |
| W4 | `serde` derive | **Closed** (closes #9). |
| W5 | Lazy connection | **Deferred.** |
| W6 | `LinkChanges::Display` | **Closed.** Compact per-link rows render in `ConfigDiff::Display`. | Plan 188 §2.5 |
| W7 | Universal `tracing::instrument` spans | **Closed.** Audit + backfill on the central methods. | Plan 192 W7 |
| W8 | Idempotent `del_*_if_exists` family | **Closed.** Replaces the `let _ = conn.del_table(...).await;` ignore pattern. | Plan 188 §2.7 |
| W9 | macvlan Source mode | **Deferred.** |

### Doc suggestions — 6 / 6 shipped

| # | Item | 0.19 disposition |
|---|------|------------------|
| D1 | VLAN parent-ordering pitfall | **Closed via #1/#2** — fix at source, no docstring needed. |
| D2 | `Box<nlink::Error>` source trap | **Closed.** Rustdoc on `Error::Kernel` + new `chain_walk` API. |
| D3 | `from_errno*` sign convention | **Closed via #3** — fix at source. |
| D4 | `InterfaceRef::Name` namespace pitfall | **Closed.** Audit found `resolve_interface` was already netlink-based; docstring corrected (the stale `/sys/class/net/` claim removed). |
| D5 | Default `ApplyOptions` semantics | **Closed via #7** — the builder methods make the knobs explicit at every call site. |
| D6 | `summary()` vs `Display` | **Closed.** `summary()` is now deprecated; `Display` is canonical. `LinkChanges::Display` (W6) lets `ConfigDiff::Display` wrap link rows cleanly. |

---

## Net-new in 0.19 — features I did not ask for that I want to use

Beyond closing my feedback, 0.19 lands a substantial amount of
work I either didn't think to ask for or that materially
exceeds what I would have asked for. The ones relevant to
nlink-lab:

### Declarative WireGuard (Plan 196)

`WireguardConfig` is the *right* shape — peer-list diff with
`add`/`remove`/`modify` granularity, `apply`/`apply_reconcile`
parity with `NftablesConfig`, dry-run support. Replaces the
imperative `set_device_with_options` we use today. Unblocks the
WG half of Plan 158e Slice 4.

### `WireguardWatcher` (Plan 199)

Polling-based per-interface state diff with `WireguardEvent`
enum (`PeerAdded`/`PeerRemoved`/`HandshakeChanged`/`EndpointChanged`).
Useful for nlink-lab's `watch` command if it ever grows WG
monitoring. Note: per the post-cycle audit (N6), the watcher
now degrades gracefully on per-interface `get_device_by_name`
failure (emits `PeerRemoved` for tracked peers and continues),
not the original failure-aborts-the-cycle shape.

### `ResyncStreamExt` combinators (Plan 195)

Stream adapters for `subscribe_all_with_resync` output —
`only_events()`, `with_resync_log()`, etc. Useful when we wire
Plan 158d.

### `facade::Stack` (Plan 200)

```rust
let stack = Stack::new()
    .network(network_config)
    .nftables(nft_config)
    .wireguard(wg_config);
stack.apply().await?;
```

One-liner that replaces the per-namespace apply orchestration
in `deploy.rs` step 11c + step 13. Worth adopting once we
migrate from the per-layer functions to the integrated path.

### Multipath route parsing (Plan 202)

`RouteMessage` `multipath` field now parsed AND emitted on
write (the post-cycle audit caught N4 — ECMP nexthops were
parse-only). nlink-lab doesn't ship ECMP routes today, but the
declarative path now supports them when we need them.

### Silent-corruption fixes nlink-lab transitively benefits from

The 0.19 audit-batch closed a stack of silent bugs that
nlink-lab uses transitively without ever seeing them surface:

- **TC filter `tcm_info` packing** — every TC filter add with
  an explicit protocol was broken; nlink-lab's flower filters
  (per-pair impairers) silently used the wrong protocol field.
- **IPv6 NAT register drop** (PR #6, @avionix-g) — nlink-lab's
  IPv6 NAT path now correctness-fixed.
- **XFRM struct sizes** (Plan 204) — `XfrmUserpolicyInfo` was
  4 bytes short; nlink-lab doesn't use XFRM yet, but the
  facade is now actually usable.
- **Devlink mcast group name mismatch** — N/A for nlink-lab.
- **`MessageIter` infinite loop** — affects any GENL dump
  parser; nlink-lab transitively safe now.
- **`audit.rs` no-timeout** — same.
- **NFT verdict constants** (`Verdict::Jump` / `Verdict::Goto`)
  — nlink-lab's nftables paths use these; the pre-0.19 code
  emitted `NFT_BREAK = -2` for `Jump`, silently terminating
  rule evaluation instead of jumping. **Real correctness fix
  for nlink-lab** even though our test suite never noticed.
- **F1 — `Connection<P>` request lock** — concurrent dumps on
  a shared `Arc<Connection>` no longer steal each other's
  responses. nlink-lab uses one connection per namespace, so
  the shared-Arc case is rare, but the lock is the right
  default.
- **N1 — namespace::create thread-bleed** — `unshare`-on-
  tokio-worker bled the new netns to every other task on the
  same worker. nlink-lab's `LabNamespace::new` callers
  transitively benefit.

These ship "for free" with the bump — no code action.

---

## What I'm deferring

To keep this commit minimal (bump + assertion flips), I am NOT
landing the following in this PR:

1. **Plan 158d RTNETLINK side** — Now feasible with
   `Route::subscribe_all_with_resync`. Will rewrite Plan 158d
   to reflect the unblock.
2. **Plan 158e Slice 4 (Vxlan + VRF + WG declarative)** — All
   three now have declarative builders. Will rewrite Slice 4
   to use them.
3. **`facade::Stack` adoption** in `deploy.rs` step 11c+13 —
   architectural win; deserves its own plan + review.
4. **`ConfigDiff::apply` inherent** in `compute_layered_diff`
   — saves one round-trip; cosmetic.
5. **`serde` derive on `LayeredDiff`** — lets us drop the
   `layered_summary: String` fallback. Schema change; needs
   its own PR.
6. **`chain_walk`-based refactor** of `Error::ext_ack` /
   `errno` / `ext_ack_offset` accessors — works fine today;
   cosmetic.

Each of these is a follow-up plan. Listing them here so future-me
remembers what 0.19 enables.

---

## Maintainer thanks

For context: I shipped a 829-line feedback report on 2026-05-30
covering 16 issues + 9 wishlist items + 6 doc suggestions
collected over the 158 arc. The 0.19 release on 2026-05-30 ships
on the same day, closes 14 of the 16 numbered items, 4 of the 9
wishlist items, and all 6 doc suggestions. Several closures
exceed what I asked for — declarative `WireguardConfig` instead
of a `LinkBuilder::wireguard`, `chain_walk` instead of just a
docstring, normalization at source instead of doc-only fixes.

The post-cycle adversarial audit also caught real bugs my report
missed (N1 namespace thread-bleed, N4 RouteMessage write-side
drops, N5 NeighborMessage write-side drops, F1 concurrent-dump
race, TC filter `tcm_info`, IPv6 NAT register, XFRM struct
sizes, NFT verdict constants) — net win for downstream
correctness regardless of feedback overlap.

Thank you. The bump was the cheapest one yet.
