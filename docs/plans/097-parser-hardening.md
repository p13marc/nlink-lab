# Plan 097: Parser Hardening — Types, Validation, Token Fixes

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `crates/nlink-lab/src/parser/nll/`

## Summary

Fix the fragile areas identified in the NLL Deep Review: add a float literal
to the lexer, validate typed values at parse time, error on unresolved
cross-references, fix the healthcheck keyword hacks, and complete validator
coverage for all container properties.

All changes improve correctness without changing NLL syntax (except float
literal, which is additive).

## Breaking Changes

**Float literal**: `cpu 0.5` now works without quotes. Previously required
`cpu "0.5"`. The quoted form continues to work.

---

## Phase 1: Float Literal in Lexer (day 1)

### Problem

`cpu "0.5"` requires quotes because `0.5` isn't a recognized token. The
lexer has `Int` (`[0-9]+`) but no float. Duration already accepts floats
(`2.5ms`) via its own regex, but bare floats fail.

### Change

Add a `Float` token to the lexer with careful priority to avoid conflicts:

```rust
#[regex(r"[0-9]+\.[0-9]+", |lex| lex.slice().to_string(), priority = 2)]
Float(String),
```

**Conflict analysis:**
- `Duration` regex: `[0-9]+(\.[0-9]+)?(ms|us|ns|s)` — priority 3, longer match wins
- `Percent` regex: `[0-9]+(\.[0-9]+)?%` — no explicit priority, but `%` suffix disambiguates
- `Cidr` regex: `[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+` — longer match, no conflict
- `Ipv4Addr` regex: `[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+` — 4 groups, Float has 2

A float `0.5` has exactly one dot. An IPv4 address has three dots. A Duration
has a unit suffix. No conflicts.

### Implementation

**Lexer** (`lexer.rs`): Add `Float(String)` token at priority 2 (same as Int).

**Parser** (`parser.rs`): Add `Token::Float(s)` to `parse_value()` (line 299):
```rust
Token::Float(s) => s.clone(),
```

Also add to `token_as_ident()` so floats can appear where idents are expected.

Change `cpu` and `memory` parsing from `expect_string()` to `parse_value()`:
```rust
Some(Token::Cpu) => {
    *pos += 1;
    cpu = Some(parse_value(tokens, pos)?);  // was: expect_string()
}
```

### Tasks

- [ ] Add `Float(String)` token to lexer with priority 2
- [ ] Add `Token::Float` to `parse_value()` match arms
- [ ] Change cpu/memory parsing from `expect_string()` to `parse_value()`
- [ ] Add memory-unit variants to RateLit: `m`, `g`, `t` (for 512m, 2g, etc.)
- [ ] Tests: `cpu 0.5`, `cpu 1.5`, `memory 512m`, `memory "256m"` (backward compat)
- [ ] Update examples to remove quotes from cpu/memory values

## Phase 2: Parse-Time Type Validation (day 1-2)

### Problem

`delay garbage` is accepted by the parser because `parse_value()` accepts
any token (Ident, String, etc.). The error only surfaces at deploy time.

### Change

Add typed parse functions that only accept the correct token type:

```rust
fn expect_duration(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    match &tokens[*pos].token {
        Token::Duration(s) => { let s = s.clone(); *pos += 1; Ok(s) }
        Token::Interp(s) => { let s = s.clone(); *pos += 1; Ok(s) } // allow interpolation
        other => Err(err(tokens, *pos, format!(
            "expected duration (e.g., 10ms, 5s), found {other}"
        ))),
    }
}

fn expect_rate(tokens: &[Spanned], pos: &mut usize) -> Result<String> { ... }
fn expect_percent(tokens: &[Spanned], pos: &mut usize) -> Result<String> { ... }
```

### Where to apply

| Property | Expected type | Current parser | Fix |
|----------|--------------|----------------|-----|
| `delay` | Duration | `parse_value()` | `expect_duration()` |
| `jitter` | Duration | `parse_value()` | `expect_duration()` |
| `loss` | Percent | `parse_value()` | `expect_percent()` |
| `corrupt` | Percent | `parse_value()` | `expect_percent()` |
| `reorder` | Percent | `parse_value()` | `expect_percent()` |
| `rate` (impair) | RateLit | `parse_value()` | `expect_rate()` |
| `egress` | RateLit | `parse_value()` | `expect_rate()` |
| `ingress` | RateLit | `parse_value()` | `expect_rate()` |
| `mtu` | Int | `expect_int()` | Already correct |
| `startup-delay` | Duration | `parse_value()` | `expect_duration()` |

Note: Allow `Token::Interp` in all typed parse functions (for `${variable}`
substitution). The actual validation happens after interpolation, but the
parse error catches obvious typos.

### Tasks

- [ ] Implement `expect_duration()`, `expect_rate()`, `expect_percent()`
- [ ] Replace `parse_value()` calls in `parse_impair_props()` with typed variants
- [ ] Replace `parse_value()` in rate props parsing with `expect_rate()`
- [ ] Replace `parse_value()` for `startup-delay` with `expect_duration()`
- [ ] Keep `Token::Interp` accepted in all typed parse functions
- [ ] Tests: `delay 10ms` (ok), `delay garbage` (error), `delay ${var}` (ok)

## Phase 3: Cross-Reference Validation (day 2)

### Problem

`resolve_cross_refs()` in `lower.rs:300` silently leaves unresolved
`${node.iface}` references as-is. A typo like `${router.eth99}` produces
a literal string in the route, which fails at deploy time with a confusing
netlink error.

### Change

After `resolve_cross_refs()`, scan the topology for any remaining `${...}`
patterns in routes and firewall rules. Emit a warning (not error, since
some interpolations may be intentional strings).

```rust
fn warn_unresolved_refs(topology: &types::Topology) {
    let pattern = "${";
    for (node_name, node) in &topology.nodes {
        for (dest, route) in &node.routes {
            if let Some(via) = &route.via {
                if via.contains(pattern) {
                    tracing::warn!(
                        "unresolved reference in route '{dest}' on '{node_name}': {via}"
                    );
                }
            }
        }
    }
}
```

Call after `resolve_cross_refs()` in `lower_with_base_dir()`.

### Tasks

- [ ] Implement `warn_unresolved_refs()` in lower.rs
- [ ] Call after `resolve_cross_refs()`
- [ ] Scan routes and firewall rules for remaining `${...}` patterns
- [ ] Emit `tracing::warn!` with node name and field location
- [ ] Tests: unresolved ref produces warning (capture tracing output)

## Phase 4: Fix Healthcheck Token Hacks (day 2)

### Problem

The healthcheck block parser reuses `Token::Delay` for "interval" and
`Token::Mtu` for "timeout". This is confusing — `delay` means impairment
delay, not polling interval.

### Change

Add proper `Interval` and `Timeout` tokens:

```rust
#[token("interval")]
Interval,
#[token("timeout")]
Timeout,
#[token("retries")]
Retries,
```

Update the healthcheck parsing block (parser.rs:567-573):

```rust
// Before (hack):
Some(Token::Delay) => { healthcheck_interval = ... }
Some(Token::Mtu) => { healthcheck_timeout = ... }

// After (proper):
Some(Token::Interval) => { *pos += 1; healthcheck_interval = ... }
Some(Token::Timeout) => { *pos += 1; healthcheck_timeout = ... }
Some(Token::Retries) => { *pos += 1; healthcheck_retries = ... }
```

Also add `retries` field to AST and types (currently missing).

### Tasks

- [ ] Add `Interval`, `Timeout`, `Retries` tokens to lexer
- [ ] Add tokens to `token_as_ident()` for backward compat
- [ ] Replace Delay/Mtu hacks in healthcheck parsing with proper tokens
- [ ] Add `retries` field to NodeDef AST, Node types, interpolation, render
- [ ] Remove the string-matching fallback for "interval"/"timeout"
- [ ] Tests: `healthcheck "cmd" { interval 2s; timeout 30s; retries 5 }`
- [ ] Update container-lifecycle.nll example

## Phase 5: Complete Validator Coverage (day 2-3)

### Problem

`validate_container_fields()` checks 9 of 25 container properties. Missing:
`cap_add`, `cap_drop`, `labels`, `pull`, `container_exec`, `healthcheck_interval`,
`healthcheck_timeout`, `startup_delay`, `env_file`, `configs`, `overlay`,
`depends_on`, `privileged`.

### Change

Add all container-only properties to the validation loop:

```rust
// Properties that require image
let container_props: &[(&str, bool)] = &[
    ("cpu", node.cpu.is_some()),
    ("memory", node.memory.is_some()),
    ("entrypoint", node.entrypoint.is_some()),
    ("hostname", node.hostname.is_some()),
    ("workdir", node.workdir.is_some()),
    ("healthcheck", node.healthcheck.is_some()),
    ("privileged", node.privileged),
    ("pull", node.pull.is_some()),
    ("startup_delay", node.startup_delay.is_some()),
    ("env_file", node.env_file.is_some()),
    ("overlay", node.overlay.is_some()),
    ("cap_add", !node.cap_add.is_empty()),
    ("cap_drop", !node.cap_drop.is_empty()),
    ("labels", !node.labels.is_empty()),
    ("container_exec", !node.container_exec.is_empty()),
    ("configs", !node.configs.is_empty()),
    ("depends_on", !node.depends_on.is_empty()),
];
for (prop, has_value) in container_props {
    if *has_value {
        issues.push(ValidationIssue { ... });
    }
}
```

Also add `depends-on-exists` validation: referenced node names must exist.

### Tasks

- [ ] Replace the manual property checks with a loop over all container props
- [ ] Add `depends-on-exists` rule: each name in depends_on must be a defined node
- [ ] Add `depends-on-no-cycle` rule: no circular dependencies
- [ ] Add render support for `cap_drop`, `healthcheck_interval`, `healthcheck_timeout`
- [ ] Tests for each new validation rule

## Progress

### Phase 1: Float Literal
- [ ] Token + parser
- [ ] cpu/memory parse change
- [ ] Tests

### Phase 2: Type Validation
- [ ] expect_duration/rate/percent functions
- [ ] Replace parse_value() calls
- [ ] Tests

### Phase 3: Cross-Ref Validation
- [ ] warn_unresolved_refs()
- [ ] Tests

### Phase 4: Healthcheck Tokens
- [ ] Interval/Timeout/Retries tokens
- [ ] Replace hacks
- [ ] Add retries field
- [ ] Tests

### Phase 5: Validator Coverage
- [ ] Complete container prop validation
- [ ] depends-on validation rules
- [ ] Render gaps
- [ ] Tests
