# Plan 125: Auto-Routing from Topology Graph

**Date:** 2026-04-01
**Status:** Ready
**Effort:** Medium (2-3 days)
**Priority:** P0 — eliminates ~30 lines of manual routes across infra topology

---

## Problem Statement

Every node needs explicit route statements: stub nodes need `route default via`,
transit routers need return routes, and multi-homed nodes need routes to remote
subnets. This is the single largest source of boilerplate — the infra example
has ~20 route statements across the main file and templates that are all
derivable from the topology graph.

## Proposed Syntax

### Lab-level directive

```nll
lab "infra" {
  routing auto
}
```

### Per-node opt-out

Nodes with manual routes keep them. Auto-generated routes are additive —
they fill gaps but don't override explicit routes.

```nll
lab "infra" { routing auto }

node dcs : router {
  # These manual routes for virtual addresses are kept.
  # Auto-routing adds routes for real subnets on top.
  route 144.0.1.18/32 via 10.2.2.2
}
```

## Algorithm

Runs after Step 12 (address assignment), before Step 13 (nftables):

```
Step 12c: Auto-generate static routes

1. Build adjacency graph G:
   - Nodes: all namespaces
   - Edges: from links (point-to-point) and network memberships (shared LAN)
   - Edge weight: 1 (hop count)
   - Edge metadata: gateway IP (neighbor's address on the shared subnet)

2. Collect all subnets:
   - For each link: both endpoint CIDRs
   - For each network: the subnet + all port addresses
   - For each node interface: addresses

3. For each node N:
   a. Find directly-connected subnets (skip these)
   b. If N has exactly one neighbor (stub node):
      - Add: route default via <neighbor's IP on shared subnet>
   c. Else (multi-homed / transit):
      - Run BFS/Dijkstra from N to all other nodes
      - For each remote subnet S reachable via next-hop H:
        - If no explicit route for S exists on N:
          - Add: route S via <H's IP on link between N and H>
   d. Optimization: if all remote subnets go via the same next-hop,
      collapse into a single default route

4. For nodes with `forward ipv4`:
   - Ensure IP forwarding sysctl is set (already handled by profile)
   - These nodes act as transit in the graph
```

## What Auto-Routing Handles

| Current manual route | Auto-generated? | Why |
|---------------------|-----------------|-----|
| `route default via host(${lan}, 3)` on cc/cu | Yes | Stub node, single neighbor |
| `route ${lan} via host(${ptp}, 1)` on BLACK | Yes | Transit, return route to LAN |
| `route 172.100.0.0/16 via host(${ptp}, 2)` on RED | Yes | Transit, route to WAN subnets |
| `route ${dc-lan} via host(${dcs-fw}, 1)` on c2-fw | Yes | Transit, return route to DC LAN |
| `route 144.0.x.x via ...` on DCS | **No** | Virtual addresses, not in topology |

## Types (`types.rs`)

Add `routing` field to `LabConfig`:

```rust
pub enum RoutingMode {
    Manual,  // default — no auto-generation
    Auto,    // compute routes from topology graph
}
```

## Parser

In lab block, recognize `routing auto` or `routing manual`:

```nll
lab "x" { routing auto }
```

## Deploy Integration

New function `auto_generate_routes()` called between Step 12 and Step 13:

```rust
async fn auto_generate_routes(
    topology: &mut Topology,
    namespace_names: &HashMap<String, String>,
    node_handles: &HashMap<String, NodeHandle>,
) -> Result<()> {
    if topology.lab.routing != RoutingMode::Auto {
        return Ok(());
    }
    let graph = build_adjacency_graph(topology);
    for (node_name, node) in &mut topology.nodes {
        let auto_routes = compute_routes_for_node(&graph, node_name, &node.routes);
        for (dest, route) in auto_routes {
            node.routes.entry(dest).or_insert(route);  // don't override manual
        }
    }
    // Apply routes via netlink (same as existing Step 12)
    for (node_name, node) in &topology.nodes {
        apply_routes(node_handles, node_name, node).await?;
    }
    Ok(())
}
```

## Tests

| Test | Description |
|------|-------------|
| `test_auto_route_stub_node` | Node with one neighbor gets default route |
| `test_auto_route_transit` | Router with two subnets gets return routes |
| `test_auto_route_no_override` | Manual routes preserved when auto is on |
| `test_auto_route_off_by_default` | No auto routes when routing=manual |
| `test_auto_route_multi_hop` | 3-hop path generates correct next-hops |
| `test_auto_route_network_members` | Nodes on same bridge are adjacent |
| Integration: `deploy_auto_routing` | Deploy with routing auto, verify ping works |

## Documentation Updates

| File | Change |
|------|--------|
| **README.md** | Add "Auto-Routing" section with example |
| **CLAUDE.md** | Add `RoutingMode` to types table; update NLL features list; add Step 12c to deploy sequence |
| **NLL_DSL_DESIGN.md** | Add `routing` to lab_prop grammar; document auto-routing algorithm |
| **examples/infra-c2-a18-a9.nll** | Add `routing auto`; remove explicit routes that become auto-generated |
| **examples/imports/a18.nll** | Remove route statements (all auto-generated) |
| **examples/imports/a9.nll** | Remove route statement (auto-generated) |

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `RoutingMode` enum, `routing` field on `LabConfig` |
| `parser.rs` | Parse `routing auto` / `routing manual` in lab block |
| `lower.rs` | Lower routing mode |
| `deploy.rs` | New Step 12c: `auto_generate_routes()` with graph algorithm |
| `render.rs` | Render `routing auto` in lab block |
| `validator.rs` | Warn if routing=auto but no forwarding nodes exist |
