# Plan 080: Bug Fixes & Safety

**Priority:** Critical
**Effort:** 1-2 days
**Target:** `deploy.rs`, `state.rs`, `running.rs`, `error.rs`, `parser/nll/`

## Summary

Fix known bugs, eliminate panic risks, and harden safety-critical paths identified
in the deep analysis report. These are correctness issues that should be addressed
before any new feature work.

## Panic Risks

### 1. Replace `/dev/urandom` unwrap with `getrandom`

**Where:** `deploy.rs` — `generate_wg_key()` function (around line 1320).

Currently opens `/dev/urandom` with `.unwrap()`. Will panic if `/dev/urandom` is
unavailable (containers, restricted environments).

```rust
// Before (panic risk):
let mut f = std::fs::File::open("/dev/urandom").unwrap();
f.read_exact(&mut key).unwrap();

// After (safe):
getrandom::fill(&mut key)
    .map_err(|e| Error::Deploy(format!("failed to generate WireGuard key: {e}")))?;
```

**Dependency:** Add `getrandom = "0.3"` to `Cargo.toml`.

### 2. Validate raw FD before use

**Where:** `deploy.rs` — veth creation (around line 250).

Direct `.as_raw_fd()` usage without checking the FD is valid. Add a guard:

```rust
let fd = ns_fd.as_raw_fd();
if fd < 0 {
    return Err(Error::Deploy(format!("invalid namespace FD for {ns_name}")));
}
```

## NLL Parser Bugs

### 3. Reject bare integer tokens as node names

**Where:** `parser/nll/parser.rs` — `parse_name()` (around line 154).

Currently `parse_name()` accepts `Int` tokens, so `node 123 { }` parses successfully
and creates a node named "123". Node names should start with an alphabetic character.

```rust
// In parse_name(): remove Int from accepted first tokens
// Only accept Ident and Interp as the first token of a name
```

Add test:
```rust
#[test]
fn reject_bare_int_as_node_name() {
    let result = parse("lab \"t\"\nnode 123 { }");
    assert!(result.is_err());
}
```

### 4. Fix rate limiting — apply to both endpoints

**Where:** `parser/nll/lower.rs` — link rate lowering (around line 642-653).

Currently only applies rate to left endpoint. Should apply to both:

```rust
// Before: only left endpoint
if let Some(rate) = &link.rate {
    let ep = format!("{}:{}", link.left_node, link.left_iface);
    topo.rate_limits.insert(ep, /* ... */);
}

// After: both endpoints
if let Some(rate) = &link.rate {
    let left_ep = format!("{}:{}", link.left_node, link.left_iface);
    let right_ep = format!("{}:{}", link.right_node, link.right_iface);
    topo.rate_limits.insert(left_ep, lower_rate_props(rate));
    topo.rate_limits.insert(right_ep, lower_rate_props(rate));
}
```

Add test with both endpoints verified.

### 5. Remove no-op `replace()` call

**Where:** `parser/nll/mod.rs` — line 31.

```rust
// Remove this line — it does nothing:
let clean_msg = msg.replace(|_| false, "");
```

### 6. Warn on extra address pairs in link block

**Where:** `parser/nll/parser.rs` — link address parsing (around line 878-883).

Extra address pairs after the first `left -- right` pair are silently dropped.
Either error or collect all pairs.

### 7. Division by zero in interpolation

**Where:** `parser/nll/lower.rs` — `eval_expr()` (around line 201-204).

Currently returns the raw `${expr}` string on division by zero, which causes confusing
errors downstream. Should error immediately:

```rust
if op == " / " && right_val == 0 {
    return Err(Error::NllParse(format!(
        "division by zero in expression: {expr}"
    )));
}
```

Note: `eval_expr` currently returns `String`, not `Result`. Signature change needed,
with callers updated.

## State Persistence Safety

### 8. Atomic state file writes

**Where:** `state.rs` — `save()` method (around line 85-99).

Use temp-file + rename pattern to prevent corruption on crash:

```rust
pub fn save(state: &LabState) -> Result<()> {
    let dir = state_dir(&state.name)?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("state.toml");
    let tmp = dir.join(".state.toml.tmp");
    let content = toml::to_string_pretty(state)?;
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
```

## Cleanup Robustness

### 9. Log warnings during destroy

**Where:** `running.rs` — `destroy()` method (around line 264-304).

Replace silent error swallowing with `tracing::warn!`:

```rust
// Before:
let _ = namespace::delete(ns);

// After:
if let Err(e) = namespace::delete(ns) {
    tracing::warn!("failed to delete namespace {ns}: {e}");
}
```

### 10. PID ownership validation in kill_process

**Where:** `running.rs` — `kill_process()` (around line 254-261).

Before killing, verify the PID's `/proc/{pid}/cmdline` or cgroup matches the lab.
At minimum, check the process exists and belongs to a lab namespace:

```rust
fn validate_pid_ownership(&self, pid: u32) -> bool {
    // Read /proc/{pid}/cgroup or /proc/{pid}/ns/net and compare
    // against known namespace inodes
    std::fs::read_link(format!("/proc/{pid}/ns/net")).is_ok()
}
```

## Compiler Warnings Cleanup

### 11. Fix all existing warnings

**Where:** Multiple files.

- `deploy.rs:16` — remove unused `self` import
- `running.rs:8` — remove unused `Severity` import
- `deploy.rs:28` — remove or use `name` field in `Container` variant
- `deploy.rs:77` — remove unused `ns_name()` method
- `error.rs:73-88` — fix `NllDiagnostic` field assignment warnings

## Progress

### Panic Risks
- [x] Replace `/dev/urandom` unwrap with `getrandom` crate
- [ ] Validate raw FD before use in veth creation

### NLL Parser Bugs
- [x] Reject bare integer tokens as node names
- [x] Fix rate limiting to apply to both endpoints (done in plan 088)
- [x] Remove no-op `replace()` call in NLL diagnostics
- [ ] Warn or error on extra address pairs in link block
- [x] Error on division by zero in interpolation (tracing::error)

### State & Cleanup
- [x] Atomic state file writes (temp + rename)
- [x] Log warnings during destroy (already implemented)
- [ ] PID ownership validation in kill_process

### Warnings
- [x] Fix all compiler warnings (deploy.rs, running.rs, CLI)
- Note: NllDiagnostic derive macro warnings are false positives (unfixable)
