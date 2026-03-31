# Plan 115: Network Subnet Auto-Assignment and Port Addresses

**Date:** 2026-03-31
**Status:** Implemented (2026-03-31)
**Effort:** Small (1 day)
**Priority:** P1 — blocks shared L2 segments with specific IPs

---

## Problem Statement

Network (bridge) blocks cannot assign IP addresses to member interfaces.
Two features are missing:

1. **`subnet` auto-assignment**: Like links have `subnet 10.0.0.0/24`, networks
   should auto-assign sequential IPs to members.
2. **Per-port addresses**: Explicitly assign IPs to specific bridge members.

Currently, the only workaround is star topology with point-to-point links
through a central router, which doesn't model a real L2 broadcast domain.

## NLL Syntax

### Auto-assignment via subnet

```nll
network c2-dc {
  members [dc1:eth0, dc2:eth0, dcs:eth0]
  subnet 10.2.1.0/24
}
# dc1:eth0 = 10.2.1.1, dc2:eth0 = 10.2.1.2, dcs:eth0 = 10.2.1.3
```

### Explicit per-port addresses

```nll
network c2-dc {
  members [dc1:eth0, dc2:eth0, dcs:eth0]
  port dc1:eth0 { 10.2.1.1/24 }
  port dc2:eth0 { 10.2.1.2/24 }
  port dcs:eth0 { 10.2.1.254/24 }
}
```

### Combined (subnet for some, explicit for gateway)

```nll
network c2-dc {
  members [dc1:eth0, dc2:eth0, dcs:eth0]
  subnet 10.2.1.0/24
  port dcs:eth0 { 10.2.1.254/24 }   # override auto-assigned .3
}
```

## Implementation

### 1. Parser — Add `subnet` to network blocks

The `Subnet` token is already defined. Add it to the network block parser
alongside `members`, `vlan-filtering`, `vlan`, `port`:

```rust
Some(Token::Subnet) => {
    *pos += 1;
    let cidr = parse_cidr_or_name(tokens, pos)?;
    net.subnet = Some(cidr);
}
```

### 2. Parser — Add addresses to port blocks

Currently port blocks parse: `pvid`, `vlans`, `tagged`, `untagged`.
Add CIDR address parsing:

```rust
// Inside port block parsing
Some(Token::Cidr(c)) => {
    port.addresses.push(c.clone());
    *pos += 1;
}
```

### 3. Lower — Subnet auto-assignment for networks

When lowering a `NetworkDef` with a `subnet`, assign sequential IPs to
members that don't have explicit port addresses:

```rust
if let Some(subnet) = &net.subnet {
    let (base_ip, prefix) = parse_cidr(subnet)?;
    let mut next_host = 1;
    for member in &net.members {
        if !port_has_address(member) {
            let ip = increment_ip(base_ip, next_host);
            port_config.addresses.push(format!("{ip}/{prefix}"));
            next_host += 1;
        }
    }
}
```

### 4. Deploy — Assign addresses on bridge member interfaces

In Step 9 (address assignment), add a section for network port addresses:

```rust
// From network port configs
for (net_name, network) in &topology.networks {
    for (endpoint_str, port) in &network.ports {
        if let Some(ep) = EndpointRef::parse(endpoint_str) {
            let conn = node_handles[&ep.node].connection()?;
            for addr_str in &port.addresses {
                let (ip, prefix) = parse_cidr(addr_str)?;
                conn.add_address_by_name(&ep.iface, ip, prefix).await?;
            }
        }
    }
}
```

### 5. DNS — Include network port addresses in hosts entries

`generate_hosts_entries()` currently only looks at link addresses and port
addresses from networks. Make sure subnet-auto-assigned addresses are included.

### 6. Cross-references

Ensure `${node.iface}` cross-references resolve addresses from network ports,
not just from link addresses.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_network_subnet` | Parser: network with subnet keyword |
| `test_parse_network_port_address` | Parser: port block with CIDR address |
| `test_lower_network_subnet` | Lower: auto-assign IPs to members |
| `test_lower_network_port_override` | Lower: explicit port address overrides auto |
| `test_render_network_subnet` | Render: roundtrip |
| Integration: `deploy_network_subnet` | Deploy bridge with subnet, verify IPs |

## File Changes

| File | Change |
|------|--------|
| `ast.rs` | Add `subnet: Option<String>` to `NetworkDef`, addresses to `PortDef` |
| `parser.rs` | Parse `subnet` and CIDR addresses in network/port blocks |
| `lower.rs` | Auto-assign IPs from subnet, merge with explicit port addresses |
| `deploy.rs` | Assign addresses from network port configs in Step 9 |
| `render.rs` | Render subnet and port addresses |
| `dns.rs` | Include network port addresses in hosts entries |
