# Plan 081: Code Quality & Type Safety

**Priority:** High
**Effort:** 2-3 days
**Target:** `types.rs`, `error.rs`, `builder.rs`, `deploy.rs`, `helpers.rs`

## Summary

Improve type safety, error handling ergonomics, and code quality across the codebase.
These changes reduce the surface area for runtime bugs by catching errors at compile
time or build time rather than deploy time.

## 1. Interface Kind Enum

**Where:** `types.rs` — `InterfaceConfig`

Replace `kind: Option<String>` with a proper enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    #[default]
    Veth,
    Dummy,
    Vxlan,
    Vlan,
    Bond,
    Wireguard,
    Loopback,
}

pub struct InterfaceConfig {
    pub kind: Option<InterfaceKind>,  // None = veth (from link)
    // ... rest unchanged
}
```

**Impact:** Eliminates string matching in `deploy.rs` Step 6. Update all match arms
from `Some("vxlan")` to `Some(InterfaceKind::Vxlan)`. Update TOML parser tests.
NLL lowering must produce enum variants instead of strings. Builder `kind()` method
takes the enum.

**Migration:** Serde `rename_all = "lowercase"` preserves TOML compatibility.

## 2. Error Type Stratification

**Where:** `error.rs`

The catch-all `Nlink(#[from] nlink::Error)` hides the operation context. Add
specific error variants:

```rust
#[derive(Debug, Error)]
pub enum Error {
    // Existing
    #[error("parse error: {0}")]
    Parse(String),

    // Replace generic Nlink with specific variants
    #[error("namespace error: {0}")]
    Namespace(String),

    #[error("link error: {0}")]
    Link(String),

    #[error("route error: {0}")]
    Route(String),

    #[error("exec failed in node {node}: {message}")]
    ExecFailed { node: String, message: String },

    #[error("lab {name} is already running")]
    AlreadyRunning { name: String },

    #[error("operation timed out after {duration}")]
    Timeout { duration: String },

    // Keep a generic fallback for truly unexpected nlink errors
    #[error("netlink error: {0}")]
    Netlink(#[from] nlink::Error),

    // ... rest unchanged
}
```

**Where to use:**
- `running.rs:117-132` — container exec failures → `ExecFailed`
- `deploy.rs` Step 3 — namespace creation → `Namespace`
- `deploy.rs` Step 5 — veth creation → `Link`
- `deploy.rs` Step 12 — route addition → `Route`
- `deploy.rs` Step 1 — check existing lab → `AlreadyRunning`

## 3. Builder Validation

**Where:** `builder.rs`

Add a `validate()` method that catches errors at build time instead of deploy time:

```rust
impl Lab {
    /// Build and validate the topology.
    ///
    /// Returns errors for: empty lab name, invalid interface names (>15 chars),
    /// duplicate endpoints, missing link node references.
    pub fn build_validated(self) -> Result<Topology> {
        let topo = self.build();
        topo.validate().bail()?;
        Ok(topo)
    }
}
```

Add validation in individual builders:

```rust
impl NodeBuilder {
    pub fn interface(mut self, name: &str, f: impl FnOnce(InterfaceBuilder) -> InterfaceBuilder) -> Self {
        assert!(name.len() <= 15, "interface name '{name}' exceeds 15-char Linux limit");
        // ...
    }
}
```

Also validate:
- Lab name is non-empty and contains valid namespace characters
- Profile names are unique (warn on overwrite)
- Endpoint format is valid ("node:iface")
- Link endpoints reference existing nodes

## 4. EndpointRef::parse Returns Result

**Where:** `types.rs` — `EndpointRef`

```rust
// Before:
impl EndpointRef {
    pub fn parse(s: &str) -> Option<EndpointRef> { ... }
}

// After:
impl EndpointRef {
    pub fn parse(s: &str) -> Result<EndpointRef> {
        let (node, iface) = s.split_once(':')
            .ok_or_else(|| Error::Parse(
                format!("invalid endpoint '{s}': expected 'node:interface' format")
            ))?;
        Ok(EndpointRef {
            node: node.to_string(),
            interface: iface.to_string(),
        })
    }
}
```

Update all callers (validator, deploy, builder) to use `?` instead of `.unwrap()`/`.expect()`.

## 5. Interface Name Validation Helper

**Where:** `helpers.rs`

Linux interface names are limited to 15 characters (`IFNAMSIZ - 1`). Add a helper:

```rust
/// Validate a Linux interface name.
///
/// Rules: 1-15 characters, no '/' or whitespace, not "." or "..".
pub fn validate_interface_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Validation("interface name is empty".into()));
    }
    if name.len() > 15 {
        return Err(Error::Validation(format!(
            "interface name '{name}' is {} chars (max 15)", name.len()
        )));
    }
    if name.contains('/') || name.contains(char::is_whitespace) {
        return Err(Error::Validation(format!(
            "interface name '{name}' contains invalid characters"
        )));
    }
    Ok(())
}
```

Use in:
- `validator.rs` — new rule `interface-name-valid`
- `deploy.rs:237-241` — veth peer name truncation (error instead of silent truncation)
- `builder.rs` — `interface()` method

## 6. Replace Hand-Rolled ISO 8601 with `time` Crate

**Where:** `deploy.rs` — `now_iso8601()` (around line 1407-1438).

```rust
// Before: 30+ lines of manual date math
fn now_iso8601() -> String { /* complex algorithm */ }

// After:
fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}
```

**Dependency:** Add `time = { version = "0.3", features = ["formatting"] }`.

## 7. Network Kind Default

**Where:** `types.rs` — `Network`

`Network.kind` is `Option<String>` but the only valid value is `"bridge"`. Default it:

```rust
pub struct Network {
    #[serde(default = "default_bridge")]
    pub kind: String,  // no longer Option
    // ...
}

fn default_bridge() -> String { "bridge".to_string() }
```

Or use an enum if more kinds are planned.

## Progress

### Type Safety
- [ ] Replace interface kind `String` with enum
- [ ] `EndpointRef::parse()` returns `Result` instead of `Option`
- [ ] Add interface name validation helper
- [ ] Default `Network.kind` to `"bridge"`

### Error Handling
- [ ] Stratify error types (Namespace, Link, Route, ExecFailed)
- [ ] Add `AlreadyRunning` and `Timeout` error variants
- [ ] Update deploy.rs to use specific error variants

### Builder
- [ ] Add `build_validated()` method
- [ ] Validate interface name length in builder
- [ ] Validate lab name is non-empty

### Cleanup
- [ ] Replace `now_iso8601()` with `time` crate
