# Plan 148: Documentation Gaps from User Report

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (1 hour)
**Priority:** P2 — several features work but aren't well documented

---

## Problem Statement

The user report identifies several documentation gaps where features exist but
users can't discover or understand them.

## Items to Document

### 1. `mgmt ... host-reachable` syntax

The user tried bare `mgmt 172.20.0.0/24` and expected root-namespace reachability.
Need to document clearly that `host-reachable` modifier is required:

```nll
lab "my-lab" {
    mgmt 172.20.0.0/24 host-reachable  /* bridge in root namespace */
}
```

Without `host-reachable`, the bridge lives in an isolated management namespace.

**Files:** README.md, USER_GUIDE.md, NLL_DSL_DESIGN.md

### 2. Partition/heal semantics

The user confirmed that heal restores original impairments but wants it documented.
Document the save/restore model:

- `--partition` saves current impairments to state, applies 100% loss
- `--heal` restores saved impairments (or clears if none existed)
- Double partition is a no-op (doesn't overwrite saved state)

**Files:** USER_GUIDE.md

### 3. Process log capture defaults and retrieval

Document:
- Default log location: `~/.local/state/nlink-lab/labs/{lab}/logs/`
- File naming: `{node}-{cmd}-{pid}.stdout` / `.stderr`
- Retrieval: `nlink-lab logs <lab> --pid <pid> [--stderr] [--tail N]`
- Deploy-time captures: `run background` processes also get logs

**Files:** USER_GUIDE.md, README.md

### 4. `wait-for --tcp` port-only behavior

Clarify that port-only shorthand resolves to `127.0.0.1:{port}`, which may not
match a service bound to a LAN IP.

**Files:** USER_GUIDE.md

### 5. SUID vs capabilities

Document that SUID root (`just install`) is recommended. Capabilities alone
(`just install-caps`) may not work on all kernel configurations due to
`mount()` syscall restrictions for namespace bind-mounts.

**Files:** USER_GUIDE.md, README.md (already partially done)

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `README.md` | +10 | Clarify mgmt host-reachable, log defaults |
| `USER_GUIDE.md` | +30 | Partition/heal semantics, log paths, wait-for docs, SUID |
| `NLL_DSL_DESIGN.md` | +5 | mgmt host-reachable syntax |
| **Total** | ~45 | |
