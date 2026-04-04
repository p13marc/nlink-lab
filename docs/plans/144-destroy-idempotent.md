# Plan 144: Make `destroy` Idempotent

**Date:** 2026-04-04
**Status:** Done
**Effort:** Trivial (30 minutes)
**Priority:** P2 — convenience for test cleanup / Drop impls

---

## Problem Statement

`nlink-lab destroy nonexistent` returns an error. In test cleanup (especially `Drop`
impls), this is inconvenient — the cleanup code must handle both "lab exists" and
"lab already destroyed" cases.

```bash
$ nlink-lab destroy nonexistent
error: lab not found: nonexistent
$ echo $?
1
```

## Proposed Behavior

`nlink-lab destroy <name>` on a non-existent lab should be a silent no-op (exit 0),
like `rm -f`. The `--force` flag already handles this, but the base `destroy` should
too.

## Design Decisions

### Not a breaking change

Currently `destroy` errors on non-existent labs. Changing to silent no-op is strictly
more permissive — no existing correct usage breaks.

### `--force` remains for stuck labs

`--force` still does best-effort cleanup (namespace deletion by prefix) when state is
corrupted or missing. The base `destroy` on a non-existent lab just does nothing.

## Implementation

### Step 1: Update destroy handler (`bins/lab/src/main.rs`)

In the destroy handler, catch `NotFound` errors and treat them as success:

```rust
Commands::Destroy { name: Some(name), force, .. } => {
    match nlink_lab::RunningLab::load(&name) {
        Ok(lab) => {
            lab.destroy().await?;
            if !quiet { eprintln!("Destroyed lab {name:?}"); }
        }
        Err(nlink_lab::Error::NotFound { .. }) => {
            if force {
                force_cleanup(&name).await;
                if !quiet { eprintln!("Lab {name:?} force-cleaned"); }
            }
            // else: silent no-op (lab doesn't exist, nothing to destroy)
        }
        Err(e) => return Err(e),
    }
}
```

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | ~5 (net) | Handle NotFound as no-op |
| **Total** | ~5 | |
