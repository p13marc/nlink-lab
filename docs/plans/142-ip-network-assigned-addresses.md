# Plan 142: Fix `ip` Command for Network-Assigned Addresses

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (2–3 hours)
**Priority:** P0 — real bug, breaks core workflow for network block users

---

## Problem Statement

`nlink-lab ip` returns empty for IPs assigned via `network` blocks with `subnet`
auto-allocation. The `node_addresses()` method only collects from `link.addresses`
and `node.interfaces`, but not from `topology.networks[].ports[].addresses`.

```nll
network lan {
    members [infra:eth0, pub:eth0, sub:eth0]
    subnet 10.1.0.0/24
}
```

After deploy, `nlink-lab ip mylab infra --iface eth0` returns an error, but
`ip addr show eth0` inside the namespace shows `10.1.0.1/24`.

## Root Cause

In `crates/nlink-lab/src/running.rs:315-373`, `node_addresses()` collects from:
1. `topology.links[].addresses` (line 326-338)
2. `topology.nodes[node].interfaces[].addresses` (line 341-350)
3. `mgmt0` from management subnet (line 352-370)

**Missing:** `topology.networks[].ports[].addresses` — where subnet auto-allocation
stores the assigned IPs after lowering.

## Implementation

### Step 1: Add network port addresses to `node_addresses()` (`running.rs`)

After the existing interface collection, add:

```rust
// From network bridge port addresses (subnet auto-allocation)
for (_net_name, network) in &self.topology.networks {
    for (member_str, port_config) in &network.ports {
        // member_str is the node name (port key)
        // But we need to match against the endpoint "node:iface" in members
        for member in &network.members {
            if let Some(ep) = EndpointRef::parse(member)
                && ep.node == node
            {
                // Check if this port has addresses
                if let Some(port) = network.ports.get(&ep.node) {
                    for addr in &port.addresses {
                        addrs.entry(ep.iface.clone()).or_default().push(addr.clone());
                    }
                }
            }
        }
    }
}
```

Actually, the port key might be the node name or the full endpoint. Need to verify
the exact key format used in `network.ports`. From the lowerer
(`lower.rs:2372-2397`), the port key is the **node name** (not endpoint string).
The interface name comes from the endpoint in `network.members`.

Simpler approach:

```rust
// From network bridge port addresses (subnet auto-allocation)
for network in self.topology.networks.values() {
    for member in &network.members {
        if let Some(ep) = EndpointRef::parse(member)
            && ep.node == node
            && let Some(port) = network.ports.get(&ep.node)
        {
            for addr in &port.addresses {
                addrs.entry(ep.iface.clone()).or_default().push(addr.clone());
            }
        }
    }
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_node_addresses_from_network` | running.rs or lower.rs | Network-assigned IPs appear in node_addresses() |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `running.rs` | +12 | Add network port address collection |
| **Total** | ~12 | |
