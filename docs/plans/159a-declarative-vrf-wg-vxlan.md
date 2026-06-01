# Plan 159a — declarative VRF + WireGuard + VXLAN (158e Slice 4)

**Date:** 2026-05-31
**Status:** Proposed
**Effort:** Medium (3–4 days, splittable into 3 phases by link kind)
**Priority:** P1 — closes the last 158e gap, removes 6 imperative
deploy steps, and is the prerequisite for Plan 159c (`Stack`
adoption).

---

## TL;DR

Plan 158e Slices 1+2+3 moved bridges, dummies, veths, addresses,
routes, qdiscs, bond enslave, VLAN, and macvlan/ipvlan into the
declarative `NetworkConfig::diff().apply()` path. Slice 4 was
**deferred** because nlink 0.18 lacked declarative builders for
VRF, WireGuard, and the VXLAN `local`/`port`/`underlay_dev`
setters. nlink 0.19 ships all three (Plans 190 §2.1 / 2.3 + Plan
196 upstream).

This plan moves three imperative deploy steps into declarative
ones:

| Today (0.18 + Slices 1–3) | After 159a |
|---|---|
| **Step 6 Vxlan** — `conn.add_link(VxlanLink::new(name, vni).local(...).remote(...).port(...))` followed by `conn.set_link_mtu(...)` | Built into the `NetworkConfig` returned by `topology_to_network_config` (step 11c) |
| **Step 6b** — `conn.add_link(VrfLink::new(name, table))` + step 10c's `conn.set_link_master(iface, vrf)` | Built into `NetworkConfig` via `LinkBuilder::vrf(table)` + `LinkBuilder::master(vrf)` (step 11c) |
| **Step 6c** — `conn.add_link(WireguardLink::new(name))` + step 10d's two-pass `wg_conn.set_device(...)` calls | Built into a new `WireguardConfig` returned by `topology_to_wireguard_config`; applied via a new `apply_wireguard_for_node` after step 11c |

After 159a, the deploy sequence shrinks from 18 numbered steps
to **13 actual mutation steps + 5 no-op markers**. Steps 6
(Vxlan branch), 6b, 6c, 10c, and 10d become no-op markers.

The Vxlan + VRF work folds cleanly into the existing
`topology_to_network_config` builder. WireGuard needs its own
`topology_to_wireguard_config` (because WG is GENL, not
RTNETLINK — a separate protocol family) and its own
`apply_wireguard_for_node` per-namespace applier mirroring the
shape of `apply_network_config_for_node` and
`apply_nftables_for_node`.

---

## Audit — what 0.19 ships (citations to `/home/mpardo/git/rip/`)

### VXLAN — gap closed

`crates/nlink/src/netlink/link/builder.rs` — `LinkBuilder` gains:

- `vxlan_local(IpAddr)` — sets the underlay source address
- `vxlan_port(u16)` — sets the UDP destination port
- `vxlan_underlay_dev(&str)` — pins the underlay device

Existing 0.18 setters already covered: `vxlan(vni)`,
`vxlan_remote(IpAddr)`, `vxlan_group(IpAddr)` (multicast).

**What's still missing:** `vxlan_ttl`, `vxlan_tos`, `vxlan_learning`,
`vxlan_l2miss`, `vxlan_l3miss`. nlink-lab's NLL doesn't surface
any of these — out of scope.

### VRF — gap closed

`crates/nlink/src/netlink/link/builder.rs` — `LinkBuilder::vrf(table: u32)`.

The full VRF declarative pattern:

```rust
NetworkConfig::new()
    .link("vrf-blue", |b| b.vrf(100))       // create VRF link with table 100
    .link("eth0", |b| b.master("vrf-blue")) // enslave eth0
    .address("eth0", "10.0.0.1/24".parse()?)
    .route(...)                              // routes get table 100 via master
```

`DeclaredLinkType` (the public enum that `LinkBuilder` produces)
now includes a `Vrf { table }` variant. `apply_diff`'s
topo-sort guarantees `vrf-blue` is created before any link with
`.master("vrf-blue")`.

**What's still missing:** VRF route-table knobs — `Plan 158e
docs/Slice 3 §6.2 12b`. The old imperative
`add_route_with_table(conn, dest, route, table, vrf_name)` set
`rtm_table` on the `RouteMessage` directly. `NetworkConfig`
routes today don't expose `rtm_table`. **Slice 4 keeps step 12b
imperative** for VRF routes only — addresses move declarative,
non-VRF routes stay declarative, VRF routes still go through
the imperative path. Document this as a known gap; file a
future upstream ask for `RouteBuilder::table(u32)`.

### WireGuard — full declarative model (Plan 196 upstream)

`crates/nlink/src/netlink/genl/wireguard/config.rs` — the
`WireguardConfig` builder:

```rust
let cfg = WireguardConfig::new()
    .device("wg0", |d| d
        .private_key(priv_key)
        .listen_port(51820)
        .peer(|p| p
            .public_key(peer_pub)
            .endpoint("10.0.0.2:51820".parse()?)
            .allowed_ip(AllowedIp::v4(net, 24))
            .persistent_keepalive(Duration::from_secs(25))))
    .device("wg1", |d| d.private_key(other_key));
```

API shape (lines 191–296 in `config.rs`):

- `WireguardConfig::new() -> Self`
- `.device(name, |b| b.private_key(k).listen_port(p).peer(|p| ...)) -> Self`
- `async fn diff(&self, conn: &Connection<Wireguard>) -> Result<WireguardConfigDiff>`
- `async fn apply(&self, conn) -> Result<WireguardApplyResult>` — diff inside
- `async fn apply_reconcile(&self, conn) -> Result<WireguardApplyResult>` — atomic-batched

`WireguardConfigDiff` (lines 629–697) carries `devices_to_add`,
`devices_to_modify` (with peer-level `peers_to_add` /
`peers_to_modify` / `peers_to_remove` `DeviceChanges` sub-fields),
and `devices_to_remove`. Implements `Display` for human output.

`DeclaredWgPeerBuilder` (line 571) — superset of the 0.18
imperative `WgPeerBuilder`:

- `.preshared_key([u8; 32])` ← new in 0.19, NLL doesn't expose yet
- `.endpoint(SocketAddr)`
- `.persistent_keepalive(Duration)`
- `.allowed_ip(AllowedIp)`

`DeclaredWgDeviceBuilder` (line 415):

- `.private_key([u8; 32])`
- `.listen_port(u16)`
- `.fwmark(u32)` ← new in 0.19, NLL doesn't expose

`WG_KEY_LEN` = 32 (same as 0.18; no change).

### Important caveat — peer cross-references

`WireguardConfig` is per-device. It does NOT cross-validate
that peer A's `allowed_ip` matches peer B's address space, or
that peer A's `endpoint` is reachable. This is identical to
0.18's `wg_conn.set_device` semantics — we already build the
peer cross-reference in `deploy.rs` (the two-pass: pass 1
generates keys + collects public keys; pass 2 uses pass-1's
keys to resolve `peer_node_name → public_key`). 159a keeps this
two-pass structure; only the *application* call collapses.

---

## What changes — file-by-file

### `crates/nlink-lab/src/deploy.rs`

**Step 6 Vxlan branch** (lines ~562–593): delete the entire
`Some(InterfaceKind::Vxlan) => { … }` arm. Replace with the
same no-op pattern Slices 2/3 use:

```rust
Some(InterfaceKind::Vxlan) => {
    // Plan 159a Slice 4 — VXLAN creation absorbed into the
    // declarative NetworkConfig path (step 11c, uses
    // LinkBuilder::vxlan + vxlan_local + vxlan_port +
    // vxlan_underlay_dev from nlink 0.19).
}
```

**Step 6 MTU set** (lines ~620–628): the conditional
`iface_config.kind == Some(InterfaceKind::Vxlan)` survives
only because Vxlan was the last kind still created here. After
Slice 4 this whole block is dead. Delete it; MTU goes through
`LinkBuilder::mtu` in `topology_to_network_config`.

**Step 6b VRF block** (lines ~698–725): delete the
`add_link(VrfLink::new(...))` + `set_link_up(vrf_name)` calls.
Same no-op marker shape.

**Step 6c WG block** (lines ~727–747): delete the
`add_link(WireguardLink::new(wg_name))` calls. Same no-op
marker shape. **Note:** the actual GENL `set_device` calls move
later (step 10d) — see below.

**Step 10c VRF enslave block** (lines ~787–806): delete the
`set_link_master(iface, vrf_name)` calls. Same no-op marker.

**Step 10d WG configure block** (lines ~808–945): the two-pass
key generation **stays** (we need pubkeys collected before peer
configs reference them). But the per-pass `wg_conn.set_device`
calls go away. Replace with:

```rust
// ── Step 10d: Apply WireguardConfig per namespace ──────
//
// Plan 159a Slice 4 — collapse the two-pass set_device
// imperative path into a single per-namespace
// WireguardConfig::diff().apply() call. Key generation
// (pass 1) and peer-resolution (pass 2) still build a
// HashMap<node, HashMap<wg_name, [u8; 32]>>; the third
// pass below converts that into a WireguardConfig and
// applies it.

let wg_public_keys = build_wg_public_key_map(topology)?;

for (node_name, node) in &topology.nodes {
    if node.wireguard.is_empty() {
        continue;
    }
    let cfg = topology_to_wireguard_config(
        node_name,
        node,
        topology,
        &wg_public_keys,
    )?;
    apply_wireguard_for_node(
        &node_handles[node_name],
        node_name,
        cfg,
    ).await?;
}
```

The two-pass key-build extracts into a helper
`build_wg_public_key_map(topology) -> Result<WgKeys>` that
collects every node's `(wg_name, public_key)` pair from
`wg_config.private_key` (or generates an auto key).

**Step 11c `topology_to_network_config` call site** (line ~983):
no change to the call shape, but the implementation grows three
new kinds. See below.

**Step 12b VRF routes** (lines ~988–1011): **stays imperative**.
`RouteBuilder` lacks a `.table(u32)` setter. File as a future
upstream ask.

### `crates/nlink-lab/src/deploy.rs` — `topology_to_network_config`

The two-pass builder (Pass 1: parents, Pass 2: VLAN children)
becomes a three-pass builder:

**Pass 1 — base links (no parent refs):**
- Bridge, Dummy, Bond, Veth (existing)
- **VRF — new** (no parent ref; the master table is a u32)
- **Vxlan — new** (parent is the underlay dev, ordered before any link that references it)

**Pass 2 — parent-dependent links:**
- VLAN (existing — VLAN's parent is another link in the config)

**Pass 3 — master refs:**
- VRF enslave (set master on every iface listed in `vrf_config.interfaces`)

The pass ordering matters because `NetworkConfig::apply`'s
topo-sort needs every parent to exist when the child is
declared. The three-pass structure makes the constraint
explicit in nlink-lab code; the upstream topo-sort makes the
constraint robust at apply time.

```rust
// Pass 1.x — VRF
for (vrf_name, vrf_config) in &node.vrfs {
    cfg = cfg.link(vrf_name, |b| b.vrf(vrf_config.table));
}

// Pass 1.y — Vxlan (must come before any link with .master(vxlan_name))
for (iface_name, iface_config) in &node.interfaces {
    if iface_config.kind != Some(InterfaceKind::Vxlan) { continue; }
    let vni = iface_config.vni.ok_or_else(|| { ... })?;
    cfg = cfg.link(iface_name, |b| {
        let mut b = b.vxlan(vni);
        if let Some(local) = &iface_config.local {
            let addr: Ipv4Addr = local.parse()?;
            b = b.vxlan_local(IpAddr::V4(addr));
        }
        if let Some(remote) = &iface_config.remote {
            let addr: Ipv4Addr = remote.parse()?;
            b = b.vxlan_remote(IpAddr::V4(addr));
        }
        if let Some(port) = iface_config.port {
            b = b.vxlan_port(port);
        }
        if let Some(underlay) = &iface_config.underlay {
            b = b.vxlan_underlay_dev(underlay);
        }
        if let Some(mtu) = iface_config.mtu {
            b = b.mtu(mtu);
        }
        b
    });
}

// Pass 3 — VRF enslave
for (vrf_name, vrf_config) in &node.vrfs {
    for iface in &vrf_config.interfaces {
        cfg = cfg.link(iface, |b| b.master(vrf_name));
    }
}
```

**VLAN parent edge case** — Slice 3 already orders VLAN after
Dummy/Veth/Bridge via two-pass. Adding Vxlan and VRF to Pass 1
keeps that intact; the VLAN parent ref still resolves.

**Master-of-VRF-of-VLAN edge case** — a VLAN child enslaved to
a VRF needs the VRF up first. Pass 1 (VRF) → Pass 2 (VLAN) →
Pass 3 (master). Order is correct.

### `crates/nlink-lab/src/deploy.rs` — new `topology_to_wireguard_config`

```rust
fn topology_to_wireguard_config(
    node_name: &str,
    node: &Node,
    topology: &Topology,
    public_keys: &HashMap<String, HashMap<String, [u8; 32]>>,
) -> Result<WireguardConfig> {
    use nlink::netlink::genl::wireguard::{
        AllowedIp, WireguardConfig,
    };

    let mut cfg = WireguardConfig::new();

    for (wg_name, wg_config) in &node.wireguard {
        let private_key = resolve_wg_private_key(wg_config)?;
        cfg = cfg.device(wg_name, |mut d| {
            d = d.private_key(private_key);
            if let Some(port) = wg_config.listen_port {
                d = d.listen_port(port);
            }
            // Per-peer config.
            for peer_node_name in &wg_config.peers {
                let peer = build_peer_for(
                    node_name, wg_name, peer_node_name,
                    topology, public_keys,
                )?;
                d = d.peer(|p| {
                    p.public_key(peer.public_key)
                     .allowed_ips(peer.allowed_ips)
                     .endpoint_opt(peer.endpoint)
                });
            }
            d
        });
    }
    Ok(cfg)
}
```

The `resolve_wg_private_key` helper preserves the current
behavior: `"auto"` or `None` → generate; explicit base64 →
decode. Same as today's pass-1 logic.

`build_peer_for(node, wg, peer_node, topology, keys)` resolves
the peer's pubkey, endpoint, and allowed_ips. Same as the
inner loop of today's pass 2.

### `crates/nlink-lab/src/deploy.rs` — new `apply_wireguard_for_node`

Mirrors `apply_network_config_for_node` shape:

```rust
async fn apply_wireguard_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    cfg: WireguardConfig,
) -> Result<()> {
    let conn = node_handle.wireguard_connection().await.map_err(|e| {
        Error::deploy_failed(format!(
            "WireguardConfig connection on '{node_name}': {e}"
        ))
    })?;
    let report = cfg.apply_reconcile(&conn).await.map_err(|e| {
        Error::deploy_failed(format!(
            "WireguardConfig::apply_reconcile on '{node_name}': {e}"
        ))
    })?;
    tracing::info!(
        node = %node_name,
        report = ?report,
        "applied WireguardConfig",
    );
    Ok(())
}
```

`apply_reconcile` (vs plain `apply`) gives us the atomic
"validate the whole config before any mutation" semantics that
matches what nftables already does.

### `crates/nlink-lab/src/deploy.rs` — `compute_layered_diff`

After the WG builder lands, `compute_layered_diff` extends to
cover the WG layer:

```rust
pub async fn compute_layered_diff(...) -> Result<LayeredDiff> {
    // ... existing network + nftables loops ...

    let wg_keys = build_wg_public_key_map(desired)?; // sync-only; no kernel touch
    let mut wireguard = HashMap::new();
    for (node_name, node) in &desired.nodes {
        if node.wireguard.is_empty() { continue; }
        let handle = match node_handle_for(running, node_name) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let cfg = topology_to_wireguard_config(
            node_name, node, desired, &wg_keys,
        )?;
        let wg_conn = handle.wireguard_connection().await?;
        let diff = cfg.diff(&wg_conn).await?;
        wireguard.insert(node_name.clone(), diff);
    }

    Ok(LayeredDiff { topology, network, nftables, wireguard })
}
```

**Caveat — key generation in `--check`:** `build_wg_public_key_map`
generates an auto key for `wg_config.private_key == None`.
Doing this during `--check` means each invocation generates a
*new* key, which would force the diff to show "peer removed +
peer added" on every check. Fix: in `--check`, read the
running lab's private keys from kernel (`wg_conn.get_device`)
and substitute. Worth a `Phase 3` of 159a — see below.

### `crates/nlink-lab/src/diff.rs`

`LayeredDiff` (Plan 158f) grows a fourth layer:

```rust
pub struct LayeredDiff {
    pub topology: TopologyDiff,
    pub network: HashMap<String, ConfigDiff>,
    pub nftables: HashMap<String, NftablesDiff>,
    pub wireguard: HashMap<String, WireguardConfigDiff>,  // NEW
}
```

`Display`, `is_empty`, `change_count` all extend to include the
WG layer.

### `crates/nlink-lab/src/validator.rs`

Plan 158e Phase 1's `validate_imperative_resource_use` warning
for VRF/WG (added in commit `f4221ec`) becomes **stale** — these
resources are now declarative. Either delete the warning or
flip the message to "uses 0.19 declarative VRF / WG / VXLAN
path".

Likely the right move is: delete the warning entirely (a
warning that exists only because of a deferred plan, now that
the plan ships).

### `crates/nlink-lab/src/types.rs`

`InterfaceConfig` might gain a `pub underlay: Option<String>`
field if not already present (for VXLAN underlay_dev). Check
the existing struct; if absent, add it + NLL DSL syntax
(`vxlan ... underlay enp3s0`).

The NLL DSL change is mechanical — see Phase 2 below.

### `crates/nlink-lab/src/parser/nll/lower.rs` (NLL DSL)

If we add `underlay` to VXLAN syntax, lower it to
`iface_config.underlay`. Minimal change.

### `bins/lab/src/main.rs`

`compute_layered_diff` callers don't need to change (the WG
layer is opaque to the CLI envelope).

If the warning in `validator.rs` is removed, the
`validate --show-ips` / `apply --check` output drops one line.
Cosmetic.

### `examples/`

`examples/wireguard.nll` (existing) — must work end-to-end.
Add to the CI test matrix if not already.

Consider adding `examples/vrf-vxlan-stack.nll` — a stack that
exercises Vxlan with `underlay_dev`, VRF enslaving the Vxlan,
and a VLAN on the VRF. Single integration test covers the full
declarative stack.

---

## Phases

### Phase 1 — VRF + VXLAN declarative (deploy + tests)

1. Audit `node.interfaces` for VXLAN call sites — confirm the
   `local`/`remote`/`port`/`underlay` fields are all on
   `InterfaceConfig`. If `underlay` missing, add the field
   + NLL syntax + lower.
2. Extend `topology_to_network_config`:
   - Pass 1.x — VRF kinds via `LinkBuilder::vrf(table)`.
   - Pass 1.y — VXLAN via `LinkBuilder::vxlan + vxlan_local + vxlan_remote + vxlan_port + vxlan_underlay_dev + mtu`.
   - Pass 3 — VRF enslave via `LinkBuilder::master(vrf_name)`.
3. Delete deploy step 6 VXLAN branch + step 6 MTU block.
4. Delete deploy step 6b VRF block.
5. Delete deploy step 10c VRF enslave block.
6. **Important** — step 12b VRF routes STAY imperative. Update
   the comment to reference the `RouteBuilder::table` upstream
   ask.
7. Unit test — `topology_to_network_config` declares a VRF +
   VLAN + a Vxlan with `local`/`port`/`underlay_dev`. Assert
   the resulting `NetworkConfig` carries all three with correct
   parent ordering.
8. Root-gated integration test (`tests/integration.rs`):
   - `slice4_vrf_vxlan_reapply_is_zero_ops` — deploy a topology
     with VRF + VXLAN, capture `cfg.diff(&conn)`, assert
     `is_empty()` on the second deploy.
   - `slice4_vxlan_underlay_dev_pinned` — `ip -j link show
     vxlan42 | jq .[0].link` matches the underlay name (sanity
     check the new setter actually lands the right field).

### Phase 2 — WireGuard declarative

1. Extract `build_wg_public_key_map(topology) -> Result<WgKeys>`
   from the current pass-1 logic in step 10d.
2. Extract `resolve_wg_private_key(wg_config)` helper
   (`"auto"`/`None` → generate, base64 → decode). Pure function.
3. Write `topology_to_wireguard_config(node_name, node, topology, wg_keys) -> Result<WireguardConfig>`.
4. Write `apply_wireguard_for_node(handle, node_name, cfg) -> Result<()>`.
5. Replace step 6c + step 10d's `wg_conn.set_device` two-pass
   loops with the new three-stage flow:
   - Pass 1: `build_wg_public_key_map` (no kernel touch).
   - Pass 2: build `WireguardConfig` per node.
   - Pass 3: call `apply_wireguard_for_node` per node.
6. Step 6c becomes a no-op marker.
7. Step 10d body is the new three-stage flow.
8. Unit test — `topology_to_wireguard_config` from
   `examples/wireguard.nll` produces a `WireguardConfig` with
   the expected `(device, peer)` pairs (test on the diff
   output's textual `Display`, not the typed shape — keeps the
   test cheap).
9. Root-gated integration test:
   - `wireguard_config_apply_idempotent` — deploy
     `examples/wireguard.nll`, capture `cfg.diff(&conn)`,
     assert `is_empty()`.
   - `wireguard_config_peer_changes_only_modify` — change the
     peer's keepalive in NLL, re-deploy, assert the diff has
     `devices_to_modify` (not `devices_to_add` + `devices_to_remove`).
10. Phase 2's `--feature wireguard` gate stays — `WireguardConfig`
    requires the upstream `wireguard` feature.

### Phase 3 — LayeredDiff + check semantics

1. Extend `LayeredDiff` with `wireguard: HashMap<String, WireguardConfigDiff>`.
2. Extend `LayeredDiff::Display`, `is_empty`, `change_count`,
   `is_no_op`.
3. Extend `compute_layered_diff` to populate the WG layer.
4. **Key-stability for `--check`:** during `--check`, fetch
   each device's private key from the running kernel
   (`wg_conn.get_device(name).await`) instead of generating a
   new auto key. If the device doesn't exist on the kernel
   side yet (newly-added node), generate as today — the diff
   will correctly show `devices_to_add`.
5. Update the JSON schema for `apply --check --json` (159d
   will fully retype this, but Phase 3 of 159a adds the
   `wireguard` field).
6. Delete the now-stale `validate_imperative_resource_use`
   warning in `validator.rs` (lines added in commit `f4221ec`).
7. Root-gated integration test:
   - `layered_diff_includes_wg_layer` — `apply --check` on a
     deployed WG topology emits a non-empty `wireguard` field;
     unchanged topology emits empty.

---

## Test plan

### Unit tests (no root, ship in `--lib`)

- `network_config_vrf_declares_link_with_table` —
  `topology_to_network_config` for a node with one VRF
  produces a `NetworkConfig` whose `links()` includes a VRF
  link with the expected table.
- `network_config_vrf_enslave_after_vrf_in_three_passes` —
  declares a VRF + an interface enslaved to it; assert iteration
  order in the resulting `NetworkConfig` puts the VRF strictly
  before the enslave (defeats HashMap iteration).
- `network_config_vxlan_local_port_underlay` — VXLAN config
  carries all three new setters when fields are present.
- `wireguard_config_from_topology_peer_pubkeys_resolve` — given
  a 3-node WG mesh in NLL, `topology_to_wireguard_config`
  produces a `WireguardConfig` whose peers reference the
  correct other-node pubkeys.
- `wireguard_config_auto_key_generation_deterministic_per_run` —
  two successive `build_wg_public_key_map` calls return the
  same keys for explicit base64 inputs but DIFFERENT keys for
  `"auto"` (i.e. RNG is hot).
- `layered_diff_includes_wireguard_layer` —
  `LayeredDiff::Display` and `change_count` correctly account
  for the WG layer.

### Root-gated integration tests (`tests/integration.rs`)

Mark with `#[cfg(target_os = "linux")] #[cfg_attr(not(...), ignore)]`
matching the existing 158a/e tests.

- `slice4_vrf_vxlan_reapply_is_zero_ops` — declares a topology
  with VRF (table 100) holding eth0 and a Vxlan on eth0.
  Deploy; capture `cfg.diff(&conn).is_empty() == true` on
  re-deploy.
- `slice4_vxlan_underlay_dev_pinned` — `ip -j link show <vxlan>`
  in the namespace; assert the JSON's `link` field matches the
  declared underlay name.
- `slice4_vrf_master_set_correctly` — `ip -j link show <iface>`
  in the namespace; assert `master == vrf-blue`.
- `wireguard_config_apply_idempotent` — deploy
  `examples/wireguard.nll`; second deploy makes zero
  `set_device` GENL calls (capture via the diff
  `is_empty()`).
- `wireguard_config_peer_keepalive_modify` — change one peer's
  keepalive; re-deploy; assert the WG layer's diff has
  exactly one `devices_to_modify` entry with one
  `peers_to_modify` sub-entry.
- `layered_diff_includes_wg_layer` — `compute_layered_diff` on
  a deployed WG topology shows a populated `wireguard` field
  pre-apply; empty post-apply.
- `slice4_examples_compile` — run `topology_to_network_config`
  + `topology_to_wireguard_config` against every
  `examples/*.nll` that uses VRF/WG/VXLAN; assert all parse +
  build without error.

### Failure-mode tests

- `vxlan_missing_vni_rejected` — declare a Vxlan without VNI;
  assert `Error::InvalidTopology`.
- `vxlan_bad_local_addr_rejected` — bad IP literal in `local`;
  assert validator catches at parse time, not deploy.
- `wireguard_no_feature_returns_clean_error` — without
  `--features wireguard`, a topology with WG fails with the
  documented "rebuild with --features wireguard" message
  (preserved from today's logic).
- `wg_peer_missing_remote_node_rejected` — peer references a
  node that doesn't exist in the topology; validator catches
  at parse, not deploy.

---

## Cross-plan notes

### Plan 159c (Stack adoption) hook

After 159a Phase 2 lands, every per-node apply path is
"network + nftables + wireguard" — exactly Stack's shape. Plan
159c uses `apply_wireguard_for_node` and replaces the three
calls (`apply_network_config_for_node`, `apply_nftables_for_node`,
`apply_wireguard_for_node`) with a single
`Stack::apply_in_namespace(name)` per node.

159a should leave the three per-layer functions PUBLIC (or at
least pub(crate)) so 159c can either replace them with a Stack
adapter or compose them directly.

### Plan 159b (watch) hook

`Connection<Wireguard>` doesn't have event subscription in 0.19
(GENL families generally don't multicast typed events the way
`Connection<Nftables>` does). 159b's watch covers
nftables + RTNETLINK only; WG drift detection is polling-based
via `WireguardWatcher` (Plan 199 upstream). If we ever expose
`lab watch --wg` it'll be a polling task using
`WireguardWatcher`, not the Plan 159b event stream.

### Plan 159d (serde schema) hook

`WireguardConfigDiff` derives `Serialize` under the `serde`
feature (per Plan 189 upstream). 159d enables this and adds the
WG layer to the typed `LayeredDiff` JSON. 159a Phase 3 adds
the field; 159d types it.

---

## Risk assessment

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `LinkBuilder::master(vrf)` topo-sort regresses VLAN-on-VRF cases | Medium — VLAN parent ordering is the documented 158e Slice 3 footgun | Low — 0.19's upstream topo-sort handles this | Phase 1 integration test `slice4_vrf_vxlan_reapply_is_zero_ops` exercises VRF + VLAN; second test for VLAN-on-VRF specifically |
| `WireguardConfig::apply_reconcile` partial failure leaves the device with some peers added | Medium | Low — upstream Plan 196 batches device + peers as one transaction at the GENL level | Pre-flight diff via `Stack::apply` semantics (when 159c lands); per-device error reporting in `apply_wireguard_for_node` |
| Key-stability bug in `--check` regenerates auto keys | High — every check would show false-positive WG diff | High if not fixed | Phase 3 step 4 — fetch running keys from kernel during `--check` |
| `vxlan_underlay_dev` rejects non-existent underlay at apply time | Low — same shape as today's kernel error | High (this is correct behavior) | Validator emits a warning if the underlay name doesn't match any other iface or known host iface |
| Removed validator warning silently breaks downstream consumers reading `--validate --show-ips --json` | Low | Low | Search for the warning text in `examples/` and `bins/`; nothing depends on it |
| Three-pass `topology_to_network_config` adds latency | Negligible — sync Rust code, no kernel touch | N/A | — |

---

## Out of scope

- **WG preshared keys, fwmark** — NLL doesn't expose; defer.
- **VRF route-table declaration** — needs upstream `RouteBuilder::table`; step 12b stays imperative.
- **VXLAN `ttl/tos/learning/l2miss/l3miss`** — NLL doesn't expose; defer.
- **WireguardWatcher** integration — see Plan 159b hook above.
- **WG peer endpoint discovery via `mgmt0`** — current `find_peer_endpoint` heuristic carries forward unchanged.

---

## Success criteria

- [ ] Deploy of `examples/wireguard.nll` end-to-end with `RUST_LOG=debug` shows ZERO `wg_conn.set_device(...)` GENL calls (all peers configured in the single `apply_reconcile`).
- [ ] Re-deploy of unchanged WG topology makes ZERO mutation calls (diff `is_empty()`).
- [ ] `slice4_vrf_vxlan_reapply_is_zero_ops` green.
- [ ] `wireguard_config_apply_idempotent` green.
- [ ] All `cargo nextest run --all-features` 100% green.
- [ ] `cargo clippy --all-features --all-targets -- -D warnings` clean.
- [ ] CHANGELOG entry under `[Unreleased]` documenting Slice 4 closure.
- [ ] CLAUDE.md "Deployment Sequence" updated — step 6 Vxlan, 6b, 6c, 10c, 10d all become no-op markers.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md) — arc context
- Plan 158e (shipped — Slices 1+2+3 in commits `4098328`,
  `5ae58a8`, `ffb0e5b`; Slice 4 deferred there and is reopened
  in this plan)
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  — item #10/#11/#12/#13 closures cited
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `crates/nlink/src/netlink/link/builder.rs` — VRF + VXLAN setters
  - `crates/nlink/src/netlink/genl/wireguard/config.rs` — `WireguardConfig`
  - `crates/nlink/src/facade/apply.rs` — `wireguard_in_namespace` (for Stack later)
