# Plan 146: `deploy --suffix` for Parallel Test Safety

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (1 hour)
**Priority:** P2 — prevents test collision with cargo-nextest

---

## Problem Statement

When multiple tests deploy the same NLL file concurrently (e.g., via `cargo nextest`),
they get the same lab name. `--force` causes one test to destroy another's lab. The
`#[lab_test]` macro appends `{fn-name}-{pid}` but there's no CLI equivalent.

## Proposed CLI

```bash
# Append a suffix to the lab name
nlink-lab deploy topology.nll --suffix mytest-$$

# Auto-generate a unique suffix (PID-based)
nlink-lab deploy topology.nll --unique
```

With `--unique`, the lab name becomes `{name}-{pid}` (e.g., `my-lab-12345`).

## Design Decisions

### `--suffix` vs `--unique`

Both are useful. `--suffix` gives full control (for naming in multi-test scenarios).
`--unique` is a shorthand that uses `std::process::id()`.

### Applies to lab name only

The suffix is appended to the lab name in the parsed topology before deployment.
The NLL file is not modified. All subsequent commands (`exec`, `destroy`, etc.)
use the suffixed name.

### Output the actual name

When `--suffix` or `--unique` is used, print the actual lab name so the caller
knows what to pass to `exec`/`destroy`:

```
Deployed lab "my-lab-12345" (3 nodes) in 7ms
```

With `--json`: include `"name": "my-lab-12345"` in the output.

## Implementation

### Step 1: CLI flags (`bins/lab/src/main.rs`)

Add to `Deploy`:

```rust
/// Append suffix to lab name (for parallel test safety).
#[arg(long)]
suffix: Option<String>,

/// Auto-generate unique lab name suffix (appends PID).
#[arg(long)]
unique: bool,
```

### Step 2: Apply suffix in deploy handler

After parsing the topology, before validation:

```rust
if unique {
    topo.lab.name = format!("{}-{}", topo.lab.name, std::process::id());
} else if let Some(ref sfx) = suffix {
    topo.lab.name = format!("{}-{sfx}", topo.lab.name);
}
```

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +15 | CLI flags + name suffix logic |
| **Total** | ~15 | |
