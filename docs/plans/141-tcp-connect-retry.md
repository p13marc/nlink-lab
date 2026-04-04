# Plan 141: `tcp-connect` Validate Assertion with Retry

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (2–3 hours)
**Priority:** P3 — reduces flaky validate failures on slow service startup

---

## Problem Statement

The `tcp-connect` assertion in validate blocks attempts a single connection. If the
service hasn't finished binding its port at validate time, the assertion fails. This is
common when NLL files include `run ... background` for services — the validate block
runs immediately after deployment, racing with service startup.

## Proposed Syntax

```nll
validate {
    tcp-connect from client to server port 15987 timeout 10s retries 20 interval 500ms
}
```

New optional parameters:
- `retries N` — number of retry attempts (default: 1, i.e., no retry)
- `interval <duration>` — wait between retries (default: 500ms)

The existing `timeout` applies to each individual connection attempt. The total time
budget is approximately `retries * (timeout + interval)`.

## Design Decisions

### Backward compatibility

Without `retries`, behaviour is identical to current: single attempt with timeout.
This is fully backward-compatible.

### Why not just increase timeout?

A long timeout on a single attempt waits for the TCP handshake to time out, which is
controlled by the kernel (typically 20+ seconds for SYN retries). Retries with a short
timeout + interval are faster to detect "not yet listening" vs "unreachable".

### Implementation location

The `tcp-connect` execution is in `deploy.rs` `run_assertions()` (lines ~2512-2544).
The retry loop wraps the existing probe command.

## Implementation

### Step 1: AST (`ast.rs`)

Add retry fields to `TcpConnect`:

```rust
TcpConnect {
    from: String,
    to: String,
    port: u16,
    timeout: Option<String>,
    retries: Option<u32>,
    interval: Option<String>,
}
```

### Step 2: Types (`types.rs`)

Add to `Assertion::TcpConnect`:

```rust
TcpConnect {
    from: String,
    to: String,
    port: u16,
    timeout: Option<String>,
    retries: Option<u32>,
    interval: Option<String>,
}
```

### Step 3: Parser (`parser.rs`)

In `parse_assertion_block()`, after parsing `timeout`, add:

```rust
// Optional retries
let retries = if eat_kw(tokens, pos, "retries") {
    Some(expect_integer(tokens, pos)? as u32)
} else {
    None
};

// Optional interval
let interval = if eat_kw(tokens, pos, "interval") {
    Some(expect_duration(tokens, pos)?)
} else {
    None
};
```

### Step 4: Lowerer (`lower.rs`)

Propagate the new fields in the assertion lowering:

```rust
Assertion::TcpConnect { from, to, port, timeout, retries, interval } => {
    types::Assertion::TcpConnect {
        from: i(&from, vars),
        to: i(&to, vars),
        port,
        timeout,
        retries,
        interval,
    }
}
```

### Step 5: Execution (`deploy.rs`)

In `run_assertions()`, wrap the `tcp-connect` probe in a retry loop:

```rust
Assertion::TcpConnect { from, to, port, timeout, retries, interval } => {
    let max_attempts = retries.unwrap_or(1);
    let retry_interval = interval
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .unwrap_or(Duration::from_millis(500));
    let timeout_secs = timeout
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .unwrap_or(Duration::from_secs(3))
        .as_secs()
        .max(1);

    let target_ip = resolve_node_ip(topology, to, from)?;
    let probe_cmd = format!("echo > /dev/tcp/{target_ip}/{port}");

    let mut last_err = String::new();
    let mut passed = false;

    for attempt in 0..max_attempts {
        let output = namespace::spawn_output_with_etc(
            &ns_name,
            &["timeout", &timeout_secs.to_string(), "bash", "-c", &probe_cmd],
        )?;

        if output.exit_code == 0 {
            passed = true;
            break;
        }

        last_err = output.stderr;

        if attempt + 1 < max_attempts {
            tokio::time::sleep(retry_interval).await;
        }
    }

    if passed {
        results.push(AssertionResult::Pass { name });
    } else {
        results.push(AssertionResult::Fail { name, detail: last_err });
    }
}
```

### Step 6: Render (`render.rs`)

In the assertion rendering, append `retries` and `interval` if present:

```rust
Assertion::TcpConnect { from, to, port, timeout, retries, interval } => {
    write!(out, "  tcp-connect {from} {to} {port}")?;
    if let Some(t) = timeout { write!(out, " timeout {t}")?; }
    if let Some(r) = retries { write!(out, " retries {r}")?; }
    if let Some(i) = interval { write!(out, " interval {i}")?; }
    writeln!(out)?;
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_parse_tcp_connect_retries` | parser.rs | Parse `retries 10 interval 500ms` |
| `test_parse_tcp_connect_no_retries` | parser.rs | Without retries → defaults (backward compat) |
| `test_tcp_connect_retry_succeeds` | integration.rs | Service starts after 2s, retries catch it |
| `test_tcp_connect_retry_exhausted` | integration.rs | Service never starts → fails after retries |
| `test_render_tcp_connect_retries` | render.rs | Renders back with retry params |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `ast.rs` | +4 | Add fields to TcpConnect variant |
| `types.rs` | +4 | Add fields to Assertion::TcpConnect |
| `parser.rs` | +12 | Parse retries/interval keywords |
| `lower.rs` | +4 | Propagate new fields |
| `deploy.rs` | +25 | Retry loop in assertion execution |
| `render.rs` | +4 | Render new fields |
| Tests | +40 | 5 test functions |
| **Total** | ~93 | |
