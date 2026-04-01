# Plan 122: Site Block Improvements (Networks + Cross-References)

**Date:** 2026-03-31
**Status:** Implemented (2026-04-01) — networks inside sites work; cross-refs deferred (IP functions provide alternative)
**Effort:** Small-Medium (1-2 days)
**Priority:** P1 — completes site grouping feature
**Depends on:** Nothing

---

## Problem Statement

Two issues with site blocks prevent them from being fully self-contained:

1. **Networks inside sites** must be placed outside the `site {}` block
   because the lowering may not correctly prefix network member endpoints.

2. **Cross-references** (`${black.eth0}`) don't resolve inside site blocks
   because the resolver runs after site name prefixing, and the unprefixed
   name `black` no longer matches the prefixed node `a18a-black`.

## Part A: Networks Inside Sites

### Current (broken or awkward)

```nll
site a18a {
  node cc { ... }
  node red : router { ... }
  # network here won't prefix members correctly
}
# Must be outside:
network a18a-lan {
  members [a18a-cc:eth0, a18a-cu:eth0, a18a-red:eth0]   # manual prefix
  subnet 10.18.1.0/24
}
```

### Target

```nll
site a18a {
  node cc { ... }
  node red : router { ... }
  network lan {
    members [cc:eth0, cu:eth0, red:eth0]   # local names, auto-prefixed
    subnet 10.18.1.0/24
  }
}
# Produces: network "a18a-lan" with members [a18a-cc:eth0, ...]
```

### Implementation

The current site lowering code in `lower.rs` already handles networks and
prefixes member node names. Verify with a test. If it works, update the
infra example. If not, fix the prefixing logic:

```rust
ast::Statement::Network(n) => {
    let mut prefixed = n.clone();
    prefixed.name = format!("{}{}", prefix, n.name);
    prefixed.members = n.members.iter().map(|m| {
        if let Some((node, iface)) = m.split_once(':') {
            format!("{}{}:{}", prefix, node, iface)
        } else {
            format!("{}{}", prefix, m)
        }
    }).collect();
    // Also prefix port endpoint keys
    prefixed.ports = n.ports.iter().map(|p| {
        let mut pp = p.clone();
        if let Some((node, iface)) = p.endpoint.split_once(':') {
            pp.endpoint = format!("{}{}:{}", prefix, node, iface);
        } else {
            pp.endpoint = format!("{}{}", prefix, p.endpoint);
        }
        pp
    }).collect();
    lower_network(&mut topology, &prefixed)?;
}
```

### Tests

| Test | Description |
|------|-------------|
| `test_site_with_network` | Network inside site, members auto-prefixed |
| `test_site_network_subnet` | Network with subnet inside site |
| `test_site_network_ports` | Port configs inside site network |

---

## Part B: Cross-References Inside Sites

### Current (broken)

```nll
site a18a {
  node red : router
  node black : router
  link red:eth1 -- black:eth0 { 10.18.2.1/24 -- 10.18.2.2/24 }
  # This cross-reference fails:
  node cc { route default via ${red.eth0} }
  # Because after prefixing, it becomes ${a18a-red.eth0}
  # but the resolver looks for "red" in the node list and finds nothing
}
```

### Target

```nll
site a18a {
  node red : router
  node black : router
  link red:eth1 -- black:eth0 { 10.18.2.1/24 -- 10.18.2.2/24 }
  node cc { route default via ${red.cc} }   # resolves within site scope
}
```

### Implementation

During site expansion, before prefixing names:
1. Build a local address map from the site's links
2. Resolve `${node.iface}` cross-references using local names
3. Then prefix all names

```rust
ast::Statement::Site(s) => {
    // Phase 1: Build local address map from site's links
    let local_addr_map = build_addr_map_from_links(&s.body);

    // Phase 2: Resolve cross-references using local names
    let resolved_body = resolve_cross_refs(&s.body, &local_addr_map);

    // Phase 3: Prefix all names and lower
    for inner in &resolved_body {
        match inner {
            ast::Statement::Node(n) => {
                let mut prefixed = n.clone();
                prefixed.name = format!("{}-{}", s.name, n.name);
                lower_node(&mut topology, &prefixed, &ctx)?;
            }
            // ... links, networks
        }
    }
}
```

### Tests

| Test | Description |
|------|-------------|
| `test_site_cross_ref` | `${red.eth0}` resolves inside site |
| `test_site_cross_ref_route` | Route via cross-reference inside site |
| `test_site_cross_ref_doesnt_leak` | Cross-ref from outside site fails gracefully |

---

## File Changes

| File | Change |
|------|--------|
| `lower.rs` | Fix network prefixing in site expansion; add cross-ref resolution phase |
| `lower.rs` tests | Add 6 tests for site networks and cross-refs |
| `examples/infra-c2-a18-a9.nll` | Move networks inside site blocks |
