# Plan 143: `exec --json` Errors as JSON

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (1–2 hours)
**Priority:** P1 — breaks test harness JSON parsing on errors

---

## Problem Statement

When `nlink-lab exec --json` fails (node not found, lab not found, command not found),
the error is printed as plain text, not JSON. This breaks JSON parsers in test harnesses:

```bash
$ nlink-lab exec --json my-lab nonexistent -- echo hello
error: node not found: nonexistent
# ^ NOT valid JSON — test harness JSON parse fails
```

## Proposed Behavior

All errors with `--json` should return valid JSON:

```json
{"error": "node not found: nonexistent", "exit_code": null, "stdout": "", "stderr": ""}
```

## Design Decisions

### Scope

Only the `exec` command needs this fix since it's the primary assertion mechanism in
test harnesses. Other commands with `--json` (`status`, `ps`, `diagnose`) don't have
the same issue because they don't feed into programmatic assertion logic.

### Exit code

When `--json` is passed, the CLI should always exit 0 for JSON-parseable output
(the error is in the JSON payload). Only exit non-zero for truly fatal errors
(can't write to stdout, etc.).

## Implementation

### Step 1: Wrap exec handler errors (`bins/lab/src/main.rs`)

Currently the exec handler has two error paths:
1. Node not found → `eprintln!` + `std::process::exit(1)` (line ~845)
2. Lab not found / exec failed → error propagated to top-level handler (line ~569)

Both need to be caught when `--json` is active.

```rust
Commands::Exec { lab, node, cmd } => {
    check_root();

    if cli.json {
        // In JSON mode, wrap ALL errors as JSON output
        match exec_json(&lab, &node, &cmd) {
            Ok(()) => {}
            Err(e) => {
                println!("{}", serde_json::json!({
                    "error": e.to_string(),
                    "exit_code": null::<i32>,
                    "stdout": "",
                    "stderr": "",
                    "duration_ms": 0,
                }));
            }
        }
        return Ok(());
    }

    // Non-JSON path (existing behavior)
    // ...
}
```

Or simpler: just wrap the existing logic in a closure and catch errors:

```rust
if cli.json {
    let result = (|| -> nlink_lab::Result<()> {
        let running = nlink_lab::RunningLab::load(&lab)?;
        let node_names: Vec<&str> = running.node_names().collect();
        if !node_names.contains(&node.as_str()) {
            return Err(nlink_lab::Error::NodeNotFound { name: node.clone() });
        }
        let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
        let start = Instant::now();
        let output = running.exec(&node, &cmd[0], &args)?;
        let duration_ms = start.elapsed().as_millis() as u64;
        println!("{}", serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "duration_ms": duration_ms,
        }));
        Ok(())
    })();
    if let Err(e) = result {
        println!("{}", serde_json::json!({
            "error": e.to_string(),
            "exit_code": null::<i32>,
            "stdout": "",
            "stderr": "",
            "duration_ms": 0,
        }));
    }
    return Ok(());
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_exec_json_node_not_found` | integration.rs | Returns JSON error, not plain text |
| `test_exec_json_lab_not_found` | integration.rs | Returns JSON error |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +25 | JSON error wrapping in exec handler |
| **Total** | ~25 | |
