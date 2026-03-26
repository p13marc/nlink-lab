# Plan 060: NLL Parser

**Priority:** High
**Effort:** 5-7 days
**Spec:** `docs/NLL_DSL_DESIGN.md`

## Summary

Implement a parser for the NLL (nlink-lab Language) DSL that compiles `.nll`
files to the same `Topology` struct used by the TOML parser. Both formats
remain supported, selected by file extension.

## Crate Selection

| Role | Crate | Version | Rationale |
|------|-------|---------|-----------|
| Lexer | **logos** | 0.16 | Derive-macro lexer, CIDR/duration/rate as first-class tokens, fastest Rust lexer |
| Parser | **winnow** | 1.0 | Already in dep tree (via `toml`), 1.0 stable API, parses token streams |
| Diagnostics | **miette** | 7.6 | Integrates with `thiserror` (already used), `#[derive(Diagnostic)]`, fancy terminal output |

### Why These?

- **logos + winnow** — logos produces typed tokens (CIDR, Duration, Rate are
  enum variants, not strings), winnow consumes them with combinators. Both are
  stable, fast, and already familiar to the Rust ecosystem. winnow is already
  compiled as a transitive dep.

- **miette over ariadne** — miette integrates with `thiserror` via derive macros
  (`#[derive(Error, Diagnostic)]`). Since `error.rs` already uses `thiserror`,
  miette fits naturally. Ariadne produces prettier output but requires manual
  `Report` construction.

- **Not chumsky** — still in alpha (1.0.0-alpha.8), API has been breaking
  between releases. winnow 1.0 is stable.

- **Not pest** — untyped `Pairs` output requires verbose manual AST conversion.
  logos+winnow build typed AST nodes directly.

## Architecture

```
Source (.nll)
  │
  ▼
Lexer (logos)            → Vec<(Token, Span)>
  │
  ▼
Parser (winnow)          → Ast { lab, profiles, nodes, links, networks, ... }
  │                        (preserves for/let nodes unexpanded)
  │
  ▼
Lowering                 → Expand for-loops, substitute ${var}, resolve profiles
  │
  ▼
Topology                 → Same struct as TOML path
  │
  ▼
Validator                → Same validation rules (unchanged)
```

Both parsers produce `Topology`. The engine is format-agnostic.
See `docs/NLL_DSL_DESIGN.md` for the full language specification, grammar, and examples.

## File Layout

NLL lives as a submodule inside the existing `nlink-lab` crate. No new crate needed.

```
crates/nlink-lab/src/
├── parser/
│   ├── mod.rs            # Public API: parse(), parse_file() with format dispatch
│   ├── toml.rs           # Existing TOML logic (moved from parser.rs)
│   └── nll/
│       ├── mod.rs         # NLL public API: parse_nll()
│       ├── lexer.rs       # logos token definitions
│       ├── ast.rs         # AST types (before lowering)
│       ├── parser.rs      # winnow grammar → AST
│       └── lower.rs       # AST → Topology (loop expansion, variable substitution)
├── error.rs              # Add NllParse variant
└── ...                   # Everything else unchanged
```

## Implementation Steps

### Phase 1: Scaffolding (day 1)

#### 1.1 Add dependencies

```toml
# crates/nlink-lab/Cargo.toml
[dependencies]
logos = "0.16"
# winnow is already a transitive dep, add it as direct:
winnow = "1.0"
miette = { version = "7.6", features = ["fancy"] }
```

#### 1.2 Restructure parser module

Move `parser.rs` → `parser/mod.rs` + `parser/toml.rs`:

```rust
// parser/mod.rs
pub mod toml;
pub mod nll;

use std::path::Path;
use crate::types::Topology;
use crate::error::Result;

/// Parse a topology from a string (auto-detect not possible, defaults to TOML).
pub fn parse(input: &str) -> Result<Topology> {
    toml::parse(input)
}

/// Parse a topology file, selecting format by extension.
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;

    match path.extension().and_then(|e| e.to_str()) {
        Some("nll") => nll::parse(&contents),
        _ => toml::parse(&contents),       // default to TOML
    }
}
```

#### 1.3 Update error.rs

Add NLL-specific error variant:

```rust
#[error("NLL parse error: {0}")]
NllParse(String),
```

Optionally integrate miette's `Diagnostic` derive for rich error output later.

#### 1.4 Verify nothing breaks

All existing tests must pass unchanged after the module restructure.

---

### Phase 2: Lexer (day 1-2)

#### 2.1 Define token enum

```rust
// parser/nll/lexer.rs
use logos::Logos;

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t]+")]              // skip horizontal whitespace
#[logos(skip r"#[^\n]*")]             // skip comments
pub enum Token<'src> {
    // ── Keywords ─────────────────────────────────
    #[token("lab")]       Lab,
    #[token("node")]      Node,
    #[token("profile")]   Profile,
    #[token("link")]      Link,
    #[token("network")]   Network,
    #[token("for")]       For,
    #[token("in")]        In,
    #[token("let")]       Let,
    #[token("impair")]    Impair,
    #[token("rate")]      Rate,

    // Node properties
    #[token("forward")]   Forward,
    #[token("sysctl")]    Sysctl,
    #[token("route")]     Route,
    #[token("lo")]        Lo,
    #[token("firewall")]  Firewall,
    #[token("vrf")]       Vrf,
    #[token("wireguard")] Wireguard,
    #[token("vxlan")]     Vxlan,
    #[token("dummy")]     Dummy,
    #[token("run")]       Run,

    // Sub-keywords
    #[token("default")]   Default,
    #[token("via")]       Via,
    #[token("dev")]       Dev,
    #[token("metric")]    Metric,
    #[token("table")]     Table,
    #[token("mtu")]       Mtu,
    #[token("policy")]    Policy,
    #[token("accept")]    Accept,
    #[token("drop")]      Drop,
    #[token("reject")]    Reject,
    #[token("ct")]        Ct,
    #[token("tcp")]       Tcp,
    #[token("udp")]       Udp,
    #[token("dport")]     Dport,
    #[token("ipv4")]      Ipv4,
    #[token("ipv6")]      Ipv6,
    #[token("key")]       Key,
    #[token("auto")]      Auto,
    #[token("listen")]    Listen,
    #[token("address")]   Address,
    #[token("peers")]     Peers,
    #[token("members")]   Members,
    #[token("port")]      Port,
    #[token("vlan-filtering")]  VlanFiltering,
    #[token("vlan")]      Vlan,
    #[token("pvid")]      Pvid,
    #[token("tagged")]    Tagged,
    #[token("untagged")]  Untagged,
    #[token("vlans")]     Vlans,
    #[token("interfaces")] Interfaces,
    #[token("vni")]       Vni,
    #[token("local")]     Local,
    #[token("remote")]    Remote,
    #[token("background")] Background,
    #[token("description")] Description,
    #[token("prefix")]    Prefix,
    #[token("egress")]    Egress,
    #[token("ingress")]   Ingress,
    #[token("delay")]     Delay,
    #[token("jitter")]    Jitter,
    #[token("loss")]      Loss,
    #[token("corrupt")]   Corrupt,
    #[token("reorder")]   Reorder,

    // ── Operators / Punctuation ──────────────────
    #[token("--")]        DashDash,
    #[token("->")]        ArrowRight,
    #[token("<-")]        ArrowLeft,
    #[token("{")]         LBrace,
    #[token("}")]         RBrace,
    #[token("[")]         LBracket,
    #[token("]")]         RBracket,
    #[token(",")]         Comma,
    #[token(":")]         Colon,
    #[token("=")]         Eq,
    #[token("..")]        DotDot,

    // ── Typed literals (order matters: longer matches first) ──
    #[regex(r"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+")]
    Cidr(&'src str),

    #[regex(r"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+")]
    Ipv4(&'src str),

    #[regex(r"[0-9]+(\.[0-9]+)?(ms|us|ns|s)\b")]
    Duration(&'src str),

    #[regex(r"[0-9]+(mbit|kbit|gbit|bit|mbyte|kbyte|gbyte|byte)\b")]
    RateLit(&'src str),

    #[regex(r"[0-9]+(\.[0-9]+)?%")]
    Percent(&'src str),

    #[regex(r"[0-9]+")]
    Int(&'src str),

    // ── Strings and identifiers ─────────────────
    #[regex(r#""[^"]*""#)]
    String(&'src str),                    // includes quotes

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_-]*")]
    Ident(&'src str),

    // ── Interpolation ───────────────────────────
    #[regex(r"\$\{[^}]+\}")]
    Interp(&'src str),                    // ${var} or ${expr}

    // ── Whitespace (newlines are significant for bare statements) ──
    #[token("\n")]
    Newline,
}
```

#### 2.2 Lexer wrapper

```rust
pub struct Spanned<'src> {
    pub token: Token<'src>,
    pub span: std::ops::Range<usize>,
}

pub fn lex(input: &str) -> Result<Vec<Spanned<'_>>> { ... }
```

#### 2.3 Lexer tests

- Test every token type individually
- Test CIDR vs bare IP vs integer disambiguation
- Test duration vs rate vs integer disambiguation
- Test comment skipping
- Test string interpolation tokens
- Test error on invalid characters

---

### Phase 3: AST (day 2)

#### 3.1 Define AST types

The AST mirrors NLL syntax, not the Topology struct. Lowering bridges the gap.

```rust
// parser/nll/ast.rs

pub struct File<'src> {
    pub lab: LabDecl<'src>,
    pub statements: Vec<Statement<'src>>,
}

pub enum Statement<'src> {
    Profile(ProfileDef<'src>),
    Node(NodeDef<'src>),
    Link(LinkDef<'src>),
    Network(NetworkDef<'src>),
    Impair(ImpairDef<'src>),
    Rate(RateDef<'src>),
    Let(LetDef<'src>),
    For(ForLoop<'src>),
}

pub struct LabDecl<'src> {
    pub name: &'src str,
    pub description: Option<&'src str>,
    pub prefix: Option<&'src str>,
}

pub struct ProfileDef<'src> {
    pub name: &'src str,
    pub props: Vec<NodeProp<'src>>,
}

pub struct NodeDef<'src> {
    pub name: StringOrInterp<'src>,     // may contain ${i}
    pub profile: Option<&'src str>,
    pub props: Vec<NodeProp<'src>>,
}

pub enum NodeProp<'src> {
    Forward(IpVersion),
    Sysctl(&'src str, &'src str),
    Lo(&'src str),                      // CIDR
    Route(RouteDef<'src>),
    Firewall(FirewallDef<'src>),
    Vrf(VrfDef<'src>),
    Wireguard(WireguardDef<'src>),
    Vxlan(VxlanDef<'src>),
    Dummy(DummyDef<'src>),
    Run(RunDef<'src>),
}

pub struct LinkDef<'src> {
    pub left: EndpointRef<'src>,
    pub right: EndpointRef<'src>,
    pub addresses: Option<(&'src str, &'src str)>,  // CIDR pair
    pub mtu: Option<u32>,
    pub impairment: Option<ImpairProps<'src>>,       // symmetric
    pub left_impair: Option<ImpairProps<'src>>,      // ->
    pub right_impair: Option<ImpairProps<'src>>,     // <-
    pub rate: Option<RateProps<'src>>,
}

pub struct ForLoop<'src> {
    pub var: &'src str,
    pub start: u32,
    pub end: u32,
    pub body: Vec<Statement<'src>>,
}

pub struct LetDef<'src> {
    pub name: &'src str,
    pub value: &'src str,               // raw value text
}

// ... (similar structs for Firewall, VRF, WireGuard, etc.)
```

---

### Phase 4: Parser (days 3-4)

#### 4.1 winnow parser for token stream

Parse `&[Spanned<'_>]` into AST using winnow combinators.

**Key parse functions:**

```rust
// parser/nll/parser.rs

fn parse_file(tokens: &mut &[Spanned<'_>]) -> PResult<File<'_>> {
    let lab = parse_lab_decl(tokens)?;
    let stmts = repeat(0.., parse_statement).parse_next(tokens)?;
    Ok(File { lab, statements: stmts })
}

fn parse_statement(tokens: &mut &[Spanned<'_>]) -> PResult<Statement<'_>> {
    alt((
        parse_profile.map(Statement::Profile),
        parse_node.map(Statement::Node),
        parse_link.map(Statement::Link),
        parse_network.map(Statement::Network),
        parse_impair.map(Statement::Impair),
        parse_rate.map(Statement::Rate),
        parse_let.map(Statement::Let),
        parse_for.map(Statement::For),
    )).parse_next(tokens)
}

fn parse_node(tokens: &mut &[Spanned<'_>]) -> PResult<NodeDef<'_>> {
    let _ = expect(Token::Node, tokens)?;
    let name = parse_ident_or_interp(tokens)?;
    let profile = opt(preceded(expect(Token::Colon, _), parse_ident))(tokens)?;
    let props = opt(delimited(
        expect(Token::LBrace, _),
        repeat(0.., parse_node_prop),
        expect(Token::RBrace, _),
    ))(tokens)?;
    Ok(NodeDef { name, profile, props: props.unwrap_or_default() })
}

fn parse_link(tokens: &mut &[Spanned<'_>]) -> PResult<LinkDef<'_>> {
    let _ = expect(Token::Link, tokens)?;
    let left = parse_endpoint(tokens)?;
    let _ = expect(Token::DashDash, tokens)?;
    let right = parse_endpoint(tokens)?;
    let body = opt(parse_link_body)(tokens)?;
    // ... fill in LinkDef from body
}
```

#### 4.2 Helper combinators

```rust
fn expect<'a>(expected: Token<'_>, tokens: &mut &'a [Spanned<'a>]) -> PResult<&'a Spanned<'a>>
fn parse_ident<'a>(tokens: &mut &'a [Spanned<'a>]) -> PResult<&'a str>
fn parse_endpoint<'a>(tokens: &mut &'a [Spanned<'a>]) -> PResult<EndpointRef<'a>>
fn parse_block<'a, T>(inner: impl Parser, tokens: ...) -> PResult<Vec<T>>
```

#### 4.3 Parser tests

One test per grammar production. Test both success and error cases.

- `parse_lab_decl` — bare and with block
- `parse_node` — bare, with profile, with body
- `parse_link` — bare, with addresses, with impairments, asymmetric
- `parse_for` — simple, nested, with interpolation
- `parse_let` — duration, rate, percent values
- `parse_firewall` — policy + rules
- `parse_network` — members, vlans, ports
- Error: missing braces, unknown keywords, bad CIDR

---

### Phase 5: Lowering (days 4-5)

#### 5.1 AST → Topology

The lowering pass:

1. **Expand `for` loops** — iterate range, substitute `${var}` in all nested
   identifiers, CIDRs, and string values
2. **Substitute `let` variables** — replace `${name}` with stored value
3. **Resolve profiles** — merge profile properties into nodes
4. **Build Topology struct** — map AST nodes to `types.rs` structs

```rust
// parser/nll/lower.rs

pub fn lower(ast: &File<'_>) -> Result<Topology> {
    let mut ctx = LowerCtx::new();

    // First pass: collect profiles and variables
    for stmt in &ast.statements {
        match stmt {
            Statement::Profile(p) => ctx.add_profile(p)?,
            Statement::Let(l) => ctx.add_variable(l)?,
            _ => {}
        }
    }

    // Second pass: expand loops and build topology
    let expanded = ctx.expand_statements(&ast.statements)?;

    // Third pass: lower to Topology
    let mut topology = Topology::default();
    topology.lab = lower_lab(&ast.lab);

    for stmt in &expanded {
        match stmt {
            Statement::Node(n)    => lower_node(&mut topology, n, &ctx)?,
            Statement::Link(l)    => lower_link(&mut topology, l)?,
            Statement::Network(n) => lower_network(&mut topology, n)?,
            Statement::Impair(i)  => lower_impair(&mut topology, i)?,
            Statement::Rate(r)    => lower_rate(&mut topology, r)?,
            _ => {}
        }
    }

    Ok(topology)
}
```

#### 5.2 String interpolation

```rust
fn interpolate(template: &str, vars: &HashMap<&str, String>) -> Result<String> {
    // Replace ${var} with value from vars map
    // Support ${expr} like ${i + 1} with simple integer arithmetic
}
```

#### 5.3 Loop expansion

```rust
fn expand_for(for_loop: &ForLoop, vars: &mut HashMap<&str, String>) -> Result<Vec<Statement>> {
    let mut result = Vec::new();
    for i in for_loop.start..=for_loop.end {
        vars.insert(for_loop.var, i.to_string());
        for stmt in &for_loop.body {
            result.push(interpolate_statement(stmt, vars)?);
        }
    }
    vars.remove(for_loop.var);
    Ok(result)
}
```

#### 5.4 Lowering tests

- Test for-loop expansion (simple, nested)
- Test variable substitution in identifiers, CIDRs, strings
- Test profile inheritance
- Test `forward ipv4` → sysctl mapping
- Test symmetric vs asymmetric impairment lowering
- Roundtrip: NLL → Topology should match TOML → Topology for equivalent inputs

---

### Phase 6: Integration (day 5-6)

#### 6.1 Wire up parser/mod.rs

```rust
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("nll") => nll::parse(&contents),
        _ => toml::parse(&contents),
    }
}
```

#### 6.2 Error reporting with miette

Add `#[derive(Diagnostic)]` to NLL parse errors for rich terminal output:

```
Error: unexpected token
  --> topology.nll:5:12
  |
5 |   node r1 [
  |            ^ expected '{', found '['
  |
```

#### 6.3 CLI changes

`bins/lab/src/main.rs` — no changes needed if `parse_file` handles dispatch.
Update help text to mention `.nll` support.

#### 6.4 Write NLL example files

Create `.nll` versions of all existing TOML examples in `examples/`:

- `examples/simple.nll`
- `examples/spine-leaf.nll`
- `examples/wan-impairment.nll`
- `examples/firewall.nll`
- `examples/vxlan-overlay.nll`
- `examples/vrf-multitenant.nll`
- `examples/wireguard-vpn.nll`
- `examples/iperf-benchmark.nll`
- `examples/vlan-trunk.nll`

Plus new examples that showcase NLL's strengths:

- `examples/ring.nll`
- `examples/satellite.nll`
- `examples/fat-tree.nll`

#### 6.5 Equivalence tests

For each example that exists in both `.toml` and `.nll`:

```rust
#[test]
fn test_nll_matches_toml_simple() {
    let toml_topo = parser::toml::parse_file("examples/simple.toml").unwrap();
    let nll_topo = parser::nll::parse_file("examples/simple.nll").unwrap();
    // Compare key fields (name, nodes, links, impairments, ...)
    assert_eq!(toml_topo.lab.name, nll_topo.lab.name);
    assert_eq!(toml_topo.nodes.len(), nll_topo.nodes.len());
    // ...
}
```

---

### Phase 7: Polish (day 6-7)

#### 7.1 Error quality

- Ensure every parse error includes file, line, column, and a helpful message
- Test error messages for common mistakes (missing brace, typo in keyword, bad CIDR)
- Add `help` hints where appropriate ("did you mean 'node'?")

#### 7.2 Validation pass in parser

Add NLL-specific validation that catches errors before lowering:

- Undefined variable in `${var}`
- Duplicate node/profile names
- `for` loop variable shadowing

These supplement (not replace) the existing `validator.rs` which runs on
the `Topology` struct after lowering.

#### 7.3 Documentation

- Update `CLAUDE.md` with NLL build/test commands
- Update CLI help text
- Add doc comments on all public parser functions

---

## Test Strategy

| Layer | What to test | Count (est.) |
|-------|-------------|--------------|
| Lexer | Each token type, disambiguation, errors | ~25 |
| Parser | Each grammar production, error cases | ~30 |
| Lowering | Loop expansion, variable substitution, profile merge | ~15 |
| Equivalence | NLL matches TOML for each example | ~9 |
| Error reporting | Error messages are helpful | ~10 |
| **Total** | | **~89** |

## Progress

### Phase 1: Scaffolding
- [x] Add logos, winnow, miette to Cargo.toml
- [x] Restructure parser.rs → parser/mod.rs + parser/toml.rs
- [x] Add NllParse error variant
- [x] Verify all existing tests pass

### Phase 2: Lexer
- [x] Define Token enum with logos derives
- [x] Implement lex() wrapper with spans
- [x] Lexer tests (25 tests)

### Phase 3: AST
- [x] Define AST types (File, Statement, NodeDef, LinkDef, etc.)
- [x] Interpolation handled via string expansion in lowering (no separate type needed)

### Phase 4: Parser
- [x] Lab declaration parser
- [x] Profile parser
- [x] Node parser (with properties: forward, sysctl, route, lo, firewall, vrf, wg, vxlan, dummy, run)
- [x] Link parser (with addresses, impairments, rate)
- [x] Network parser (members, vlans, ports)
- [x] For loop parser
- [x] Let declaration parser
- [x] Impair/rate standalone parsers
- [x] Parser tests (18 tests)

### Phase 5: Lowering
- [x] Variable substitution (let bindings)
- [x] For loop expansion (simple and nested)
- [x] String interpolation in identifiers, CIDRs
- [x] Profile inheritance resolution
- [x] `forward ipv4` → sysctl expansion
- [x] Symmetric impairment → two Impairment entries
- [x] Asymmetric impairment (-> / <-) handling
- [x] Lower all node properties to Topology types
- [x] Lower links, networks, firewall, VRF, WireGuard
- [x] Lowering tests (14 tests)

### Phase 6: Integration
- [x] Format dispatch in parser/mod.rs (by file extension)
- [x] miette error reporting integration (NllDiagnostic with source spans)
- [x] Write .nll example files (9)
- [x] Equivalence tests (NLL == TOML for 6 shared examples)
- [x] Update CLI help text

### Phase 7: Polish
- [x] Error messages include byte offset for miette rendering
- [x] NLL-specific pre-lowering validation (undefined profiles, empty ranges)
- [x] Documentation updates (CLAUDE.md, plans/README.md)
