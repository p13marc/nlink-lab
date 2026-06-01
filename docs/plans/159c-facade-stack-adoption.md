# Plan 159c — adopt `facade::Stack` for per-namespace apply

**Date:** 2026-05-31
**Status:** Proposed (blocked on Plan 159a Phase 2 — needs `WireguardConfig`)
**Effort:** Small-Medium (2 days)
**Priority:** P2 — architectural cleanup; cuts ~150 LOC of
orchestration from `deploy.rs` and gives us free pre-flight
validation. Not a behavior change visible to users.

---

## TL;DR

After Plan 159a Phase 2, every per-node apply in `deploy.rs` is
exactly three calls:

```rust
apply_network_config_for_node(handle, name, network_cfg).await?;
apply_nftables_for_node(handle, name, fw, nat).await?;
apply_wireguard_for_node(handle, name, wg_cfg).await?;
```

nlink 0.19's `facade::Stack` is a one-call orchestrator over
exactly these three layers, with pre-flight validation (every
layer's diff runs first, before any layer mutates). Plan 159c
replaces the three per-layer calls with one
`Stack::apply_in_namespace(ns)` call per node.

Net effect:

- ~150 LOC reduction across `deploy.rs` (step 11c + step 13 +
  step 10d collapse to one call site)
- Pre-flight validation we don't have today — if WG would fail,
  we know BEFORE the network layer mutates
- `StackApplyReport` aggregates per-layer outcomes for a single
  `tracing::info!` per node instead of three
- `compute_layered_diff` also collapses — `Stack::diff_in_namespace(ns)`
  returns a typed `StackDiff` that maps cleanly to `LayeredDiff`

This is "after 159a" work because Stack expects a
`WireguardConfig`; 159a is where we build one. If 159a doesn't
ship, 159c still works but only collapses two layers (network
+ nftables) — see "Stripped-down variant" below.

---

## Audit — `facade::Stack` shape (citations to `/home/mpardo/git/rip/`)

### Construction

`crates/nlink/src/facade/stack.rs:40..91`:

```rust
pub struct Stack { /* network, nftables, wireguard fields */ }

impl Stack {
    pub fn new() -> Self;
    pub fn network(self, cfg: NetworkConfig) -> Self;
    pub fn nftables(self, cfg: NftablesConfig) -> Self;
    pub fn wireguard(self, cfg: WireguardConfig) -> Self;
}
```

Builder. Any subset of layers can be set; missing layers are
treated as "no-op".

### Apply

`crates/nlink/src/facade/stack.rs:92..129`:

```rust
pub async fn apply(&self) -> Result<StackApplyReport>;
pub async fn apply_in_namespace(&self, ns: &str) -> Result<StackApplyReport>;
```

Semantics (line 84..91):

> **Pre-flight validation.** Calls `self.diff().await?` first
> to validate every layer's diff against the current kernel
> state before any mutation. Catches the high-value failure
> modes (missing kernel module, invalid key, family-resolution
> failure, permission, missing netns).

`apply_in_namespace` opens connections through
`namespace::connection_for::<P>(ns)` for each layer
(`crates/nlink/src/facade/apply.rs:37..89`):

```rust
pub async fn network_in_namespace(ns: &str, cfg) -> Result<ApplyResult> {
    let conn = namespace::connection_for::<Route>(ns)?;
    cfg.apply(&conn).await
}
// nftables + wireguard equivalents
```

### Diff

`crates/nlink/src/facade/stack.rs:133..160`:

```rust
pub async fn diff(&self) -> Result<StackDiff>;
pub async fn diff_in_namespace(&self, ns: &str) -> Result<StackDiff>;

#[derive(Debug, Default)]
pub struct StackDiff {
    pub network: Option<ConfigDiff>,
    pub nftables: Option<NftablesDiff>,
    pub wireguard: Option<WireguardConfigDiff>,
}

impl StackDiff {
    pub fn is_empty(&self) -> bool;
    pub fn change_count(&self) -> usize;
}
```

`StackDiff` maps 1:1 to the per-node entries in nlink-lab's
`LayeredDiff`. We can replace the per-protocol diff loops in
`compute_layered_diff` with a single `Stack::diff_in_namespace`
call per node.

### Report

`crates/nlink/src/facade/stack.rs:166..183`:

```rust
#[derive(Debug, Default)]
pub struct StackApplyReport {
    pub network: Option<ApplyResult>,
    pub nftables_change_count: Option<usize>,
    pub wireguard: Option<WireguardApplyResult>,
}

impl StackApplyReport {
    pub fn is_noop(&self) -> bool;
}
```

---

## Caveat — namespace handle compatibility

nlink-lab's `NodeHandle` is fd-based:

```rust
// crates/nlink-lab/src/namespace.rs (approx)
impl NodeHandle {
    fn open_ns_fd(&self) -> Result<NamespaceFd>;
    fn connection<P>(&self) -> Result<Connection<P>>;  // uses setns + the fd
    fn namespace_name(&self) -> &str;
}
```

`Stack::apply_in_namespace(ns: &str)` opens connections via
`namespace::connection_for::<P>(ns)` (name-based, not fd-based).
For nlink-lab's existing flow this is fine — every NodeHandle
exposes `namespace_name()` and the kernel can resolve it.

**Risk:** if `namespace::connection_for` resolves the name via
`/var/run/netns/<name>` and we've mounted the namespace
somewhere else, the lookup fails. Audit needed.

**Mitigation:** Phase 1 of 159c is a compatibility audit:

1. Grep for `LabNamespace::create` / `LabNamespace::open_ns_fd`
   call sites.
2. Confirm every namespace creation goes through the standard
   `ip netns add`-style mount (i.e. `/var/run/netns/<name>` is
   the canonical path).
3. If not, build an `apply_in_namespace_fd(fd: BorrowedFd, ...)`
   helper that uses `setns` on the fd; file as upstream ask if
   absent.

The codebase comment at line 56 of `deploy.rs` notes that
`open_ns_fd` returns a `NamespaceFd` for moving links across
namespaces. The standard `connection<P>()` path almost
certainly already uses the name-based lookup internally;
confirmation in Phase 1 will settle the question.

---

## What changes — file-by-file

### `crates/nlink-lab/src/deploy.rs`

#### `apply_stack_for_node` — new unified per-node applier

Replaces the three per-layer functions:

```rust
async fn apply_stack_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    network: NetworkConfig,
    fw: Option<&FirewallConfig>,
    nat: Option<&NatConfig>,
    wireguard: Option<WireguardConfig>,
) -> Result<()> {
    use nlink::facade::Stack;

    let mut stack = Stack::new();

    if !is_empty_network_config(&network) {
        stack = stack.network(network);
    }
    if fw.is_some() || nat.is_some() {
        let nft_cfg = topology_to_nftables_config(fw, nat)?;
        stack = stack.nftables(nft_cfg);
    }
    if let Some(wg) = wireguard {
        stack = stack.wireguard(wg);
    }

    let ns_name = node_handle.namespace_name();
    let report = stack.apply_in_namespace(ns_name).await.map_err(|e| {
        Error::deploy_failed(format!(
            "Stack apply on '{node_name}' (ns='{ns_name}'): {e}"
        ))
    })?;

    tracing::info!(
        node = %node_name,
        ns = %ns_name,
        net = ?report.network.as_ref().map(|r| r.changes_made),
        nft = ?report.nftables_change_count,
        wg = ?report.wireguard.as_ref().map(|r| r.devices_changed),
        no_op = report.is_noop(),
        "Stack reconcile complete",
    );

    Ok(())
}

fn is_empty_network_config(cfg: &NetworkConfig) -> bool {
    cfg.links().is_empty()
        && cfg.addresses().is_empty()
        && cfg.routes().is_empty()
        && cfg.qdiscs().is_empty()
}
```

#### `deploy.rs` step 11c + step 13 + step 10d collapse

Today (after 159a Phase 2):

```rust
// Step 11c — network
for (node_name, node) in &topology.nodes {
    let cfg = topology_to_network_config(...)?;
    apply_network_config_for_node(handle, node_name, cfg).await?;
}

// Step 12b — VRF routes (still imperative; stays)
for ... { add_route_with_table(...).await?; }

// Step 13 — nftables
for (node_name, node) in &topology.nodes {
    let fw = topology.effective_firewall(node);
    let nat = node.nat.as_ref();
    if fw.is_some() || nat.is_some() {
        apply_nftables_for_node(handle, node_name, fw, nat).await?;
    }
}

// Step 10d — WG (after 159a)
let wg_keys = build_wg_public_key_map(topology)?;
for (node_name, node) in &topology.nodes {
    if node.wireguard.is_empty() { continue; }
    let cfg = topology_to_wireguard_config(node_name, node, topology, &wg_keys)?;
    apply_wireguard_for_node(handle, node_name, cfg).await?;
}
```

After 159c — step 10d's key-build stays (synchronous), then
steps 11c + 13 + the WG apply collapse:

```rust
// Step 10d Phase 1 — key generation (sync, no kernel touch)
let wg_keys = build_wg_public_key_map(topology)?;

// Step 11c+13+10d-Phase2 — Stack apply per namespace
for (node_name, node) in &topology.nodes {
    let net = topology_to_network_config(...)?;
    let fw = topology.effective_firewall(node);
    let nat = node.nat.as_ref();
    let wg = if node.wireguard.is_empty() {
        None
    } else {
        Some(topology_to_wireguard_config(node_name, node, topology, &wg_keys)?)
    };
    apply_stack_for_node(&node_handles[node_name], node_name, net, fw, nat, wg).await?;
}

// Step 12b — VRF routes (still imperative; stays)
for ... { add_route_with_table(...).await?; }
```

Step ordering in the CLAUDE.md "Deployment Sequence" needs a
post-159c update:

- Step 10d Phase 1 → renamed "Step 10d': WG key generation (sync)"
- Step 11c → renamed "Step 11c': Stack reconcile (network + nftables + WG) per namespace"
- Step 13 → no-op marker
- Step 10d Phase 2 (kernel) → no-op marker

#### `apply_diff` (live reconcile) — same collapse

`apply_diff`'s Phase 6 (after node creation) currently calls
`apply_network_config_for_node` and (separately) the nftables
+ WG paths. Same collapse — one `apply_stack_for_node` per
newly-added node + per-modified node.

The audit point from commit `f4221ec`'s findings — Phase 6
missed non-link address sources for newly-added nodes — is
preserved because `apply_stack_for_node` runs the full network
config (not just the link-pair addresses).

#### `compute_layered_diff` — Stack::diff_in_namespace

Replace the per-protocol loops with one `Stack::diff_in_namespace`
call per node:

```rust
pub async fn compute_layered_diff(running, desired) -> Result<LayeredDiff> {
    let topology_diff = crate::diff::diff_topologies(running.topology(), desired);
    let auto_routes = if desired.lab.routing == RoutingMode::Auto {
        auto_generate_routes(desired)
    } else {
        HashMap::new()
    };

    let wg_keys = build_wg_public_key_map(desired)?;
    let mut per_node = HashMap::new();

    for (node_name, node) in &desired.nodes {
        let handle = match node_handle_for(running, node_name) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let net = topology_to_network_config(node_name, node, desired, auto_routes.get(node_name))?;
        let fw = desired.effective_firewall(node);
        let nat = node.nat.as_ref();
        let wg = if node.wireguard.is_empty() {
            None
        } else {
            Some(topology_to_wireguard_config(node_name, node, desired, &wg_keys)?)
        };

        let mut stack = Stack::new();
        if !is_empty_network_config(&net) {
            stack = stack.network(net);
        }
        if fw.is_some() || nat.is_some() {
            stack = stack.nftables(topology_to_nftables_config(fw, nat)?);
        }
        if let Some(c) = wg {
            stack = stack.wireguard(c);
        }
        let diff = stack.diff_in_namespace(handle.namespace_name()).await
            .map_err(|e| Error::deploy_failed(format!(
                "Stack diff on '{node_name}': {e}"
            )))?;
        per_node.insert(node_name.clone(), diff);
    }

    Ok(LayeredDiff { topology: topology_diff, per_node })
}
```

#### `LayeredDiff` shape

The per-protocol HashMap fields collapse into a single
`HashMap<String, StackDiff>`:

```rust
pub struct LayeredDiff {
    pub topology: TopologyDiff,
    pub per_node: HashMap<String, StackDiff>,
}
```

Or — to preserve 159d's per-layer typed serialization — keep
separate fields but populate them from `StackDiff`:

```rust
pub struct LayeredDiff {
    pub topology: TopologyDiff,
    pub network: HashMap<String, ConfigDiff>,
    pub nftables: HashMap<String, NftablesDiff>,
    pub wireguard: HashMap<String, WireguardConfigDiff>,
}

impl LayeredDiff {
    fn populate_from_stack(&mut self, node_name: &str, stack: StackDiff) {
        if let Some(n) = stack.network { self.network.insert(node_name.into(), n); }
        if let Some(n) = stack.nftables { self.nftables.insert(node_name.into(), n); }
        if let Some(w) = stack.wireguard { self.wireguard.insert(node_name.into(), w); }
    }
}
```

**Recommendation:** keep the per-protocol fields shape — 159d
serializes each layer independently in the JSON schema; users
can `jq '.network.router'` directly. The Stack-driven build is
an internal detail.

### `crates/nlink-lab/Cargo.toml`

`facade::Stack` is gated under nlink's `facade` feature (or
similar — confirm in Phase 1). If gated, add the feature:

```toml
nlink = { version = "0.19", features = ["full", "facade"] }
```

If `facade` is included in `full` (likely), no Cargo change.

### `crates/nlink-lab/src/deploy.rs` — delete dead code

The three per-layer functions become internal helpers used
only by Stack assembly, OR they get fully removed if Stack
covers every call site. Audit:

- `apply_network_config_for_node` — called from steps 11c (deploy)
  + apply_diff Phase 6. Both move to Stack → can delete.
- `apply_nftables_for_node` — called from step 13 (deploy) +
  apply_diff Phase 6. Both move to Stack → can delete.
- `apply_wireguard_for_node` (post-159a) — only called from
  step 10d Phase 2. Moves to Stack → can delete.

All three become unused. **Plan 159c Phase 3 deletes them.**
This is what surfaces the LOC reduction.

### `CLAUDE.md`

Update the "Deployment Sequence" section:

```diff
- 10d. Configure WireGuard devices
- 11. Apply sysctls per namespace
- 11b. Auto-generate routes from topology graph
- 11c. Apply `NetworkConfig::diff().apply()` per namespace
- 12. Add routes — (no-op marker)
- 12b. Add VRF routes
- 13. Apply `NftablesConfig::diff().apply_reconcile()` per namespace
+ 10d. WireGuard key generation (sync)
+ 11. Apply sysctls per namespace
+ 11b. Auto-generate routes from topology graph
+ 11c. `Stack::apply_in_namespace(ns)` per node — atomic
+      network + nftables + WG reconcile with pre-flight
+      validation. Replaces former steps 10d/11c/13.
+ 12. (no-op marker — addresses + routes in 11c)
+ 12b. Add VRF routes (still imperative — `RouteBuilder::table`
+      upstream gap)
+ 13. (no-op marker — nftables in 11c)
```

---

## Phases

### Phase 1 — namespace compatibility audit

1. Read `crates/nlink-lab/src/namespace.rs`:
   - Confirm `NodeHandle::namespace_name()` returns the same
     name that `namespace::connection_for(name)` expects.
   - Trace `LabNamespace::create` — does it mount the netns at
     `/var/run/netns/<name>` (the canonical ip-netns path) or
     somewhere else?
2. Trace nlink's `namespace::connection_for(name)`:
   - `setns(/var/run/netns/<name>)` is the standard kernel path.
3. If the paths match, Phase 1 is a doc-only PR — confirm in
   the plan that `Stack::apply_in_namespace(name)` is safe.
4. If they don't match — file an upstream ask for an
   fd-based `Stack::apply_in_namespace_fd(BorrowedFd, ...)`
   and pause 159c until that lands. The fd-based API is
   trivial upstream — nlink already has
   `namespace::connection_for_fd` shape internally for the
   nlink-lab use case.
5. Write a unit test:
   - `lab_namespace_name_resolves_via_connection_for` — create a
     namespace, attempt `nlink::namespace::connection_for::<Route>(name)`,
     assert success.

### Phase 2 — `apply_stack_for_node` + Stack wiring in deploy

1. Write `apply_stack_for_node(handle, name, net, fw, nat, wg)`.
2. Replace deploy step 11c body — single loop over nodes, build
   the per-node Stack, call `apply_stack_for_node`.
3. Mark step 13 as no-op marker.
4. Mark step 10d Phase 2 as no-op marker (Phase 1 key-gen stays).
5. Verify the per-step ordering:
   - Pre-flight: every node's stack validates before any
     mutation. **This is a semantic change** vs the current
     per-step sequential apply — today, network on node A could
     succeed while nftables on node A is about to fail, leaving
     node A inconsistent. Stack catches this in pre-flight.
6. Per-node tracing — one `tracing::info!` per node with the
   aggregated change counts.
7. Unit tests:
   - `apply_stack_for_node_skips_empty_layers` — empty network
     + no firewall + no WG = no Stack mutations, no kernel
     touch.
8. Root-gated integration test:
   - `stack_apply_idempotent_reapply` — deploy a 3-node
     topology with firewall + WG + bridges; second deploy
     reports `report.is_noop() == true` for every node.

### Phase 3 — `apply_diff` (live reconcile) Stack adoption

1. Replace `apply_diff`'s Phase 6 per-layer calls with
   `apply_stack_for_node` per newly-added / modified node.
2. Ensure newly-added nodes get the full stack (preserves the
   commit `a5c3698` fix for non-link address sources).
3. Integration test:
   - `apply_diff_via_stack_picks_up_dummy_addresses` — add a
     node with a dummy interface (no link to other nodes),
     `apply`; assert the dummy + its address land in the
     namespace. Same shape as the existing test from `a5c3698`.

### Phase 4 — `compute_layered_diff` Stack::diff adoption

1. Replace per-protocol diff loops with `Stack::diff_in_namespace`.
2. Map `StackDiff` to `LayeredDiff` per-layer fields (or refactor
   `LayeredDiff` if 159d hasn't shipped yet).
3. Integration test:
   - `compute_layered_diff_via_stack_matches_per_protocol` —
     same topology, compute via Stack and via the old per-
     protocol path, assert structurally equal. (Old path can
     stay for one release as a regression backstop, then
     delete.)

### Phase 5 — delete `apply_*_for_node` helpers + CHANGELOG

1. Confirm no remaining call sites for the three per-layer
   helpers.
2. Delete them. Net LOC reduction ~80 (the three function
   bodies).
3. CHANGELOG entry under `[Unreleased]`.
4. Update CLAUDE.md "Deployment Sequence" + the deploy.rs
   section header comments.

---

## Stripped-down variant (if 159a doesn't ship)

`Stack` works with any subset of layers. If 159a is deferred
(WG stays imperative), 159c still collapses network + nftables:

```rust
async fn apply_stack_for_node(handle, name, net, fw, nat) -> Result<()> {
    let mut stack = Stack::new();
    if !is_empty_network_config(&net) { stack = stack.network(net); }
    if fw.is_some() || nat.is_some() {
        stack = stack.nftables(topology_to_nftables_config(fw, nat)?);
    }
    stack.apply_in_namespace(handle.namespace_name()).await?;
    Ok(())
}
```

WG continues through the existing imperative step 10d. Net
LOC reduction smaller (~70), no per-flight validation across
the WG layer, but still a useful cleanup.

**Recommend:** wait for 159a; the full-stack story is the
right shape.

---

## Pre-flight validation — what we gain

Today's flow on a node with both a broken network config
(e.g. address conflict) and a broken WG config (e.g. invalid
private key):

1. Step 11c — network: apply tries, conflicts with running
   state, partial success.
2. Step 13 — nftables: applies cleanly.
3. Step 10d — WG: tries `set_device`, kernel rejects, error
   returned. Lab is now half-deployed.

Stack flow:

1. Pre-flight: `Stack::diff_in_namespace(ns)` runs every
   layer's diff. The network conflict surfaces in the
   `ConfigDiff` (specifically, `ApplyOptions::dry_run=false`
   skips actual mutation but the diff against running state
   still reports the conflict). The WG private-key
   rejection surfaces too.
2. Both errors aggregate; Stack returns one error containing
   both — no mutation made.
3. User fixes both errors; re-runs.

This is the explicit value-add Stack provides per its rustdoc
(stack.rs:84..91):

> Catches the high-value failure modes (missing kernel module,
> invalid key, family-resolution failure, permission, missing
> netns). Residual race window documented in the rustdoc.

Document this in CHANGELOG as the user-visible win.

**Residual race window:** Stack's rustdoc acknowledges that
between pre-flight and the WG layer apply, a peer could
disappear (race with concurrent mutators). nlink-lab's
deploy never sees concurrent mutators (we hold flock on the
state file), so the window is effectively closed for us.

---

## Test plan

### Unit tests

- `apply_stack_for_node_skips_empty_layers` — Stack with no
  layers is `apply().await?` no-op.
- `apply_stack_for_node_serializes_per_node_logs` —
  `tracing` output has one line per node, aggregated layer
  counts.
- `lab_namespace_name_resolves_via_connection_for` — Phase 1
  audit confirmation.

### Root-gated integration tests

- `stack_apply_idempotent_reapply` — 3-node topology, second
  deploy is no-op.
- `stack_apply_preflight_catches_bad_wg_key` — declare an
  invalid private key (e.g. 31 bytes); assert deploy fails
  BEFORE any network mutation. (Tricky: NLL validator should
  catch this earlier; if it does, demote to a unit test on
  Stack itself.)
- `apply_diff_via_stack_picks_up_dummy_addresses` — Phase 3
  regression-backstop.
- `compute_layered_diff_via_stack_matches_per_protocol` —
  Phase 4 cross-check.
- `stack_apply_partial_failure_leaves_network_unchanged` —
  intentionally inject an nft conflict; assert network layer
  is NOT touched.

### Performance

Stack adds one extra dump round-trip per node (the pre-flight
diff). On a 16-node lab that's 16 extra round-trips on the
fast path. Each is a few ms — likely under 100 ms total.
Acceptable; document.

If users push back on the latency, add a `--no-preflight`
flag that bypasses the validation. Defer until asked.

---

## Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `Stack::apply_in_namespace(name)` doesn't work with our namespace path layout | High | Low (standard `ip netns` path) | Phase 1 audit; fall back to per-layer if absent |
| Pre-flight latency on large labs | Low (~100ms for 16 nodes) | High (it's a real cost) | Document; offer `--no-preflight` if requested |
| `StackDiff` change_count differs from sum of per-protocol counts (counting bug upstream) | Low | Low | Phase 4 cross-check test |
| Removing per-layer helpers breaks downstream callers | Low — none today | Low | Grep before delete; one release with `#[deprecated]` if external callers exist |
| Stack's `is_noop()` semantics don't match nlink-lab's "no-op deploy" expectation | Medium | Low | Test: deploy unchanged topology, assert `is_noop() == true` for every node |
| `wg.devices_changed` field doesn't exist on `WireguardApplyResult` (I'm guessing the field name) | Low | Medium | Confirm in audit Phase 1; adjust the tracing call shape |

---

## Out of scope

- **Stack apply for `host`-side macvlan/ipvlan moves** — these
  are imperative cross-namespace operations; Stack doesn't
  model them.
- **Per-node parallel apply via Stack** — Stack runs layers
  sequentially within a namespace. Per-node parallelism is a
  separate cross-cutting concern; defer to a future plan.
- **Stack apply rollback on failure** — Stack docs acknowledge
  no rollback support; nlink-lab inherits the partial-apply
  semantics. Document.

---

## Success criteria

- [ ] Deploy of a 3-node topology with WG + firewall produces
  exactly ONE `tracing::info!` per node with aggregated
  layer counts.
- [ ] Re-deploy of unchanged topology — every node's
  `StackApplyReport::is_noop() == true`.
- [ ] `apply_network_config_for_node`, `apply_nftables_for_node`,
  `apply_wireguard_for_node` deleted from `deploy.rs`.
- [ ] `compute_layered_diff` reduced to a single loop with one
  Stack call per node.
- [ ] Integration tests above all green.
- [ ] `cargo clippy --all-features --all-targets -- -D warnings` clean.
- [ ] CHANGELOG entry documenting the pre-flight validation
  semantic change.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md)
- [Plan 159a](159a-declarative-vrf-wg-vxlan.md) — prerequisite (provides `WireguardConfig`)
- Plan 158e (shipped, see `CHANGELOG.md`) — original per-layer per-node design
- Plan 158f (shipped, see `CHANGELOG.md`) — `LayeredDiff` foundation
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md) — facade adoption note
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `crates/nlink/src/facade/stack.rs` — `Stack` struct
  - `crates/nlink/src/facade/apply.rs` — `*_in_namespace` helpers
  - `crates/nlink/src/facade/diff.rs` — diff helpers
