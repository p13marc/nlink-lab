# Plan 129: NAT `translate` Shorthand

**Date:** 2026-04-01
**Status:** Draft
**Effort:** Small (half day)
**Priority:** P3 — nice-to-have simplification for address mapping

---

## Problem Statement

The FIREWALL's DNAT rules encode a mechanical 1:1 mapping between virtual
addresses (144.0.x.x) and modem addresses (172.100.x.x). The for-loop
version is already compact, but the mapping itself is policy that could
be expressed more declaratively.

## Proposed Syntax

```nll
nat {
  masquerade src ${c2}
  translate 144.0.0.0/8 to 172.100.0.0/16
}
```

The `translate` directive generates DNAT rules by:
1. For each host address H in the source range (144.0.x.x)
2. Find the corresponding address in the destination range (172.100.x.x)
3. The mapping preserves the host portion: 144.0.Y.Z → 172.100.Y.Z
4. Only generate rules for addresses that actually exist in the topology
   (i.e., 172.100.Y.Z is assigned to a real node)

## Implementation

### Lowerer

During NAT lowering, if a `translate` rule is encountered:

```rust
NatAction::Translate { src_range, dst_range } => {
    // Find all host addresses in dst_range that exist in the topology
    let assigned = collect_assigned_addresses(topology, dst_range);
    for (dst_ip, node_name) in assigned {
        // Compute corresponding source address
        let src_ip = map_address(dst_ip, dst_range, src_range);
        // Generate DNAT rule: src_ip → dst_ip
        rules.push(NatRule {
            action: NatAction::Dnat,
            dst: Some(format!("{src_ip}/32")),
            target: Some(dst_ip.to_string()),
            ..
        });
    }
}
```

### Types

Add `Translate` variant to `NatAction`:

```rust
pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Translate,  // new
}

pub struct NatRule {
    // ... existing fields
    pub translate_src: Option<String>,  // source range for translate
    pub translate_dst: Option<String>,  // destination range for translate
}
```

## Tests

| Test | Description |
|------|-------------|
| `test_parse_nat_translate` | Parse `translate A to B` |
| `test_lower_translate` | Expand translate to DNAT rules from topology |
| `test_translate_only_assigned` | Only generate rules for addresses in use |

## Documentation Updates

| File | Change |
|------|--------|
| **NLL_DSL_DESIGN.md** | Add `translate` to nat_rule grammar |
| **examples/infra-c2-a18-a9.nll** | Optionally simplify NAT block |

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `Translate` to `NatAction`, translate fields to `NatRule` |
| `ast.rs` | Add `NatRuleDef` translate variant |
| `parser.rs` | Parse `translate SRC to DST` in nat block |
| `lower.rs` | Expand translate to DNAT rules |
| `deploy.rs` | No change (expanded rules are standard DNAT) |
