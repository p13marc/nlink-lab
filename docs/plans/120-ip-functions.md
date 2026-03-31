# Plan 120: IP Computation Functions (`subnet`, `host`)

**Date:** 2026-03-31
**Status:** Implemented (2026-03-31)
**Effort:** Medium (2-3 days)
**Priority:** P0 — unblocks parametric templates, loop-generated rules, auto-addressing
**Depends on:** Nothing

---

## Problem Statement

NLL's lexer recognizes CIDR/IPv4 as typed tokens before interpolation runs.
Writing `10.${id}.1.0/24` is impossible — the lexer rejects it. This blocks
parametric site templates, loop-generated NAT rules, and computed addressing.

Every major infrastructure DSL (Terraform, netlab, Nix) solves this with
either late typing or computation functions. Following Terraform's proven
`cidrsubnet`/`cidrhost` pattern, NLL should add built-in IP functions.

## New Built-in Functions

### `subnet(base, prefix_len, index)` → CIDR string

Carve subnet #`index` with prefix `/prefix_len` from `base`.

```nll
subnet("10.0.0.0/16", 24, 18)     # → "10.0.18.0/24"
subnet("10.0.0.0/16", 24, 0)      # → "10.0.0.0/24"
subnet("10.0.0.0/8", 16, 2)       # → "10.2.0.0/16"
subnet("172.100.0.0/16", 24, 5)   # → "172.100.5.0/24"
```

Equivalent to Terraform's `cidrsubnet(prefix, newbits, netnum)` but with
absolute prefix length instead of additional bits.

### `host(cidr, host_number)` → IP string

Get host #`host_number` from a subnet.

```nll
host("10.0.18.0/24", 1)           # → "10.0.18.1"
host("10.0.18.0/24", 254)         # → "10.0.18.254"
host("172.16.0.0/30", 1)          # → "172.16.0.1"
host("172.16.0.0/30", 2)          # → "172.16.0.2"
```

Equivalent to Terraform's `cidrhost(prefix, hostnum)`.

### Usage in NLL

Functions return strings that are valid wherever CIDRs or IPs are expected:

```nll
let base = subnet("10.0.0.0/8", 16, 2)       # "10.2.0.0/16"
let lan = subnet(${base}, 24, 1)               # "10.2.1.0/24"

node server { route default via host(${lan}, 1) }
link a:eth0 -- b:eth0 { host(${lan}, 1)/24 -- host(${lan}, 2)/24 }
network lan { subnet ${lan} }
nat { masquerade src ${base} }
```

## Implementation

### 1. Lexer — No changes

Functions are parsed as `Ident(name)` followed by `LParen ... RParen`.
The existing `LParen`/`RParen` tokens and `Ident` are sufficient.

### 2. AST (`ast.rs`)

Add a function call expression:

```rust
/// A built-in function call (evaluated at lowering time).
#[derive(Debug, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub args: Vec<FunctionArg>,
}

/// Function argument: a literal, variable reference, or nested function call.
#[derive(Debug, Clone)]
pub enum FunctionArg {
    String(String),
    Int(i64),
    Var(String),           // ${varname}
    Call(FunctionCall),    // nested: subnet(subnet(...), ...)
}
```

### 3. Parser (`parser.rs`)

Extend `parse_value()` and `parse_cidr_or_name()` to recognize function calls:

```rust
fn parse_value_or_call(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if let Some(Token::Ident(name)) = at(tokens, *pos) {
        if matches!(at(tokens, *pos + 1), Some(Token::LParen)) {
            return parse_function_call(tokens, pos);
        }
    }
    parse_value(tokens, pos)
}

fn parse_function_call(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    let name = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::LParen)?;
    let mut args = Vec::new();
    loop {
        if eat(tokens, pos, &Token::RParen) { break; }
        args.push(parse_value_or_call(tokens, pos)?);
        eat(tokens, pos, &Token::Comma);
    }
    // Return as a deferred expression string: "subnet(10.0.0.0/16, 24, 18)"
    Ok(format!("{}({})", name, args.join(", ")))
}
```

At parse time, function calls are stored as strings in the AST (same as
interpolation expressions). They're evaluated during lowering.

### 4. Lower (`lower.rs`)

Add function evaluation during the interpolation/resolution phase:

```rust
fn eval_function(expr: &str) -> Result<String> {
    // Parse "subnet(10.0.0.0/16, 24, 18)" or "host(10.0.18.0/24, 1)"
    if let Some(inner) = expr.strip_prefix("subnet(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.splitn(3, ',').map(|s| s.trim()).collect();
        let base: ipnet::Ipv4Net = parts[0].trim_matches('"').parse()?;
        let prefix: u8 = parts[1].parse()?;
        let index: u32 = parts[2].parse()?;
        let subnets: Vec<_> = base.subnets(prefix)?.collect();
        return Ok(subnets[index as usize].to_string());
    }
    if let Some(inner) = expr.strip_prefix("host(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.splitn(2, ',').map(|s| s.trim()).collect();
        let net: ipnet::Ipv4Net = parts[0].trim_matches('"').parse()?;
        let host_num: u32 = parts[1].parse()?;
        let hosts: Vec<_> = net.hosts().collect();
        return Ok(hosts[host_num as usize - 1].to_string());
    }
    Err(...)
}
```

Integrate into the existing `interpolate()` / `resolve()` pipeline so that
function results can themselves contain `${var}` references that get resolved.

### 5. Dependencies

Add `ipnet` crate to `Cargo.toml`:

```toml
ipnet = "2"
```

This crate provides `Ipv4Net::subnets(prefix)` and `Ipv4Net::hosts()` —
the exact operations needed.

### 6. Validation

After function evaluation, validate that:
- `subnet()` index doesn't exceed available subnets
- `host()` number doesn't exceed available hosts
- Results are valid CIDR/IP strings

### 7. Tests

| Test | Description |
|------|-------------|
| `test_subnet_basic` | `subnet("10.0.0.0/16", 24, 18)` → `"10.0.18.0/24"` |
| `test_subnet_from_8` | `subnet("10.0.0.0/8", 16, 2)` → `"10.2.0.0/16"` |
| `test_subnet_index_0` | `subnet("10.0.0.0/16", 24, 0)` → `"10.0.0.0/24"` |
| `test_host_basic` | `host("10.0.18.0/24", 1)` → `"10.0.18.1"` |
| `test_host_last` | `host("10.0.18.0/24", 254)` → `"10.0.18.254"` |
| `test_host_slash30` | `host("172.16.0.0/30", 2)` → `"172.16.0.2"` |
| `test_nested_call` | `host(subnet("10.0.0.0/16", 24, 1), 3)` → `"10.0.1.3"` |
| `test_let_with_function` | `let x = subnet(...)` then `${x}` in route |
| `test_function_in_route` | `route default via host("10.0.0.0/24", 1)` |
| `test_function_in_link` | Link addresses using `host()`/`subnet()` |
| `test_function_in_nat` | NAT rules using `host()` |
| `test_subnet_overflow` | Index exceeds available subnets → error |
| `test_host_overflow` | Host number exceeds subnet → error |
| `test_parse_function_call` | Parser: `subnet("10.0.0.0/16", 24, 18)` tokenizes correctly |

### File Changes

| File | Change |
|------|--------|
| `Cargo.toml` | Add `ipnet = "2"` dependency |
| `ast.rs` | Add `FunctionCall`, `FunctionArg` types (optional — can use string repr) |
| `parser.rs` | Extend `parse_value()` to handle `ident(args...)` calls |
| `lower.rs` | Add `eval_function()` to resolve function calls during lowering |
| `render.rs` | Render function calls as-is (they're strings in the AST) |
| `examples/` | Update infra example to use functions |
