# Plan 110: Integration Test Expansion

**Date:** 2026-03-30
**Status:** Ready
**Effort:** Medium (2-3 days)
**Depends on:** Nothing

---

## Problem Statement

nlink-lab has only 5 integration tests despite having 20+ features. Missing
coverage creates regression risk for:

- VLAN filtering with bridge ports
- WireGuard tunnel establishment
- VRF routing isolation
- DNS resolution (end-to-end)
- Complex multi-hop routing
- Firewall packet filtering (not just rule application)
- TC rate limiting under load
- Container + namespace mixed topologies
- Topology apply/diff (hot reload)
- Asymmetric impairments
- Management network

## Current Tests

```
deploy_simple          # basic 2-node topology
deploy_spine_leaf      # 3-tier with loops/variables
deploy_firewall        # nftables rule application
deploy_from_builder    # builder DSL
deploy_vrf             # VRF creation
deploy_wireguard       # WireGuard interface creation
exec_ip_addr           # exec command
exec_ip_route          # route verification
exec_ping              # ICMP connectivity
exit_code_forwarded    # exec exit codes
netem_applied          # impairment verification
state_persistence      # state save/load
sysctl_forwarding      # sysctl application
deploy_bridge_vlan     # VLAN bridge creation
apply_add_node_and_link     # hot-reload add
apply_impairment_change     # hot-reload impair
apply_remove_node           # hot-reload remove
```

## New Tests to Add

### Connectivity & Routing

| Test | Description | Example |
|------|-------------|---------|
| `multi_hop_ping` | 3+ nodes, verify end-to-end ping through router | client -> router -> server |
| `multi_hop_traceroute` | Verify path through intermediate hops | traceroute shows router IPs |
| `ipv6_connectivity` | IPv6-only topology, ping6 between nodes | `examples/ipv6-simple.nll` |
| `asymmetric_routes` | Different paths for different subnets | Multi-gateway routing |

### VLAN

| Test | Description | Example |
|------|-------------|---------|
| `vlan_isolation` | Hosts on different VLANs cannot ping | VLAN 100 vs VLAN 200 |
| `vlan_trunk_tagged` | Tagged trunk port carries multiple VLANs | Router with sub-interfaces |
| `vlan_pvid_untagged` | Access ports strip tags correctly | PVID assignment |

### WireGuard

| Test | Description | Example |
|------|-------------|---------|
| `wireguard_tunnel_ping` | Ping through WireGuard tunnel | site-to-site VPN |
| `wireguard_key_generation` | Auto-generated keys are valid | Check wg show output |

### VRF

| Test | Description | Example |
|------|-------------|---------|
| `vrf_isolation` | Traffic in VRF red cannot reach VRF blue | Overlapping subnets |
| `vrf_routing` | Routes within VRF work correctly | Per-VRF default gateway |

### Firewall

| Test | Description | Example |
|------|-------------|---------|
| `firewall_drop_policy` | Default drop blocks unmatched traffic | Ping blocked, TCP 8080 allowed |
| `firewall_src_match` | Source CIDR matching works | Only trusted subnet can SSH |

### DNS

| Test | Description | Example |
|------|-------------|---------|
| `dns_hosts_resolve` | `getent hosts <node>` returns correct IP | Deploy with `dns hosts` |
| `dns_multi_homed_alias` | `router-eth0` alias resolves | Multi-interface node |

### Rate Limiting & Impairment

| Test | Description | Example |
|------|-------------|---------|
| `rate_limit_effective` | Throughput is capped at configured rate | iperf3 < configured rate |
| `asymmetric_impairment` | `->` and `<-` have different delays | Measure RTT asymmetry |

### Container

| Test | Description | Example |
|------|-------------|---------|
| `container_connectivity` | Container node can ping namespace node | Mixed topology |
| `container_dns_hosts` | Container resolves namespace nodes by name | --add-host verification |

### Hot Reload

| Test | Description | Example |
|------|-------------|---------|
| `apply_add_network` | Add bridge network to running lab | diff + apply |
| `apply_change_firewall` | Modify firewall rules on running lab | Rule update |

## Implementation Approach

Each test follows the existing pattern using `#[lab_test]` macro:

```rust
#[lab_test("examples/vlan-trunk.nll")]
async fn vlan_isolation(lab: RunningLab) {
    // host1 (VLAN 100) can ping host2 (VLAN 100)
    let out = lab.exec("host1", "ping", &["-c1", "-W1", "10.0.100.2"]).unwrap();
    assert_eq!(out.exit_code, 0);

    // host1 (VLAN 100) cannot ping host3 (VLAN 200)
    let out = lab.exec("host1", "ping", &["-c1", "-W1", "10.0.200.1"]).unwrap();
    assert_ne!(out.exit_code, 0);
}
```

Tests that need specific topologies not covered by existing examples should use
the builder DSL with `#[lab_test(topology = fn_name)]`.

### File Changes

| File | Change |
|------|--------|
| `tests/integration.rs` | Add 15-20 new integration tests |
| `examples/` | May need 1-2 new examples for specific test scenarios |
