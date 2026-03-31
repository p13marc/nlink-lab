# Plan 124: Conditional Logic (`if` blocks, `for ... if` filtering)

**Date:** 2026-03-31
**Status:** Draft
**Effort:** Medium (1-2 days)
**Priority:** P3 — flexibility for advanced topologies
**Depends on:** Nothing

---

## Problem Statement

A9 (simplified, 2 nodes) and A18 (full, 4 nodes) have different structures.
There's no way to conditionally include nodes, links, or rules based on a
parameter. Users must use separate template files or duplicate code.

Additionally, `for` loops can't filter values — you can't skip certain
iterations based on a condition.

## Proposed Syntax

### Standalone `if` blocks

```nll
param simplified default false

node cc : router
node cu { route default via ${cc.eth0} }
link cc:eth0 -- cu:eth0 { subnet 10.0.0.0/24 }

if ${simplified} == false {
  node red : router
  node black : router
  link red:eth1 -- black:eth0 { subnet 10.0.1.0/24 }
}
```

### `for ... if` filtering

```nll
# Only create links for specific (asset, link_type) pairs
for asset in [18, 19, 9, 10] {
  for lt in [1, 2, 3, 4] {
    if (${asset} <= 19 && ${lt} <= 3) || (${asset} <= 10 && (${lt} == 1 || ${lt} == 4)) {
      link c2-fw:l${lt}-a${asset} -- a${asset}-black:l${lt} { ... }
    }
  }
}
```

### Simpler `for ... if` (filter syntax)

```nll
for lt in [1, 2, 3, 4] if ${lt} != 4 {
  # skip wifi (lt=4) for A18 assets
}
```

## Implementation

### `if` as a statement

```rust
// AST
pub struct IfDef {
    pub condition: String,    // expression string
    pub body: Vec<Statement>,
    pub else_body: Option<Vec<Statement>>,
}

// Statement::If(IfDef)
```

The condition is evaluated during lowering using the same expression
evaluator that handles `${...}` interpolation (arithmetic, comparisons,
ternary). If truthy, the body statements are expanded; otherwise the
else_body (if present).

### `for ... if` filter

```rust
// Extend ForLoop
pub struct ForLoop {
    pub var: String,
    pub range: ForRange,
    pub filter: Option<String>,   // NEW: condition to check per iteration
    pub body: Vec<Statement>,
}
```

During expansion, skip iterations where the filter evaluates to false.

### Condition Language

Reuse the existing interpolation expression evaluator which already supports:
- Arithmetic: `+`, `-`, `*`, `/`, `%`
- Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Ternary: `${x == 1 ? "yes" : "no"}`
- Boolean: `&&`, `||` (NEW — need to add)

### Tests

| Test | Description |
|------|-------------|
| `test_if_true` | `if 1 == 1 { node a }` → node created |
| `test_if_false` | `if 1 == 0 { node a }` → no node |
| `test_if_else` | `if ${x} { node a } else { node b }` |
| `test_for_if_filter` | `for i in 1..5 if ${i} != 3 { ... }` → 4 iterations |
| `test_if_with_variable` | `let x = 1; if ${x} == 1 { ... }` |

### File Changes

| File | Change |
|------|--------|
| `ast.rs` | Add `IfDef`, `Statement::If`; add `filter` to `ForLoop` |
| `parser.rs` | Parse `if` blocks and `for ... if` |
| `lower.rs` | Evaluate conditions during expansion |
| `lower.rs` | Add `&&`, `\|\|` to expression evaluator |
