# Plan 123: Extended Auto-Addressing (Loopbacks, Management, Pools)

**Date:** 2026-03-31
**Status:** Implemented (2026-04-01)
**Effort:** Small (1 day)
**Priority:** P2 — reduces manual IP assignment
**Depends on:** Nothing (benefits from Plan 120 IP functions)

---

## Problem Statement

NLL has subnet pools for `/30` point-to-point link auto-assignment, but
many interfaces still need manual IPs:
- Loopback addresses (router IDs)
- Management interfaces
- Multi-homed node interfaces that aren't on links

netlab auto-assigns from named pools for all interface types. NLL should
extend its pool system similarly.

## Proposed Syntax

### Pool-based loopback assignment

```nll
pool loopbacks 10.255.0.0/24 /32

node router1 { lo pool loopbacks }     # 10.255.0.1/32
node router2 { lo pool loopbacks }     # 10.255.0.2/32
node router3 { lo pool loopbacks }     # 10.255.0.3/32
```

### Pool-based management addresses

```nll
lab "dc" {
  mgmt 172.20.0.0/24     # existing: creates mgmt bridge
}

# With auto-addressing, each node gets a sequential mgmt IP:
# router1 → 172.20.0.1, router2 → 172.20.0.2, etc.
# This already works via the mgmt network auto-assign.
```

### Pool reference in any address context

```nll
pool servers 10.1.0.0/24 /32

node web1 {
  # Assign from pool to a specific interface
  dummy mgmt0 { address pool servers }    # 10.1.0.1/32
}
node web2 {
  dummy mgmt0 { address pool servers }    # 10.1.0.2/32
}
```

## Implementation

### 1. Parser

The `lo` property already accepts a CIDR address. Extend it to accept
`pool IDENT`:

```rust
// In parse_node_prop, "lo" branch:
if eat_kw(tokens, pos, "lo") {
    if eat_kw(tokens, pos, "pool") {
        let pool_name = expect_ident(tokens, pos)?;
        Ok(NodeProp::Lo(format!("pool:{pool_name}")))
    } else {
        let addr = parse_cidr_or_name(tokens, pos)?;
        Ok(NodeProp::Lo(addr))
    }
}
```

### 2. Lower

During lowering, when encountering `"pool:loopbacks"` as a loopback address:
1. Look up the pool in `ctx.pools`
2. Allocate the next address from the pool
3. Replace with the actual CIDR

```rust
if lo_addr.starts_with("pool:") {
    let pool_name = &lo_addr[5..];
    let pool = ctx.pools.get_mut(pool_name)
        .ok_or_else(|| Error::invalid_topology(format!("unknown pool '{pool_name}'")))?;
    let allocated = pool.next()?;
    lo_addr = allocated;
}
```

### 3. Tests

| Test | Description |
|------|-------------|
| `test_lo_pool_allocation` | 3 nodes with `lo pool x` get sequential /32s |
| `test_lo_pool_exhaustion` | Pool runs out → error |
| `test_lo_explicit_still_works` | `lo 10.0.0.1/32` unchanged |

### File Changes

| File | Change |
|------|--------|
| `parser.rs` | Accept `pool IDENT` in `lo` property |
| `lower.rs` | Allocate from pool during lowering |
