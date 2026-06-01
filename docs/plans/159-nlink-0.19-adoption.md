# Plan 159 — nlink 0.19 adoption arc

**Date:** 2026-05-31
**Status:** Proposed (umbrella plan)
**Effort:** Medium — 6 sub-plans, ~3–5 days each, fully independent
**Priority:** Mixed — sub-plans range P1 (Slice 4 closes the
last 158e gap) to P3 (chain_walk refactor, ergonomic)

---

## TL;DR

nlink 0.19 (shipped 2026-05-30) closes 14/16 numbered items + 4/9
wishlist items + all 6 doc suggestions from `nlink-feedback.md`,
**and** ships net-new capabilities that further collapse the
nlink-lab deploy path:

| New 0.19 capability | nlink-lab use | Plan |
|----|----|----|
| `LinkBuilder::vrf()` | Move step 6b/10c VRF creation to declarative step 11c | [159a](159a-declarative-vrf-wg-vxlan.md) |
| `WireguardConfig` + `apply_reconcile` | Move steps 6c/10d WG creation+peer-config to declarative `WireguardConfig::diff().apply()` | [159a](159a-declarative-vrf-wg-vxlan.md) |
| `LinkBuilder::vxlan_local/_port/_underlay_dev` | Move step 6 Vxlan creation to declarative step 11c | [159a](159a-declarative-vrf-wg-vxlan.md) |
| `Connection<Route>::subscribe_all_with_resync` + `ResyncStreamExt` | Implement `nlink-lab watch` covering BOTH nftables AND RTNETLINK drift | [159b](159b-watch-route-events.md) |
| `facade::Stack` | Replace the three per-namespace `apply_*_for_node` calls (steps 11c + 13 + 10d) with one `Stack::apply_in_namespace` per node | [159c](159c-facade-stack-adoption.md) |
| `serde` derive on `ConfigDiff` / `NftablesDiff` / `WireguardConfigDiff` | Drop the `layered_summary: String` JSON fallback; serialize the typed diff directly | [159d](159d-serde-layered-diff.md) |
| `ConfigDiff::apply` inherent | Save one round-trip in `compute_layered_diff` | [159e](159e-confdiff-apply-inherent.md) |
| `del_*_if_exists` family | Replace `let _ = conn.del_table(...).await;` ignore pattern | [159e](159e-confdiff-apply-inherent.md) |
| `Error::chain_walk` / `root_cause` | Replace 3 hand-rolled `downcast_ref` loops in `error.rs` | [159f](159f-chain-walk-refactor.md) |

This umbrella picks the ship order, names the cross-plan
dependencies, and points future-me at the right entry for each
piece of work. None of these is required — 0.18 baseline works
fine. Each sub-plan is independently shippable.

---

## What 0.19 changes for nlink-lab — beyond the bump

`nlink-0.19-realignment.md` (companion document, sits next to
`nlink-feedback.md`) is the report-grade per-item closeout.
This plan is the *implementation* roadmap.

### The 158e Slice 4 gap

Plan 158e Slices 1+2+3 moved bridges, dummies, veths, addresses,
routes, qdiscs, bond enslave, VLAN, and macvlan/ipvlan into the
declarative `NetworkConfig::diff().apply()` path. Slice 4 was
**deferred** because 0.18 lacked:

- `LinkBuilder::vrf` (couldn't declare VRF link kind)
- declarative WireGuard (no `WireguardConfig`)
- VXLAN `local` / `port` / `underlay_dev` setters

0.19 ships all three. **Plan 159a closes Slice 4** — VRF, WG, and
VXLAN move from steps 6 / 6b / 6c / 10c / 10d (imperative) into
the same declarative step 11c (NetworkConfig + a new
`apply_wireguard_for_node`).

After 159a, the only resources still imperative in the deploy
path are: namespaces (step 3), hwsim (3b), mgmt bridge (3d),
sysctls (11), processes (16), and validation (17). None of these
are netlink. **The "declarative deploy" arc is complete.**

### The Plan 158d unblock

Plan 158d was rewritten 2026-05-29 to ship nftables-event watch
only because `Connection<Route>` had no `subscribe`/`events`
surface. 0.19 ships full Route resync (`subscribe_all_with_resync`
+ `into_events_with_resync` + `NetworkEvent` typed enum
covering 18 RTNETLINK message types).

**Plan 159b replaces 158d** with a watch implementation that
covers BOTH nftables (drift on firewall/NAT changes) AND
RTNETLINK (drift on link/addr/route/neighbor/qdisc changes),
using `ResyncStreamExt` combinators to compose the two streams
into a single per-node event tail.

### The facade::Stack composition

`facade::Stack` is a one-call orchestrator for
NetworkConfig + NftablesConfig + WireguardConfig per namespace.
After 159a, every per-node apply path in nlink-lab is exactly
"network + nftables + wg" — a 1:1 match for `Stack`.

**Plan 159c** replaces the three sequential `apply_network_*` /
`apply_nftables_*` / WG calls in deploy steps 11c + 13 (and the
analogous calls in `apply_diff`'s Phase 6) with a single
`Stack::apply_in_namespace`. Net: ~150 LOC less orchestration
in `deploy.rs`, and we get Stack's pre-flight validation
(diff every layer before mutating any) for free.

This is a P2 architectural cleanup — it's "after Slice 4 lands"
work, because Stack expects all three layers as configs and we
can't pass a WG config until 159a builds one.

### `serde` derive + drop the layered_summary fallback

Plan 158f Phase 2 wired `compute_layered_diff` and exposed it
through `apply --check --json`. Because nlink 0.18's diffs
didn't derive `Serialize`, we ship a `layered_summary: String`
field carrying the formatted `Display` output. JSON schema:

```json
{
  "lab": "...",
  "no_op": false,
  "change_count": 12,
  "diff": { "nodes_added": [...], ... },
  "layered_summary": "TopologyDiff (2 changes):\n  ...\n"
}
```

0.19's opt-in `serde` feature derives `Serialize`/`Deserialize`
on the diff types. **Plan 159d** enables `nlink/serde`, derives
`Serialize` on `LayeredDiff`, drops the `layered_summary`
fallback, and updates the JSON schema to a typed shape:

```json
{
  "lab": "...",
  "no_op": false,
  "change_count": 12,
  "topology": { "nodes_added": [...], ... },
  "network": { "<node>": { "links_to_add": [...], ... } },
  "nftables": { "<node>": { "tables_to_add": [...], ... } },
  "wireguard": { "<node>": { "devices_to_add": [...], ... } }
}
```

External CLI consumers gain typed access to the per-namespace
diff without parsing the human-readable string. Schema bump is
backwards-incompatible for downstream `jq` queries that read
`.layered_summary` — call out in CHANGELOG.

### ConfigDiff::apply inherent + del_*_if_exists

Two janitor adoptions that aren't worth their own plans
individually:

- **`ConfigDiff::apply` inherent** — today `compute_layered_diff`
  calls `cfg.diff(&conn).await` then we store the diff and the
  apply path re-runs `cfg.apply(&conn, opts)` which calls diff
  internally again. With `ConfigDiff::apply(&conn, opts)` we
  store the diff once and reuse it. Saves one dump round-trip
  per (node, protocol) on every `apply --check && apply`.
- **`del_*_if_exists`** — `apply_nftables_for_node` (and a few
  other sites) use `let _ = conn.del_table(...).await;` to
  swallow ENOENT for "table didn't exist yet". 0.19's
  `del_table_if_exists` returns `Ok(false)` instead. Cleaner
  error semantics, no `.await?` paranoia.

**Plan 159e** bundles both. ~80 LOC change, no behavior diff.

### Error::chain_walk refactor

nlink-lab's `Error::ext_ack`/`errno`/`ext_ack_offset` accessors
walk the `std::error::Error::source` chain with a hand-rolled
`downcast_ref::<nlink::Error>` loop (3 nearly-identical 12-line
functions in `crates/nlink-lab/src/error.rs`). 0.19 ships
`nlink::Error::chain_walk()` and `root_cause()` that walk the
chain *and* transparently unwrap `Box<nlink::Error>` (the trap
described in my feedback report #4).

**Plan 159f** replaces the 3 hand-rolled loops with calls to
`root_cause()`. Saves ~30 LOC + makes the box-source trap
impossible (we don't box today, but the trap could regress
silently).

This is P3 — pure cleanup, no observable difference.

---

## Ship order + dependencies

```
            ┌─────────┐
            │  bump   │  ← already done (this commit)
            │  0.18→  │
            │  0.19   │
            └────┬────┘
                 │
        ┌────────┼─────────┬─────────┬─────────┐
        ▼        ▼         ▼         ▼         ▼
   ┌──────┐ ┌──────┐  ┌──────┐  ┌──────┐  ┌──────┐
   │ 159a │ │ 159f │  │ 159d │  │ 159e │  │ 159b │
   │Slice4│ │chain_│  │serde │  │apply │  │watch │
   │      │ │walk  │  │diff  │  │+ del │  │      │
   └───┬──┘ └──────┘  └──────┘  └──────┘  └──────┘
       │
       ▼
   ┌──────┐
   │ 159c │
   │Stack │
   └──────┘
```

| Plan | Depends on | Why |
|------|------------|-----|
| 159a | bump only | Independent — builds `WireguardConfig` from `node.wireguard` |
| 159b | bump only | Independent — wires `Connection<Route>::subscribe_all_with_resync` |
| 159c | **159a** | Stack expects a WireguardConfig; 159a builds it |
| 159d | bump only | Independent — `serde` feature toggle + schema bump |
| 159e | bump only | Independent — inherent apply + del_*_if_exists |
| 159f | bump only | Independent — pure refactor |

**Recommended order:** 159a → 159f → 159d → 159e → 159c → 159b.

Rationale:

- **159a first** — biggest leverage (closes 158e Slice 4); unblocks 159c.
- **159f second** — small, low-risk, clears the way before any error-path work.
- **159d third** — schema bump; ship early so downstream consumers have lead time.
- **159e fourth** — janitor; no user-visible change.
- **159c fifth** — architectural cleanup; needs 159a's WG config.
- **159b last** — net-new feature; can ship whenever there's user demand.

Sub-plans can be reordered freely — every plan documents its
own preconditions.

---

## What stays imperative after the arc

Even with the full 159 arc shipped, these deploy paths stay
imperative for reasons that are architectural, not "upstream
gap":

- **Namespace create/move** (step 3, 3b, 3d) — not netlink at
  all; `unshare`/`setns` + sysfs.
- **macvlan/ipvlan host-side create + move** (step 6a) —
  declarative `LinkBuilder::macvlan` exists, but the "create
  on host, move to ns" pattern needs an imperative
  `set_link_netns_fd` between the two. `NetworkConfig` doesn't
  model cross-namespace moves.
- **Sysctl writes** (step 11) — sysfs, not netlink.
- **Process spawn / containers** (step 16) — runc/containerd.
- **Validation assertions** (step 17) — application-level
  probes (TCP connect, ping, DNS lookup).

These will never become declarative `NetworkConfig`-style. The
"declarative deploy" goal stops at the netlink layer.

---

## Risks

| Risk | Mitigation |
|------|------------|
| 159a regresses WG peer-config order (peer A's keys depend on B's pubkey) | `WireguardConfig::diff` is per-device, not cross-device — keys still need a 2-pass build like the current code. 159a Phase 1 keeps the 2-pass key generation; only step 10d's per-peer `set_device` calls collapse into `WireguardConfig::apply`. |
| 159b spams the user's terminal on noisy nodes | `--filter <regex>`, `--node <name>`, `--family route\|nftables`, NDJSON `--json` for piping to `jq`. Match what 158d Phase 2 spec'd. |
| 159c breaks the per-namespace concurrency we have today | `Stack::apply_in_namespace(ns)` opens its own connections through `namespace::connection_for`; we need to confirm fd-based namespace handles (`LabNamespace::open_ns_fd`) work with the name-based API or build a thin adapter. Phase 1 of 159c is a compatibility audit. |
| 159d schema break for downstream `jq` queries | Major schema version bump in `docs/json-schemas/layered-diff.schema.json` ($id v2). Keep `layered_summary` for one release as a deprecation period; remove in the next. |
| 159e idempotency of `del_*_if_exists` on stale connections | Tests in `tests/integration.rs` already cover the "delete-then-redeploy" path. Add one assertion that the `Ok(false)` arm is hit on the cold path. |
| 159f Box<Error> source regression | Today we don't box. Add a unit test that *does* box and asserts `errno()` still walks through (proves we're using `chain_walk` correctly). |

---

## Out of scope for the 159 arc

- **WireGuard pre-shared keys** — `DeclaredWgPeerBuilder::preshared_key` ships, but our NLL DSL doesn't surface PSK yet. Filed as a future enhancement; not on the critical path.
- **WireGuard fwmark** — `DeclaredWgDeviceBuilder::fwmark` ships, also not exposed in NLL.
- **MPTCP, MACsec, Ethtool subscriptions** — 0.19 has subscribe support for all GENL families. nlink-lab doesn't model any of these.
- **`WireguardWatcher`** — Plan 199 upstream is a polling-based per-interface WG state diff. Useful for a future `lab status --wg` style report; out of scope for the watch CLI (159b uses event subscriptions, not polling).
- **multipath route declarations** — 0.19's Plan 202 ECMP nexthops parse + emit. NLL DSL doesn't yet expose ECMP routes.
- **netkit / ovpn link kinds** — 0.19 ships declarative builders; no nlink-lab usage.

These are listed so future-me doesn't re-discover them.

---

## Success criteria for the arc

- [ ] **159a:** `examples/wireguard.nll` deploys end-to-end with zero call sites in `deploy.rs` to `nlink::netlink::link::{VrfLink, WireguardLink, VxlanLink}` (declarative path covers all three). Re-deploy of the unchanged topology makes zero kernel calls on the link layer.
- [ ] **159b:** `nlink-lab watch <lab>` runs and emits a typed event for every drift (test: `nft add rule … bypassing apply` produces a `NewRule` event; `ip link add … bypassing apply` produces a `NewLink` event).
- [ ] **159c:** `apply_*_for_node` removed from `deploy.rs`; replaced by a single `Stack::apply_in_namespace` call per node. Integration test `stack_apply_idempotent_reapply` shipped.
- [ ] **159d:** `apply --check --json` emits typed network/nftables/wireguard diffs; `layered_summary` removed (or marked deprecated). Schema v2 documented.
- [ ] **159e:** `let _ = conn.del_*(...).await;` ignore-patterns gone from `crates/nlink-lab/src/`. `compute_layered_diff` makes one less round-trip per (node, protocol).
- [ ] **159f:** Hand-rolled `downcast_ref` loops in `error.rs` replaced; new test covers the boxed-source case.
- [ ] **Overall:** Across 159a–f, `cargo nextest run --all-features` 100% green; CI green; nightly `rustfmt` clean; `cargo clippy --all-features -- -D warnings` clean.

---

## Cross-references

- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md) — per-item closeout against `nlink-feedback.md`
- [`nlink-feedback.md`](../../nlink-feedback.md) — the 2026-05-30 feedback report
- Plan 158 arc (shipped — see `CHANGELOG.md` and
  [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  for what landed in 0.16/0.17/0.18 adoption); plan files for
  158, 158a-f removed per the "completed plans get removed"
  convention. Notable references:
  - Plan 158e (`NetworkConfig` adoption) — Slice 4 reopened as 159a
  - Plan 158d (`nlink-lab watch`) — superseded by 159b
  - Plan 158f (`LayeredDiff`) — 159d/159e build on this
- [CHANGELOG.md](../../CHANGELOG.md) `[Unreleased]` — bump entry + this arc's deliverables once shipped
