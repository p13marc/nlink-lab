# Plan 158a — nftables reconcile via `NftablesConfig`

**Date:** 2026-05-27 (rewritten 2026-05-29 — nlink 0.18 lands)
**Status:** Proposed (PR A of the Plan 158 arc) — **all
upstream prerequisites now shipped in nlink 0.18.0**; Phase 0
deleted; Phases 1 + 2 ship together.
**Effort:** Medium (2–3 days now that Phase 0 is gone)
**Priority:** P1 — closes the TODO that has lived in
`deploy.rs:2906` since Plan 152 (April 2026), and gives
nlink-lab a reconcile story comparable to the per-pair impair
arc Plan 152 already shipped.

---

## TL;DR

nlink 0.16 shipped `NftablesConfig` with `rule_keyed` USERDATA-
tagged identity + an atomic `apply()` that batches every
mutation into a single `NFNL_MSG_BATCH_BEGIN…BATCH_END`.
nlink 0.17 fixed Plan 178 (false-positive replace on every
re-diff). With both releases adopted, nlink-lab can replace
its current `del_table` + sequential `add_table`/`add_chain`/
`add_rule` path with:

```rust
let cfg = topology_to_nftables_config(node);
let diff = cfg.diff(&conn).await?;
diff.apply(&conn).await?;     // atomic batch; idempotent re-apply = 0 ops
```

Net wins:

1. **Zero traffic drop on rule edits.** Today
   `apply_nftables_diff` calls `del_table` then re-adds the
   whole tree — a brief window where the chain has no rules.
   The new path commits everything in one batch.
2. **Idempotent re-apply is 0 ops.** Today a no-op `apply`
   still deletes and rebuilds. With per-rule USERDATA keys,
   the diff finds zero changes and `apply` returns
   `Ok(0)`.
3. **Foreign-edit absorption.** Rules without an
   `nlink-lab:<key>` USERDATA comment are left alone — users
   can hand-edit a node's ruleset via `nlink-lab exec node --
   nft -f extra.nft` and the next `apply` doesn't clobber it.
4. **Atomicity is verified by the kernel, not our code.**
   The kernel rolls back the whole batch on any failure, so
   we never leave a half-applied ruleset.

The migration is mostly mechanical because nlink-lab already
uses the same `Rule` builder methods (`.match_tcp_dport`,
`.match_saddr_v4`, `.masquerade`, etc.) — they're exposed
verbatim inside the `rule_keyed(chain, key, |r| {…})` closure.

---

## Audit

### Current state — imperative, full-rebuild

`apply_nftables_diff` in
`crates/nlink-lab/src/deploy.rs:2910`:

```rust
// 1. Delete the existing table (if any).
if change.was_present
    && let Err(e) = nft_conn.del_table("nlink-lab", Family::Inet).await
{
    tracing::warn!(…);
}

// 2. Re-apply firewall + NAT from the desired config.
if let Some(fw) = &change.desired_firewall { apply_firewall(&handle, &change.node, fw).await?; }
if let Some(nat) = &change.desired_nat   { apply_nat(&handle, &change.node, nat).await?;   }
```

`apply_firewall` (deploy.rs:1625-1709) and `apply_nat`
(deploy.rs:1827-1922) each:

- Call `nft_conn.add_table("nlink-lab", Family::Inet)` (or
  ignore the `AlreadyExists` error).
- Call `nft_conn.add_chain(...)` for each chain
  (input/forward/prerouting/postrouting).
- Loop over rules and call `nft_conn.add_rule(rule)` one at
  a time.

Each `add_*` is a separate netlink round-trip — there is no
atomicity guarantee across them. The diff path's
"delete-then-rebuild" coarse-grained reconcile is the
documented compromise; the comment at
`deploy.rs:2905-2909` says:

> A fully-incremental reconcile (rule-by-rule diffing inside
> the table) is doable but requires upstreaming a per-rule
> diff API to nlink. The full-rebuild approach is correct and
> lossless for existing connections (conntrack state is
> preserved across the table swap because the conntrack zone
> isn't tied to the table).

### nlink 0.17 shape — what we get to call

(All citations relative to `/home/mpardo/git/rip/`.)

- `NftablesConfig::new()` →
  `crates/nlink/src/netlink/nftables/config/types.rs:21`.
- `.table(name, family, |t| …)` builder →
  `config/types.rs:29` (closure receives
  `DeclaredTableBuilder`).
- `.chain("input", |c| …)` →
  `config/types.rs:135` (closure receives
  `DeclaredChainBuilder`).
- `DeclaredChainBuilder` methods: `.hook(Hook)`
  (`types.rs:263`), `.priority(Priority)`
  (`types.rs:269`), `.policy(Policy)`
  (`types.rs:277`).
- `.rule_keyed(chain, key, |r| …)` →
  `config/types.rs:166` — closure gets the same `Rule`
  builder we already use today, with `.family()` pre-set
  from the parent table.
- `Rule` matchers we already use (all available in 0.17):
  `.match_tcp_dport(port)`, `.match_udp_dport(port)`,
  `.match_l4proto(p)`, `.match_saddr_v4(addr, prefix)`,
  `.match_daddr_v4(addr, prefix)`, `.accept()`, `.drop()`,
  `.masquerade()`, `.snat(addr, port_opt)`,
  `.dnat(addr, port_opt)`.
- `NftablesConfig::diff(&conn) -> Result<NftablesDiff>` →
  `config/diff.rs:298`.
- `NftablesDiff::apply(&conn) -> Result<usize>` →
  `config/apply.rs:51` (returns the change count, atomic).
- `NftablesDiff::apply_reconcile(&conn, ReconcileOptions) ->
  Result<ReconcileReport>` → `config/apply.rs:190`.
- USERDATA storage: keys live in `Rule.comment` (string).
  **The library auto-wraps user-supplied keys with the
  literal prefix `"nlink:"`** before writing
  `NFTA_RULE_USERDATA`
  (`crates/nlink/src/netlink/nftables/userdata.rs:47-58`),
  and strips it on parse
  (`userdata.rs:75-95`). User-side keys do *not* include
  `nlink:`. We pass keys like
  `"nlink-lab/fw/input/0"`; the kernel-side comment
  becomes `comment "nlink:nlink-lab/fw/input/0"` and
  the diff round-trips back to `"nlink-lab/fw/input/0"`.
  `NFTNL_UDATA_COMMENT_MAXLEN = 128` bounds the total
  encoded comment (including `"nlink:"` prefix + trailing
  NUL); our keys must stay ≤ 121 bytes — comfortable
  headroom for the `<kind>/<chain>/<index>` shape.

### ✅ NAT-chain support resolved by nlink 0.18.0

The Phase 0 upstream coordination called for in the original
plan landed as **Plan 180 in nlink 0.18.0**:
- `DeclaredChainBuilder::chain_type(ChainType)` —
  `crates/nlink/src/netlink/nftables/config/types.rs` (the
  new setter on `DeclaredChainBuilder`).
- `DeclaredChain.chain_type: Option<ChainType>` field + getter.
- `config/apply.rs` chain reconstruction threads
  `chain_type` into the runtime `Chain` build.

Plus a bonus `Chain::device(name)` /
`DeclaredChainBuilder::device(name)` for `Family::Netdev`
base chains hooked at `ingress`/`egress`. nlink-lab doesn't
use netdev hooks yet but they're worth knowing about for
future ingress filtering work.

Net result: **Phase 1 (firewall reconcile) and Phase 2 (NAT
reconcile) ship in the same cycle**, both via
`NftablesConfig::diff().apply()`. No more split between
declarative firewall + imperative NAT.

---

## Goals

1. **`apply_firewall` and `apply_nat` produce an
   `NftablesConfig`**, not a sequence of imperative
   `add_*` calls. The initial-deploy path and the
   reconcile path use the same `topology_to_nftables_config`
   builder.
2. **Per-rule keys are deterministic and stable.** Re-running
   `apply` on an unchanged topology produces an empty
   `NftablesDiff` (`change_count == 0`).
3. **Atomic apply.** Verified by an integration test that
   races a packet send against the apply and confirms
   no `prerouting` chain ever has fewer rules than expected
   mid-flight.
4. **`apply_nftables_diff` shrinks to**:

   ```rust
   let desired_cfg = topology_to_nftables_config(node, …);
   let diff = desired_cfg.diff(&conn).await?;
   let report = diff.apply_reconcile(&conn, opts).await?;
   tracing::info!(…, "nft reconcile: {} ops in {} attempt(s)", …);
   ```

5. **Foreign rules survive.** A rule without the
   `nlink-lab:` comment prefix is never removed by the
   diff.

---

## Per-rule key schema

A key is a colon-separated UTF-8 string. Pattern:

```
nlink-lab:<kind>:<chain>:<index>[:<discriminator>]
```

Where:

- `<kind>` is `fw` (firewall rule) or `nat` (NAT rule).
- `<chain>` is `input`, `forward`, `prerouting`,
  `postrouting`.
- `<index>` is the 0-based ordinal of the rule inside its
  list in the NLL source — preserves user intent across
  reapplies even when an early rule is edited.
- `<discriminator>` is optional, used only when two rules in
  the same `(kind, chain, index)` slot would otherwise
  collide (currently impossible — guard with a `debug_assert`
  on the builder side).

Examples:

```
nlink-lab/fw/input/0
nlink-lab/fw/forward/3
nlink-lab/nat/postrouting/0
nlink-lab/nat/prerouting/1
```

Why ordinal-based rather than content-hash:

- Stable across `match` text rewrites (`tcp dport 80` →
  `tcp dport 8080` should be a `replace`, not a `delete`
  + `add` — the user clearly wanted the same rule edited).
- Independent of nft expression byte-equivalence quirks
  (Plan 178 in nlink papered over the worst ones but key-
  based identity is more robust).

The discriminator slot exists to absorb future expansions
(e.g. lab-versioned keys). Leave it unused at v1.

---

## Phases

### ~~Phase 0 — Upstream `DeclaredChainBuilder::chain_type` to nlink~~ ✅ Shipped in nlink 0.18.0

Plan 180 in the nlink repo. Delete this phase from the
plan — `chain_type` is available; Phase 2 is unblocked.

### Phase 1 — Firewall reconcile (1 day, P1)

#### 1.1 New helper `firewall_config_for_node`

Add to `crates/nlink-lab/src/deploy.rs` (near
`apply_firewall`):

```rust
/// Build the declarative `NftablesConfig` for a single node's
/// firewall rules. Mirrors the imperative shape `apply_firewall`
/// emitted in 0.4.x, but as a diff-able declaration.
fn firewall_config_for_node(
    fw: &crate::types::FirewallConfig,
) -> nlink::netlink::nftables::config::NftablesConfig {
    use nlink::netlink::nftables::config::NftablesConfig;
    use nlink::netlink::nftables::types::{ChainType, Family, Hook, Policy, Priority};

    let policy = match fw.policy.as_deref() {
        Some("drop") => Policy::Drop,
        _ => Policy::Accept,
    };

    let mut cfg = NftablesConfig::new();
    cfg = cfg.table("nlink-lab", Family::Inet, |t| {
        let t = t
            .chain("input", |c| {
                c.hook(Hook::Input)
                    .priority(Priority::Filter)
                    .policy(policy)
            })
            .chain("forward", |c| {
                c.hook(Hook::Forward)
                    .priority(Priority::Filter)
                    .policy(policy)
            });

        let mut t = t;
        for (idx, rule) in fw.rules.iter().enumerate() {
            let action = rule.action.as_deref().unwrap_or("accept");
            let match_expr = rule.match_expr.as_deref().unwrap_or("");
            let key = format!("nlink-lab/fw/input/{idx}");
            t = t.rule_keyed("input", &key, |r| {
                let r = if !match_expr.is_empty() {
                    apply_match_expr(r, match_expr)
                        .expect("rule lowering must succeed")
                } else { r };
                match action {
                    "drop" => r.drop(),
                    _      => r.accept(),
                }
            });
        }
        t
    });
    cfg
}
```

Note the `.expect()` — pushing parse failures back through
the closure return is non-trivial in 0.17's builder shape.
Mitigation: pre-validate `match_expr` in `validator.rs` so
deploy-time lowering can never fail. (One-line hardening, in
scope.)

#### 1.2 New `apply_firewall_declarative`

Replaces the existing `apply_firewall` body:

```rust
async fn apply_firewall(
    node_handle: &NodeHandle,
    node_name: &str,
    fw: &crate::types::FirewallConfig,
) -> Result<()> {
    use nlink::netlink::Nftables;
    use nlink::netlink::nftables::config::ReconcileOptions;

    let nft_conn: Connection<Nftables> = node_handle.connection()
        .map_err(|e| Error::deploy_failed(
            format!("nftables connection for '{node_name}': {e}")
        ))?;

    let cfg = firewall_config_for_node(fw);
    let diff = cfg.diff(&nft_conn).await
        .map_err(|e| Error::Firewall {
            node: node_name.into(),
            detail: format!("diff: {e}"),
        })?;
    let report = diff
        .apply_reconcile(&nft_conn, ReconcileOptions::default())
        .await
        .map_err(|e| Error::Firewall {
            node: node_name.into(),
            detail: format!("apply: {e}"),
        })?;
    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count,
        "nftables firewall reconcile"
    );
    Ok(())
}
```

#### 1.3 Update `apply_nftables_diff` (incremental)

`apply_nftables_diff` already calls `apply_firewall` for
each changed node. With 1.2 in place, the
"delete-table-then-rebuild" step becomes redundant for
firewall-only changes. Adjust the function:

```rust
async fn apply_nftables_diff(
    running: &mut RunningLab,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    if diff.nftables_changed.is_empty() { return Ok(()); }
    for change in &diff.nftables_changed {
        let handle = node_handle_for(running, &change.node)?;
        // No more del_table call. apply_firewall + apply_nat
        // do their own per-rule reconcile via diff+apply.
        if let Some(fw)  = &change.desired_firewall { apply_firewall(&handle, &change.node, fw).await?; }
        if let Some(nat) = &change.desired_nat      { apply_nat(&handle, &change.node, nat).await?; }
        if change.was_present
            && change.desired_firewall.is_none()
            && change.desired_nat.is_none()
        {
            // Edge case: the node had a ruleset, now wants none.
            // Apply an empty NftablesConfig (deletes our table
            // atomically via the diff's tables_to_delete path).
            apply_firewall(&handle, &change.node, &Default::default()).await?;
        }
    }
    Ok(())
}
```

### Phase 2 — NAT reconcile (1 day, P1) — unblocked by nlink 0.18

Same pattern as 1.1/1.2 but for `apply_nat`. The keying
scheme is `nlink-lab/nat/<chain>/<idx>/<kind>` to absorb the
multi-variant NAT rule (`Masquerade` / `Snat` / `Dnat`)
inside a single index slot if needed.

`nat_config_for_node` declares the chains via:

```rust
.chain("prerouting",  |c| {
    c.hook(Hook::Prerouting)
        .priority(Priority::DstNat)
        .chain_type(ChainType::Nat)    // landed in nlink 0.18 (Plan 180)
})
.chain("postrouting", |c| {
    c.hook(Hook::Postrouting)
        .priority(Priority::SrcNat)
        .chain_type(ChainType::Nat)
})
```

Without `chain_type(ChainType::Nat)` the kernel would default
the chains to `ChainType::Filter` and reject any
`masquerade`/`snat`/`dnat` verdict with `EOPNOTSUPP` (the
entire batch then rolls back). This is exactly the bug that
motivated nlink Plan 180.

### Phase 3 — Initial-deploy unification (0.5 day, P2)

Once Phases 1+2 are in, the initial deploy
(`deploy_with_config` in `deploy.rs`) also calls
`apply_firewall` + `apply_nat` per node. With those routed
through `diff().apply()`, the initial deploy and apply
paths share a single code path — fewer divergent behaviors
to maintain.

No code change required for this phase if Phases 1+2 land
the helpers at the right level. Phase 3 is just deleting
the now-redundant comment in `deploy.rs:2905-2909` and
updating the function's docstring.

---

## Tests

### Unit tests (no root required)

In `crates/nlink-lab/src/deploy.rs`'s existing `#[cfg(test)]
mod tests`:

| Test | Description |
|------|-------------|
| `firewall_config_key_schema` | Build a `FirewallConfig` with 3 rules; call `firewall_config_for_node`; walk the resulting `NftablesConfig` and assert each rule's key matches `nlink-lab/fw/input/{0,1,2}`. |
| `firewall_config_drop_policy_propagates` | Policy "drop" → both `input` and `forward` chains get `Policy::Drop`. |
| `firewall_config_empty_rule_list_emits_chains_only` | Empty `rules` vec still produces 2 chains with the right policy + zero rules. |
| `nat_config_key_schema` | Same shape for the NAT helper, assert keys for masquerade / snat / dnat all distinct and discoverable. |

### Integration tests (root-gated, runs under `integration-tests.yml`)

Add to `crates/nlink-lab/tests/integration.rs` (already
configured with `#[lab_test]` macro that early-exits on
non-root).

| Test | Description |
|------|-------------|
| `nftables_reapply_is_zero_ops` | Deploy a topology with 3 firewall rules. Call `apply_with_same_topology()`; assert the second apply's `ReconcileReport.change_count == 0`. |
| `nftables_rule_edit_replaces_in_place` | Deploy topology with `dport 80`. Edit NLL to `dport 8080`. Apply. Assert `nft list ruleset` shows the new port AND no transient empty-chain window (sample via `ip netns exec lab-foo nft list chain inet nlink-lab input` immediately after `apply` returns). |
| `nftables_foreign_rule_survives_apply` | Deploy minimal lab. Run `nlink-lab exec node -- nft add rule inet nlink-lab input tcp dport 9999 accept` (no `comment`). Re-apply the lab's original NLL. Assert the foreign rule is still in `nft list ruleset` after `apply`. |
| `nftables_remove_firewall_clears_table` | Deploy lab WITH firewall. Apply lab WITHOUT firewall. Assert `nft list table inet nlink-lab` returns ENOENT (or empty if the table is auto-recreated by something else). |
| `nat_masquerade_reapply_is_zero_ops` | Same as `nftables_reapply_is_zero_ops` but for a NAT-only topology. |

Each integration test is decorated with `#[lab_test]` so it
acquires + tears down a fresh lab automatically.

### CI gate

`.github/workflows/integration-tests.yml` (or whatever the
existing root-gated job is) already covers the integration
test surface. No new workflow needed — the tests inherit
the existing privileged-runner allocation. Confirm by
opening the existing workflow file (referenced in
`crates/nlink-lab/CLAUDE.md`'s "Deployment Sequence" header
indirectly; the simpler check is `gh run list` after the PR
opens).

---

## Acceptance

- `cargo test -p nlink-lab --lib deploy::tests::firewall_config_*`
  passes (4 new unit tests).
- Root-gated integration test surface passes locally
  (`sudo cargo test -p nlink-lab --test integration
  nftables_ nat_`).
- A re-apply of an unchanged topology that uses firewall
  rules logs `nftables firewall reconcile attempts=1
  changes=0`.
- `apply_nftables_diff` no longer contains
  `nft_conn.del_table`.
- The TODO comment at `deploy.rs:2905-2909` ("requires
  upstreaming a per-rule diff API to nlink") is removed.
- CHANGELOG `[Unreleased]` entry under **Changed**:
  > nlink-lab now reconciles nftables firewall + NAT rules
  > per-rule via nlink 0.17's `NftablesConfig` declarative
  > API. Re-deploying an unchanged ruleset makes zero kernel
  > calls; editing a single rule in-place no longer causes
  > a transient empty-chain window.

---

## Out of scope

- **Spawned-process reconcile.** Same scope decision as
  Plan 152 — restarting a `node.exec` block on apply is
  not handled. Rule-by-rule reconcile of nftables is
  independent of this.
- **`PerHostLimiter::reconcile`.** The
  `apply_rate_limits_diff` path at `deploy.rs:2962` still
  uses the coarse full-HTB-rebuild approach. The reconcile
  primitive **already exists upstream** at
  `crates/nlink/src/netlink/ratelimit.rs:749` (mirror of
  `PerPeerImpairer::reconcile`). Switching nlink-lab over
  is a separate plan (159 candidate). The PR A cleanup
  pass should at minimum delete the stale `deploy.rs:2956-
  2961` comment that claimed this was missing.
- **Multi-table support.** nlink-lab always writes to a
  single table `nlink-lab` per namespace. Supporting
  user-controlled multi-table configs is out of scope.
- **`nft list ruleset --json` for `nlink-lab inspect`.**
  Useful CLI affordance, but lives in its own follow-up.

---

## Files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | `nlink = "0.18"` bump (paired with 158b/c/d into one commit). Already landed in `b60b5855..96a7f51c` chain — see the realignment commit. |
| `crates/nlink-lab/src/deploy.rs` | New `firewall_config_for_node` + `nat_config_for_node` builders. Rewrite of `apply_firewall` (~1625-1709), `apply_nat` (~1827-1922), and `apply_nftables_diff` (~2900-2947) bodies. Delete the "full-rebuild" docstring. ~+250 / −100 LOC. |
| `crates/nlink-lab/src/error.rs` | No change (existing `Error::Firewall` variant already fits). |
| `crates/nlink-lab/src/validator.rs` | Add early validation of `match_expr` strings so deploy-time lowering can `.expect()` safely. |
| `crates/nlink-lab/tests/integration.rs` | 5 new `#[lab_test]` integration tests (see Tests section). |
| `crates/nlink-lab/src/deploy.rs` (tests mod) | 4 new unit tests for the config builders. |
| `CHANGELOG.md` | New entry under `[Unreleased] → Changed` (see Acceptance). |
| `docs/plans/README.md` | Mark Plan 158a status after ship. |

### ~~Upstream coordination (Phase 0, separate repo)~~

Shipped in nlink 0.18.0 — Plan 180. No separate-repo work
required.
