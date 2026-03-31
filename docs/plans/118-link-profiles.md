# Plan 118: Link Type Profiles (Named Impairment Presets)

**Date:** 2026-03-31
**Status:** Implemented (2026-03-31)
**Effort:** Medium (1 day)
**Priority:** P3 — reduces repetition for multi-link topologies

---

## Problem Statement

Topologies with multiple modem links of the same type (radio, SAT, WiFi)
repeat the same impairment values on every link:

```nll
link fw:radio1 -- a18:radio1 { ... delay 15ms jitter 10ms loss 2% }
link fw:radio2 -- a19:radio1 { ... delay 15ms jitter 10ms loss 2% }
link fw:radio3 -- a20:radio1 { ... delay 15ms jitter 10ms loss 2% }
```

The existing `defaults link { mtu 9000 }` applies to ALL links globally.
There's no way to define per-type defaults.

## NLL Syntax

### Define link profiles

```nll
defaults radio { delay 15ms jitter 10ms loss 2% }
defaults satellite { delay 300ms jitter 50ms }
defaults fiber { delay 1ms }
defaults wifi { delay 3ms jitter 2ms loss 0.5% }
```

### Apply to links

```nll
link fw:radio -- a18:radio : radio {
  172.100.3.2/24 -- 172.100.3.18/24
}

link fw:sat -- a18:sat : satellite {
  172.100.2.2/24 -- 172.100.2.18/24
}
```

The `: profile_name` after the endpoint pair applies the named defaults.
Link-level properties override profile values.

### Override specific values

```nll
link fw:radio -- a18:radio : radio {
  172.100.3.2/24 -- 172.100.3.18/24
  loss 5%          # override radio default of 2%
}
```

## Implementation

### 1. Types

```rust
pub struct LinkProfile {
    pub impairment: Option<Impairment>,
    pub rate_limit: Option<RateLimit>,
    pub mtu: Option<u32>,
}
```

Add `link_profiles: HashMap<String, LinkProfile>` to `Topology`.

### 2. AST + Parser

Extend `defaults` blocks to support named variants:

```
defaults_block = "defaults" ("link" | "impair" | "rate" | IDENT) "{" ... "}"
```

When the defaults target is an ident (not `link`/`impair`/`rate`), it creates
a named link profile.

Extend link parsing to accept `: profile_name` after endpoints:

```
link_decl = "link" endpoint "--" endpoint (":" IDENT)? block?
```

### 3. Lower

When lowering a link with a profile reference:
1. Look up the named profile
2. Apply profile impairment/rate/mtu as defaults
3. Override with any link-level values

### 4. Render

Render named defaults blocks and `: profile` on links.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_named_defaults` | Parser: `defaults radio { delay 15ms }` |
| `test_parse_link_with_profile` | Parser: `link a:e -- b:e : radio { ... }` |
| `test_lower_link_profile_applied` | Lower: profile impairment appears on link |
| `test_lower_link_profile_override` | Lower: link-level overrides profile |

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `LinkProfile`, `link_profiles` on `Topology` |
| `ast.rs` | Extend `DefaultsDef` for named profiles, add profile ref to `LinkDef` |
| `parser.rs` | Parse named defaults + link profile reference |
| `lower.rs` | Apply profile defaults to links during lowering |
| `render.rs` | Render named defaults and link profiles |
