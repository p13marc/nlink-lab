# Plan 132: `spawn` CLI Command

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (half day)
**Priority:** P0 — blocks post-deploy service orchestration

---

## Problem Statement

`nlink-lab exec` runs synchronously and blocks until the command exits. The Rust API
has `RunningLab::spawn()` for background processes, but there's no CLI equivalent.
Integration tests need to start services after deployment in controlled order:
deploy topology → start mediator → wait for port → start bridge → run tests.

NLL `run ... background` works at deploy time only — it can't express post-deploy
sequencing.

## Proposed CLI

```bash
nlink-lab spawn <lab> <node> -- <cmd> [args...]
# Output: PID: 12345

nlink-lab spawn --json <lab> <node> -- <cmd> [args...]
# Output: {"pid": 12345, "node": "infra", "command": "mediator --port 8080"}
```

The spawned process is tracked by `nlink-lab ps` and killable via `nlink-lab kill`.

## Design Decisions

### State persistence

`RunningLab::spawn()` already stores `(node_name, pid)` in `self.pids`, but this is
in-memory only. After spawn, we must re-save the state file so that subsequent
`nlink-lab ps` / `nlink-lab kill` invocations (separate processes) can see the new PID.

### Stdout/stderr of spawned process

For this plan (minimal), stdout/stderr of the spawned process are inherited (printed
to the terminal). This matches the behaviour of `run ... background` in deploy. Plan 140
(process output capture) addresses log capture separately.

Actually, since this is a daemon-style spawn, stdout/stderr should be redirected to
`/dev/null` by default (the process runs in the background after the CLI exits).
Add a `--foreground` or `--attach` flag later if needed.

### Process detachment

The spawned process must survive after the `nlink-lab spawn` CLI process exits. Use
`std::process::Command` with `.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())`
and `pre_exec` to call `setsid()` to detach from the terminal session.

## Implementation

### Step 1: CLI definition (`bins/lab/src/main.rs`)

Add a new `Commands` variant:

```rust
/// Spawn a background process in a lab node.
Spawn {
    /// Lab name.
    lab: String,

    /// Node name.
    node: String,

    /// Command and arguments.
    #[arg(trailing_var_arg = true, required = true)]
    cmd: Vec<String>,
},
```

### Step 2: CLI handler (`bins/lab/src/main.rs`)

```rust
Commands::Spawn { lab, node, cmd } => {
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    let pid = running.spawn(&node, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    // Re-save state with new PID
    running.save_state()?;
    if cli.json {
        println!("{}", serde_json::json!({
            "pid": pid,
            "node": node,
            "command": cmd.join(" "),
        }));
    } else {
        println!("PID: {pid}");
    }
}
```

### Step 3: Add `save_state()` to `RunningLab` (`running.rs`)

Currently, state is only saved at the end of `deploy()`. Add a method to re-persist:

```rust
/// Re-save the current state (e.g., after spawning a new process).
pub fn save_state(&self) -> Result<()> {
    let lab_state = LabState {
        name: self.topology.lab.name.clone(),
        created_at: /* preserve original */,
        namespaces: self.namespace_names.clone(),
        pids: self.pids.clone(),
        wg_public_keys: /* preserve */,
        containers: self.containers.clone(),
        runtime: self.runtime_binary.clone(),
        dns_injected: self.dns_injected,
        wifi_loaded: self.wifi_loaded,
    };
    state::save(&self.topology.lab.name, &lab_state, &self.topology)
}
```

This requires either:
- Storing the original `created_at` and `wg_public_keys` in `RunningLab`, or
- Having `save_state()` do a read-modify-write (load existing state, update pids, re-save)

The read-modify-write approach is simpler and less invasive:

```rust
pub fn save_state(&self) -> Result<()> {
    let (mut lab_state, _) = state::load(self.name())?;
    lab_state.pids = self.pids.clone();
    state::save(self.name(), &lab_state, &self.topology)
}
```

### Step 4: Fix `RunningLab::spawn()` process detachment (`running.rs`)

Currently `spawn()` uses `namespace::spawn_with_etc()` which may not detach the process.
Ensure the child is detached with `setsid()` and has null stdin/stdout/stderr so it
survives CLI exit:

```rust
pub fn spawn(&mut self, node: &str, cmd: &[&str]) -> Result<u32> {
    let ns = self.namespace_for(node)?;
    let pid = namespace::spawn_detached_with_etc(ns, cmd)?;
    self.pids.push((node.to_string(), pid));
    Ok(pid)
}
```

If `namespace::spawn_detached_with_etc` doesn't exist, add it — wrapping the existing
spawn with `.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())` and
a `pre_exec(|| { libc::setsid(); Ok(()) })`.

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_spawn_cli_outputs_pid` | integration.rs | Spawn returns valid PID |
| `test_spawn_tracked_by_ps` | integration.rs | PID appears in `nlink-lab ps` |
| `test_spawn_killable` | integration.rs | `nlink-lab kill` terminates it |
| `test_spawn_json_output` | integration.rs | `--json` returns structured output |
| `test_spawn_survives_cli_exit` | integration.rs | Process still alive after CLI returns |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +25 | CLI variant + handler |
| `running.rs` | +20 | `save_state()` + detached spawn |
| Tests | +40 | 5 test functions |
| **Total** | ~85 | |
