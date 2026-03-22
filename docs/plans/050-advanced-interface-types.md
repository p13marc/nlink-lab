# Plan 050: Advanced Interface Types

**Priority:** Medium
**Effort:** 2-3 days
**Target:** `crates/nlink-lab/src/deploy.rs`

## Summary

Deploy the remaining interface types that are already parsed and validated but skipped
during deployment. These enable VRF multi-tenancy, WireGuard VPN tunnels, bonded
interfaces, and VLAN sub-interfaces.

## Current State

The deployer's Step 6 handles `dummy` and `vxlan` interfaces. All other `kind` values
log a warning and are skipped. The types (`VrfConfig`, `WireguardConfig`, `InterfaceConfig`)
already exist with full serde support.

## Implementation

### VRF Interfaces + Enslavement

**Where:** `deploy.rs` Step 6, after existing interface creation loop.

```rust
// Create VRF interfaces
for (vrf_name, vrf_config) in &node.vrfs {
    conn.add_link(nlink::netlink::link::VrfLink::new(vrf_name, vrf_config.table)).await?;
    conn.set_link_up(vrf_name).await?;
}
```

After Step 10 (bring up), enslave interfaces to their VRFs:

```rust
// Enslave interfaces to VRFs
for (vrf_name, vrf_config) in &node.vrfs {
    for iface in &vrf_config.interfaces {
        conn.set_link_master(iface, vrf_name).await?;
    }
}
```

VRF routes go into Step 12 — add routes with `.table(vrf_config.table)`:

```rust
for (dest, route) in &vrf_config.routes {
    let route = Ipv4Route::new(...)
        .table(vrf_config.table)
        .gateway(...)
    conn.add_route(route).await?;
}
```

**nlink API:** `VrfLink::new(name, table)`, `set_link_master(iface, master)`

### WireGuard Interfaces

**Where:** `deploy.rs` Step 6, new match arm.

Two-phase setup:
1. Create the WireGuard netlink interface
2. Configure it via Generic Netlink (private key, listen port, peers)

```rust
// Phase 1: create the interface
conn.add_link(nlink::netlink::link::WireguardLink::new(wg_name)).await?;

// Phase 2: configure via genl
let wg_conn: Connection<Wireguard> = namespace::connection_for(ns_name)?;
wg_conn.set_device(wg_name, |d| {
    let mut d = d;
    if wg_config.private_key.as_deref() == Some("auto") {
        d = d.private_key(generate_wg_key());
    }
    if let Some(port) = wg_config.listen_port {
        d = d.listen_port(port);
    }
    d
}).await?;
```

**Key generation:** For `private_key = "auto"`, generate a random 32-byte key using
`/dev/urandom` or the `rand` crate. Store the generated key pair in the state file
so peers can reference each other.

**Peer resolution:** The `peers` field lists node names. During deployment, resolve
each peer's WireGuard public key and endpoint address (from link addresses). This
requires a two-pass approach:
1. Create all WG interfaces and generate keys
2. Configure peers with resolved keys and endpoints

**nlink API:** `WireguardLink::new(name)`, `Connection<Wireguard>::set_device()`,
`WgDeviceBuilder::private_key()`, `WgPeerBuilder::new(pubkey).endpoint(addr).allowed_ip()`

### Bond Interfaces

**Where:** `deploy.rs` Step 6, new match arm for `kind = "bond"`.

```rust
Some("bond") => {
    conn.add_link(nlink::netlink::link::BondLink::new(iface_name)).await?;
}
```

Enslavement of member interfaces happens after Step 10 (interfaces must be down to
be enslaved to a bond on some modes):

```rust
// Bond member enslavement would need a new topology field, e.g.:
// [nodes.r1.interfaces.bond0]
// kind = "bond"
// members = ["eth1", "eth2"]  // new field needed in InterfaceConfig
```

**Note:** `InterfaceConfig` currently has no `members` field. Either add one or use
a convention (e.g., bond members listed in a separate section). For MVP, just create
the bond interface — member enslavement can be a follow-up.

**nlink API:** `BondLink::new(name).mode(BondMode::ActiveBackup)`, `set_link_master()`

### VLAN Sub-interfaces

**Where:** `deploy.rs` Step 6, new match arm for `kind = "vlan"`.

VLAN interfaces need a parent interface and VLAN ID. The current `InterfaceConfig`
doesn't have a `parent` field, but we can infer it from naming conventions or add
a field.

**Option A:** Use naming convention — `eth0.100` means VLAN 100 on `eth0`.
**Option B:** Add a `parent` field to `InterfaceConfig`.

For now, use the `vni` field as the VLAN ID (it's semantically close) and require
a `parent` field to be added to `InterfaceConfig`:

```rust
// types.rs — add to InterfaceConfig:
pub parent: Option<String>,

// deploy.rs:
Some("vlan") => {
    let parent = iface_config.parent.as_deref().ok_or(...)?;
    let vid = iface_config.vni.ok_or(...)? as u16;
    conn.add_link(VlanLink::new(iface_name, parent, vid)).await?;
}
```

**nlink API:** `VlanLink::new(name, parent, vlan_id)`

### Bridge VLAN Port Configuration

**Where:** `deploy.rs` Step 4 bridge loop, after attaching members.

Currently bridges are created and members attached, but VLAN port config
(`pvid`, `tagged`, `untagged`) from `PortConfig` is not applied.

```rust
// After attaching member to bridge:
for (port_node, port_config) in &network.ports {
    for &vid in &port_config.vlans {
        let mut vlan = BridgeVlanBuilder::new(vid).dev(&peer_name);
        if port_config.tagged == Some(true) {
            // tagged — no special flags
        } else if port_config.untagged == Some(true) {
            vlan = vlan.untagged();
        }
        if Some(vid) == port_config.pvid {
            vlan = vlan.pvid();
        }
        conn.add_bridge_vlan(vlan).await?;
    }
    if let Some(pvid) = port_config.pvid {
        let vlan = BridgeVlanBuilder::new(pvid).dev(&peer_name).pvid().untagged();
        conn.add_bridge_vlan(vlan).await?;
    }
}
```

**nlink API:** `BridgeVlanBuilder::new(vid).dev(name).pvid().untagged()`

## Progress

### VRF

- [ ] Create VRF interfaces in Step 6
- [ ] Bring up VRF interfaces in Step 10
- [ ] Enslave interfaces to VRFs after Step 10
- [ ] Add VRF routes with `.table()` in Step 12
- [ ] Test: VRF topology from NLINK_LAB.md section 4.4

### WireGuard

- [ ] Create WireGuard interfaces in Step 6
- [ ] Key generation helper (`generate_wg_key() -> [u8; 32]`)
- [ ] Public key derivation from private key
- [ ] Configure WG device via `Connection<Wireguard>::set_device()`
- [ ] Peer resolution: map node names to public keys and endpoints
- [ ] Store generated keys in state file
- [ ] Test: WireGuard VPN topology from NLINK_LAB.md section 4.4

### Bond

- [ ] Create bond interfaces in Step 6
- [ ] Add `members` field to `InterfaceConfig` (optional)
- [ ] Enslave members to bond after creation

### VLAN

- [ ] Add `parent` field to `InterfaceConfig`
- [ ] Create VLAN sub-interfaces in Step 6
- [ ] Update parser tests for new field

### Bridge VLAN Ports

- [ ] Apply `BridgeVlanBuilder` per port after attaching members
- [ ] Handle pvid + untagged flags
- [ ] Test: VLAN trunk topology from NLINK_LAB.md section 4.4
