# Plan 159e — `ConfigDiff::apply` inherent + `del_*_if_exists` adoption

**Date:** 2026-05-31
**Status:** Substantially moot — superseded by 159c
**Effort:** Small (4–6 hours)
**Priority:** P3 — pure janitor / efficiency.

> **2026-06-01 implementation audit:** the `del_*_if_exists`
> half of this plan was based on a wrong assumption. nlink 0.19
> only ships the `_if_exists` family for **nftables**
> (`del_table_if_exists`, `del_chain_if_exists`,
> `del_rule_if_exists`). The RTNETLINK call sites nlink-lab
> uses (`del_route_v4`, `del_link`, `del_qdisc`) have no
> `_if_exists` siblings in 0.19 — confirmed by walking
> `/home/mpardo/git/rip/crates/nlink/src/netlink/`. Separately,
> nlink-lab doesn't call `del_table`/`del_chain`/`del_rule`
> directly — the nftables layer goes through
> `NftablesConfig::diff().apply_reconcile()` which handles
> deletes internally. So the Phase 1 adoption has zero sites
> to touch. The `ConfigDiff::apply` inherent half is folded
> into [Plan 159c](159c-facade-stack-adoption.md) (Stack does
> the diff-once-and-apply path natively). **159e is effectively
> deferred until either (a) nlink ships `_if_exists` for the
> RTNETLINK families, or (b) a future refactor introduces
> direct `del_table/chain/rule` call sites in nlink-lab.**
> Both halves preserved below as historical context.

---

## TL;DR

Two unrelated 0.19 adoptions that are each small enough to ship
together:

1. **`ConfigDiff::apply(&conn, opts)` inherent (feedback #5).**
   `compute_layered_diff` today calls `cfg.diff(&conn).await`
   to capture the typed diff, then the caller calls
   `cfg.apply(&conn, opts)` which calls `diff` again internally.
   With 0.19's inherent `ConfigDiff::apply(&conn, opts)`, we
   capture the diff once and reuse it — one fewer dump
   round-trip per (node, RTNETLINK) on every
   `apply --check && apply` flow.

2. **`del_*_if_exists` family (feedback W8).** Five `let _ =
   conn.del_*(...).await;` sites swallow ENOENT to mean "the
   resource didn't exist; that's fine". 0.19 ships
   `del_table_if_exists`, `del_chain_if_exists`,
   `del_rule_if_exists`, `del_link_if_exists`,
   `del_route_v4_if_exists`, `del_route_v6_if_exists`,
   `del_qdisc_if_exists` — each returns `Ok(bool)` (true if
   the resource was deleted, false if it didn't exist).
   Replaces the `let _ = ...` ignore pattern with explicit
   "this might not exist" semantics.

Net: ~30 LOC cleanup, one round-trip saved per (node, family)
on `apply --check && apply`, cleaner error semantics on cleanup
paths.

---

## Adoption 1 — `ConfigDiff::apply` inherent

### Audit — current shape

`crates/nlink-lab/src/deploy.rs:2768..2832` `compute_layered_diff`:

```rust
let diff = cfg.diff(&conn).await?;       // dump 1
network.insert(node_name.clone(), diff);
// ... (later) the apply path runs cfg.apply(&conn, opts) which
// calls cfg.diff(&conn) internally — dump 2.
```

The two dumps cost one extra round-trip per node per family
(RTNETLINK + nftables = 2 extra). On a 16-node lab with both
layers populated that's 32 extra dumps.

### 0.19 surface

`crates/nlink/src/netlink/config/types.rs` (Plan 188 §2.1):

```rust
impl ConfigDiff {
    pub async fn apply(
        &self,
        conn: &Connection<Route>,
        opts: ApplyOptions,
    ) -> Result<ApplyResult>;

    // 0.19 also has — same shape, atomic batch semantics:
    pub async fn apply_reconcile(
        &self,
        conn: &Connection<Route>,
        opts: ApplyOptions,
    ) -> Result<ApplyResult>;
}
```

`NftablesDiff::apply` already shipped in 0.18.
`WireguardConfigDiff::apply` ships in 0.19 alongside (Plan 196).

### Adoption

`compute_layered_diff` already captures the diff. Add an
optional "and apply now" path that consumes the captured diff:

```rust
pub async fn compute_and_apply_layered(
    running: &RunningLab,
    desired: &Topology,
) -> Result<(LayeredDiff, AppliedReport)> {
    // ... existing diff loops ...

    // Apply each captured diff directly (no second dump).
    for (node_name, diff) in &network {
        let handle = node_handle_for(running, node_name)?;
        let conn = handle.connection::<Route>()?;
        let result = diff.apply(&conn, ApplyOptions::default().with_continue_on_error(true)).await?;
        // ...
    }

    Ok((layered, applied))
}
```

But — **159c's Stack adoption supersedes this.** Stack does
diff once internally (in pre-flight validation) and applies.
If 159c lands first, this adoption becomes "make sure Stack
isn't dumping twice" — verify in the 159c audit phase.

If 159e ships BEFORE 159c, the cleanup site is
`compute_layered_diff` + a new `apply_captured_layered` helper
that mirrors the per-node apply loop but consumes the
captured `LayeredDiff` instead of re-dumping.

**Recommend:** ship 159e AFTER 159c, so the only adoption is in
the non-Stack path (which may not exist after 159c). At that
point the only beneficiary is the legacy `--check && apply`
two-call flow where the user inspects the diff then commits.

### Two-call optimization

The CLI flow:

```bash
nlink-lab apply --check my-lab.nll       # dump 1+2 (network+nft)
# user inspects, commits
nlink-lab apply my-lab.nll               # dump 3+4 (re-dump)
```

The `--check` invocation discards its diff. The subsequent
`apply` re-dumps. We can't easily share state across CLI
invocations (no daemon). So this optimization saves nothing
across invocations. The win is only inside a single
invocation that both diffs AND applies — which is exactly
what `apply` (without `--check`) does today.

**Inside `apply`:** today's flow is `cfg.apply(&conn, opts)`
which dumps internally. Captured-diff doesn't help. We'd need
to change the API to expose a single `apply_diffed` path.

**The actual win is small.** Reconsider scope. Maybe ship just
the `del_*_if_exists` half of 159e and defer
`ConfigDiff::apply` inherent until there's a real flow that
benefits.

**Decision (current):** ship both halves of 159e. The inherent
`ConfigDiff::apply` adoption is a one-line code-shape
improvement at the helper level even if the wall-clock save
is small. It also documents intent (we have a captured diff;
use it).

---

## Adoption 2 — `del_*_if_exists`

### Audit — current sites

```text
crates/nlink-lab/src/deploy.rs:3228:        let _ = conn.del_route_v4("0.0.0.0", 0).await;
crates/nlink-lab/src/deploy.rs:3229:        let _ = conn.del_route_v6("::", 0).await;
crates/nlink-lab/src/scenario.rs:238:    let _ = conn.del_qdisc(&ep.iface, nlink::TcHandle::ROOT).await;
crates/nlink-lab/src/running.rs:1184:                let _ = root_conn.del_link(&peer).await;
crates/nlink-lab/src/running.rs:1187:            let _ = root_conn.del_link(&bridge_name).await;
```

Each is a "delete this; if it didn't exist, that's expected".
0.19 supplies typed `Ok(bool)` shapes for all five.

### Adoption sites (line-by-line)

**`deploy.rs:3228..3229`** — `apply_diff`'s route cleanup
during node removal. Default route on a removed node may or
may not exist (depending on what the user declared). Replace:

```rust
let _ = conn.del_route_v4("0.0.0.0", 0).await;
let _ = conn.del_route_v6("::", 0).await;
```

with:

```rust
let _ = conn.del_route_v4_if_exists("0.0.0.0", 0).await?;
let _ = conn.del_route_v6_if_exists("::", 0).await?;
```

The `let _ = ...await?` shape preserves the "we don't care
about the bool" semantics while still propagating real errors
(not ENOENT) — strictly stronger than the current ignore.

**`scenario.rs:238`** — qdisc cleanup at scenario teardown.
Same shape:

```rust
let _ = conn.del_qdisc_if_exists(&ep.iface, nlink::TcHandle::ROOT).await?;
```

**`running.rs:1184`** — veth peer cleanup. The peer may or may
not exist on the host side. Same shape.

**`running.rs:1187`** — bridge cleanup. Same shape.

### Combined value

The semantic improvement isn't behavior — it's correctness on
the error path. Today:

```rust
let _ = conn.del_route_v4("0.0.0.0", 0).await;
// Swallows ENOENT (route absent — fine).
// Also swallows EPERM, ENOSYS, EINVAL, network unreachable,
// kernel module gone, etc. — all silently dropped.
```

After:

```rust
let _ = conn.del_route_v4_if_exists("0.0.0.0", 0).await?;
// ENOENT → Ok(false) — fine.
// EPERM / EINVAL / etc. → still bubbles as Error.
```

Real errors stop hiding behind `let _ = ...`. The `let _ = ...
await?` shape says "I don't care about the bool, but I do care
if there's an actual problem."

---

## What changes — file-by-file

### `crates/nlink-lab/src/deploy.rs`

- `compute_layered_diff` — unchanged externally. Internal
  helper `apply_captured_layered` (if shipped) uses
  `diff.apply(...)` instead of re-dumping.
- Lines 3228–3229: replace two `del_route_v4` / `del_route_v6`
  ignores with `_if_exists` shapes.

### `crates/nlink-lab/src/scenario.rs`

- Line 238: replace `del_qdisc` ignore with `_if_exists`.

### `crates/nlink-lab/src/running.rs`

- Lines 1184, 1187: replace two `del_link` ignores with
  `_if_exists`.

### Tests

- `del_route_v4_if_exists_on_nonexistent_route_succeeds` —
  unit-level: open a fresh namespace, call
  `del_route_v4_if_exists("0.0.0.0", 0)`, assert `Ok(false)`.
- Same for `del_qdisc_if_exists`, `del_link_if_exists`.
- Integration: `apply_diff_node_removal_cleans_default_routes`
  — pre-existing test; verify still passes after the swap.

---

## Phases

### Phase 1 — `del_*_if_exists` swap (the only deterministic win)

1. Replace the 5 ignore sites with `_if_exists` calls + `?`.
2. Confirm clippy + tests stay green.
3. Add a unit test per swap site (3 unit tests).
4. CHANGELOG entry.
5. Done — ship.

### Phase 2 — `ConfigDiff::apply` inherent (optional)

Either ship this with 159e Phase 1 as a single PR, or defer:

1. Audit whether 159c (Stack) renders the optimization moot —
   Stack's pre-flight + apply already shares state internally.
2. If 159c already lands, skip Phase 2.
3. If 159c is not yet landed, write
   `apply_captured_layered(running, layered) -> Result<...>`
   that consumes a `LayeredDiff` and calls `diff.apply(...)`
   on each captured per-layer diff.
4. Wire into the `apply` CLI path so that
   `compute && apply` shares the diff.

The Phase 2 win is small; only worth shipping if 159c is
deferred.

---

## Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `_if_exists` returns Err on a transient kernel race | Low | Low — kernel ENOENT is the documented "doesn't exist" path | Same behavior as today's `let _ = ...`; the `?` propagates the kernel error if it's not ENOENT |
| Some `del_*` call sites we didn't grep for | Low | Medium — `grep` may miss patterns | Phase 1 grep is conservative; if other patterns exist, ship in a follow-up |
| Phase 2 saves nothing measurable | Low | High — round-trip is already fast | Document; ship only if free |
| `ConfigDiff::apply` semantics differ from `cfg.apply` (do they internally re-diff?) | Medium | Low — upstream confirms they share the internal path | Phase 2 audit confirms; unit test asserts same result |

---

## Test plan

### Unit tests

- `del_route_v4_if_exists_returns_false_for_absent_route`
- `del_qdisc_if_exists_returns_false_for_absent_qdisc`
- `del_link_if_exists_returns_false_for_absent_link`
- (Phase 2 only) `confdiff_apply_inherent_returns_same_as_cfg_apply`

### Integration tests

- All existing tests should pass unchanged.
- (Phase 2 only) `apply_captured_layered_makes_one_less_dump` —
  count `conn.get_links()` invocations via a test connection
  mock; assert one fewer call than the legacy path.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md)
- [Plan 159c](159c-facade-stack-adoption.md) — supersedes
  Phase 2 if shipped first
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  — items #5, W8 cited
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `crates/nlink/src/netlink/config/types.rs` — `ConfigDiff::apply` inherent
  - `crates/nlink/src/netlink/nftables/connection.rs` — `del_*_if_exists`
  - `crates/nlink/src/netlink/route.rs` — `del_route_*_if_exists`
  - `crates/nlink/src/netlink/connection.rs` — `del_link_if_exists`, `del_qdisc_if_exists`
