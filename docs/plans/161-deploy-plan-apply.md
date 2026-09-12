# Plan 161 — deploy engine: one plan/apply path

**Status:** shipped (deep-analysis series, wave 5; epic #73).

## Why

`deploy.rs` was a 5k-line file with a 1.1k-line `deploy()` interleaving
planning and kernel mutation across "18 steps" (eight of them no-op
markers), a parallel 350-line `apply_diff` that re-implemented a subset
imperatively (and rewrote the state file from scratch), a third partial
traversal in `compute_layered_diff`, and a fixed-schema `Cleanup` guard
that missed the mgmt bridge, host-side macvlans, spawned PIDs and logs.
Nothing in `deploy()` was testable without root.

## Shape

```
deploy(t)       = execute(plan(t))
apply(cur, des) = execute(Plan::diff(plan(cur), plan(des)))   # purge on
```

- `deploy/plan/*` — pure planners producing `Vec<Op>` (`op.rs`), stage-
  sorted. Unit-tested rootless: determinism, unique keys, stage order,
  every example plans, diff semantics.
- `deploy/apply.rs` — `execute(&Plan, &mut ApplyEnv, &mut Journal)`: the
  only kernel-touching code. Each op records its inverse (`rollback::Undo`)
  after success.
- `deploy/rollback.rs` — `Journal`, persisted as `journal.json` while it
  grows; unwound newest-first on error; a pending journal is unwound by
  the next `deploy` and by `destroy --orphans`.
- `Plan::diff` — removals (inverse ops, reverse stage order, skipping
  resources inside a dying namespace) then new/changed ops in stage order.
  `Stack`/`LinksUp`/DNS ops always re-run (idempotent reconcile); one-shot
  process ops only for new nodes; everything else re-created when its
  payload differs.

## Constraints found against nlink 0.26

- `facade::Stack::apply_in` orders WireGuard after the network layer and
  takes no `ApplyOptions`; nlink-lab needs `ensure_devices` *before*
  addresses land and purge on apply → we keep our own three-call stack
  and adopt only `NamespaceSpec` (`NsRef`). Upstream ask: `Stack::
  apply_in_with(ns, ApplyOptions)` with WG-bootstrap-first ordering.
- Declarative netem has no `rate` and only `jitter_ms` → netem stays
  `Op::Netem` via the idempotent `replace_qdisc`. Upstream ask: `rate` +
  sub-ms jitter on `QdiscBuilder::netem`.
- `DiffOptions::purge` never removes links/qdiscs and only main-table
  routes → removals of veths, qdiscs, rate limits and VRF-table routes
  are explicit inverse ops (VRF-table route *removal* on apply is a known
  gap: the route is re-declared but a dropped one is not purged).
- `NetworkConfig`/`WireguardConfig` have no `PartialEq` → stack ops are
  always reconciled rather than compared.

## Follow-ups

- `RunningLab::destroy` still has its own teardown; expressing it as
  `unwind(plan(topology).inverses())` would make destroy and rollback one
  path.
- Double-fork background processes so the CLI never leaves zombies (#30).
- `ConnectionPool` for the per-node connections `execute` opens.
