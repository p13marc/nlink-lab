# Plan 119: Site / Asset Grouping

**Date:** 2026-03-31
**Status:** Draft
**Effort:** Medium (1-2 days)
**Priority:** P4 — readability for large topologies

---

## Problem Statement

Flat node lists become hard to read in multi-site topologies. The C2/A18/A9
infrastructure has 9 nodes across 3 physical locations, with no structural
grouping in the NLL file beyond comments:

```nll
# ═══ Asset C2 ═══
node dc1 { ... }
node dc2 { ... }
node dcs { ... }
node c2-fw { ... }

# ═══ Asset A18 ═══
node a18-cc { ... }
...
```

Node names must be manually prefixed (`a18-cc`, `a9-cu`) to avoid conflicts.

## NLL Syntax

### Site blocks with automatic prefixing

```nll
site c2 "Command & Control" {
  node dc1 { route default via ${dcs.dc1} }
  node dc2 { route default via ${dcs.dc2} }
  node dcs : router { ... }
  node firewall : router { ... }

  link dc1:eth0 -- dcs:dc1 { subnet 10.2.1.0/24 }
  link dcs:eth1 -- firewall:eth0 { 10.2.2.1/24 -- 10.2.2.2/24 }
}

site a18 "Remote Drone Alpha" {
  node cc { route default via ${red.cc} }
  node cu { route default via ${red.cu} }
  node red : router { ... }
  node black : router { ... }

  link cc:eth0 -- red:cc { subnet 10.18.1.0/24 }
  link red:eth1 -- black:eth0 { 10.18.2.1/24 -- 10.18.2.2/24 }
}

# Cross-site links use full names
link c2.firewall:fo -- a18.black:fo { 172.100.1.2/24 -- 172.100.1.18/24 }
```

**Inside a site block:** Node names are local (just `dc1`, not `c2-dc1`).
**Outside site blocks:** Reference as `site.node` (e.g., `c2.dc1`).
**Lowering:** Nodes are prefixed with `site-` → `c2-dc1`, `a18-cc`, etc.

### Flat (backward compatible)

Sites are purely optional. Existing flat topologies work unchanged.

## Design Decisions

### Prefix strategy

`site.node` in NLL → `site-node` in namespace names. The dot is syntactic
sugar; the lowered topology has flat names with dash prefix.

### Cross-references inside sites

`${dcs.dc1}` inside site `c2` resolves to `c2-dcs`'s `dc1` interface address.
No prefix needed for intra-site references.

### Links inside vs outside sites

- **Inside site block:** Both endpoints are local to the site.
- **Outside site blocks:** Endpoints use `site.node:iface` syntax.
- **Cross-site links** must be outside site blocks.

## Implementation

### 1. AST

```rust
pub struct SiteDef {
    pub name: String,
    pub description: Option<String>,
    pub statements: Vec<Statement>,  // nodes, links, networks inside
}
```

Add `Statement::Site(SiteDef)` variant.

### 2. Parser

```
site = "site" IDENT STRING? "{" statement* "}"
```

Parse site blocks at the top level alongside other statements.

### 3. Lower

During lowering, expand site blocks:
1. Prefix all node names with `site-`
2. Prefix all link endpoint node references with `site-`
3. Prefix network member references with `site-`
4. Resolve `site.node:iface` cross-site references

### 4. Render

Render site blocks when nodes share a common prefix, or always render flat
(the renderer doesn't need to reconstruct sites).

## Risks

- **Complexity:** Sites add a scoping layer that affects name resolution,
  cross-references, and error messages.
- **Import interaction:** How do sites interact with `import`? A site could
  import a module, or a module could define a site.
- **Recommendation:** Start with basic site blocks (just prefixing), add
  cross-references and imports later.

## File Changes

| File | Change |
|------|--------|
| `ast.rs` | Add `SiteDef`, `Statement::Site` |
| `parser.rs` | Parse `site` blocks |
| `lower.rs` | Expand sites: prefix names, resolve references |
| `render.rs` | Optional: render site blocks |
