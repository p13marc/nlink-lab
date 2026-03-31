# Plan 117: Route Groups (Multi-Destination Shorthand)

**Date:** 2026-03-31
**Status:** Implemented (2026-03-31)
**Effort:** Small (half day)
**Priority:** P3 — reduces repetition

---

## Problem Statement

Nodes with many routes to the same gateway produce verbose, repetitive NLL:

```nll
node dcs : router {
  route 144.18.1.0/24 via 10.2.2.2
  route 144.18.2.0/24 via 10.2.2.2
  route 144.18.3.0/24 via 10.2.2.2
  route 144.9.1.0/24 via 10.2.2.2
  route 144.9.4.0/24 via 10.2.2.2
}
```

## NLL Syntax

### List of destinations

```nll
node dcs : router {
  route [144.18.1.0/24, 144.18.2.0/24, 144.18.3.0/24] via 10.2.2.2
  route [144.9.1.0/24, 144.9.4.0/24] via 10.2.2.2
}
```

### Single destination (unchanged)

```nll
node dcs : router {
  route default via 10.2.2.2
  route 10.0.0.0/8 dev eth1
}
```

## Implementation

### Parser

In `parse_route_def()`, check if the destination is `[` (list) or a single value:

```rust
let destinations = if eat(tokens, pos, &Token::LBracket) {
    let mut dests = Vec::new();
    loop {
        dests.push(parse_cidr_or_name(tokens, pos)?);
        if !eat(tokens, pos, &Token::Comma) { break; }
    }
    expect(tokens, pos, &Token::RBracket)?;
    dests
} else {
    vec![parse_route_destination(tokens, pos)?]
};
```

### Lower

Expand list destinations into individual route entries:

```rust
for dest in &route.destinations {
    node.routes.insert(dest.clone(), RouteConfig { via, dev, metric });
}
```

### Render

Render list routes back as lists when multiple routes share the same gateway.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_route_list` | Parser: `route [10.0.0.0/8, 10.1.0.0/8] via 10.2.2.2` |
| `test_lower_route_list_expands` | Lower: list → individual routes |
| `test_single_route_unchanged` | Backward compat: `route default via 10.0.0.1` |

## File Changes

| File | Change |
|------|--------|
| `parser.rs` | ~15 lines in route parsing |
| `lower.rs` | Expand list destinations |
| `render.rs` | Group routes by gateway for rendering |
