# Plan 134: `exec --json` Structured Output

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (2–3 hours)
**Priority:** P1 — enables clean assertion scripting in test harnesses

---

## Problem Statement

`nlink-lab exec` prints raw stdout/stderr and propagates the exit code. When tests use
`exec` to run assertions, they need exit code, stdout, and stderr in one parseable
structure. Currently these must be captured separately via shell redirections.

## Proposed CLI

```bash
nlink-lab exec --json my-lab host -- ping -c1 10.0.0.1
```

```json
{
  "exit_code": 0,
  "stdout": "PING 10.0.0.1 ...\n64 bytes from 10.0.0.1: ...\n",
  "stderr": "",
  "duration_ms": 1023
}
```

## Design Decisions

### Exit code behaviour with `--json`

Without `--json`, the CLI propagates the child's exit code as its own exit code. With
`--json`, the CLI always exits 0 (the exit code is in the JSON payload). This allows
the caller to parse the JSON without worrying about non-zero exit interfering with
`set -e` or similar.

Exception: if the *lab* or *node* doesn't exist, exit non-zero with a JSON error object.

### Duration measurement

Measure wall-clock time of the exec call using `Instant::now()` around the exec. This
is useful for latency assertions in tests.

## Implementation

### Step 1: CLI handler (`bins/lab/src/main.rs`)

The existing handler (lines ~696-718) currently does:

```rust
Commands::Exec { lab, node, cmd } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let output = running.exec(&node, &cmd[0], &cmd[1..])?;
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    if output.exit_code != 0 {
        return Ok(ExitCode::from(output.exit_code as u8));
    }
}
```

Change to:

```rust
Commands::Exec { lab, node, cmd } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let start = Instant::now();
    let output = running.exec(&node, &cmd[0], &cmd[1..])?;
    let duration_ms = start.elapsed().as_millis() as u64;

    if cli.json {
        println!("{}", serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "duration_ms": duration_ms,
        }));
    } else {
        print!("{}", output.stdout);
        eprint!("{}", output.stderr);
        if output.exit_code != 0 {
            return Ok(ExitCode::from(output.exit_code as u8));
        }
    }
}
```

That's it. The global `--json` flag is already defined; `exec` just doesn't use it yet.

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_exec_json_success` | integration.rs | JSON output with exit_code 0 |
| `test_exec_json_failure` | integration.rs | JSON output with non-zero exit_code, CLI still exits 0 |
| `test_exec_json_has_duration` | integration.rs | `duration_ms` field is present and > 0 |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +12 (net) | JSON branch in exec handler |
| Tests | +25 | 3 test functions |
| **Total** | ~37 | |
