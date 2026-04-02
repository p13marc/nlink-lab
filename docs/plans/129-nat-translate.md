# Plan 129: NAT `translate` Shorthand

**Date:** 2026-04-02
**Status:** Done
**Effort:** Small (half day)
**Priority:** P3 — readability improvement for prefix-to-prefix NAT mapping

---

## Problem Statement

The for-loop approach for 1:1 NAT mapping works but encodes a mechanical relationship:

```nll
for lt in 1..3 {
  for asset in [1, 2, 3] {
    dnat dst 144.0.${lt}.${asset}/32 to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
  }
}
```

The intent is simple: "map every host in 144.0.0.0/8 to its counterpart in 172.100.0.0/16,
preserving the host portion." The `translate` directive makes this a single declarative line.

## Proposed Syntax

```nll
nat {
  masquerade src ${base}
  translate 144.0.0.0/8 to 172.100.0.0/16
}
```

**Semantics:** For every host address in the destination range (172.100.x.x) that is
actually assigned to a node interface in the topology, generate a DNAT rule mapping
the corresponding source address (144.0.Y.Z -> 172.100.Y.Z) to that host.

Only addresses that exist in the topology produce rules (sparse mapping). This avoids
generating thousands of unused rules for large prefix ranges.

## Design Decisions

### Per-Host Expansion vs nftables Netmap

nftables supports native prefix-to-prefix translation (`dnat ip prefix to ip daddr map`),
but this approach has drawbacks for nlink-lab:

| Approach | Pros | Cons |
|----------|------|------|
| **Per-host DNAT expansion** | Works with existing nlink API; topology-aware sparse mapping; no nlink changes | O(n) rules for n hosts |
| **nftables netmap** | Single rule; kernel-native performance | Requires new nlink bitwise expression API; maps entire prefix including unused addresses |

**Decision: Per-host expansion.** Lab topologies have tens of hosts, not thousands.
Sparse mapping (only assigned addresses) is the correct semantic. No nlink changes needed.

### Expansion Phase

Translate rules are expanded **after** lowering, as a post-processing pass. This is
necessary because:

1. The lowerer processes nodes sequentially — when lowering node A's NAT, node B's
   addresses may not be resolved yet
2. IP functions (`subnet()`/`host()`) and loops must be fully expanded before we can
   scan assigned addresses
3. After expansion, the resulting `NatRule`s are standard `Dnat` — deploy.rs needs
   zero changes

### Address Mapping Algorithm

For `translate 144.0.0.0/8 to 172.100.0.0/16`:

```
Given: dst_ip = 172.100.1.18 (assigned in topology)
       dst_prefix = 172.100.0.0/16
       src_prefix = 144.0.0.0/8

host_bits = 172.100.1.18 & ~mask(/16) = 0.0.1.18
src_net   = 144.0.0.0 & mask(/8)      = 144.0.0.0
result    = 144.0.0.0 | 0.0.1.18      = 144.0.1.18

Generated rule: dnat dst 144.0.1.18/32 to 172.100.1.18
```

**Validation:** `dst_prefix_len >= src_prefix_len` must hold. If the destination prefix
is wider than the source, multiple destination hosts would map to the same source address.

## Implementation

### Step 1: Types (`types.rs`)

Add `Translate` variant to `NatAction`:

```rust
pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Translate,
}
```

For `Translate` rules, `src` holds the source prefix (e.g., "144.0.0.0/8") and
`target` holds the destination prefix (e.g., "172.100.0.0/16"). The `dst` and
`target_port` fields are `None`.

### Step 2: AST (`ast.rs`)

No new types needed. `NatRuleDef` already has `action: String`, `src`, `target`.
The translate rule uses: `action: "translate"`, `src: Some("144.0.0.0/8")`,
`target: Some("172.100.0.0/16")`.

### Step 3: Parser (`parser.rs`)

In `parse_nat_def()`, add a branch after the `snat` handler:

```rust
} else if eat_kw(tokens, pos, "translate") {
    let src_range = parse_cidr_or_name(tokens, pos)?;
    expect_kw(tokens, pos, "to")?;
    let dst_range = parse_cidr_or_name(tokens, pos)?;
    rules.push(ast::NatRuleDef {
        action: "translate".into(),
        src: Some(src_range),
        dst: None,
        target: Some(dst_range),
        target_port: None,
    });
}
```

Update the error message to include `translate`:
```
"expected NAT rule (masquerade, dnat, snat, translate), found {other}"
```

### Step 4: Lowerer (`lower.rs`)

**4a.** In the NAT lowering match (lines ~1638, ~1784), add the translate case:

```rust
"translate" => types::NatAction::Translate,
```

**4b.** Add a post-lowering function at the end of `lower()`, after the topology is
fully built:

```rust
fn expand_translate_rules(topology: &mut Topology) {
    // 1. Collect all assigned IPv4 addresses: (Ipv4Addr, prefix_len) -> node_name
    let mut assigned: Vec<(Ipv4Addr, &str)> = Vec::new();
    for node in &topology.nodes {
        // Collect from link addresses, network addresses, loopback
        for link in &topology.links {
            // ... collect addresses assigned to this node's interfaces
        }
    }

    // 2. For each node with NAT rules, expand Translate rules
    for node in &mut topology.nodes {
        if let Some(nat) = &mut node.nat {
            let mut expanded = Vec::new();
            for rule in &nat.rules {
                if rule.action == NatAction::Translate {
                    let (src_net, src_prefix) = parse_cidr(&rule.src.as_ref().unwrap());
                    let (dst_net, dst_prefix) = parse_cidr(&rule.target.as_ref().unwrap());
                    // For each assigned address in the dst range, generate a DNAT rule
                    for (addr, _node) in &assigned {
                        if in_prefix(*addr, dst_net, dst_prefix) {
                            let mapped = map_address(*addr, dst_net, dst_prefix, src_net, src_prefix);
                            expanded.push(NatRule {
                                action: NatAction::Dnat,
                                src: None,
                                dst: Some(format!("{mapped}/32")),
                                target: Some(addr.to_string()),
                                target_port: None,
                            });
                        }
                    }
                } else {
                    expanded.push(rule.clone());
                }
            }
            nat.rules = expanded;
        }
    }
}
```

Helper functions:

```rust
fn in_prefix(addr: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    let mask = !0u32 << (32 - prefix);
    (u32::from(addr) & mask) == (u32::from(net) & mask)
}

fn map_address(addr: Ipv4Addr, dst_net: Ipv4Addr, dst_prefix: u8,
               src_net: Ipv4Addr, src_prefix: u8) -> Ipv4Addr {
    let host_bits = u32::from(addr) & !(!0u32 << (32 - dst_prefix));
    let src_masked = u32::from(src_net) & (!0u32 << (32 - src_prefix));
    Ipv4Addr::from(src_masked | host_bits)
}
```

### Step 5: Deploy (`deploy.rs`)

**No changes.** After `expand_translate_rules()`, all translate rules are standard
`Dnat` rules. Add a defensive panic in the `apply_nat()` match arm:

```rust
crate::types::NatAction::Translate => {
    unreachable!("translate rules should be expanded before deploy");
}
```

### Step 6: Render (`render.rs`)

Add a `NatAction::Translate` arm:

```rust
NatAction::Translate => {
    if let (Some(src), Some(target)) = (&rule.src, &rule.target) {
        writeln!(out, "  translate {src} to {target}")?;
    }
}
```

### Step 7: Validator (`validator.rs`)

Add validation for translate rules:

- Source and target must be valid CIDRs
- `dst_prefix_len >= src_prefix_len` (destination cannot be wider than source)
- Warn if no topology addresses fall in the destination range

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_parse_nat_translate` | parser.rs | Parse `translate A to B` inside nat block |
| `test_parse_nat_translate_in_for` | parser.rs | Translate inside for-loop (interpolated ranges) |
| `test_lower_translate_basic` | lower.rs | Expand translate to DNAT rules from topology |
| `test_lower_translate_sparse` | lower.rs | Only assigned addresses produce rules |
| `test_lower_translate_no_matches` | lower.rs | Empty expansion when no addresses in range |
| `test_map_address` | lower.rs | Address mapping correctness (prefix swapping) |
| `test_map_address_different_prefix` | lower.rs | /8 to /16 mapping preserves correct bits |
| `test_render_translate` | render.rs | Translate renders back to NLL syntax |
| `test_validate_translate_prefix_mismatch` | validator.rs | Error when dst prefix wider than src |

## Documentation Updates

| File | Change |
|------|--------|
| `NLL_DSL_DESIGN.md` | Add `translate SRC to DST` to nat_rule grammar |
| `CLAUDE.md` | Mention translate in NAT DSL description |
| `README.md` | Add translate example if NAT section exists |
| `examples/multi-site.nll` | Simplify NAT block using translate |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `types.rs` | +2 | Add `Translate` variant |
| `parser.rs` | +15 | Parse `translate SRC to DST` |
| `lower.rs` | +60 | Post-lower expansion + helpers |
| `deploy.rs` | +3 | Defensive unreachable arm |
| `render.rs` | +5 | Render translate syntax |
| `validator.rs` | +15 | Prefix length validation |
| Tests | +80 | 9 test functions |
| Docs | ~20 | Grammar + examples |
| **Total** | ~200 | |
