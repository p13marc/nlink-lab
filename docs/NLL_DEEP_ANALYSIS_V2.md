# NLL Deep Analysis v2 — From Procedural to Declarative

**Date:** 2026-04-01
**Based on:** `examples/infra-c2-a18-a9.nll` (post-optimization: 113 + 30 lines)
**Research:** Ansible inventory, Terraform for_each, Kubernetes selectors,
netlab auto-routing, wmediumd, OSPF/BGP principles

---

## Executive Summary

The infra example is now well-structured with parametric imports, IP functions,
for-loops, and shared networks. But it's still **procedural** — the user writes
explicit routes, explicit NAT rules, and explicit member lists. The next
evolution is making the DSL **declarative**: express *what* you want (connectivity,
reachability) and let the system derive *how* (routes, NAT, addresses).

Five improvements would make this transformation, ordered by impact:

1. **Auto-routing from topology graph** — eliminate all manual route statements
2. **Fleet inventory with for_each** — generate imports from data, not repetition
3. **Reachability-driven NAT** — infer NAT from topology, not manual rules
4. **Network impairment matrix** — per-pair quality on shared media
5. **Implicit member lists** — derive network membership from connections

---

## Current State Analysis

### What's Good

The infra example already uses every DSL feature well:
- `subnet()`/`host()` — zero hardcoded IPs in C2 section
- Parametric imports — A18/A19 defined once, instantiated twice
- `for`-inside-blocks — 10 routes from 6 lines, 10 NAT rules from 8 lines
- Shared networks — modem media as L2 segments
- DNS, link profiles, site grouping — all used appropriately

### What's Still Procedural

```nll
# 1. Manual routes — 12 lines that say "DCS reaches X via firewall"
node dcs : router {
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      route 144.0.${lt}.${asset}/32 via host(${dcs-fw}, 2)
    }
  }
  ...
}

# 2. Manual NAT — 12 lines that say "firewall translates 144.x to 172.x"
nat {
  masquerade src ${c2}
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      dnat dst 144.0.${lt}.${asset}/32 to host(...)
    }
  }
}

# 3. Manual member lists — must list every modem endpoint
network fo {
  members [c2-fw:fo, a18-black:fo, a19-black:fo, a9-cc:fo, a10-cc:fo]
}

# 4. Repeated import lines — one per asset instance
import "imports/a18.nll" as a18(id=18)
import "imports/a18.nll" as a19(id=19)
```

### The Ideal Version

What the infra topology SHOULD look like:

```nll
import "imports/a18.nll" as a18(id=18)
import "imports/a18.nll" as a19(id=19)
import "imports/a9.nll" as a9(id=9)
import "imports/a9.nll" as a10(id=10)

lab "infra" { dns hosts }
profile router { forward ipv4 }

let c2 = subnet("10.0.0.0/8", 16, 2)

node dc1
node dc2
node dcs : router
node c2-fw : router { nat auto }

network c2-dc { members [dc1:eth0, dc2:eth0, dcs:eth0]; subnet subnet(${c2}, 24, 1) }
link dcs:eth1 -- c2-fw:eth0 { subnet subnet(${c2}, 24, 2) }

network fo    { members [c2-fw:fo,    *-black:fo,  *-cc:fo];    subnet 172.100.1.0/24 }
network sat   { members [c2-fw:sat,   *-black:sat];              subnet 172.100.2.0/24 }
network radio { members [c2-fw:radio, *-black:radio];            subnet 172.100.3.0/24 }
network wifi  { members [c2-fw:wifi,  *-cc:wifi];                subnet 172.100.4.0/24 }

routing auto    # compute all static routes from topology graph
```

**~20 lines** vs 113 today. The key differences:
- `routing auto` eliminates ALL route and NAT statements
- `*-black:fo` glob pattern eliminates manual member enumeration
- `nat auto` on the firewall means "generate NAT from addressing"
- No explicit routes on any node

---

## Improvement 1: Auto-Routing (P0 — Highest Impact)

### Problem

DCS has 10 explicit routes. Every host has a manual `route default via ...`.
Every router template has manual `route` statements for return paths. This is
the single largest source of boilerplate.

### Proposed Feature: `routing auto`

```nll
lab "infra" {
  routing auto    # or: routing static-auto
}
```

When enabled, the deployer computes static routes from the topology graph
after all addresses are assigned (Step 12):

**Algorithm:**
1. Build adjacency graph from links and network memberships
2. For each node:
   - If stub (single neighbor): add `default via <neighbor>`
   - If transit (multiple neighbors): run shortest-path to all other subnets
   - For each remote subnet: add `route <subnet> via <next-hop>`
3. Skip routes for directly-connected subnets
4. Optimize: collapse to default route where possible

**What it replaces in the infra example:**
- All 10 routes on DCS → auto-generated (DCS has one exit: c2-fw)
- `route default via ...` on dc1, dc2, cc, cu → auto-generated (stub nodes)
- `route ${lan} via ...` on BLACK → auto-generated (transit node)
- `route 172.100.0.0/16 via ...` on RED → auto-generated (transit node)

**What it doesn't replace:**
- The 144.0.x.x routes on DCS — these are virtual/translated addresses that
  don't exist in the topology. Auto-routing computes from real subnets.
- NAT DNAT rules — these need the 144→172 mapping which is policy, not topology.

**Partial auto-routing:** Allow per-node opt-out:

```nll
lab "infra" { routing auto }

node dcs : router {
  # Auto-routing handles reachability to all connected subnets.
  # Only need manual routes for virtual (NATted) addresses:
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      route 144.0.${lt}.${asset}/32 via host(${dcs-fw}, 2)
    }
  }
}
```

### Impact

Eliminates ~20 lines of explicit routes across main file + templates.
The A18 template would shrink from 32 to ~20 lines (no more route statements
except the 172.100 return route which auto-routing handles).

### Implementation

- **Effort:** Medium (graph algorithm + deploy integration)
- **Where:** New Step 12b after address assignment, before nftables
- **Dependencies:** Needs all addresses resolved first (after subnet auto-assign)
- **Crate:** No external deps — BFS/Dijkstra on adjacency list

---

## Improvement 2: Fleet Inventory with `for_each` (P1)

### Problem

Four import lines, nearly identical:

```nll
import "imports/a18.nll" as a18(id=18)
import "imports/a18.nll" as a19(id=19)
import "imports/a9.nll" as a9(id=9)
import "imports/a9.nll" as a10(id=10)
```

### Proposed Feature: `for_each` on imports

```nll
# Fleet definition: template → list of instances
fleet a18 from "imports/a18.nll" {
  a18(id=18)
  a19(id=19)
}

fleet a9 from "imports/a9.nll" {
  a9(id=9)
  a10(id=10)
}
```

Or more compactly with a data-driven approach:

```nll
# Import the same template multiple times with different params
import "imports/a18.nll" for_each {
  a18: { id = 18 }
  a19: { id = 19 }
}

import "imports/a9.nll" for_each {
  a9:  { id = 9 }
  a10: { id = 10 }
}
```

### Impact

Reduces 4 lines to 2 blocks, and scales to N instances without additional lines.
For large fleets (50+ drones), this is significant.

### Implementation

- **Effort:** Small (parser change to allow `for_each` after import)
- **Where:** Import resolution in lowerer

---

## Improvement 3: Reachability-Driven NAT (P1)

### Problem

The NAT block on c2-fw has 10+ DNAT rules that encode the 144→172 translation
policy. This is the most complex and error-prone part of the topology.

### Proposed Feature: Declarative NAT from addressing

```nll
node c2-fw : router {
  # "Translate DCS's 144.0.x.x addresses to modem 172.100.x.x addresses"
  nat {
    masquerade src ${c2}
    # Instead of manual DNAT rules:
    translate 144.0.0.0/8 to 172.100.0.0/16
  }
}
```

The `translate` directive generates DNAT rules by matching the addressing
pattern: for each host in 144.0.x.x that's reachable via a modem network,
generate a DNAT rule to the corresponding 172.100.x.x address.

### Alternative: Keep explicit but simplify

The current for-loop approach is actually quite clean. The real issue is that
the 144→172 mapping is inherently policy (the user chose this scheme). A
`translate` shorthand would help but isn't essential.

### Impact

Moderate — saves ~8 lines but the for-loop version is already readable.

---

## Improvement 4: Network Impairment Matrix (P2)

### Problem

Shared modem networks have uniform characteristics, but in reality:
- Radio link C2↔A18 at 5km has different loss than C2↔A19 at 50km
- Satellite link to A18 has different latency than to A19

### Proposed Feature: Per-pair impairment on networks

```nll
network radio {
  members [c2-fw:radio, a18-black:radio, a19-black:radio]
  subnet 172.100.3.0/24

  # Per-pair impairment matrix
  impair c2-fw -- a18-black { delay 15ms jitter 5ms loss 1% }
  impair c2-fw -- a19-black { delay 40ms jitter 20ms loss 5% }
  impair a18-black -- a19-black { delay 60ms jitter 30ms loss 8% }
}
```

### Implementation

Uses TC classes with u32 filters on bridge ports, matching by destination IP.
Each pair gets a TC class with its own netem qdisc.

### Impact

Enables realistic simulation of distance-dependent link quality —
critical for the drone use case where assets are at varying ranges.

- **Effort:** Medium-Large (TC class management on bridges)
- **Dependencies:** Need per-bridge-port TC infrastructure

---

## Improvement 5: Glob Patterns in Member Lists (P2)

### Problem

Network member lists must enumerate every endpoint:

```nll
network fo {
  members [c2-fw:fo, a18-black:fo, a19-black:fo, a9-cc:fo, a10-cc:fo]
}
```

Adding a new A18 drone requires updating the member list.

### Proposed Feature: Glob/tag-based membership

```nll
network fo {
  members [c2-fw:fo, *-black:fo, *-cc:fo]    # glob pattern
  subnet 172.100.1.0/24
}
```

Or tag-based:

```nll
node a18-black { tag modem-fo }
node a19-black { tag modem-fo }
node a9-cc { tag modem-fo }

network fo {
  members [tag=modem-fo]:fo    # all nodes with tag
  subnet 172.100.1.0/24
}
```

### Impact

Member lists stay fixed regardless of fleet size. Adding a new drone
only requires the import line — the networks adapt automatically.

- **Effort:** Medium (glob matching on node names, or tag system)
- **Dependencies:** Node name resolution must happen before network creation

---

## Improvement 6: Implicit Default Routes on Stub Nodes (P3)

### Problem

Every leaf node has `route default via <gateway>`:

```nll
node cc { route default via host(${lan}, 3) }
node cu { route default via host(${lan}, 3) }
```

### Proposed Feature: Automatic default routes

If a node has exactly one neighbor (connected via one link/network), the
system can auto-generate `route default via <neighbor's IP>`.

This is a subset of `routing auto` (Improvement 1) but much simpler to
implement — just check node degree in the adjacency graph.

### Impact

Eliminates the most common route statement in templates.

---

## Priority-Ordered Roadmap

| # | Feature | Impact | Effort | Lines Saved |
|---|---------|--------|--------|-------------|
| 1 | `routing auto` | Eliminates all route statements | Medium | ~30 |
| 2 | Fleet `for_each` imports | Scales to N instances | Small | ~2 per fleet |
| 3 | Glob/tag network members | Auto-adapts to fleet size | Medium | ~5 per network |
| 4 | Network impairment matrix | Realistic per-pair simulation | Medium-Large | 0 (new capability) |
| 5 | NAT `translate` shorthand | Simplifies address mapping | Small | ~8 |
| 6 | Stub default routes | Simplest auto-routing | Small | ~4 per template |

### Recommended Execution Order

```
#6 (stub defaults) ─── quick win, independent
#1 (routing auto) ──── subsumes #6, highest impact
#2 (fleet imports) ─── independent, quick
#3 (glob members) ──── independent, medium
#4 (impairment matrix) ── new capability, medium-large
#5 (NAT translate) ─── nice-to-have, low priority
```

### Target Result

With improvements 1-3 implemented, the main infra file would be:

```nll
import "imports/a18.nll" for_each { a18: {id=18}, a19: {id=19} }
import "imports/a9.nll" for_each { a9: {id=9}, a10: {id=10} }

lab "infra" { dns hosts; routing auto }
profile router { forward ipv4 }

let c2 = subnet("10.0.0.0/8", 16, 2)
node dc1
node dc2
node dcs : router {
  # Only virtual address routes (not derivable from topology)
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      route 144.0.${lt}.${asset}/32 via host(subnet(${c2}, 24, 2), 2)
    }
  }
  for asset in [9, 10] {
    for lt in [1, 4] {
      route 144.0.${lt}.${asset}/32 via host(subnet(${c2}, 24, 2), 2)
    }
  }
}
node c2-fw : router {
  nat {
    masquerade src ${c2}
    for asset in [18, 19] {
      for lt in [1, 2, 3] {
        dnat dst 144.0.${lt}.${asset}/32 to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
      }
    }
    for asset in [9, 10] {
      for lt in [1, 4] {
        dnat dst 144.0.${lt}.${asset}/32 to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
      }
    }
  }
}

network c2-dc { members [dc1:eth0, dc2:eth0, dcs:eth0]; subnet subnet(${c2}, 24, 1) }
link dcs:eth1 -- c2-fw:eth0 { subnet subnet(${c2}, 24, 2) }

network fo    { members [c2-fw:fo,    *-black:fo,  *-cc:fo]; subnet 172.100.1.0/24 }
network sat   { members [c2-fw:sat,   *-black:sat];           subnet 172.100.2.0/24 }
network radio { members [c2-fw:radio, *-black:radio];         subnet 172.100.3.0/24 }
network wifi  { members [c2-fw:wifi,  *-cc:wifi];             subnet 172.100.4.0/24 }

validate {
  reach dc1 dcs
  reach c2-fw a18-black
  reach c2-fw a9-cc
}
```

And the A18 template would be:

```nll
lab "a18-template"
profile router { forward ipv4 }
param id

let base = subnet("10.0.0.0/8", 16, ${id})

node cc
node cu
node red : router
node black : router { nat { masquerade src ${base} } }

network lan { members [cc:eth0, cu:eth0, red:eth0]; subnet subnet(${base}, 24, 1) }
link red:eth1 -- black:eth0 { subnet subnet(${base}, 24, 2) }
# routing auto handles all routes automatically
```

**~50 lines total** (main + templates) for 16 nodes, 7 networks, full NAT,
computed addressing, auto-routing. Down from 170 lines originally.
