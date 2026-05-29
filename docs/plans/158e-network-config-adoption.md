# Plan 158e — `NetworkConfig` adoption for declarative RTNETLINK deploy

**Date:** 2026-05-29
**Status:** Proposed (PR E of the Plan 158 arc — new with 0.18
expansion)
**Effort:** Large (5–7 days, splittable into two phases)
**Priority:** P1 — the single largest LOC reduction
opportunity in the deploy path; biggest UX win is
idempotent re-deploy across links / addresses / routes.

---

## TL;DR

nlink 0.16+ ships `NetworkConfig` — the RTNETLINK twin of
`NftablesConfig`. Per-object identity for links / addresses /
routes / qdiscs, a `diff(&conn) → NetworkDiff`, and a
best-effort `apply(&conn, opts)`. nlink-lab's current deploy
is a careful 18-step imperative sequence (`deploy.rs:152..
1564`) where every step is hand-rolled and partial failure
leaves orphan state.

After Plan 158e, the deploy path for the **majority** of
nlink-lab's topology compresses to:

```rust
for (node, ns_name) in &namespaces {
    let conn = Connection::<Route>::new_in_namespace_path(...)?;
    let cfg = topology_to_network_config(node, topology);
    let diff = cfg.diff(&conn).await?;
    diff.apply(&conn, ApplyOptions::default()).await?;
}
```

That replaces deploy steps 4 + 5 + 6 + 6a + 9 + 10 + 10b +
12 (bridges, veths, extra interfaces, macvlan/ipvlan,
addresses, ifup, bond enslave, routes) — ~1500 LOC of
imperative deploy collapses to ~400 LOC of declarative
mapping. Idempotent re-deploys make zero kernel calls on
unchanged topologies (mirrors the nftables story Plan 158a
gives us).

**What stays imperative** (upstream gaps audited 2026-05-29):

- **Steps 3 + 3b-d** — namespace + hwsim + mgmt bridge.
  Not RTNETLINK; `NetworkConfig` doesn't claim this scope.
- **Step 6b** — VRF: upstream `LinkBuilder` doesn't model
  VRF (no `LinkBuilder::vrf()`); stays on
  `nlink::netlink::link::VrfLink`.
- **Steps 6c + 10d** — WireGuard: upstream
  `DeclaredLinkType` lacks `Wireguard`. WireGuard remains
  on the imperative `Connection::<Wireguard>` path
  (WireGuard is GENL, not RTNETLINK — a different
  protocol family entirely).
- **Step 11** — sysctls: not RTNETLINK. Stays imperative.
- **Step 13** — firewall + NAT: handled by Plan 158a
  (`NftablesConfig`).
- **Step 14** — per-pair impair: already uses
  `PerPeerImpairer::reconcile` (Plan 152).
- **Step 14b** — `PerPeerImpairer::reconcile` — already
  reconcile-driven.
- **Step 15** — rate limits: addressed by Plan 158g
  (`PerHostLimiter::reconcile`).
- **Step 16** — processes / containers: not netlink.
- **Step 17** — validate assertions: post-deploy work.

Net result: **steps 4, 5, 6, 6a, 9, 10, 10b, 12** (8 of 18
steps; the largest cohort) collapse. **Steps 14, 15 already
on reconcile primitives. Steps 3, 3b-d, 6b, 6c, 10d, 11,
13, 14b, 16, 17 stay imperative or have their own plan.**

The key UX win — and reason this is P1 — is that
`apply --check` on a deployed lab can now report
"links + addresses + routes are in sync, no changes" via
a single `NetworkConfig::diff()` call. Today
`apply --check` walks resource-by-resource through 8 hand-
rolled diff functions in `crates/nlink-lab/src/diff.rs`.

---

## Audit — what nlink 0.18 ships (cites in `/home/mpardo/git/rip/`)

### `NetworkConfig` builder surface — usable

`crates/nlink/src/netlink/config/types.rs:11-91` —
`NetworkConfig::new().link(name, |b| ...).address(dev,
cidr).route(dst, |r| ...).qdisc(dev, |q| ...)`.

`LinkBuilder` (`types.rs:277-426`) supports:

| Link kind | Builder method | Notes |
|-----------|----------------|-------|
| Dummy     | `.dummy()`     | OK |
| Veth      | `.veth(peer)`  | OK |
| Bridge    | `.bridge()`    | OK |
| VLAN      | `.vlan(parent, vid)` | OK |
| VXLAN     | `.vxlan(vni).vxlan_remote(IpAddr)` | **partial** — no local addr, no UDP port, no multicast group, no underlay device |
| macvlan   | `.macvlan(parent).macvlan_mode(MacvlanMode)` | OK |
| Bond      | `.bond().bond_mode(BondMode).miimon(ms).xmit_hash_policy(p).min_links(n)` | OK for our usage |
| IFB       | `.ifb()`       | not used by nlink-lab |
| WireGuard | — | **MISSING** — gap |
| VRF       | — | **MISSING** — gap |

Common setters: `.master(name)` (bridge enslave), `.mtu(n)`,
`.up()` / `.down()`, `.address(mac)` (sets MAC).

`NetworkConfig::diff(&conn) → Result<NetworkDiff>` lives at
`config/diff.rs:298` and:

- Links diff by `name`.
- Addresses diff by `(dev, address, prefix_len)`.
- Routes diff by `(destination, prefix_len, table)`.
- Qdiscs diff by `(dev, parent)`.
- Detects same-kind / different-options changes — e.g.
  HTB `default_class` 0x10 → 0x20 produces a
  `qdiscs_to_replace` entry (Plan 147 §4.4 in nlink).

`NetworkDiff` fields:

```rust
pub struct NetworkDiff {
    pub links_to_add: Vec<DeclaredLink>,
    pub links_to_remove: Vec<String>,
    pub links_to_modify: Vec<(String, LinkChanges)>,
    pub addresses_to_add: Vec<DeclaredAddress>,
    pub addresses_to_remove: Vec<(String, IpAddr, u8)>,
    pub routes_to_add: Vec<DeclaredRoute>,
    pub routes_to_remove: Vec<(IpAddr, u8, u32)>, // dest, prefix, table
    pub qdiscs_to_add: Vec<DeclaredQdisc>,
    pub qdiscs_to_remove: Vec<(String, QdiscParent)>,
    pub qdiscs_to_replace: Vec<DeclaredQdisc>,
}
```

`NetworkDiff::apply(&conn, ApplyOptions)`:

- **NOT ATOMIC** — RTNETLINK has no `BATCH_BEGIN/END`
  primitive. Best-effort with per-operation error handling
  via `ApplyOptions::continue_on_error`.
- Apply order (`config/apply.rs:110-116`):
  1. Create links
  2. Modify links
  3. Add addresses
  4. Add routes
  5. Configure qdiscs
  6. Remove resources (purge only)
- No `apply_reconcile` — unlike `NftablesDiff`,
  `NetworkDiff` lacks retry-on-conflict bounded-backoff.

### Per-namespace usage

`Connection::<Route>::new_in_namespace(ns_fd)` and
`new_in_namespace_path(path)` exist at
`crates/nlink/src/netlink/connection.rs:135-166`. Open the
conn in a namespace, hand to `apply_config()` — the conn's
netlink socket is bound to that ns, all ops scope there.

This is the only ergonomic shape nlink-lab needs — we
already use `connection_for(ns_name)` everywhere.

---

## Current state — what nlink-lab does today

Deploy is 18 steps in `crates/nlink-lab/src/deploy.rs`:

| Step | What | Lives at | Future |
|------|------|----------|--------|
| 3    | Create namespaces / containers | `:168` | **Stays imperative.** Not RTNETLINK. |
| 3b   | Load mac80211_hwsim | `:227` | Stays imperative. Wi-Fi. |
| 3c   | Move PHYs to namespaces | `:259` | Stays imperative. Wi-Fi GENL. |
| 3d   | Create host-reachable mgmt bridge | `:283` | **Plan 158e** — fold into per-host `NetworkConfig` on init ns. |
| 4    | Create bridge networks | `:368` | **Plan 158e.** |
| 5    | Create veth pairs | `:500` | **Plan 158e.** |
| 6    | Create additional interfaces (vxlan, bond, vlan, wg) | `:548` | **Plan 158e** (except WireGuard — stays imperative). |
| 6a   | Create macvlan/ipvlan | `:648` | **Plan 158e.** (Imperative IPVLAN — already minimal.) |
| 6b   | Create VRF | `:714` | **Stays imperative.** Upstream `LinkBuilder` lacks VRF. |
| 6c   | Create WireGuard | `:743` | **Stays imperative.** Upstream `LinkBuilder` lacks WG; WG is GENL anyway. |
| 7    | Assign interfaces to bridges (master) | (inline) | **Plan 158e** — `LinkBuilder::master`. |
| 8    | VLAN trunks on bridge ports | (inline) | **Plan 158e** if upstream supports trunks (verify) — otherwise stays imperative. |
| 9    | Set addresses | `:766` | **Plan 158e.** |
| 10   | Bring interfaces up | `:949` | **Plan 158e** — `LinkBuilder::up()`. |
| 10b  | Enslave bond members | `:970` | **Plan 158e** — `LinkBuilder::master`. |
| 10c  | Enslave to VRFs | `:991` | **Stays imperative.** (Tied to step 6b.) |
| 10d  | Configure WireGuard devices | `:1012` | **Stays imperative.** (Tied to step 6c.) |
| 11   | Apply sysctls | `:1151` | **Stays imperative.** Not RTNETLINK. |
| 11b  | Auto-generate routes from graph | `:1170` | **Plan 158e** — feeds `.route()` calls. |
| 12   | Add routes | `:1177` | **Plan 158e.** |
| 13   | Apply firewall rules | `:1226` | **Plan 158a** (`NftablesConfig`). |
| 14   | Apply impairments (PerPeerImpairer) | `:1243` | Already reconcile-driven (Plan 152). |
| 14b  | Per-pair network impairments | `:1738` | Already reconcile-driven (Plan 152). |
| 15   | Apply rate limits | (inline) | **Plan 158g** (`PerHostLimiter::reconcile`). |
| 15b  | Inject /etc/hosts entries | `:1303` | Stays imperative. Not netlink. |
| 15c  | Per-namespace DNS files | `:1311` | Stays imperative. |
| 16   | Spawn background processes | `:1323` | Stays imperative. |
| 16b  | Start Wi-Fi daemons | `:1491` | Stays imperative. |
| 17   | Run validate assertions | `:1620` | Stays imperative. Post-deploy work. |
| 18   | Write state file | `:1564` | Stays imperative. |

**Steps absorbed by 158e: 8 (3d, 4, 5, 6 partial, 6a, 9, 10, 10b, 12) — roughly 1500 LOC.**

`crates/nlink-lab/src/diff.rs` currently has 8 per-resource
diff functions (links, addresses, routes, sysctls, NAT,
firewall, network impair, rate-limit, ...). After 158a +
158e + 158g, ~5 of those (links, addresses, routes,
firewall, rate-limit) move upstream entirely. Sysctls,
NAT (folded into firewall), network impair stay.

---

## Goals

1. **A `topology_to_network_config(node: &Node, topo: &Topology) ->
   NetworkConfig` function** that maps a single node's
   topology view into a `NetworkConfig` covering: bridges
   the node owns, veths terminating on this node, macvlan/
   ipvlan on this node, addresses on all interfaces in
   this node, routes from the node's route table, qdiscs
   for non-impair/non-rate-limit cases.
2. **Per-namespace apply** — open
   `Connection::<Route>::new_in_namespace_path(...)?`,
   call `cfg.diff(&conn).await?.apply(&conn, opts).await?`.
   Wraps in nlink-lab error types via `Error::Netlink`
   (post-158b refactor) so `ext_ack` flows through.
3. **Idempotent re-deploy** — running `nlink-lab deploy`
   twice on an unchanged NLL produces zero kernel calls
   for the link/addr/route layer (modulo namespace +
   sysctls + processes, which are out of scope here).
4. **`apply --check` integration** — the existing
   `apply --check` flag walks the topology diff today via
   nlink-lab's own diff engine. Post-158e, it walks
   nlink's `NetworkDiff` directly. Plan 158f handles the
   Display.
5. **Apply ordering safety** — `NetworkConfig::apply`
   creates links before addresses before routes
   internally, so the existing nlink-lab ordering
   constraints are preserved automatically. Confirm with a
   "create-and-address-in-one-apply" integration test.
6. **Graceful coexistence with imperative VRF + WireGuard
   + sysctls** — the imperative steps must run **after**
   `NetworkConfig::apply` (so the link they enslave/route
   into already exists) but **before** the rate-limit /
   impair steps. Insert them at step 10c (VRF enslave) +
   step 10d (WG config).
7. **Validation in `crates/nlink-lab/src/validator.rs`** —
   detect at parse time any NLL feature that maps to an
   upstream `NetworkConfig` gap (VRF, WireGuard) and
   warn-but-continue with a "this resource stays
   imperative" diagnostic.

---

## Phases

### Phase 1 — Links + addresses + routes via `NetworkConfig` (3 days, P1)

#### 1.1 New module: `crates/nlink-lab/src/network_config.rs`

```rust
//! Map a topology + node into an nlink `NetworkConfig`.
//!
//! Plan 158e — partial-but-large declarative deploy path.
//! Handles: bridges, veths, macvlan, ipvlan, vxlan, bond,
//! addresses, routes, qdiscs (when not impair / rate-limit).
//!
//! NOT handled (caller stays on imperative path):
//! - WireGuard (upstream LinkBuilder lacks .wireguard())
//! - VRF (upstream LinkBuilder lacks .vrf())
//! - sysctls (not RTNETLINK)
//! - per-pair impair (uses PerPeerImpairer::reconcile already)
//! - rate-limits (uses PerHostLimiter::reconcile, Plan 158g)

use nlink::netlink::config::{NetworkConfig, LinkBuilder, RouteBuilder};
use nlink::netlink::link::{BondMode, MacvlanMode};
use nlink::netlink::nftables::types::Family;
use std::net::IpAddr;

use crate::types::{Node, Topology, Link, Network, Address};

/// Build the declarative NetworkConfig covering everything in
/// `topo` that's owned by `node_name` and expressible via
/// upstream's `LinkBuilder` / `RouteBuilder`. Resources outside
/// that vocabulary (WireGuard, VRF) are skipped here and
/// handled imperatively by the caller (step 6c / 10d / 6b).
pub fn topology_to_network_config(
    node_name: &str,
    topo: &Topology,
) -> NetworkConfig {
    let mut cfg = NetworkConfig::new();

    // Bridges this node owns (if mgmt bridge is host-side,
    // it's built separately in step 3d).
    for (net_name, net) in &topo.networks {
        // Only build the bridge once, on whichever node "owns" it.
        if net.owner.as_deref() != Some(node_name) { continue; }
        let bridge_name = net.bridge_name();
        cfg = cfg.link(&bridge_name, |b| b.bridge().up());
    }

    // Veth half-pairs terminating on this node.
    for link in &topo.links {
        let (mine, peer) = match link.endpoint_for(node_name) {
            Some((m, p)) => (m, p),
            None => continue,
        };
        cfg = cfg.link(&mine.iface, |b| {
            let b = b.veth(&peer.iface).up();
            if let Some(parent) = link.bridge_for_node(node_name) {
                b.master(&parent)
            } else {
                b
            }
        });
    }

    // macvlan / ipvlan
    for mv in topo.macvlans_on(node_name) {
        cfg = cfg.link(&mv.name, |b| b.macvlan(&mv.parent)
            .macvlan_mode(mv.mode)
            .up());
    }

    // VXLAN
    for vx in topo.vxlans_on(node_name) {
        cfg = cfg.link(&vx.name, |b| b
            .vxlan(vx.vni)
            .vxlan_remote(vx.remote)
            .up());
    }

    // Bond + enslave members (step 7 + 10b)
    for bond in topo.bonds_on(node_name) {
        cfg = cfg.link(&bond.name, |b| b.bond()
            .bond_mode(bond.mode)
            .up());
        for member in &bond.members {
            cfg = cfg.link(member, |b| b.master(&bond.name));
        }
    }

    // Addresses (step 9 collapsed)
    for (iface, addrs) in topo.addresses_on(node_name) {
        for addr in addrs {
            // Upstream returns Result<Self> for .address — invalid
            // CIDRs were caught by the validator already, so
            // unwrap is sound. Guard with a debug_assert.
            cfg = cfg.address(&iface, addr).unwrap_or_else(|e| {
                debug_assert!(false, "validator should have caught: {e}");
                NetworkConfig::new() // unreachable; keeps types aligned
            });
        }
    }

    // Routes (steps 11b + 12)
    for route in topo.routes_on(node_name) {
        cfg = match cfg.route(&route.dest_cidr, |r| {
            let mut r = r;
            if let Some(gw) = route.via { r = r.via(gw); }
            if let Some(dev) = &route.dev { r = r.dev(dev); }
            if let Some(metric) = route.metric { r = r.metric(metric); }
            r
        }) {
            Ok(c) => c,
            Err(e) => {
                debug_assert!(false, "validator should have caught: {e}");
                cfg
            }
        };
    }

    cfg
}
```

#### 1.2 New apply path: `apply_network_config_per_node`

Replaces the body of deploy steps 4 + 5 + 6 + 6a + 9 + 10 +
10b + 12 with a single per-namespace `diff().apply()`:

```rust
async fn apply_network_config_per_node(
    topology: &Topology,
    node_handles: &HashMap<String, NodeHandle>,
) -> Result<()> {
    use nlink::netlink::config::ApplyOptions;
    use nlink::netlink::Route;

    for (node_name, handle) in node_handles {
        let cfg = network_config::topology_to_network_config(node_name, topology);
        if cfg.is_empty() { continue; }

        let conn: Connection<Route> = handle.connection()
            .map_err(|e| Error::deploy_failed(format!(
                "NetworkConfig connection on '{node_name}': {e}"
            )))?;

        let diff = cfg.diff(&conn).await.map_err(|e| Error::NetlinkOp {
            op: "NetworkConfig::diff".into(),
            node: node_name.into(),
            source: e,  // post-158b
        })?;

        let opts = ApplyOptions::default();
        diff.apply(&conn, opts).await.map_err(|e| Error::NetlinkOp {
            op: "NetworkConfig::apply".into(),
            node: node_name.into(),
            source: e,  // post-158b
        })?;

        tracing::info!(
            node = %node_name,
            "NetworkConfig applied: {} links, {} addrs, {} routes",
            diff.links_to_add.len() + diff.links_to_modify.len(),
            diff.addresses_to_add.len(),
            diff.routes_to_add.len(),
        );
    }
    Ok(())
}
```

#### 1.3 Reorder deploy

New deploy outline post-158a + 158e + 158g:

```text
3   Create namespaces                   (unchanged)
3b  Wi-Fi hwsim                          (unchanged)
3c  Move PHYs                            (unchanged)
3d  Host mgmt bridge                     (unchanged — host ns)
NEW 4-12 collapsed: apply_network_config_per_node
6b  VRF interfaces                       (unchanged — gap)
6c  WireGuard interfaces                 (unchanged — gap)
10c VRF enslave                          (unchanged — gap)
10d WireGuard configure                  (unchanged — gap)
11  Sysctls                              (unchanged — not RTNETLINK)
13  Firewall + NAT                       (Plan 158a — NftablesConfig)
14  Impairments                          (Plan 152 — reconcile-driven)
14b Per-pair impair                      (Plan 152 — reconcile-driven)
15  Rate limits                          (Plan 158g — PerHostLimiter::reconcile)
15b /etc/hosts                            (unchanged)
15c DNS files                            (unchanged)
16  Background processes                 (unchanged)
16b Wi-Fi daemons                        (unchanged)
17  Validate assertions                  (unchanged)
18  Write state file                     (unchanged)
```

Removing 8 steps, adding 1. The new step does the heavy
lifting in a single declarative-config commit.

### Phase 2 — `apply --check` integration + diff Display (1 day, P2)

After 158a + 158e ship, the `nlink-lab apply --check` flag
walks two upstream diffs (`NetworkDiff` + `NftablesDiff`)
plus a small nlink-lab-side diff for sysctls / spawned
processes / per-pair impair. Plan 158f handles the
human-readable rendering.

For `--json` output, serialize the two upstream
`*Diff` structs directly (they're `#[non_exhaustive]` —
add a `serde::Serialize` derive via the new optional
`serde` feature on `nlink`, or hand-roll a shim that
projects them into nlink-lab's existing JSON envelope).

### Phase 3 — Cleanup imperative deploy code (1 day, P3)

Once Phase 1 lands and the integration tests are green,
delete the imperative implementations of steps 4, 5, 6
(non-WG/VRF), 6a, 9, 10, 10b, 12 from `deploy.rs`. That's
~1500 LOC. The `apply_diff` reconcile path in
`crates/nlink-lab/src/deploy.rs:2514` also collapses: most
of its per-resource branches route through
`apply_network_config_per_node` instead.

Update the deploy-sequence docstring in
`crates/nlink-lab/CLAUDE.md`.

---

## Tests

### Unit tests (no root)

`crates/nlink-lab/src/network_config.rs` `#[cfg(test)] mod
tests`:

| Test | Description |
|------|-------------|
| `simple_two_node_topology` | 2 nodes + 1 link with addresses; assert `NetworkConfig` has 2 veth links + 2 address entries. |
| `bridge_with_members_enslaves_via_master` | 3 nodes on a shared network; assert each member's veth ends in `.master(bridge_name)`. |
| `bond_with_members_emits_master_on_each_member` | Bond with 2 enslaved interfaces; assert both members carry `.master(bond_name)`. |
| `routes_include_default_via_default_route` | Route table includes default; assert the `NetworkConfig` uses `Ipv4Route::default_route()` (nlink 0.18 helper). |
| `macvlan_carries_parent_and_mode` | Macvlan on a parent; assert mode is wired. |
| `wireguard_node_skipped_silently` | Topology with a WireGuard interface; assert it's NOT in `NetworkConfig` (handled imperatively). |
| `vrf_node_skipped_silently` | Same shape for VRF. |
| `empty_node_produces_empty_config` | Standalone node with no interfaces. `is_empty()` true. |

### Integration tests (root-gated)

`crates/nlink-lab/tests/integration.rs`:

| Test | Description |
|------|-------------|
| `network_config_deploy_idempotent` | Deploy a 3-node lab with bridges + addresses + routes. Re-run deploy on same NLL. Confirm `NetworkDiff::is_empty()` on the second pass via instrumentation. |
| `network_config_idempotent_with_macvlan` | Same with macvlan on the parent host iface. |
| `network_config_idempotent_with_bond` | Bond + 2 enslaved veths; idempotent re-deploy. |
| `apply_after_link_edit_minimizes_changes` | Deploy; edit one address in NLL; apply. Assert exactly 1 address-add + 1 address-remove in the diff (not full rebuild). |
| `network_config_coexists_with_wireguard` | Topology with WG: deploy succeeds, WG is functional, `NetworkConfig` doesn't touch WG link. |
| `network_config_coexists_with_vrf` | Same for VRF. |
| `network_config_apply_includes_ext_ack_on_failure` | Deploy a topology with an invalid address (e.g. 256.0.0.1 — caught earlier — instead use a route via a nonexistent interface). Confirm error message includes kernel's `ext_ack`. |

Each is `#[lab_test]`-driven and runs under the existing
root-gated workflow.

---

## Acceptance

- `cargo test -p nlink-lab --lib network_config::tests` passes.
- Root-gated `cargo test -p nlink-lab --test integration network_config_` suite passes.
- A re-deploy of an unchanged topology logs `NetworkConfig applied: 0 links, 0 addrs, 0 routes`.
- Deploy step count drops from 18 to ~10 in `CLAUDE.md`'s
  deploy-sequence section (the 8 absorbed steps are gone;
  some renumbering happens).
- `crates/nlink-lab/src/deploy.rs` LOC drops by ~1200–1500.
- CHANGELOG entry under `[Unreleased] → Changed`:
  > Deploy now uses `nlink::NetworkConfig::diff().apply()`
  > for the link / address / route / bond / vxlan / macvlan
  > layer. Re-deploying an unchanged topology makes zero
  > kernel calls for that layer. WireGuard, VRF, and
  > sysctls still go through imperative paths until the
  > upstream `LinkBuilder` covers them.

---

## Out of scope

- **WireGuard declarative support.** Upstream
  `DeclaredLinkType` lacks `Wireguard`. Stays on
  `Connection::<Wireguard>` (GENL family). If upstream
  ever adds it, a follow-up plan moves WG into
  `NetworkConfig` too.
- **VRF declarative support.** Same shape — upstream
  gap. Open question: a small upstream PR adding
  `LinkBuilder::vrf(table_id)` is probably as small as
  Plan 180's `chain_type` was; could batch as a
  "nlink-lab-driven asks round 2" if Plan 158e proves out.
- **`apply_reconcile` on `NetworkConfig`.** Upstream has
  no retry-on-conflict for RTNETLINK. Less of a problem
  than for nftables since RTNETLINK ops don't have a
  batch-conflict failure mode like nft does. Acceptable
  as-is.
- **Atomic apply.** RTNETLINK has no `BATCH_BEGIN/END`.
  Per-resource error handling via
  `ApplyOptions::continue_on_error` is the best we can
  do without rolling our own apply-then-rollback layer
  (rollback is hard for routes — restoring a deleted
  route requires knowing its prior shape, which means
  doing the dump twice). Acceptable.
- **Atomicity ordering across multiple namespaces.**
  Phase 1 applies per-namespace serially. A 50-node lab
  takes N round-trips. Parallelizing across namespaces
  (the existing deploy already does this for steps
  5 + 6) is Phase 4 future work; the existing parallel-
  per-namespace deploy machinery in `deploy.rs:182`
  (the `JoinSet`) can wrap `apply_network_config_per_node`
  per-node directly.

---

## Files

| File | Change |
|------|--------|
| `crates/nlink-lab/src/lib.rs` | New `pub mod network_config;`. |
| `crates/nlink-lab/src/network_config.rs` | NEW — `topology_to_network_config` mapper. ~+400 LOC. |
| `crates/nlink-lab/src/deploy.rs` | New `apply_network_config_per_node` async fn. Delete imperative bodies of steps 4 + 5 + 6 (non-WG/VRF) + 6a + 9 + 10 + 10b + 12. Reorder so the new step runs before VRF/WG/sysctls. **~−1500 / +400 LOC.** |
| `crates/nlink-lab/src/diff.rs` | Delete link / address / route / qdisc diff helpers (now upstream). Sysctls / per-pair impair / firewall stay. **~−700 LOC.** |
| `crates/nlink-lab/src/validator.rs` | Add "stays imperative" warning when topology uses VRF or WireGuard. |
| `crates/nlink-lab/CLAUDE.md` | Update the "Deployment Sequence" section. |
| `crates/nlink-lab/tests/integration.rs` | 7 new root-gated integration tests. |
| `CHANGELOG.md` | New entry under `[Unreleased] → Changed`. |
| `docs/ARCHITECTURE.md` | Update the deploy walkthrough to reflect the collapsed steps. |
