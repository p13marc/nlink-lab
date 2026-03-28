# Plan 083: Validator & Deployer Hardening

**Priority:** Medium
**Effort:** 2-3 days
**Target:** `validator.rs`, `deploy.rs`

## Summary

Add missing validation rules to catch errors early and improve the deployer's
robustness with health checks, better rollback, and connection reuse.

## Part 1: New Validation Rules

### Rule 15: `interface-name-length`

Linux interface names must be 1-15 characters (`IFNAMSIZ - 1`). Currently only
enforced implicitly when the kernel rejects the name at deploy time.

**Where:** `validator.rs` — new function after existing rules.

```rust
fn validate_interface_names(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    for link in &topo.links {
        for ep in &link.endpoints {
            if let Some(ref_ ) = EndpointRef::parse(ep) {
                if ref_.interface.len() > 15 {
                    diags.push(Diagnostic::error(
                        "interface-name-length",
                        format!("interface '{}' on node '{}' exceeds 15-char limit ({} chars)",
                            ref_.interface, ref_.node, ref_.interface.len()),
                    ));
                }
            }
        }
    }
    // Also check explicit interfaces in nodes
    for (node_name, node) in &topo.nodes {
        for (iface_name, _) in &node.interfaces {
            if iface_name.len() > 15 {
                diags.push(Diagnostic::error(
                    "interface-name-length",
                    format!("interface '{iface_name}' on node '{node_name}' exceeds 15-char limit"),
                ));
            }
        }
    }
}
```

### Rule 16: `subnet-overlap`

Warn when two addresses on different links share the same subnet but could cause
routing ambiguity.

**Where:** `validator.rs` — new function.

Collect all (subnet, node) pairs. If the same subnet appears on multiple interfaces
of the same node (excluding loopback), warn:

```rust
fn validate_subnet_overlap(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    // Build map: (node, subnet) -> [interface, ...]
    // Warn if same node has same subnet on multiple non-loopback interfaces
}
```

**Severity:** Warning (not error) — some topologies intentionally use overlapping subnets.

### Rule 17: `wireguard-peer-exists`

WireGuard peer references must point to existing nodes that have WireGuard interfaces.

**Where:** `validator.rs` — new function.

```rust
fn validate_wireguard_peers(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    for (node_name, node) in &topo.nodes {
        for (wg_name, wg_config) in &node.wireguard {
            for peer in &wg_config.peers {
                if !topo.nodes.contains_key(peer) {
                    diags.push(Diagnostic::error(
                        "wireguard-peer-exists",
                        format!("WireGuard peer '{peer}' referenced from {node_name}:{wg_name} does not exist"),
                    ));
                } else if topo.nodes[peer].wireguard.is_empty() {
                    diags.push(Diagnostic::error(
                        "wireguard-peer-exists",
                        format!("WireGuard peer '{peer}' has no WireGuard interfaces"),
                    ));
                }
            }
        }
    }
}
```

### Rule 18: `vrf-table-unique`

VRF table IDs must be unique within a node. Two VRFs with the same table ID on the
same node would cause routing conflicts.

```rust
fn validate_vrf_tables(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    for (node_name, node) in &topo.nodes {
        let mut seen: HashMap<u32, &str> = HashMap::new();
        for (vrf_name, vrf_config) in &node.vrfs {
            if let Some(existing) = seen.get(&vrf_config.table) {
                diags.push(Diagnostic::error(
                    "vrf-table-unique",
                    format!("VRF '{vrf_name}' and '{existing}' on node '{node_name}' share table {}",
                        vrf_config.table),
                ));
            }
            seen.insert(vrf_config.table, vrf_name);
        }
    }
}
```

### Rule 19: `duplicate-link-endpoint`

The same `node:interface` pair should not appear in multiple links.

```rust
fn validate_duplicate_endpoints(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, link) in topo.links.iter().enumerate() {
        for ep in &link.endpoints {
            if let Some(prev) = seen.insert(ep.clone(), i) {
                diags.push(Diagnostic::error(
                    "duplicate-link-endpoint",
                    format!("endpoint '{ep}' used in link {prev} and link {i}"),
                ));
            }
        }
    }
}
```

### Fix: Route reachability for empty subnets

**Where:** `validator.rs` — `validate_route_reachability()` (around line 740).

Current code skips the check when `subnets` is empty. Should still warn:

```rust
// Before:
if !reachable && !subnets.is_empty() { ... }

// After:
if !reachable {
    diags.push(Diagnostic::warning(...));
}
```

## Part 2: Deployer Improvements

### Health checks between steps

Add lightweight verification after critical deployment steps to fail fast:

```rust
// After Step 3 (create namespaces):
for ns in &namespaces {
    let output = namespace::spawn_output(ns, "true", &[])?;
    if output.status.code() != Some(0) {
        return Err(Error::Deploy(format!(
            "namespace '{ns}' created but cannot execute commands"
        )));
    }
}
```

Apply after:
- Step 3: Verify namespaces can run commands
- Step 5: Verify veth interfaces exist in both namespaces
- Step 10: Verify interfaces are UP

### Veth peer name collision detection

**Where:** `deploy.rs` — peer name generation (around line 237-241).

When names are truncated to 15 chars, collisions become possible. Detect and error:

```rust
let peer_name = format!("v-{}-{}", node_name, iface_name);
let truncated = if peer_name.len() > 15 { &peer_name[..15] } else { &peer_name };

if used_peer_names.contains(truncated) {
    return Err(Error::Deploy(format!(
        "veth peer name collision: '{truncated}' (from {node_name}:{iface_name})"
    )));
}
used_peer_names.insert(truncated.to_string());
```

### WireGuard peer endpoint resolution clarity

**Where:** `deploy.rs` — `find_peer_endpoint()`.

Currently returns any address from any link. Should prefer the address that's on a
link directly connecting the two peers:

```rust
fn find_peer_endpoint(topo: &Topology, local: &str, peer: &str) -> Option<String> {
    // First: try direct link between local and peer
    for link in &topo.links {
        if link.endpoints.iter().any(|e| e.starts_with(&format!("{local}:")))
            && link.endpoints.iter().any(|e| e.starts_with(&format!("{peer}:")))
        {
            // Return peer's address on this link
            return extract_peer_address(link, peer);
        }
    }
    // Fallback: any address on the peer node
    find_any_address(topo, peer)
}
```

### Firewall match expression safety

**Where:** `deploy.rs` — `apply_match_expr()` (around line 1104-1142).

Unrecognized match expressions currently log a warning but the rule is still added
(potentially without the intended filter). Change to error:

```rust
// Before:
_ => {
    tracing::warn!("unrecognized match expression: {expr}");
}

// After:
_ => {
    return Err(Error::Deploy(format!(
        "unsupported firewall match expression: '{expr}'. \
         Supported: 'ct state ...', 'tcp dport N', 'udp dport N'"
    )));
}
```

## Progress

### New Validation Rules
- [ ] Rule 15: `interface-name-length` (max 15 chars)
- [ ] Rule 16: `subnet-overlap` (warning for same subnet on same node)
- [ ] Rule 17: `wireguard-peer-exists` (peer node exists with WG interface)
- [ ] Rule 18: `vrf-table-unique` (no duplicate table IDs per node)
- [ ] Rule 19: `duplicate-link-endpoint` (same endpoint in multiple links)
- [ ] Fix: route reachability check for empty subnet list

### Deployer Hardening
- [ ] Health checks after Steps 3, 5, 10
- [ ] Veth peer name collision detection
- [ ] WireGuard peer endpoint resolution: prefer direct links
- [ ] Firewall: error on unrecognized match expressions

### Tests
- [ ] Unit tests for each new validation rule
- [ ] Test: interface name > 15 chars rejected
- [ ] Test: duplicate endpoints rejected
- [ ] Test: WireGuard peer referencing non-existent node
- [ ] Test: VRF table ID collision
