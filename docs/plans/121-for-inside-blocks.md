# Plan 121: `for` Loops Inside Blocks

**Date:** 2026-03-31
**Status:** Implemented (2026-04-01)
**Effort:** Medium (1-2 days)
**Priority:** P0 — eliminates repetitive NAT/firewall/route rules
**Depends on:** Plan 120 (IP functions) for full value, but independently useful

---

## Problem Statement

`for` loops only work at the top-level statement level. They can't generate
repeated items inside `nat {}`, `firewall {}`, node property blocks, or
`validate {}` blocks. This forces manual enumeration of rules that follow
a mechanical pattern.

**Example:** FIREWALL has 10 DNAT rules, one per (asset, link_type) pair.
These follow a formula but must be written individually.

## Proposed Syntax

```nll
node c2-fw : router {
  # for loop generates routes inside a node block
  for asset in [18, 19] {
    for lt in [1, 2, 3] {
      route host("144.0.0.0/8", ${lt} * 256 + ${asset})/32 via 10.2.2.2
    }
  }

  nat {
    masquerade src 10.2.0.0/16
    # for loop generates DNAT rules inside a nat block
    for asset in [18, 19] {
      for lt in [1, 2, 3] {
        dnat dst host("144.0.0.0/8", ${lt} * 256 + ${asset})/32 to host(subnet("172.100.0.0/16", 24, ${lt}), ${asset})
      }
    }
  }

  firewall policy drop {
    accept ct established,related
    # for loop generates firewall rules
    for port in [80, 443, 8080] {
      accept tcp dport ${port}
    }
  }
}

validate {
  # for loop generates assertions
  for asset in [18, 19, 9, 10] {
    reach c2-fw a${asset}-black
  }
}
```

## Blocks That Support `for`

| Block | Generates | Example |
|-------|-----------|---------|
| Node property block | Routes, interfaces, exec commands | `for x { route ... }` |
| `nat {}` | NAT rules | `for x { dnat ... }` |
| `firewall {} ` | Firewall rules | `for x { accept ... }` |
| `validate {}` | Assertions | `for x { reach ... }` |
| `scenario step {}` | Actions | `for x { down ... }` |
| `benchmark {}` | Tests | `for x { ping ... }` |

## Implementation

### 1. AST Changes

Add `For` variant to each block's item enum. Most blocks use a flat list
of items (rules, routes, assertions). Add a wrapper that can contain
either a direct item or a for-loop that expands to items.

**Option A — Minimal AST change:**
Don't change the AST. Instead, handle `for` in the PARSER by collecting
expanded items. When the parser sees `for` inside a block, it:
1. Parses the for-loop header (variable, range)
2. Parses the body as block items
3. Expands the loop immediately (inline interpolation)
4. Pushes expanded items into the parent block's list

This avoids AST changes entirely — the lowerer sees pre-expanded items.

**Option B — AST-level loops:**
Add `ForLoop` variants to each block's item list. The lowerer expands
them. This preserves the loop structure for rendering but requires
every block parser and lowerer to handle loops.

**Recommendation:** Option A (parser-level expansion) is simpler and
consistent with how top-level `for` loops already work — they're expanded
during lowering into flat statement lists.

### 2. Parser Changes

For each block parser that supports `for`, add a check at the item level:

```rust
// Inside parse_nat_def(), parse_firewall_def(), etc.
loop {
    skip_newlines(tokens, pos);
    if eat(tokens, pos, &Token::RBrace) { break; }

    // NEW: Handle for-loops
    if matches!(at(tokens, *pos), Some(Token::For)) {
        let for_loop = parse_for(tokens, pos)?;
        // Expand the loop body, interpolating the variable
        for binding in expand_range(&for_loop.range) {
            let mut vars = HashMap::new();
            vars.insert(for_loop.var.clone(), binding);
            for item in &for_loop.body {
                // Parse each body item with variable substitution
                rules.push(interpolate_and_parse_item(item, &vars)?);
            }
        }
        continue;
    }

    // Normal item parsing...
}
```

Actually, the cleaner approach: parse the for-loop body as raw tokens,
then re-parse with variable substitution for each iteration. This reuses
the existing for-loop expansion logic.

### 3. Block-Specific Handlers

Each block needs a `for` handler. The implementation is the same pattern
for all:

1. Parse `for VAR in RANGE { BODY }`
2. For each value in range, substitute `${VAR}` in body
3. Re-parse body as block items
4. Append to parent's item list

### 4. Tests

| Test | Description |
|------|-------------|
| `test_for_in_nat` | `nat { for x in [1,2] { dnat ... } }` → 2 rules |
| `test_for_in_firewall` | `firewall { for p in [80,443] { accept tcp dport ${p} } }` → 2 rules |
| `test_for_in_routes` | Node with `for x in [1,2,3] { route ... }` → 3 routes |
| `test_for_in_validate` | `validate { for x in [a,b,c] { reach x y } }` → 3 assertions |
| `test_nested_for_in_block` | Nested for loops inside nat block |
| `test_for_with_function` | `for x { dnat ... host(subnet(...), ${x}) }` |

### File Changes

| File | Change |
|------|--------|
| `parser.rs` | Add `for` handling to `parse_nat_def`, `parse_firewall_def`, `parse_node_block`, `parse_validate`, `parse_scenario` |
| `lower.rs` | May need minor changes if expansion is deferred to lowering |
