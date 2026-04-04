# Plan 145: Include Log Paths in `ps --json`

**Date:** 2026-04-04
**Status:** Done
**Effort:** Trivial (30 minutes)
**Priority:** P3 — debugging convenience

---

## Problem Statement

`nlink-lab ps --json` returns `{node, pid, alive}` but does not include log file
paths. When debugging a test failure, knowing where a process's logs are is essential.

## Proposed Behavior

```json
[{
  "node": "infra",
  "pid": 12345,
  "alive": true,
  "stdout_log": "/home/user/.local/state/nlink-lab/labs/mylab/logs/infra-mediator-12345.stdout",
  "stderr_log": "/home/user/.local/state/nlink-lab/labs/mylab/logs/infra-mediator-12345.stderr"
}]
```

When no logs exist for a PID (e.g., container background exec), the fields are `null`.

## Implementation

### Step 1: Add log paths to ProcessInfo (`running.rs`)

```rust
pub struct ProcessInfo {
    pub node: String,
    pub pid: u32,
    pub alive: bool,
    pub stdout_log: Option<String>,
    pub stderr_log: Option<String>,
}
```

Update `process_status()` to populate from `self.process_logs`:

```rust
pub fn process_status(&self) -> Vec<ProcessInfo> {
    self.pids.iter().map(|(node, pid)| {
        let alive = unsafe { libc::kill(*pid as i32, 0) } == 0;
        let logs = self.process_logs.get(pid);
        ProcessInfo {
            node: node.clone(),
            pid: *pid,
            alive,
            stdout_log: logs.map(|(s, _)| s.clone()),
            stderr_log: logs.map(|(_, s)| s.clone()),
        }
    }).collect()
}
```

### Step 2: Update text output in CLI (`main.rs`)

The text table already shows `NODE | PID | STATUS`. No change needed — the new
fields only appear in JSON output via `serde::Serialize`.

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `running.rs` | +8 | Add fields to ProcessInfo + populate |
| **Total** | ~8 | |
