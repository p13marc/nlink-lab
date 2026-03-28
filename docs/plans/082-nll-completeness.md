# Plan 082: NLL Parser Completeness

**Priority:** Medium
**Effort:** 3-4 days
**Target:** `parser/nll/parser.rs`, `parser/nll/lower.rs`, `parser/nll/lexer.rs`

## Summary

Complete the NLL DSL implementation by closing gaps between the language specification
(`docs/NLL_DSL_DESIGN.md`) and the actual parser/lowering code. Also improve error
messages and handle edge cases.

## 1. Image/Cmd Lowering

**Status:** Parsed in AST but never lowered to `Topology`.

**Where:** `parser/nll/lower.rs` — node lowering (around line 478).

The parser correctly parses `image "ubuntu:22.04"` and `cmd "sleep infinity"` into
`NodeDef.image` and `NodeDef.cmd`, but the lowering step ignores them.

```rust
// In lower_node():
if let Some(image) = &node_def.image {
    n.container = Some(ContainerConfig {
        image: image.clone(),
        cmd: node_def.cmd.clone(),
        ..Default::default()
    });
}
```

Also fix the parser to accept `cmd` as a string list, not just a single string:

```rust
// parser.rs — parse_node_body():
if eat(tokens, pos, &Token::Cmd) {
    if at(tokens, *pos, &Token::LBracket) {
        cmd = Some(parse_string_list(tokens, pos)?);
    } else {
        cmd = Some(vec![expect_string(tokens, pos)?]);
    }
}
```

**Tests:**
```nll
lab "container-test"
node web image "nginx:latest" cmd ["nginx", "-g", "daemon off;"]
node db image "postgres:16"
```

Verify: `topology.nodes["web"].container.image == "nginx:latest"` and
`topology.nodes["web"].container.cmd == Some(vec!["nginx", "-g", "daemon off;"])`.

## 2. ICMP and IP Header Firewall Rules

**Status:** Only `ct state`, `tcp dport`, and `udp dport` are supported. The spec
implies ICMP and IP matching should work.

**Where:** `parser/nll/parser.rs` — `parse_firewall_match()` (around line 596-641).

Add new match types:

```rust
// ICMP type matching
Token::Ident if slice == "icmp" => {
    expect_ident_eq(tokens, pos, "type")?;
    let icmp_type = expect_int(tokens, pos)?;
    MatchExpr::Icmp { icmp_type }
}

// IP source/destination
Token::Ident if slice == "ip" => {
    let direction = expect_one_of_idents(tokens, pos, &["saddr", "daddr"])?;
    let addr = expect_cidr_or_addr(tokens, pos)?;
    MatchExpr::Ip { direction, addr }
}

// Protocol matching
Token::Ident if slice == "meta" => {
    expect_ident_eq(tokens, pos, "l4proto")?;
    let proto = expect_ident(tokens, pos)?;
    MatchExpr::L4Proto { proto }
}
```

**AST changes** (`ast.rs`):

```rust
pub enum MatchExpr {
    CtState(Vec<String>),
    TcpDport(u16),
    UdpDport(u16),
    IcmpType(u32),          // New
    IpSaddr(String),        // New
    IpDaddr(String),        // New
    L4Proto(String),        // New
}
```

**Lowering** (`lower.rs`): Map new AST variants to `FirewallRule` match expressions.

**Deploy** (`deploy.rs`): Extend `apply_match_expr()` to handle the new types via
nlink's Rule builder.

**Tests:**
```nll
firewall policy drop {
    accept ct established,related
    accept icmp type 8        # echo request
    accept ip saddr 10.0.0.0/8
    drop tcp dport 22
}
```

## 3. Interpolation Without Spaces

**Status:** `${i + 1}` works but `${i+1}` does not (returns raw string).

**Where:** `parser/nll/lower.rs` — `eval_expr()` (around line 135-217).

The current code splits on `" + "`, `" - "`, etc. (with spaces). Support both forms:

```rust
fn eval_expr(expr: &str, vars: &HashMap<String, String>) -> Result<String> {
    let expr = expr.trim();

    // Try with-space operators first, then without
    for (spaced, unspaced) in [(" + ", "+"), (" - ", "-"), (" * ", "*"), (" / ", "/")] {
        let op = if expr.contains(spaced) { spaced }
                 else if expr.contains(unspaced) { unspaced }
                 else { continue };

        if let Some((left, right)) = expr.split_once(op) {
            let left_val = resolve_operand(left.trim(), vars)?;
            let right_val = resolve_operand(right.trim(), vars)?;
            // ... compute
        }
    }
    // Fall through to simple variable lookup
    resolve_variable(expr, vars)
}
```

**Tests:**
```nll
let base_delay = 10
for i in 1..3 {
    link spine${i}:eth0 -- leaf${i}:eth0 {
        delay ${base_delay*i}ms    # no spaces
    }
}
```

## 4. Error on Undefined Variables

**Status:** Undefined variables silently produce `"${varname}"` in output strings,
causing confusing downstream parse failures.

**Where:** `parser/nll/lower.rs` — `eval_expr()` / `interpolate()`.

Change behavior: if a variable is not found in the context, return an error:

```rust
fn resolve_variable(name: &str, vars: &HashMap<String, String>) -> Result<String> {
    vars.get(name)
        .cloned()
        .ok_or_else(|| Error::NllParse(format!("undefined variable: ${{{name}}}")))
}
```

**Exception:** During AST validation (before loop expansion), variables from enclosing
`for` loops won't be in scope yet. Validation should only check `let`-bound variables;
loop variable resolution happens during expansion.

## 5. Duplicate Name Detection

**Status:** Duplicate profile/node/network names silently overwrite previous definitions.

**Where:** `parser/nll/lower.rs` — profile/node collection phase.

```rust
// During profile collection:
if ctx.profiles.contains_key(&name) {
    errors.push(format!("duplicate profile name: {name}"));
}
ctx.profiles.insert(name, profile);

// During node lowering:
if topo.nodes.contains_key(&name) {
    errors.push(format!("duplicate node name: {name}"));
}
```

Same for networks.

## 6. Better Error Messages

**Where:** `parser/nll/parser.rs` — various `expect()` calls.

Current messages say "expected statement" without listing valid options. Improve:

```rust
// Before:
return Err(err(tokens, *pos, "expected statement".to_string()));

// After:
return Err(err(tokens, *pos,
    "expected statement: lab, profile, node, link, network, for, let, \
     impairments, rate, or firewall".to_string()
));
```

Apply to:
- Top-level statement expectations
- Link block contents ("expected address pair, mtu, delay, jitter, loss, rate, or `->`/`<-`")
- Node block contents ("expected forward, route, interface, vrf, wireguard, vxlan, firewall, or run")

## 7. UTF-8 Aware Column Counting

**Where:** `parser/nll/lexer.rs` — column calculation (around line 272).

Current code uses byte offsets, which miscounts columns when multi-byte characters
(emoji in comments, non-ASCII identifiers) appear before the error:

```rust
// Before (byte-based):
let col = span.start - input[..span.start].rfind('\n').map_or(0, |p| p + 1) + 1;

// After (char-based):
let line_start = input[..span.start].rfind('\n').map_or(0, |p| p + 1);
let col = input[line_start..span.start].chars().count() + 1;
```

## Progress

### Feature Completeness
- [x] Lower `image`/`cmd` from AST to Topology (done in plan 088)
- [x] Support `cmd` as string list in parser (done in plan 088)
- [ ] Add ICMP type matching to firewall rules
- [ ] Add IP saddr/daddr matching to firewall rules
- [ ] Add `meta l4proto` matching to firewall rules
- [ ] Extend `apply_match_expr()` in deploy.rs for new match types

### Interpolation & Variables
- [x] Support `${i+1}` without spaces (trim operands)
- [ ] Error on undefined variables instead of silent passthrough
- [ ] Test multi-variable expressions

### Validation
- [x] Detect duplicate profile names (warning)
- [x] Detect duplicate node names (error)
- [x] Detect duplicate network names (error)

### Error Quality
- [x] List valid keywords in "expected statement" errors
- [x] List valid properties in node block errors
- [x] UTF-8 aware column counting in lexer diagnostics
