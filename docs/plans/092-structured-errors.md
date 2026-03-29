# Plan 092: Structured Error Context

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `crates/nlink-lab/src/`

## Summary

Improve error quality so every error tells the user **what was being attempted**,
not just what failed. Replace the catch-all `DeployFailed(String)` with specific
variants, fix 3 unsafe `unwrap()` calls in deploy.rs, and add deployment phase
context to all error paths.

## Current State

### What works well

- 117 `.map_err()` calls across the codebase — most critical paths are wrapped
- ~70% of error messages include operation + resource context
  (e.g., `"failed to add address '10.0.0.1'/24 to 'eth0' on 'node1': {e}"`)
- `miette` integration for NLL parse errors — rich diagnostics with source spans
- `running.rs` is exemplary — zero unsafe unwraps, proper error variants

### What needs improvement

| Issue | Severity | Location |
|-------|----------|----------|
| 3 unsafe `EndpointRef::parse().unwrap()` | **Critical** | deploy.rs:497, 1555, 1572 |
| `DeployFailed(String)` is a catch-all | High | error.rs, deploy.rs (45+ uses) |
| No deployment phase in errors | Medium | deploy.rs |
| `Validation(String)` joins errors with `;` | Medium | error.rs, validator.rs |
| `State` error lacks operation type | Low | error.rs, state.rs |

## Phase 1: Fix Unsafe Unwraps (day 1)

Replace the 3 `EndpointRef::parse().unwrap()` calls in deploy.rs with proper
error handling. These can panic mid-deployment if validation was skipped or
an endpoint string is malformed.

### Locations

```
deploy.rs  — apply_diff Phase 5, setting addresses:
    let ep = EndpointRef::parse(ep_str).unwrap();  // line ~1555
    let ep = EndpointRef::parse(ep_str).unwrap();  // line ~1572

deploy.rs  — deploy(), link address assignment:
    let ep = EndpointRef::parse(ep_str).unwrap();  // line ~497
```

### Fix

```rust
// Before (panics on malformed endpoint)
let ep = EndpointRef::parse(ep_str).unwrap();

// After (returns error with context)
let ep = EndpointRef::parse(ep_str).ok_or_else(|| Error::InvalidEndpoint {
    endpoint: ep_str.clone(),
})?;
```

### Tasks

- [ ] Replace all `EndpointRef::parse().unwrap()` in deploy.rs with `?`
- [ ] Grep for any other `.unwrap()` in non-test code and assess risk
- [ ] Add a test that verifies deploy handles malformed endpoints gracefully

## Phase 2: Specific Deploy Error Variants (day 1-2)

Break `DeployFailed(String)` into specific error variants that can be
pattern-matched by callers and provide structured context.

### New error variants

```rust
pub enum Error {
    // ... existing variants ...

    /// Namespace operation failed (create, delete, open, setns).
    #[error("{op} namespace '{ns}': {source}")]
    Namespace {
        op: &'static str,  // "create", "delete", "open"
        ns: String,
        source: nlink::Error,
    },

    /// Netlink connection or link operation failed.
    #[error("{op} on node '{node}': {source}")]
    NetlinkOp {
        op: String,         // "create veth pair", "set address", "set link up"
        node: String,
        source: nlink::Error,
    },

    /// Route configuration failed.
    #[error("add route '{dest}' on node '{node}': {source}")]
    Route {
        dest: String,
        node: String,
        source: nlink::Error,
    },

    /// Firewall (nftables) configuration failed.
    #[error("apply firewall on node '{node}': {detail}")]
    Firewall {
        node: String,
        detail: String,
    },

    /// Container runtime operation failed.
    #[error("{op} container '{name}': {detail}")]
    Container {
        op: &'static str,  // "create", "inspect", "exec", "remove"
        name: String,
        detail: String,
    },

    /// Generic deploy failure (escape hatch for rare cases).
    #[error("deploy failed: {0}")]
    DeployFailed(String),
}
```

### Migration strategy

Replace `Error::deploy_failed(format!(...))` calls incrementally:

1. **Namespace operations** → `Error::Namespace { .. }`
2. **Link/address/interface operations** → `Error::NetlinkOp { .. }`
3. **Route operations** → `Error::Route { .. }`
4. **Firewall operations** → `Error::Firewall { .. }`
5. **Container operations** → `Error::Container { .. }`
6. Keep `DeployFailed` for truly generic cases (should be <5 uses)

### Tasks

- [ ] Add new error variants to `error.rs`
- [ ] Migrate namespace errors in deploy.rs (~8 sites)
- [ ] Migrate netlink/link errors in deploy.rs (~15 sites)
- [ ] Migrate route errors in deploy.rs (~3 sites)
- [ ] Migrate firewall errors in deploy.rs (~2 sites)
- [ ] Migrate container errors in deploy.rs + container.rs (~10 sites)
- [ ] Update `apply_diff()` to use new variants
- [ ] Verify remaining `deploy_failed()` calls are truly generic

## Phase 3: Deployment Phase Context (day 2)

Add phase/step information to deployment errors so users know where in the
18-step sequence a failure occurred.

### Approach

Wrap deploy steps with a context helper that tags errors with the current phase:

```rust
/// Tag errors with the deployment phase for user context.
fn phase_context(phase: &'static str) -> impl FnOnce(Error) -> Error + '_ {
    move |e| {
        // Prepend phase to error message for display
        Error::DeployPhase {
            phase,
            source: Box::new(e),
        }
    }
}
```

Or simpler — use `tracing::info_span!` to add phase context to logs,
and include the phase in error messages:

```rust
// Step 3: Create namespaces
tracing::info!("step 3/18: creating namespaces");
for (node_name, node) in &topology.nodes {
    namespace::create(&ns_name).map_err(|e| Error::Namespace {
        op: "create",
        ns: ns_name.clone(),
        source: e,
    })?;
}
```

### Tasks

- [ ] Add `tracing::info!` markers for each deployment step
- [ ] Ensure every error in deploy.rs includes which step/phase context
- [ ] Add phase context to `apply_diff()` errors

## Phase 4: Validation Error Structure (day 3)

Make validation errors programmatically inspectable instead of joining
all messages into a single `Validation(String)`.

### Current problem

```rust
// bail() joins all errors with "; " — hard to inspect
pub fn bail(&self) -> Result<()> {
    let messages: Vec<String> = self.errors().map(|i| i.to_string()).collect();
    Err(Error::Validation(messages.join("; ")))
}
```

### Fix

```rust
/// Validation failed with one or more issues.
#[error("validation failed ({count} errors)", count = .0.len())]
ValidationErrors(Vec<ValidationIssue>),
```

Then `bail()` returns `Error::ValidationErrors(issues)` with the full list.
The `Display` impl can still format them nicely, but callers can pattern-match
and inspect individual issues.

### Tasks

- [ ] Add `ValidationErrors(Vec<ValidationIssue>)` variant
- [ ] Make `ValidationIssue` implement `Clone` (already does)
- [ ] Update `bail()` to use new variant
- [ ] Update CLI to render validation errors with miette (one per line)
- [ ] Deprecate `Validation(String)` or remove if no external users

## Phase 5: State Error Context (day 3)

Enhance `Error::State` to capture the operation type.

### Current

```rust
#[error("state error: {message} (path: {path})")]
State { message: String, path: PathBuf },
```

### Improved

```rust
#[error("{op} state file: {detail} (path: {path})")]
State {
    op: &'static str,  // "read", "write", "parse", "delete"
    detail: String,
    path: PathBuf,
},
```

### Tasks

- [ ] Update `State` variant with `op` field
- [ ] Update all construction sites in state.rs (~6 sites)
- [ ] Verify error messages are clear

## Progress

### Phase 1: Fix Unsafe Unwraps
- [ ] Replace `EndpointRef::parse().unwrap()` (3 sites)
- [ ] Audit other unwraps
- [ ] Add test

### Phase 2: Specific Deploy Error Variants
- [ ] Add new error variants
- [ ] Migrate namespace errors
- [ ] Migrate netlink errors
- [ ] Migrate route errors
- [ ] Migrate firewall errors
- [ ] Migrate container errors
- [ ] Update `apply_diff()`

### Phase 3: Deployment Phase Context
- [ ] Add tracing markers
- [ ] Phase context in errors
- [ ] Phase context in `apply_diff()`

### Phase 4: Validation Error Structure
- [ ] Add `ValidationErrors` variant
- [ ] Update `bail()`
- [ ] Update CLI rendering

### Phase 5: State Error Context
- [ ] Update `State` variant
- [ ] Update construction sites
