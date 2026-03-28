# Plan 086: Feature Flags & Publishing

**Priority:** Medium
**Effort:** 2-3 days
**Target:** `Cargo.toml` (workspace + crates), `crates/nlink-lab/src/lib.rs`

## Summary

Add Cargo feature flags for optional functionality, prepare the crate for
crates.io publication, and gate heavy dependencies behind features.

## 1. Feature Flag Design

### Workspace Cargo.toml

```toml
[workspace.features]
default = ["containers", "nftables", "tc", "wireguard", "vxlan", "nll", "diagnostics"]
containers = []
nftables = []
tc = []
wireguard = ["dep:x25519-dalek", "dep:base64"]
vxlan = []
nll = ["dep:logos"]
diagnostics = []
full = ["containers", "nftables", "tc", "wireguard", "vxlan", "nll", "diagnostics"]
```

### nlink-lab Cargo.toml

```toml
[features]
default = ["containers", "nftables", "tc", "wireguard", "vxlan", "nll", "diagnostics"]

# Node types
containers = []         # Docker/Podman container node support

# Network features
nftables = []          # Firewall rule deployment
tc = []                # Traffic control (netem impairments, HTB rate limiting)
wireguard = ["dep:x25519-dalek", "dep:base64"]  # WireGuard VPN interfaces
vxlan = []             # VXLAN overlay tunnels

# Parser
nll = ["dep:logos"]    # NLL DSL parser (TOML parser always available)

# Runtime
diagnostics = []       # Health check and diagnostics engine

# All features
full = ["containers", "nftables", "tc", "wireguard", "vxlan", "nll", "diagnostics"]

[dependencies]
# Always available
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "2"
tracing = "0.1"
tokio = { version = "1", features = ["full"] }
miette = { version = "7", features = ["fancy"] }
nlink = { git = "https://github.com/p13marc/nlink", features = ["full"] }
getrandom = "0.3"
time = { version = "0.3", features = ["formatting"] }

# Feature-gated
logos = { version = "0.15", optional = true }
x25519-dalek = { version = "2", features = ["static_secrets"], optional = true }
base64 = { version = "0.22", optional = true }
```

### Code Gating

**Parser dispatch** (`parser/mod.rs`):
```rust
#[cfg(feature = "nll")]
mod nll;

pub fn parse_file(path: &Path) -> Result<Topology> {
    match path.extension().and_then(|e| e.to_str()) {
        #[cfg(feature = "nll")]
        Some("nll") => nll::parse(&std::fs::read_to_string(path)?),

        #[cfg(not(feature = "nll"))]
        Some("nll") => Err(Error::Parse(
            "NLL parser not available (compile with feature 'nll')".into()
        )),

        _ => toml::parse(&std::fs::read_to_string(path)?),
    }
}
```

**Deploy gating** (`deploy.rs`):
```rust
// WireGuard
#[cfg(feature = "wireguard")]
{
    // WireGuard interface creation, key generation, peer config
}

// nftables
#[cfg(feature = "nftables")]
{
    // Firewall rule deployment
}

// TC / netem
#[cfg(feature = "tc")]
{
    // Impairment and rate limit application
}
```

**Container support** (`deploy.rs`, `running.rs`):
```rust
#[cfg(feature = "containers")]
{
    // Container creation, exec, destroy
}
```

**Diagnostics** (`running.rs`):
```rust
#[cfg(feature = "diagnostics")]
pub fn diagnose(&self) -> Result<Vec<InterfaceDiag>> { ... }
```

### Compile-Time Validation

If a topology uses a feature that's not compiled in, the deployer should error
clearly at validation time:

```rust
// In validator.rs:
#[cfg(not(feature = "wireguard"))]
fn validate_no_wireguard(topo: &Topology, diags: &mut Vec<Diagnostic>) {
    for (name, node) in &topo.nodes {
        if !node.wireguard.is_empty() {
            diags.push(Diagnostic::error(
                "feature-required",
                format!("node '{name}' uses WireGuard but feature 'wireguard' is not enabled"),
            ));
        }
    }
}
```

## 2. nlink-lab-cli Feature Forwarding

The CLI binary should forward features to the library:

```toml
# bins/lab/Cargo.toml
[dependencies]
nlink-lab = { path = "../../crates/nlink-lab", features = ["full"] }

[features]
default = ["full"]
full = []
minimal = []  # For embedded/lightweight use
```

## 3. Publishing Preparation

### Pre-requisites

1. **nlink must be published first** — currently a git dependency.
   Until nlink is on crates.io, nlink-lab cannot be published.

2. **Version strategy:**
   - `nlink-lab` 0.1.0 — initial release
   - `nlink-lab-macros` 0.1.0 — proc macro crate
   - Both must have matching versions

3. **Metadata in Cargo.toml:**
```toml
[package]
name = "nlink-lab"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
description = "Network lab engine using Linux namespaces"
repository = "https://github.com/p13marc/nlink-lab"
keywords = ["networking", "testing", "namespaces", "lab", "topology"]
categories = ["network-programming", "development-tools::testing"]
```

### Checklist

- [ ] All public types have doc comments
- [ ] `cargo doc --no-deps` builds without warnings
- [ ] `cargo package --list` shows only intended files
- [ ] `.cargo/config.toml` excludes test fixtures if large
- [ ] README.md has crates.io badge placeholder
- [ ] CHANGELOG.md exists (even if minimal for 0.1.0)
- [ ] License files present (MIT + Apache-2.0)

## 4. CI Feature Matrix

Test multiple feature combinations in CI:

```yaml
strategy:
  matrix:
    features:
      - ""                    # no features
      - "nll"                 # NLL parser only
      - "tc,nftables"         # network features only
      - "containers"          # container support only
      - "full"                # everything
```

This catches compile errors from missing feature gates.

## Progress

### Feature Flags
- [ ] Design feature flag structure in Cargo.toml
- [ ] Gate NLL parser behind `nll` feature
- [ ] Gate WireGuard behind `wireguard` feature
- [ ] Gate nftables behind `nftables` feature
- [ ] Gate TC/netem behind `tc` feature
- [ ] Gate containers behind `containers` feature
- [ ] Gate diagnostics behind `diagnostics` feature
- [ ] Add compile-time validation for missing features
- [ ] CLI forwards features correctly

### Publishing
- [x] Add crate metadata (keywords, categories, readme)
- [x] Verify `cargo doc --no-deps` builds with zero warnings
- [ ] Verify `cargo package` includes correct files
- [x] Add CHANGELOG.md
- [x] Add license files (MIT + Apache-2.0)

### CI
- [ ] Feature matrix in CI (5 combinations)
- [ ] Test with `--no-default-features`
- [ ] Test with `--all-features`
