# Plan 152: Complete `apply` Reconcile — Declarative Live Editing

**Date:** 2026-04-27
**Status:** Proposed
**Effort:** Medium-Large (4–6 days, splittable into 3 phases)
**Priority:** P1 — the only feature that meaningfully differentiates
us from "containerlab without Docker." nlink 0.15.1 just made the
implementation cheap.

---

## Problem Statement

`nlink-lab apply` exists and works for a subset of resources (see
`crates/nlink-lab/src/deploy.rs:2476` `apply_diff`). The current
coverage is:

- ✅ Nodes added/removed
- ✅ Links added/removed
- ✅ Per-endpoint (point-to-point) impairments added/removed/changed
- ✅ Lock contention on the same lab via `state::lock()`

The gaps that prevent calling this feature "done":

- ❌ **Network-level per-pair impairments** (Plan 128's feature) are
  not in the diff engine and not in `apply_diff`. Editing the
  `network { impair … }` block requires a destroy+redeploy.
- ❌ **Rate limits** changes go through `RateLimiter::apply` which
  blows away the root qdisc each time. Should use a reconcile-style
  diff.
- ❌ **Routes / sysctls / firewall / NAT** changes require
  redeploy.
- ❌ **No `--diff-only` JSON output** for CI consumption (today only
  `Display` form on stdout).
- ❌ **No `reconcile_dry_run`-style preview** that says exactly which
  kernel calls would be made.
- ❌ **Validation: live state vs. NLL drift detection.** No
  `nlink-lab apply --check` that fails if live state has been
  hand-edited away from the NLL definition.

nlink 0.15.1 ships `PerPeerImpairer::reconcile()` and
`PerPeerImpairer::reconcile_dry_run()` that diff a live TC tree
against a desired one and emit only the deltas. This is the model
to extend across the rest of `apply_diff`.

## Goals

1. Editing any field in the NLL file → `nlink-lab apply` →
   live state converges with zero packet loss for unchanged
   resources, including network-level per-pair impair.
2. `nlink-lab apply --dry-run --json` outputs a structured diff that
   CI can consume.
3. `nlink-lab apply --check` exits non-zero if live state has
   drifted from the NLL — useful as a CI gate.
4. The reconcile path makes **zero kernel calls** when nothing
   changed (verified by an integration test that intercepts the
   netlink socket).

## Phases

### Phase A — Network-level impair reconcile (1.5 days, P0)

Wire the new `PerPeerImpairer::reconcile()` into `apply_diff`.

#### A.1 Extend `TopologyDiff`

```rust
// In crates/nlink-lab/src/diff.rs
pub struct TopologyDiff {
    // existing fields …

    /// Network-level per-pair impair changes (Plan 128 feature).
    pub network_impairs_changed: Vec<NetworkImpairChange>,
}

pub struct NetworkImpairChange {
    pub network: String,
    pub src_node: String,
    /// `None` → the impairer for this (network, src) is removed.
    pub desired: Option<NetworkImpairerSpec>,
}

pub struct NetworkImpairerSpec {
    pub iface: String,
    pub rules: Vec<(IpAddr, NetemConfig, Option<Rate>)>,
}
```

The diff engine groups rules by `(network, src_node)` because
that's the unit `PerPeerImpairer` works on. A change to a single
rule on `radio` for source `hq-fw` triggers one
`PerPeerImpairer::reconcile()` call on `hq-fw`'s radio interface,
which in turn makes a single `change_qdisc` netlink call for the
affected leaf — not a tree rebuild.

#### A.2 Extend `apply_diff`

```rust
// Phase 5 in deploy.rs:apply_diff (after impairment reconcile)
for change in &diff.network_impairs_changed {
    let handle = node_handle_for(running, &change.src_node)?;
    let conn: Connection<Route> = handle.connection()?;

    match &change.desired {
        Some(spec) => {
            let mut impairer = PerPeerImpairer::new(spec.iface.clone());
            for (dst_ip, netem, cap) in &spec.rules {
                let mut p = PeerImpairment::new(netem.clone());
                if let Some(c) = cap { p = p.rate_cap(*c); }
                impairer = impairer.impair_dst_ip(*dst_ip, p);
            }
            impairer.reconcile(&conn).await?;
        }
        None => {
            // Network-level impair removed entirely — clear the tree.
            PerPeerImpairer::new(spec_iface_for_removal)
                .clear(&conn).await?;
        }
    }
}
```

Note: a `--fallback-to-apply` flag (off by default) lets users
opt into the destructive rebuild when the live root qdisc isn't
HTB (i.e. someone manually installed something). Match nlink's
`ReconcileOptions::with_fallback_to_apply(true)`.

#### A.3 Tests

```rust
#[tokio::test]
#[ignore] // root-gated
async fn apply_reconcile_network_impair_zero_packet_loss() {
    let topo1 = parse(SATELLITE_3_NODE_NLL);
    let lab = topo1.deploy().await.unwrap();

    // Start a long-running ping.
    let ping_task = tokio::spawn(lab.exec_capture(
        "hq-fw", "ping -i 0.05 -c 200 alpha"));

    // Mid-flight, edit NLL: change one impair rule's delay.
    let topo2 = parse(SATELLITE_3_NODE_NLL_DELAY_CHANGED);
    let diff = diff_topologies(&topo1, &topo2);
    apply_diff(&mut lab.running(), &topo2, &diff).await.unwrap();

    let ping_output = ping_task.await.unwrap();
    let stats = parse_ping_stats(&ping_output);

    // Asserting zero packet loss is the contract of reconcile.
    assert_eq!(stats.packet_loss_pct, 0.0);
    // The latency change should be visible in the latter half of the run.
    assert!(stats.median_after_change_ms > stats.median_before_change_ms);
}
```

### Phase B — Reconcile other resources (2 days, P1)

Generalize the reconcile pattern. For each resource type, build a
`reconcile_<resource>` function that:

1. Dumps live state.
2. Diffs against desired.
3. Issues only `add_*` / `change_*` / `del_*` calls for actual
   changes.

| Resource | Status today | Reconcile approach |
|---|---|---|
| Per-iface netem | `add_qdisc` blows away root | Use `change_qdisc` if kind/handle match, `replace_qdisc` otherwise |
| Rate limits (HTB) | `RateLimiter::apply` blows away root | Per-rule diff; nlink's `PerHostLimiter::reconcile` (if upstream adds it) or hand-rolled |
| Static routes | Removed+re-added on full redeploy | `route::diff_routes` already exists; wire to `apply_diff` |
| Sysctls | Re-applied on full redeploy | Read current, only `set_sysctl` for diffs |
| nftables rules | Re-applied on full redeploy | Use nftables transaction API (atomic ruleset replace by table) |
| NAT rules | Re-applied on full redeploy | Same as nftables |
| /etc/hosts (DNS) | Re-injected on full redeploy | Compute diff, only edit changed lines |
| Firewall (per-node) | Re-applied on full redeploy | Same as nftables |

Each of these is its own ~half-day mini-PR. They can ship
incrementally; users see them as `apply` becoming progressively
more powerful.

For nftables specifically, **don't** try to reconcile rule-by-rule.
The nftables atomic transaction model means the right approach is:
build the desired ruleset, get a serialized form, compare to live
serialized form, and if they differ, do an atomic `flush table` +
`add table` in one transaction. Pseudocode:

```rust
let desired_ruleset = build_nftables_ruleset(node);
let live_ruleset = nft.dump_ruleset(table_name).await?;
if desired_ruleset != live_ruleset {
    nft.transaction(|tx| {
        tx.flush_table(table_name);
        for rule in desired_ruleset { tx.add_rule(rule); }
    }).await?;
}
```

### Phase C — `--check` and CI integration (1 day, P2)

#### C.1 `nlink-lab apply --check`

```bash
$ nlink-lab apply --check examples/satellite-mesh.nll
ok: live state matches NLL (3 nodes, 1 network, 6 impair rules)

$ nlink-lab apply --check examples/satellite-mesh.nll
drift detected:
  - hq-fw -> alpha: live delay=20ms, desired=15ms
  - alpha -> bravo: live missing, desired present
exit 1
```

This is a wrapper around `reconcile_dry_run()` that errors on
non-empty diff. Useful in CI: run a soak test, then assert the
lab hasn't been hand-edited.

#### C.2 `nlink-lab apply --dry-run --json`

```json
{
  "labels": {"lab": "satellite-mesh", "nll_path": "examples/..."},
  "would_change": {
    "network_impairs": [
      {
        "network": "radio",
        "src_node": "hq-fw",
        "rules_modified": 1,
        "rules_added": 0,
        "rules_removed": 0,
        "kernel_calls": 1
      }
    ],
    "links": {"added": [], "removed": []},
    "nodes": {"added": [], "removed": []}
  },
  "no_op": false
}
```

CI consumes this to gate auto-apply: only proceed if
`kernel_calls < threshold` and no drops are expected.

#### C.3 Documentation

- `docs/cli/apply.md` — full reference: flags, exit codes,
  examples, "what reconcile means" explanation.
- `docs/cookbook/declarative-reconcile.md` — recipe: the GitOps
  pattern — `nlink-lab apply` in a CI loop, with `--check` as a
  gate.

## Implementation order

1. Phase A first — the showcase. Plan 128 + Plan 152 Phase A
   together is the strongest "we ship reconcile-style apply"
   story.
2. Phase B in priority order: routes (most common edit) →
   sysctls → rate limits → nftables → DNS → NAT.
3. Phase C last — needs Phase A+B to be honest about coverage.

## Tests

| Test | Description |
|------|-------------|
| `tests/apply_no_op.rs` | Apply identical NLL twice; second call has zero netlink writes. Verify by intercepting the socket. |
| `tests/apply_reconcile_impair.rs` | Phase A acceptance test (zero-packet-loss). |
| `tests/apply_reconcile_routes.rs` | Add/remove/modify routes converges without flapping connectivity. |
| `tests/apply_reconcile_nftables.rs` | Rule changes apply atomically; conntrack is preserved. |
| `tests/apply_check_drift.rs` | Hand-modify a route, run `apply --check`, assert non-zero exit and correct drift report. |

## Documentation Updates

| File | Change |
|------|--------|
| `docs/cli/apply.md` | Full reference page (Phase 150) |
| `docs/cookbook/declarative-reconcile.md` | Plan 150 cookbook recipe |
| `CLAUDE.md` | Update apply description in CLI table |
| `docs/COMPARISON.md` | Add reconcile to capability matrix as nlink-lab differentiator |

## Open Questions

1. **PerHostLimiter reconcile.** nlink 0.15.1 has
   `PerPeerImpairer::reconcile` but not `PerHostLimiter::reconcile`.
   Should we wait for upstream to add it (1–2 weeks at recent
   pace), or hand-roll the same pattern in nlink-lab? Lean
   toward upstreaming the helper to nlink to keep the surface
   consistent — file an issue with the same shape we used for
   `PerPeerImpairer`.

2. **Live-state read for nftables.** Reading nftables rule output
   and reconstructing a `serialized_form` for comparison is
   non-trivial. Spike a prototype before committing to the atomic
   table-flush approach; a more conservative fallback is "if any
   nftables config differs from desired, flush+rebuild that
   table" (still better than full-node redeploy).

3. **Reconcile for spawned processes.** Step 16 of deploy spawns
   background processes. If their command line changes in the NLL,
   should `apply` SIGTERM and respawn? Not in this plan — defer to
   a separate plan. For now, `apply` will warn if `exec` blocks
   change and tell the user to redeploy.

## File Changes

| File | Change |
|------|--------|
| `crates/nlink-lab/src/diff.rs` | Add `network_impairs_changed`, `routes_changed`, `sysctls_changed`, `firewall_changed`, `nat_changed`, `rate_limits_changed` |
| `crates/nlink-lab/src/deploy.rs::apply_diff` | New phases for each resource |
| `crates/nlink-lab/src/running.rs` | Helper methods for live-state reads |
| `bins/lab/src/main.rs` | `--check`, `--json`, expanded `--dry-run` |

## Acceptance

- Editing any field in NLL → `apply` converges live state with the
  documented zero-packet-loss guarantee for impair changes.
- `apply --check` is a usable CI drift gate.
- `apply --dry-run --json` produces machine-parseable output.
- `apply` of an unchanged NLL makes zero kernel calls (verified by
  socket interception test).
