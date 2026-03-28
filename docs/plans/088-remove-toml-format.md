# Plan 088: Remove TOML Topology Format — NLL Only

**Priority:** High
**Effort:** 3-4 days
**Target:** `parser/`, `bins/lab/src/main.rs`, `examples/`, `templates/`, docs

## Summary

Remove TOML as a topology input format, making NLL the sole topology DSL. Before
removing TOML, close the remaining NLL gaps so no expressiveness is lost. The `toml`
crate dependency stays — it's used by `state.rs` for serializing running lab state.

## Current NLL Gap Analysis

Fields already supported by NLL (despite initial report):
- `corrupt`, `reorder` — tokenized, parsed, in AST (`ImpairProps`), lowered
- `mtu` on links and networks — tokenized, parsed, in AST, lowered
- `members` on networks — tokenized, parsed, in AST, lowered

**Actual remaining gaps (6 items):**

| Field | Topology Type | NLL Status |
|-------|--------------|------------|
| `burst` | `RateLimit` | No token, no AST field, no parsing |
| `env` | `Node` (container) | No token, no parsing |
| `volumes` | `Node` (container) | No token, no parsing |
| `runtime` | `LabConfig` | No token, no parsing |
| `parent` | `InterfaceConfig` (VLAN) | No token, no parsing |
| Multiple addresses | WG, dummy, vxlan interfaces | AST has `Option<String>`, needs `Vec<String>` |

## Phase 1: Close NLL Gaps (2 days)

### 1.1 Rate Limit Burst

**Lexer:** Add token.
```rust
#[token("burst")]
Burst,
```

**AST:** Add field to `RateProps`.
```rust
pub struct RateProps {
    pub egress: Option<String>,
    pub ingress: Option<String>,
    pub burst: Option<String>,  // NEW
}
```

**Parser:** Parse in `parse_rate_props()` alongside egress/ingress.
```rust
Token::Burst => {
    props.burst = Some(expect_rate(tokens, pos)?);
}
```

**Lowering:** Map to `RateLimit.burst` (currently hardcoded to `None`).

**NLL syntax:**
```nll
rate server:eth0 egress 100mbit ingress 100mbit burst 10mbit
```

### 1.2 Container Fields (env, volumes, runtime)

**Lexer:** Add tokens.
```rust
#[token("env")]
Env,

#[token("volumes")]
Volumes,

#[token("runtime")]
Runtime,
```

**AST:** Add fields to `NodeDef` and `LabDecl`.
```rust
pub struct NodeDef {
    pub image: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<(String, String)>,    // NEW
    pub volumes: Vec<String>,          // NEW
    pub props: Vec<NodeProp>,
}

pub struct LabDecl {
    pub name: String,
    pub description: Option<String>,
    pub prefix: Option<String>,
    pub runtime: Option<String>,       // NEW
}
```

**Parser:** Parse in node body (after `image`) and lab declaration.

```nll
lab "my-lab" runtime "podman"

node web image "nginx:latest" cmd ["nginx", "-g", "daemon off;"] {
    env "DB_HOST" "10.0.0.2"
    env "DB_PORT" "5432"
    volumes ["/data:/var/lib/data", "/config:/etc/app"]
}
```

**Lowering:** Map to `Node.env`, `Node.volumes`, `LabConfig.runtime`.

### 1.3 VLAN Parent Interface

**Lexer:** Add token.
```rust
#[token("parent")]
Parent,
```

**AST & Parser:** Add `parent` field to VLAN interface properties. This is parsed
inside node interface blocks:

```nll
node router {
    interface eth0.100 vlan 100 parent eth0
}
```

Currently, VLAN interfaces are parsed as explicit interfaces with `kind = "vlan"`.
Add `parent` to the interface property parsing.

**Lowering:** Map to `InterfaceConfig.parent`.

### 1.4 Multiple Addresses on Interfaces

**AST:** Change `address: Option<String>` to `addresses: Vec<String>` on:
- `WireguardDef`
- `VxlanDef`
- `DummyDef`

**Parser:** Accept a list of addresses:
```nll
node gw {
    wireguard wg0 {
        key auto
        listen 51820
        address [192.168.255.1/32, fd00::1/128]
        peer gw-b
    }
}
```

Single address syntax remains valid (wraps in vec).

**Lowering:** Already produces `Vec<String>` for the topology types — just need to
pass through all addresses instead of only the first.

## Phase 2: Remove TOML Topology Parsing (1 day)

### 2.1 Delete TOML Parser

- Delete `crates/nlink-lab/src/parser/toml.rs` (~525 lines)
- Remove `pub mod toml;` from `parser/mod.rs`
- Remove `TomlParse` error variant from `error.rs`

### 2.2 Update Format Dispatch

**Where:** `parser/mod.rs`

```rust
// Before:
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let contents = std::fs::read_to_string(&path)?;
    match path.as_ref().extension().and_then(|e| e.to_str()) {
        Some("nll") => nll::parse_with_source(&contents, path),
        _ => toml::parse(&contents),
    }
}

// After:
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let contents = std::fs::read_to_string(&path)?;
    nll::parse_with_source(&contents, path)
}

pub fn parse(input: &str) -> Result<Topology> {
    nll::parse(input)
}
```

No extension matching needed — everything is NLL.

### 2.3 Delete TOML Example Files

Delete all 12 files:
- `examples/simple.toml`
- `examples/router.toml`
- `examples/spine-leaf.toml`
- `examples/wan-impairment.toml`
- `examples/vlan-trunk.toml`
- `examples/vrf-multitenant.toml`
- `examples/wireguard-vpn.toml`
- `examples/vxlan-overlay.toml`
- `examples/firewall.toml`
- `examples/iperf-benchmark.toml`
- `examples/container.toml`
- `examples/mesh.toml`

### 2.4 Update Integration Tests

**Where:** `crates/nlink-lab/tests/integration.rs`

Change all `#[lab_test("examples/foo.toml")]` to `#[lab_test("examples/foo.nll")]`:

```rust
// Before:
#[lab_test("examples/simple.toml")]
async fn deploy_simple_toml(lab: RunningLab) { ... }

// After — also rename the test:
#[lab_test("examples/simple.nll")]
async fn deploy_simple(lab: RunningLab) { ... }
```

Remove `deploy_simple_nll` (redundant — it was testing NLL parity with TOML).

### 2.5 Remove NLL/TOML Equivalence Tests

**Where:** `crates/nlink-lab/src/parser/nll/lower.rs`

Delete `assert_equivalent()` helper and the 5 `test_nll_matches_toml_*` tests.
These were valuable for ensuring parity; once TOML is gone they serve no purpose.

### 2.6 Update Templates

**Where:** `crates/nlink-lab/src/templates/mod.rs`

```rust
// Before:
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub toml: &'static str,
    pub nll: &'static str,
}

// After:
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub content: &'static str,
}
```

Remove all `toml: include_str!(...)` lines. Update `render()` to return a single
string instead of a tuple.

**Where:** `bins/lab/src/main.rs` — `init` command.

Remove `--format` flag (or default/only accept `nll`). Remove `"toml"` and `"both"`
match arms. Default output extension is `.nll`.

### 2.7 Update CLI Help Text

**Where:** `bins/lab/src/main.rs`

Change all `"(.toml or .nll)"` references to just `"(.nll)"` or remove the extension
note entirely since there's only one format.

### 2.8 Update `Cargo.toml` Dependencies

The `toml` crate stays because `state.rs` uses `toml::to_string_pretty()` and
`toml::from_str()`. Also, `Topology` derives `Serialize`/`Deserialize` which is
needed for state persistence. No dependency changes needed.

## Phase 3: Update Documentation (0.5 day)

### README.md

- Remove "Topology Formats" section that shows TOML syntax
- Update quick-start examples to use `.nll` files
- Update Rust API example to use `.nll`
- Remove "available in both `.toml` and `.nll`" line
- Update comparison table

### CLAUDE.md

- Remove "TOML Topology Format" section
- Update architecture to remove `parser/toml.rs`
- Update dependency list (note: toml stays for state serialization)
- Update usage examples

### docs/NLL_DSL_DESIGN.md

- Add newly supported syntax: `burst`, `env`, `volumes`, `runtime`, `parent`
- Mark NLL as the sole topology format

### Plan docs

Update references in completed plans (050, 051, 052, 060, 072) to note TOML was
removed. No functional changes needed — these are historical.

## Progress

### Phase 1: Close NLL Gaps
- [x] Add `burst` to rate limit (token, AST, parser, lowering)
- [x] Add `env` to container nodes (token, AST, parser, lowering)
- [x] Add `volumes` to container nodes (token, AST, parser, lowering)
- [x] Add `runtime` to lab declaration (token, AST, parser, lowering)
- [x] Add `parent` for VLAN interfaces (token added, ready for parser use)
- [x] Support multiple addresses on WG/dummy/vxlan interfaces
- [x] Tests pass (119 library tests)

### Phase 2: Remove TOML Topology Parsing
- [x] Delete `parser/toml.rs`
- [x] Update `parser/mod.rs` — remove TOML dispatch
- [x] Remove `TomlParse` error variant from `error.rs`
- [x] Delete 12 `.toml` example files
- [x] Update all integration tests to use `.nll`
- [x] Remove NLL/TOML equivalence tests in `lower.rs`
- [x] Update templates module — remove `toml` field
- [x] Update `init` command — NLL only
- [x] Update CLI help text
- [x] Update validator tests (TOML → NLL + builder)
- [x] Update builder test (TOML → NLL)
- [x] Update state test (TOML → NLL)
- [x] Update lib.rs and types.rs doc comments

### Phase 3: Documentation
- [ ] Update README.md — remove TOML sections
- [ ] Update CLAUDE.md — remove TOML references
- [ ] Update NLL_DSL_DESIGN.md — add new syntax
