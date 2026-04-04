# Plan 133: `wait-for` Port/Service Readiness

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (half day)
**Priority:** P1 — eliminates flaky sleep-based polling in test scripts

---

## Problem Statement

`nlink-lab wait` only checks if the lab state file exists (lab deployed). There's no
way to wait for a service inside a node to become ready. Every integration test ends up
writing its own bash retry loop:

```bash
for i in $(seq 1 20); do
    nlink-lab exec my-lab node -- bash -c 'echo > /dev/tcp/127.0.0.1/8080' && break
    sleep 0.5
done
```

This is fragile, verbose, and non-portable.

## Proposed CLI

```bash
# Wait for TCP port to accept connections inside a node
nlink-lab wait-for <lab> <node> --tcp <ip:port> --timeout <seconds>

# Wait for a command to succeed (exit 0)
nlink-lab wait-for <lab> <node> --exec "curl -sf http://localhost:8080/health" --timeout 10

# Wait for a file to exist
nlink-lab wait-for <lab> <node> --file /var/run/service.pid --timeout 5
```

Default timeout: 30s. Default poll interval: 500ms. Exit 0 on success, exit 1 on timeout.

## Design Decisions

### Separate command vs extending `wait`

The existing `wait` command waits for the lab to exist. `wait-for` waits for a condition
inside a running node — semantically different. A separate subcommand avoids overloading
`wait` with incompatible flags.

### Implementation location

Readiness checks run in the node's namespace. The simplest approach: use
`RunningLab::exec()` to run a probe command inside the node.

- **TCP check:** `exec(node, "bash", &["-c", &format!("echo > /dev/tcp/{ip}/{port}")])`
  — this uses bash built-in TCP, no extra dependencies.
- **Exec check:** `exec(node, "sh", &["-c", &user_command])` — arbitrary command.
- **File check:** `exec(node, "test", &["-e", &path])` — filesystem check.

### Library-level API

Also expose as a library method for programmatic use:

```rust
impl RunningLab {
    pub async fn wait_for_tcp(&self, node: &str, addr: &str, port: u16, timeout: Duration) -> Result<()>;
    pub async fn wait_for_exec(&self, node: &str, cmd: &str, timeout: Duration) -> Result<()>;
    pub async fn wait_for_file(&self, node: &str, path: &str, timeout: Duration) -> Result<()>;
}
```

## Implementation

### Step 1: CLI definition (`bins/lab/src/main.rs`)

```rust
/// Wait for a service or condition inside a lab node.
WaitFor {
    /// Lab name.
    lab: String,

    /// Node name.
    node: String,

    /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
    #[arg(long)]
    tcp: Option<String>,

    /// Wait for command to succeed (exit 0).
    #[arg(long)]
    exec: Option<String>,

    /// Wait for file to exist.
    #[arg(long)]
    file: Option<String>,

    /// Timeout in seconds (default: 30).
    #[arg(short, long, default_value = "30")]
    timeout: u64,

    /// Poll interval in milliseconds (default: 500).
    #[arg(long, default_value = "500")]
    interval: u64,
},
```

### Step 2: Library methods (`running.rs`)

```rust
/// Wait for a TCP port to accept connections inside a node's namespace.
pub async fn wait_for_tcp(
    &self,
    node: &str,
    ip: &str,
    port: u16,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let probe = self.exec(node, "bash", &["-c", &format!("echo > /dev/tcp/{ip}/{port}")]);
        if probe.is_ok_and(|o| o.exit_code == 0) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::deploy_failed(format!(
                "timeout waiting for {ip}:{port} on node '{node}'"
            )));
        }
        tokio::time::sleep(interval).await;
    }
}

/// Wait for a command to succeed (exit 0) inside a node's namespace.
pub async fn wait_for_exec(
    &self,
    node: &str,
    cmd: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let probe = self.exec(node, "sh", &["-c", cmd]);
        if probe.is_ok_and(|o| o.exit_code == 0) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::deploy_failed(format!(
                "timeout waiting for command to succeed on node '{node}': {cmd}"
            )));
        }
        tokio::time::sleep(interval).await;
    }
}

/// Wait for a file to exist inside a node's namespace.
pub async fn wait_for_file(
    &self,
    node: &str,
    path: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let probe = self.exec(node, "test", &["-e", path]);
        if probe.is_ok_and(|o| o.exit_code == 0) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::deploy_failed(format!(
                "timeout waiting for file '{path}' on node '{node}'"
            )));
        }
        tokio::time::sleep(interval).await;
    }
}
```

### Step 3: CLI handler (`bins/lab/src/main.rs`)

```rust
Commands::WaitFor { lab, node, tcp, exec, file, timeout, interval } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let timeout = Duration::from_secs(timeout);
    let interval = Duration::from_millis(interval);

    let result = if let Some(tcp_addr) = tcp {
        let (ip, port) = parse_tcp_addr(&tcp_addr)?;
        running.wait_for_tcp(&node, &ip, port, timeout, interval).await
    } else if let Some(cmd) = exec {
        running.wait_for_exec(&node, &cmd, timeout, interval).await
    } else if let Some(path) = file {
        running.wait_for_file(&node, &path, timeout, interval).await
    } else {
        return Err("one of --tcp, --exec, or --file is required".into());
    };

    match result {
        Ok(()) => {
            if !cli.quiet {
                eprintln!("ready");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::FAILURE);
        }
    }
}
```

Helper to parse TCP address (accept both `ip:port` and bare `port`):

```rust
fn parse_tcp_addr(addr: &str) -> Result<(String, u16)> {
    if let Some((ip, port)) = addr.rsplit_once(':') {
        Ok((ip.to_string(), port.parse()?))
    } else {
        Ok(("127.0.0.1".to_string(), addr.parse()?))
    }
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_wait_for_tcp_immediate` | integration.rs | Port already open → returns instantly |
| `test_wait_for_tcp_timeout` | integration.rs | Nothing listening → times out with exit 1 |
| `test_wait_for_exec_success` | integration.rs | Command eventually succeeds |
| `test_wait_for_file_exists` | integration.rs | File created after delay → detected |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +45 | CLI variant + handler + helper |
| `running.rs` | +65 | Three wait_for_* methods |
| Tests | +35 | 4 test functions |
| **Total** | ~145 | |
