# Plan 131: Root-Namespace Reachable Management Network

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Medium (1–2 days)
**Priority:** P0 — blocks external integration testing use case

---

## Problem Statement

The `mgmt 172.20.0.0/24` directive creates a bridge in a dedicated management namespace
(`{lab}-mgmt`). Nodes can reach each other via this bridge, but the host/root namespace
has no route into any node. Integration tests running in the root namespace cannot open
TCP connections to services inside lab nodes without funnelling everything through
`nlink-lab exec`.

## Proposed Syntax

Add a `host-reachable` modifier to the existing `mgmt` directive. The bare `mgmt` form
retains current behaviour (bridge in isolated management namespace) for backward
compatibility:

```nll
lab "my-lab" {
    mgmt 172.20.0.0/24 host-reachable
}
```

**Semantics:**

- Create a Linux bridge in the **root** namespace (name: `nlab-{lab}`, truncated to 15 chars)
- For each node: a veth pair with one end (`mgmt0`) in the node namespace, peer attached
  to the root-namespace bridge
- Assign sequential IPs: bridge gets `.1`, nodes get `.2`, `.3`, … (from the CIDR)
- On destroy: remove bridge and all veth peers from root namespace

This matches the containerlab model.

## Design Decisions

### Why a new modifier, not a new keyword?

A `host-reachable` modifier on the existing `mgmt` directive is the minimal change.
Alternative considered: a separate `mgmt-bridge` keyword — rejected because it would
duplicate most of the mgmt plumbing and diverge the mental model.

### Why root namespace, not a named namespace?

The whole point is reachability from the test process (which runs in the root namespace).
A named namespace doesn't help unless the test harness does `ip netns exec` itself.

### Bridge naming

`nlab-{lab_name}` truncated to 15 chars (Linux IFNAMSIZ limit). The `nlab-` prefix
avoids collision with user-defined bridges.

### IP assignment

Sequential from the CIDR: `.1` for the bridge itself, then `.2`, `.3`, … for nodes
in `topology.nodes` iteration order (sorted by name for determinism). The prefix length
from the CIDR is used for all addresses.

## Implementation

### Step 1: Types (`types.rs`)

Add a `host_reachable` field to `LabConfig`:

```rust
/// Whether the management bridge should be created in the root namespace
/// (host-reachable) instead of a dedicated management namespace.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
pub mgmt_host_reachable: bool,
```

### Step 2: AST (`ast.rs`)

Add to `LabDecl`:

```rust
pub mgmt_host_reachable: bool,
```

### Step 3: Lexer (`lexer.rs`)

No new token needed. `host-reachable` will be parsed as the identifier `host` followed
by `-` followed by `reachable`, or as a single hyphenated keyword string. The simplest
approach: after parsing `mgmt <cidr>`, check for an identifier `host-reachable` using
`eat_kw(tokens, pos, "host-reachable")`.

Actually, since the lexer splits on `-`, the parser should look for identifier `host`
then `Minus` then identifier `reachable`. Or add a dedicated compound keyword check.
Check how other hyphenated keywords (`startup-delay`, `depends-on`) are handled:

In the parser, `eat_kw` checks for `Ident` tokens. Hyphenated keywords like
`depends-on` and `startup-delay` are already handled by consuming the hyphenated form
from the ident token directly (the lexer likely produces a single `Ident("depends-on")`
or similar). Verify the lexer behaviour and follow the same pattern.

### Step 4: Parser (`parser.rs`)

In `parse_lab_decl()`, after parsing `mgmt`:

```rust
} else if eat_kw(tokens, pos, "mgmt") {
    mgmt = Some(parse_cidr_or_name(tokens, pos)?);
    // Check for optional "host-reachable" modifier
    if eat_kw(tokens, pos, "host-reachable") {
        mgmt_host_reachable = true;
    }
```

### Step 5: Lowerer (`lower.rs`)

In `lower_lab()`, propagate the field:

```rust
mgmt_host_reachable: lab.mgmt_host_reachable,
```

### Step 6: Deploy (`deploy.rs`)

This is the main change. When `topology.lab.mgmt_host_reachable` is true and
`topology.lab.mgmt_subnet` is `Some(subnet)`:

**New deployment step (after step 3 "create namespaces", before step 4):**

```
Step 3b: Create host-reachable management bridge
```

1. Parse the CIDR to get (base_ip, prefix_len)
2. Create bridge in root namespace:
   ```rust
   let root_conn: Connection<Route> = Connection::new().await?;
   let bridge_name = format!("nlab-{}", topology.lab.prefix());
   // truncate to 15 chars
   let bridge = BridgeLink::new(&bridge_name);
   root_conn.add_link(bridge).await?;
   root_conn.set_link_up(&bridge_name).await?;
   // Assign .1 address to bridge
   root_conn.add_address(&bridge_name, &format!("{}.1/{}", base, prefix_len)).await?;
   ```
3. For each node (sorted by name):
   ```rust
   let mgmt_iface = "mgmt0";
   let peer_name = format!("nm{}", &node_name[..min(node_name.len(), 11)]);
   let veth = VethLink::new(mgmt_iface, &peer_name)
       .peer_netns_fd(root_ns_fd);
   // Actually: create in node ns with peer in root ns
   // The veth is created in node_conn, peer goes to root ns
   node_conn.add_link(veth).await?;
   root_conn.set_link_master(&peer_name, &bridge_name).await?;
   root_conn.set_link_up(&peer_name).await?;
   // Assign .N address to mgmt0 in node ns
   node_conn.add_address(mgmt_iface, &format!("{}.{}}/{}", base, n, prefix_len)).await?;
   node_conn.set_link_up(mgmt_iface).await?;
   ```

**Important detail:** The root namespace doesn't have an FD we can open with
`namespace::open`. We need to either:
- Use `peer_netns_fd` with `/proc/1/ns/net` (root ns)
- Or create the veth in the root namespace and move one end into the node namespace

The second approach may be simpler: create veth pair in root ns, then move `mgmt0` end
into the node ns via `set_link_netns(mgmt_iface, node_ns_fd)`. Check what nlink API
is available for this.

### Step 7: Destroy (`running.rs`)

In `destroy()`, after deleting the management namespace, add cleanup for root-ns bridge:

```rust
// 4b. Delete root-namespace management bridge if host-reachable
if self.topology.lab.mgmt_host_reachable {
    let bridge_name = format!("nlab-{}", self.topology.lab.prefix());
    // truncate to 15 chars
    let root_conn: Connection<Route> = Connection::new().await?;
    let _ = root_conn.del_link(&bridge_name).await;
    // Peer veths are auto-deleted when bridge is removed
}
```

### Step 8: State (`state.rs`)

Add `mgmt_host_reachable: bool` to `LabState` (with `#[serde(default)]`) so that
destroy knows whether to clean up the root bridge. Alternatively, just check
`topology.lab.mgmt_host_reachable` which is already persisted via topology.

### Step 9: Render (`render.rs`)

After rendering `mgmt <subnet>`, append ` host-reachable` if the flag is set.

### Step 10: Validator (`validator.rs`)

Add validation:
- If `mgmt_host_reachable` is set, `mgmt_subnet` must also be set
- Warn if the subnet is too small for the number of nodes + 1 (bridge)
- Warn if the management subnet overlaps with any link subnet

### Step 11: Cleanup guard (macro `lab_test`)

The `__LabGuard` Drop impl in the macro should also attempt to remove `nlab-*` bridges
from the root namespace. Add:

```rust
let _ = std::process::Command::new("ip")
    .args(["link", "delete", &format!("nlab-{}", self.name)])
    .status();
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_parse_mgmt_host_reachable` | parser.rs | Parse `mgmt 172.20.0.0/24 host-reachable` |
| `test_parse_mgmt_without_host_reachable` | parser.rs | Bare `mgmt` still works unchanged |
| `test_lower_mgmt_host_reachable` | lower.rs | Flag propagates to `LabConfig` |
| `test_render_mgmt_host_reachable` | render.rs | Renders back with modifier |
| `test_validate_mgmt_host_reachable_no_subnet` | validator.rs | Error if flag set without subnet |
| `test_deploy_mgmt_host_reachable` | integration.rs | Bridge exists in root ns, nodes reachable (requires root) |
| `test_destroy_mgmt_host_reachable` | integration.rs | Bridge removed from root ns after destroy |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `types.rs` | +4 | Add `mgmt_host_reachable` field |
| `ast.rs` | +2 | Add field to `LabDecl` |
| `parser.rs` | +8 | Parse `host-reachable` modifier |
| `lower.rs` | +2 | Propagate field |
| `deploy.rs` | +60 | New step 3b: root-ns bridge + veth + addr |
| `running.rs` | +15 | Destroy root-ns bridge |
| `render.rs` | +3 | Render modifier |
| `validator.rs` | +15 | Validation rules |
| Tests | +60 | 7 test functions |
| **Total** | ~170 | |
