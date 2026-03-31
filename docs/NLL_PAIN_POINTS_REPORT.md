# NLL Pain Points Report — Real Infrastructure Analysis

**Date:** 2026-03-31 (revised)
**Based on:** `examples/infra-c2-a18-a9.nll` (1× C2, 2× A18, 2× A9)
**Topology:** 16 nodes, 15 links, 3 networks, 10 modem links, 10 DNAT rules
**Research:** Terraform/HCL, netlab, Nix, Dhall, Jsonnet, containerlab

---

## Executive Summary

Writing a real multi-site infrastructure topology exposed fundamental
limitations in the NLL DSL. The core issue is **repetition without
abstraction** — identical site definitions must be copy-pasted because
interpolation doesn't work inside IP addresses and loops don't work
inside blocks.

After researching how Terraform, netlab, Nix, Dhall, and other DSLs solve
these problems, the recommended path is clear: **add IP computation functions
and deferred type resolution**, following the patterns proven by HCL and netlab.

**Current:** 170 lines with copy-paste
**Target:** ~50 lines with no duplication

---

## The Root Problem: Early Lexing Kills Interpolation

NLL's lexer recognizes CIDR (`10.0.0.0/24`), IPv4 (`10.0.0.1`), and Duration
(`10ms`) as typed tokens before the parser runs. When a user writes
`10.${id}.1.0/24`, the lexer tries to match a CIDR, fails at `${`, and errors.

**Every major DSL** that supports parameterization solves this differently:
- **Terraform/HCL:** Everything is a string first, type-checked after interpolation
- **Nix:** String interpolation everywhere, types checked at evaluation
- **Dhall:** Functions compute values, types checked at output
- **Nickel:** Contracts validate after string assembly

NLL is the only DSL that rejects interpolation inside typed literals.
This single design choice blocks parametric templates, loop-generated
addresses, and IP computation functions.

---

## Proposed Solution: IP Functions + Deferred Resolution

Instead of trying to fix interpolation inside CIDR literals (which requires
a lexer redesign), add **IP computation built-in functions** following
Terraform's proven `cidrsubnet`/`cidrhost` pattern. This is cleaner,
more powerful, and avoids the string-parsing ambiguity entirely.

### New Built-in Functions

```nll
# subnet(base, new_prefix_len, index) → CIDR
# Carve subnet #index with /new_prefix_len from base
subnet("10.0.0.0/16", 24, 18)     # → 10.0.18.0/24
subnet("10.0.0.0/16", 24, 19)     # → 10.0.19.0/24

# host(cidr, host_number) → IP
# Get host #N from a subnet
host("10.0.18.0/24", 1)           # → 10.0.18.1
host("10.0.18.0/24", 254)         # → 10.0.18.254

# net(cidr) → CIDR (identity, for clarity)
# mask(cidr) → prefix length
```

These are compile-time pure functions, evaluated during lowering. The `ipnet`
Rust crate provides all the math (`Ipv4Net::subnets()`, `hosts()`).

### How This Solves Every Pain Point

**Parametric sites:**
```nll
# a18-template.nll
param id

let base = subnet("10.0.0.0/8", 16, ${id})    # 10.{id}.0.0/16
let lan = subnet(${base}, 24, 1)               # 10.{id}.1.0/24
let link = subnet(${base}, 24, 2)              # 10.{id}.2.0/24

node cc { route default via host(${lan}, 3) }
node cu { route default via host(${lan}, 3) }
node red : router { route 172.100.0.0/16 via host(${link}, 2) }
node black : router {
  route ${lan} via host(${link}, 1)
  nat { masquerade src ${base} }
}

network lan {
  members [cc:eth0, cu:eth0, red:eth0]
  subnet ${lan}
}
link red:eth1 -- black:eth0 { host(${link}, 1)/24 -- host(${link}, 2)/24 }
```

**Instantiation:**
```nll
import "a18-template.nll" as a18a(id=18)
import "a18-template.nll" as a18b(id=19)
import "a9-template.nll" as a9a(id=9)
import "a9-template.nll" as a9b(id=10)
```

**Loop-generated NAT rules:**
```nll
node c2-fw : router {
  nat {
    masquerade src 10.2.0.0/16
    for asset in [18, 19] {
      for lt in [1, 2, 3] {
        dnat dst host("144.0.0.0/8", ${lt} * 256 + ${asset}) to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
      }
    }
  }
}
```

---

## Pain Point Analysis (Revised)

### 1. Parametric Sites (P0)

**Problem:** A18a and A18b are identical except for IP octets.

**Solution:** `import` with parameters already works. The blocker was CIDR
interpolation, which IP functions solve. No new language feature needed —
just document the pattern and implement the functions.

**Before:** 60 lines duplicated
**After:** 15-line template + 2 import lines

### 2. Loops Inside Blocks (P0)

**Problem:** `for` only works at statement level, not inside `nat {}`,
`firewall {}`, or node property blocks.

**Solution:** Allow `for` inside any block that accepts repeated items.
Terraform solves this with `dynamic` blocks; NLL can reuse its existing
`for` syntax. The parser needs to recognize `for` inside `nat`, `firewall`,
`validate`, and node blocks, and the lowerer expands them.

**Example:**
```nll
nat {
  masquerade src 10.2.0.0/16
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      dnat dst host("144.0.0.0/8", ${lt} * 256 + ${asset}) to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
    }
  }
}
```

**Effort:** Medium. Parser + lowerer changes.

### 3. Networks Inside Sites (P1)

**Problem:** Networks placed inside `site {}` blocks don't work correctly.

**Solution:** The current site lowering already prefixes network members.
This may just be a bug. Test and fix.

**Effort:** Small.

### 4. Cross-References Inside Sites (P1)

**Problem:** `${black.eth0}` doesn't resolve inside a site block because the
resolver runs after prefixing.

**Solution:** With IP functions, this becomes less critical — you'd write
`host(${link}, 2)` instead of `${black.eth0}`. But for ergonomics, resolve
cross-references using local names before applying the site prefix.

**Effort:** Medium.

### 5. Auto-Addressing (P2 — New Recommendation)

**Inspired by:** netlab's addressing pool system.

**Problem:** Users still manually assign IPs on many links and interfaces.
NLL has subnet pools for `/30` point-to-point links, but not for:
- Loopback addresses (router IDs)
- Management interfaces
- Multi-subnet nodes

**Solution:** Extend the pool system with named allocation:

```nll
pool loopbacks 10.255.0.0/24 /32
pool management 172.20.0.0/24 /32

node router { lo pool loopbacks }          # auto-assign 10.255.0.1/32
node router2 { lo pool loopbacks }         # auto-assign 10.255.0.2/32
```

**Effort:** Small (pool already supports sequential allocation).

### 6. Conditional Logic (P3 — New Recommendation)

**Inspired by:** Terraform `for ... if`, Jsonnet conditionals.

**Problem:** A9 (simplified) and A18 (full) have different structures.
You can't conditionally include nodes based on a parameter.

**Solution:** Add `if` to `for` loops and as standalone blocks:

```nll
for lt in [1, 2, 3, 4] {
  if ${lt} <= 3 {
    link c2-fw:link${lt} -- a18-black:link${lt} { ... }
  }
}
```

**Effort:** Medium. Useful but not blocking.

---

## Implementation Roadmap

| Phase | Feature | Effort | Impact |
|-------|---------|--------|--------|
| **1** | IP functions (`subnet`, `host`) | Medium | Unblocks everything |
| **2** | `for` inside blocks (nat, firewall, node props) | Medium | Eliminates repetitive rules |
| **3** | Networks inside sites (fix) | Small | Complete site grouping |
| **4** | Cross-refs inside sites | Medium | Ergonomics |
| **5** | Auto-addressing for loopbacks/mgmt | Small | Reduces manual IPs |
| **6** | Conditional `if` blocks | Medium | Flexibility |

### Phase 1 is the key unlock

IP functions require:
1. New lexer token: `FunctionCall` (or parse `ident(...)` in expressions)
2. New AST node: `FunctionCall { name, args }`
3. Evaluation in lowerer: `subnet()` → compute via `ipnet` crate, return string
4. Allow function calls anywhere a CIDR/IP/string is expected

The `ipnet` crate (already widely used in Rust networking) provides all
the math. Implementation is primarily parser + lowerer, no deploy changes.

### Target Result

The infra example with all improvements:

```nll
lab "infra" { dns hosts }
profile router { forward ipv4 }
defaults fiber { delay 1ms }

# Fleet
import "a18.nll" as a18a(id=18)
import "a18.nll" as a18b(id=19)
import "a9.nll" as a9a(id=9)
import "a9.nll" as a9b(id=10)

# C2
let c2 = subnet("10.0.0.0/8", 16, 2)             # 10.2.0.0/16
let dc-lan = subnet(${c2}, 24, 1)                  # 10.2.1.0/24
let dcs-fw = subnet(${c2}, 24, 2)                  # 10.2.2.0/24

node dc1 { route default via host(${dc-lan}, 3) }
node dc2 { route default via host(${dc-lan}, 3) }
node dcs : router {
  for asset in [18, 19, 9, 10] {
    for lt in [1, 2, 3, 4] {
      route host("144.0.0.0/8", ${lt} * 256 + ${asset})/32 via host(${dcs-fw}, 2)
    }
  }
}
node c2-fw : router {
  route ${dc-lan} via host(${dcs-fw}, 1)
  nat {
    masquerade src ${c2}
    for asset in [18, 19, 9, 10] {
      for lt in [1, 2, 3, 4] {
        dnat dst host("144.0.0.0/8", ${lt} * 256 + ${asset})/32 to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
      }
    }
  }
}

network c2-dc { members [dc1:eth0, dc2:eth0, dcs:eth0]; subnet ${dc-lan} }
link dcs:eth1 -- c2-fw:eth0 { host(${dcs-fw}, 1)/24 -- host(${dcs-fw}, 2)/24 }

# Modem links
for asset in [18, 19] {
  for lt in [1, 2, 3] {
    link c2-fw:l${lt}-a${asset} -- a${asset}-black:l${lt} : fiber {
      host(subnet("172.100.0.0/16", 24, ${lt}), 2)/24 -- host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})/24
    }
  }
}
for asset in [9, 10] {
  for lt in [1, 4] {
    link c2-fw:l${lt}-a${asset} -- a${asset}-cc:l${lt} : fiber {
      host(subnet("172.100.0.0/16", 24, ${lt}), 2)/24 -- host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})/24
    }
  }
}
```

**~50 lines** (from 170), **zero duplication**, fully parameterized.
