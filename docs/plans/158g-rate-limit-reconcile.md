# Plan 158g — Adopt `RateLimiter::reconcile` (needs small upstream + nlink-lab swap)

**Date:** 2026-05-29
**Status:** Proposed (PR G of the Plan 158 arc — new with 0.18
expansion)
**Effort:** Small upstream (~0.5 day) + Small downstream
(~0.5 day) = 1 day total
**Priority:** P2 — closes the **last** rebuild-style reconcile
path in nlink-lab's deploy. After 158a (nftables) + 158e
(NetworkConfig) + 158g, every reconcile is incremental.

---

## TL;DR

The original Plan 158 audit observed:

> `PerHostLimiter::reconcile` already exists in nlink at
> `crates/nlink/src/netlink/ratelimit.rs:749`. The stale
> comment in nlink-lab's `deploy.rs:2956-2961` claimed it
> was missing.

That observation was **half right**. `PerHostLimiter::reconcile`
does exist — but nlink-lab's `apply_rate_limits_diff` uses
the **simpler `RateLimiter`** primitive (ingress + egress on
an interface, with an IFB redirection for the ingress side),
not `PerHostLimiter` (per-destination egress with a default
rate). The two have different semantics:

| Primitive | Ingress? | Per-destination rules? | Reconcile? |
|-----------|----------|------------------------|------------|
| `RateLimiter` (`ratelimit.rs:139`) | ✅ via IFB | ❌ flat-rate iface | **❌ no `reconcile()`** |
| `PerHostLimiter` (`ratelimit.rs:518`) | ❌ egress only | ✅ `.limit_ip(...)`, `.limit_subnet(...)` | ✅ `reconcile()` at line 749 |

nlink-lab's NLL `limit eth0 egress 100mbps ingress 50mbps`
maps to `RateLimiter::new(eth0).egress(...).ingress(...)`.
Migrating to `PerHostLimiter` would lose ingress shaping —
not a clean swap.

The right answer is to upstream a `RateLimiter::reconcile()`
that mirrors `PerHostLimiter::reconcile()` for the
ingress+egress flat-rate shape. Tiny upstream PR; closes
the gap; lets us drop the "delete-then-rebuild" path in
nlink-lab's `apply_rate_limits_diff`.

After this plan, **every reconcile path in nlink-lab is
incremental** — re-applying an unchanged topology to a
live lab produces zero kernel calls for impair, per-pair
impair, nftables, RTNETLINK (links/addrs/routes), and rate
limits.

---

## Audit

### nlink-lab today

`crates/nlink-lab/src/deploy.rs:2954-3021` —
`apply_rate_limits_diff`. Per-change:

- **Added / changed**: `RateLimiter::new(iface).egress(...)
  .ingress(...).apply(&conn)`. `RateLimiter::apply` is
  destructive — it deletes the root qdisc and installs a
  fresh HTB tree. A single egress edit causes the whole
  HTB tree to be rebuilt; a few packets in flight drop.
- **Removed**: `conn.del_qdisc(iface, TcHandle::ROOT)`.
  Imperative tear-down, fine.

The TODO comment at `deploy.rs:2956-2964` already flagged
the gap:

> A fully-incremental rate-limit reconcile is doable but
> requires upstreaming a `PerHostLimiter::reconcile()` to
> nlink (mirror of `PerPeerImpairer::reconcile`)

The comment names the wrong type — `PerHostLimiter`
already has `reconcile()`. The missing primitive is
`RateLimiter::reconcile()`.

### `PerHostLimiter::reconcile` shape (the upstream reference)

`crates/nlink/src/netlink/ratelimit.rs:749-810`:

```rust
pub async fn reconcile(&self, conn: &Connection<Route>) -> Result<ReconcileReport> {
    self.reconcile_with_options(conn, ReconcileOptions::new()).await
}

pub async fn reconcile_dry_run(...) -> Result<ReconcileReport> { ... }

pub async fn reconcile_with_options(
    &self,
    conn: &Connection<Route>,
    opts: ReconcileOptions,
) -> Result<ReconcileReport> {
    let link = conn.get_link_by_name(&self.dev).await?
        .ok_or_else(|| Error::InvalidMessage(format!("interface not found: {}", self.dev)))?;
    let ifindex = link.ifindex();
    self.reconcile_inner(conn, ifindex, opts).await
}
```

The `reconcile_inner` body diffs the live HTB+filter tree
against the declared one, emits only the deltas. Same
shape we want for `RateLimiter`.

---

## Goals

1. **Upstream a `RateLimiter::reconcile(&self, &Connection<Route>)
   -> Result<ReconcileReport>`** + `reconcile_dry_run` + `reconcile_with_options`. Mirrors `PerHostLimiter::reconcile` exactly, scoped to the simpler ingress+egress flat-rate shape.
2. **Migrate nlink-lab `apply_rate_limits_diff`** to call
   the new `reconcile()` instead of `apply()`.
3. **Remove the stale TODO comment** at deploy.rs:2956-
   2964 (the comment named the wrong type, and the gap
   it described will be closed by Phase 1).
4. **Integration test**: re-applying an unchanged
   rate-limit topology produces zero kernel calls.

---

## Phases

### Phase 0 — Upstream `RateLimiter::reconcile` (0.5 day)

Open a PR on nlink. Implementation outline:

```rust
// crates/nlink/src/netlink/ratelimit.rs (add to RateLimiter impl)

impl RateLimiter {
    /// Reconcile the live qdisc tree on this interface
    /// against the declared limiter. Idempotent — calling
    /// `reconcile()` twice with no other changes makes
    /// zero kernel calls.
    ///
    /// Mirrors [`PerHostLimiter::reconcile`] for the
    /// simpler ingress + egress flat-rate shape.
    pub async fn reconcile(&self, conn: &Connection<Route>) -> Result<ReconcileReport> {
        self.reconcile_with_options(conn, ReconcileOptions::new()).await
    }

    pub async fn reconcile_dry_run(&self, conn: &Connection<Route>) -> Result<ReconcileReport> {
        self.reconcile_with_options(conn, ReconcileOptions::new().with_dry_run(true)).await
    }

    pub async fn reconcile_with_options(
        &self,
        conn: &Connection<Route>,
        opts: ReconcileOptions,
    ) -> Result<ReconcileReport> {
        let link = conn.get_link_by_name(&self.dev).await?
            .ok_or_else(|| Error::InvalidMessage(format!("interface not found: {}", self.dev)))?;
        let ifindex = link.ifindex();

        // Walk the desired vs live qdisc tree:
        // - Root HTB qdisc with the right rate? Same → no-op.
        // - Different rate? change_qdisc (in-place).
        // - Missing? add_qdisc.
        // - Live tree is the wrong shape (TBF, ingress qdisc, etc.)?
        //   Either error (with_fallback_to_apply=false) or rebuild
        //   (with_fallback_to_apply=true).
        //
        // For the ingress side: same shape, but the IFB
        // redirection bookkeeping needs idempotent treatment —
        // creating an IFB device that already exists should
        // be a no-op.
        ...
    }
}
```

The core diff logic is small (RateLimiter exposes only
egress + ingress + a few burst/latency options). Re-use
`PerHostLimiter`'s qdisc-walk helpers if they live at
crate scope; otherwise factor out a shared
`reconcile_htb_root` helper.

Tests upstream:

- Unit: build a `RateLimiter`, call `reconcile_dry_run`
  on a kernel without the interface, assert the right
  add-shape is produced.
- Integration (root-gated): `RateLimiter::new(dev).egress(...).apply()`
  then `RateLimiter::new(dev).egress(...).reconcile()`
  — assert `ReconcileReport.attempts == 1` and
  `change_count == 0`.

### Phase 1 — Migrate `apply_rate_limits_diff` (0.3 day)

```rust
// crates/nlink-lab/src/deploy.rs — apply_rate_limits_diff body

match &change.desired {
    Some(rl) => {
        let mut limiter = RateLimiter::new(&ep.iface);
        if let Some(egress) = &rl.egress {
            let bits = parse_rate_bps(egress)?;
            limiter = limiter.egress(Rate::bits_per_sec(bits));
        }
        if let Some(ingress) = &rl.ingress {
            let bits = parse_rate_bps(ingress)?;
            limiter = limiter.ingress(Rate::bits_per_sec(bits));
        }
        let report = limiter.reconcile(&conn).await
            .map_err(|e| Error::NetlinkOp {
                op: "RateLimiter::reconcile".into(),
                node: ep.node.clone(),
                source: e,                       // post-158b
            })?;
        tracing::info!(
            iface = %ep.iface,
            attempts = report.attempts,
            changes = report.change_count,
            "rate-limit reconcile",
        );
    }
    None => {
        // Removed — explicit teardown is fine, no reconcile
        // primitive needed for the deletion side.
        use nlink::TcHandle;
        if let Err(e) = conn.del_qdisc(ep.iface.as_str(), TcHandle::ROOT).await {
            tracing::warn!(...);
        }
    }
}
```

### Phase 2 — Remove the stale comment + update docstrings (0.2 day)

In `crates/nlink-lab/src/deploy.rs`:

- Delete the docstring at lines 2950-2964 that named the
  wrong type and described a missing primitive that now
  exists.
- Add a one-paragraph rustdoc on the function explaining
  that it routes through `RateLimiter::reconcile` and is
  fully incremental.

Update `crates/nlink-lab/CLAUDE.md` step 15 description
to reflect the reconcile shape.

---

## Tests

### Unit (no root)

No new unit tests on nlink-lab side — the reconcile shape
is exercised entirely by the integration tests below. The
upstream PR carries its own unit tests for the diff logic.

### Integration (root-gated)

| Test | Description |
|------|-------------|
| `rate_limit_idempotent_reapply` | Deploy a topology with `limit eth0 egress 100mbps ingress 50mbps`. Run `nlink-lab apply` on the same NLL again. Confirm zero kernel calls via the `RateLimiter::reconcile` `ReconcileReport.change_count == 0`. |
| `rate_limit_edit_in_place` | Deploy with egress 100mbps. Edit NLL to 200mbps. Apply. Confirm in-place reconfigure (verify with `tc qdisc show dev eth0`) — the HTB root qdisc isn't recreated. |
| `rate_limit_removal_clears_qdisc` | Deploy with limit. Edit NLL to remove the limit. Apply. Confirm root qdisc is gone (`tc qdisc show dev eth0` returns the kernel default). |

---

## Acceptance

- Upstream PR for `RateLimiter::{reconcile, reconcile_dry_run, reconcile_with_options}` is open + merged + released in an nlink patch (0.18.1 or 0.19.0).
- `crates/nlink-lab/src/deploy.rs::apply_rate_limits_diff` calls `limiter.reconcile(&conn)` instead of `limiter.apply(&conn)`.
- The stale TODO comment at deploy.rs:2956-2964 is gone.
- 3 new root-gated integration tests pass.
- `nlink-lab apply` on an unchanged rate-limit-bearing topology logs `rate-limit reconcile attempts=1 changes=0`.
- `crates/nlink-lab/CLAUDE.md` step 15 description updated.
- CHANGELOG entry under `[Unreleased] → Changed`:
  > Rate-limit changes now reconcile incrementally via
  > `nlink::RateLimiter::reconcile`. Editing a rate inplace
  > no longer rebuilds the HTB tree, eliminating the
  > brief packet-drop window present in 0.5.x. Closes the
  > last "delete-then-rebuild" reconcile path in the
  > deploy.

---

## Out of scope

- **Per-host rate-limits.** `PerHostLimiter` is more
  powerful but requires NLL syntax extension
  (`limit eth0 default 100mbps host 10.0.0.5 to 10mbps`)
  and loses the ingress shape. Defer to a separate plan
  if users ask.
- **Migrating to `PerHostLimiter` as the underlying
  primitive.** Same reason — different semantics, would
  break NLL.
- **Reconciling the IFB ingress side specifically.** The
  upstream PR needs to handle this; nlink-lab inherits
  whatever shape it ships.

---

## Files

### Upstream (nlink)

| File | Change |
|------|--------|
| `crates/nlink/src/netlink/ratelimit.rs` | New `RateLimiter::reconcile{,_dry_run,_with_options}`. Mirror `PerHostLimiter::reconcile` shape. ~+100 LOC. |
| `crates/nlink/tests/integration/...` | New idempotent-reapply integration test. |
| `crates/nlink/CHANGELOG.md` | New entry under `[Unreleased] → Added`. |

### nlink-lab

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Bump `nlink` to whichever patch release ships `RateLimiter::reconcile`. |
| `crates/nlink-lab/src/deploy.rs` | Body change in `apply_rate_limits_diff`; delete TODO comment. ~+15 / −40 LOC. |
| `crates/nlink-lab/CLAUDE.md` | Step 15 description update. |
| `crates/nlink-lab/tests/integration.rs` | 3 new root-gated tests. |
| `CHANGELOG.md` | New entry under `[Unreleased] → Changed`. |
