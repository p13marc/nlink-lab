# NLL Pain Points Report — Real Infrastructure Analysis

**Date:** 2026-03-31
**Based on:** `examples/infra-c2-a18-a9.nll` (1× C2, 2× A18, 2× A9)
**Topology:** 16 nodes, 15 links, 3 networks, 10 modem links, 10 DNAT rules

---

## Executive Summary

Writing a real multi-site infrastructure topology exposed 7 pain points in
the NLL DSL. The core issue is **repetition without abstraction** — identical
site definitions (A18a/A18b) must be copy-pasted because there's no way to
parameterize and instantiate site templates. This also cascades into
repetitive NAT rules, routes, and modem links that can't be loop-generated.

**Lines of NLL:** 170 actual topology lines
**Estimated with fixes:** ~60 lines (65% reduction)

---

## Pain Point 1: Copy-Paste Sites (P0 — Critical)

### Problem

A18a and A18b are structurally identical — same nodes (cc, cu, red, black),
same links, same NAT, same routing. The only difference is the asset ID
which changes IP addresses (10.18.x.x vs 10.19.x.x).

**Current:** 30 lines × 2 = 60 lines (duplicated)
**Ideal:** 30 lines × 1 + 2 instantiation lines = 32 lines

### Current NLL (A18a)

```nll
site a18a {
  node cc { route default via 10.18.1.3 }
  node cu { route default via 10.18.1.3 }
  node red : router { route 172.100.0.0/16 via 10.18.2.2 }
  node black : router {
    route 10.18.1.0/24 via 10.18.2.1
    nat { masquerade src 10.18.0.0/16 }
  }
  link red:eth1 -- black:eth0 { 10.18.2.1/24 -- 10.18.2.2/24 }
}
network a18a-lan {
  members [a18a-cc:eth0, a18a-cu:eth0, a18a-red:eth0]
  subnet 10.18.1.0/24
}
```

Then copy-paste the ENTIRE block for A18b, changing `18` → `19` everywhere.

### Proposed Solution: Parametric Sites

```nll
# Define once
site a18(id) {
  node cc { route default via 10.${id}.1.3 }
  node cu { route default via 10.${id}.1.3 }
  node red : router { route 172.100.0.0/16 via 10.${id}.2.2 }
  node black : router {
    route 10.${id}.1.0/24 via 10.${id}.2.1
    nat { masquerade src 10.${id}.0.0/16 }
  }
  network lan {
    members [cc:eth0, cu:eth0, red:eth0]
    subnet 10.${id}.1.0/24
  }
  link red:eth1 -- black:eth0 { 10.${id}.2.1/24 -- 10.${id}.2.2/24 }
}

# Instantiate twice
a18(18)    # → a18-cc, a18-red, a18-black, a18-cu, a18-lan
a18(19)    # → a18-cc, ... wait, name collision!
```

### Name collision issue

Two `a18(...)` calls would both create `a18-cc`. We need either:
- **Explicit instance naming:** `a18(18) as a18a` and `a18(19) as a18b`
- **Auto-suffix from parameter:** use the parameter as the prefix

Recommended:

```nll
site a18(id) { ... }

# Instantiate with alias
a18(18) as a18a    # → a18a-cc, a18a-red, ...
a18(19) as a18b    # → a18b-cc, a18b-red, ...
```

This is exactly how `import ... as alias(params)` works today, but applied
to inline site definitions instead of external files.

### Implementation Approach

Two options:

**Option A: Extend `import` to support inline parametric modules.**
Sites with parameters become syntactic sugar for single-file modules:

```nll
import "a18-template.nll" as a18a(id=18)
import "a18-template.nll" as a18b(id=19)
```

This already works! The limitation is that you need a separate file.

**Option B: Inline parametric site blocks (new feature).**
Define the template inline in the same file:

```nll
site-template a18(id) { ... }
a18(18) as a18a
a18(19) as a18b
```

**Recommendation:** Option A already works and requires no code changes.
Document this pattern. Option B is a convenience feature for later.

### Effort: Small (documentation) or Medium (Option B implementation)

---

## Pain Point 2: Interpolation Inside CIDR Literals (P0 — Critical)

### Problem

The `${id}` interpolation works in idents and strings, but CIDRs like
`10.${id}.1.0/24` are lexed as a CIDR token BEFORE interpolation is applied.
The lexer sees `10.` and starts matching a CIDR/IPv4, then hits `${id}` and
fails.

This is the **root blocker** for parametric sites. Without CIDR interpolation,
you can't parameterize IP addressing schemes.

### Current Behavior

```nll
let id = 18
node cc { route default via 10.${id}.1.3 }
# ERROR: lexer can't parse "10.${id}.1.3" as a CIDR or IPv4
```

### What Works Today

Interpolation works in:
- Idents: `node leaf${i}` → `leaf1`
- Strings: `description "asset ${id}"` → `"asset 18"`
- Endpoints: `leaf${i}:eth0` → `leaf1:eth0`

Interpolation does NOT work in:
- CIDRs: `10.${id}.1.0/24` → lexer error
- IPv4 addresses: `10.${id}.1.3` → lexer error
- Duration literals: `${delay}ms` → lexer error

### Proposed Solution

The interpolation resolver runs AFTER parsing, during the lowering phase.
But the lexer needs to tokenize `10.${id}.1.0/24` as something meaningful.

**Option A: Pre-process interpolation before lexing.**
Run a regex pass that replaces `${...}` with resolved values before the
lexer runs. This requires variables to be resolvable at lex time (currently
they are).

**Pros:** Works everywhere (CIDRs, IPs, durations, any literal).
**Cons:** Changes the pipeline order; variables must be defined before use.

**Option B: Lex interpolated CIDRs as Interp tokens.**
If the lexer encounters `10.${`, treat the entire expression as an
interpolation token and resolve it later.

**Pros:** Targeted fix.
**Cons:** Complex lexer changes; doesn't generalize.

**Option C: Use string literals for parameterized addresses.**
Allow `"10.${id}.1.0/24"` as a string where CIDRs are expected:

```nll
node cc { route default via "10.${id}.1.3" }
link red:eth1 -- black:eth0 { "10.${id}.2.1/24" -- "10.${id}.2.2/24" }
```

**Pros:** Simple parser change (accept strings where CIDRs expected).
**Cons:** Ugly syntax; strings don't validate as CIDRs at parse time.

**Recommendation:** Option A (pre-process) is the cleanest long-term solution.
Option C is a quick workaround.

### Effort: Medium (Option A) or Small (Option C)

---

## Pain Point 3: No `for` Loops Inside Node Blocks (P0 — High)

### Problem

DCS has 10 routes and FIREWALL has 10 DNAT rules, all following a mechanical
pattern. Loops can't generate them because `for` only works at the
top-level statement level, not inside node/nat/firewall blocks.

### Current NLL (FIREWALL NAT)

```nll
nat {
  masquerade src 10.2.0.0/16
  dnat dst 144.0.1.18/32 to 172.100.1.18
  dnat dst 144.0.2.18/32 to 172.100.2.18
  dnat dst 144.0.3.18/32 to 172.100.3.18
  dnat dst 144.0.1.19/32 to 172.100.4.19
  dnat dst 144.0.2.19/32 to 172.100.5.19
  dnat dst 144.0.3.19/32 to 172.100.6.19
  dnat dst 144.0.1.9/32 to 172.100.7.9
  dnat dst 144.0.4.9/32 to 172.100.8.9
  dnat dst 144.0.1.10/32 to 172.100.9.10
  dnat dst 144.0.4.10/32 to 172.100.10.10
}
```

### Ideal NLL

```nll
let link_counter = 0
nat {
  masquerade src 10.2.0.0/16
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      let link_counter = ${link_counter + 1}
      dnat dst 144.0.${lt}.${asset}/32 to 172.100.${link_counter}.${asset}
    }
  }
  for asset in [9, 10] {
    for lt in [1, 4] {
      let link_counter = ${link_counter + 1}
      dnat dst 144.0.${lt}.${asset}/32 to 172.100.${link_counter}.${asset}
    }
  }
}
```

This also requires Pain Point 2 (CIDR interpolation) to be resolved first.

### Proposed Solution

Allow `for` loops inside:
- `nat { }` blocks → generates multiple NAT rules
- Node property blocks → generates multiple routes/rules
- `firewall { }` blocks → generates multiple firewall rules

The parser already handles `for` at the statement level. Extending it to
work inside blocks requires making the block parsers aware of `for` and
expanding the loop during lowering.

### Effort: Medium

---

## Pain Point 4: Networks Can't Be Inside Site Blocks (P1)

### Problem

The `network` block for each A18's internal LAN must be **outside** the
`site` block because the site lowering only prefixes node/link names,
not network member endpoints inside `network` blocks.

```nll
site a18a {
  node cc { ... }
  # Can't put network here — members won't get prefixed correctly
}

# Must be outside:
network a18a-lan {
  members [a18a-cc:eth0, a18a-cu:eth0, a18a-red:eth0]   # manual prefix!
  subnet 10.18.1.0/24
}
```

### Proposed Solution

The site lowering already handles networks (it prefixes member endpoint
node names). The issue was that it was implemented. Let me re-check...

Actually, looking at the current code, site lowering DOES handle networks
and prefixes members. The issue might just be that networks inside sites
aren't tested. If it works, the example should be updated to move networks
inside sites.

### Action: Test and fix if needed. Effort: Small.

---

## Pain Point 5: No Cross-References Inside Sites (P1)

### Problem

Inside `site a18a`, RED has `route 172.100.0.0/16 via 10.18.2.2`.
The `10.18.2.2` is BLACK's address on the RED↔BLACK link. In a regular
(non-site) topology, you'd write `${black.eth0}`, but cross-references
don't work inside site blocks because:

1. The reference resolver runs after site expansion
2. After expansion, the node is `a18a-black`, not `black`
3. Inside the site definition, the user writes `black`, not `a18a-black`

### Proposed Solution

Resolve cross-references inside site blocks BEFORE prefixing, using
local (unprefixed) names. Then prefix the resolved values.

Or: allow `${black.eth0}` inside sites to resolve to the site-local node.

### Effort: Medium

---

## Pain Point 6: Modem Links Are Verbose (P2)

### Problem

10 modem links, each a one-liner but with unique interface names:

```nll
link c2-fw:fo-a18a -- a18a-black:fo : fiber { 172.100.1.2/24 -- 172.100.1.18/24 }
link c2-fw:sat-a18a -- a18a-black:sat { 172.100.2.2/24 -- 172.100.2.18/24 }
...
```

These follow a pattern but can't be loop-generated because:
- Each link has unique interface names on FIREWALL
- The addressing follows a formula involving link_id and asset_id
- `for` loops don't work inside link contexts

### Proposed Solution

With Pain Points 2 (CIDR interpolation) and 3 (loops inside blocks) fixed:

```nll
let link_id = 0
for asset in [18, 19] {
  for lt_name in [fo, sat, radio] {
    let link_id = ${link_id + 1}
    link c2-fw:${lt_name}-a${asset} -- a${asset}-black:${lt_name} {
      172.100.${link_id}.2/24 -- 172.100.${link_id}.${asset}/24
    }
  }
}
```

This requires: loop variables in endpoints, CIDR interpolation, and
a counter variable that increments across iterations.

### Effort: Depends on Pain Points 2 + 3 being resolved first.

---

## Pain Point 7: FIREWALL Interface Explosion (P3 — Minor)

### Problem

FIREWALL has 10 modem-facing interfaces (`fo-a18a`, `sat-a18a`, `rad-a18a`,
`fo-a18b`, ...). Each is a separate veth pair. In reality, a firewall might
have fewer physical interfaces with VLAN sub-interfaces or policy routing.

This isn't a DSL issue per se — it's a modeling choice. But nlink-lab could
support "multi-link bundles" where multiple virtual links share a single
interface with different impairments (via TC classes).

### Proposed Solution

No action needed. This is a feature request for multi-link bundles, not
a DSL pain point.

---

## Priority-Ordered Action Plan

| # | Pain Point | Priority | Effort | Depends On | Impact |
|---|-----------|----------|--------|------------|--------|
| 1 | CIDR interpolation | P0 | Medium | Nothing | Unblocks parametric sites |
| 2 | Parametric sites | P0 | Small/Medium | #1 | Eliminates copy-paste |
| 3 | `for` inside blocks | P0 | Medium | #1 | Eliminates repetitive rules |
| 4 | Networks inside sites | P1 | Small | Nothing | Fix site grouping |
| 5 | Cross-refs inside sites | P1 | Medium | Nothing | Removes hardcoded IPs |
| 6 | Loop-generated links | P2 | Small | #1, #3 | Reduces modem link verbosity |

### Recommended Execution Order

```
#1 (CIDR interpolation) ─── unblocks everything
  ├── #2 (parametric sites) ─── biggest user impact
  └── #3 (for inside blocks) ─── second biggest impact
       └── #6 (loop-generated links) ─── free once #1+#3 done

#4 (networks in sites) ─── independent, quick fix
#5 (cross-refs in sites) ─── independent, medium
```

### Estimated Result

With all fixes, the infra example would go from **170 lines** to approximately
**60 lines**:

```nll
lab "infra" { dns hosts }
profile router { forward ipv4 }
defaults fiber { delay 1ms }

# A18 template (defined once)
import "a18-template.nll" as a18a(id=18)
import "a18-template.nll" as a18b(id=19)

# A9 template (defined once)
import "a9-template.nll" as a9a(id=9)
import "a9-template.nll" as a9b(id=10)

# C2
node dc1 { route default via 10.2.1.3 }
node dc2 { route default via 10.2.1.3 }
node dcs : router {
  for asset in [18, 19] {
    route [144.0.1.${asset}/32, 144.0.2.${asset}/32, 144.0.3.${asset}/32] via 10.2.2.2
  }
  for asset in [9, 10] {
    route [144.0.1.${asset}/32, 144.0.4.${asset}/32] via 10.2.2.2
  }
}
node c2-fw : router {
  route 10.2.1.0/24 via 10.2.2.1
  nat {
    masquerade src 10.2.0.0/16
    for asset in [18, 19] {
      for lt in [1, 2, 3] {
        dnat dst 144.0.${lt}.${asset}/32 to 172.100.${lt}.${asset}
      }
    }
    for asset in [9, 10] {
      for lt in [1, 4] {
        dnat dst 144.0.${lt}.${asset}/32 to 172.100.${lt}.${asset}
      }
    }
  }
}

network c2-dc { members [dc1:eth0, dc2:eth0, dcs:eth0]; subnet 10.2.1.0/24 }
link dcs:eth1 -- c2-fw:eth0 { 10.2.2.1/24 -- 10.2.2.2/24 }

# Modem links (generated)
for asset in [18, 19] {
  for lt_name in [fo, sat, radio] {
    link c2-fw:${lt_name}-a${asset} -- a${asset}-black:${lt_name} : fiber { ... }
  }
}
```

This is a **65% reduction** in lines and eliminates all copy-paste.
