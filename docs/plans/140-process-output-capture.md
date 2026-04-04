# Plan 140: Process Output Capture

**Date:** 2026-04-04
**Status:** Done
**Effort:** Medium (1 day)
**Priority:** P3 — debugging aid for test failures

---

## Problem Statement

Background processes spawned via `run ... background` or `lab.spawn()` have their
stdout/stderr discarded (redirected to `/dev/null`). When an integration test fails,
the first question is "what did the service print?" Without log capture, debugging
namespace test failures requires manual reproduction.

## Proposed CLI

```bash
# Spawn with log capture to a directory
nlink-lab spawn my-lab infra --log-dir /tmp/lab-logs -- mediator --port 15987
# Creates /tmp/lab-logs/infra-mediator-{pid}.stdout
#         /tmp/lab-logs/infra-mediator-{pid}.stderr

# Retrieve logs of a tracked process
nlink-lab logs my-lab --pid 12345
nlink-lab logs my-lab --pid 12345 --stderr

# Follow logs (tail -f equivalent)
nlink-lab logs my-lab --pid 12345 --follow

# Show last N lines
nlink-lab logs my-lab --pid 12345 --tail 50
```

## Design Decisions

### Log storage location

Default: `$XDG_STATE_HOME/nlink-lab/labs/{lab}/logs/` (next to state.json). Can be
overridden with `--log-dir`.

### Log file naming

`{node}-{cmd_basename}-{pid}.stdout` and `.stderr`. The command basename (e.g.,
`mediator` from `/usr/bin/mediator`) avoids path characters in filenames.

### Deploy-time log capture

Background processes spawned during deployment (from NLL `run ... background`) should
also capture logs by default. This is a change from current behaviour (discard). The
log directory is always created for deployed labs.

### State tracking

Store log file paths in `LabState`:

```rust
pub logs: HashMap<u32, LogPaths>,  // pid → log paths
```

```rust
pub struct LogPaths {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}
```

## Implementation

### Step 1: Log directory creation (`deploy.rs`)

After creating the state directory for the lab, also create a `logs/` subdirectory:

```rust
let log_dir = state::lab_dir(&topology.lab.name)?.join("logs");
std::fs::create_dir_all(&log_dir)?;
```

### Step 2: Spawn with file redirection (`running.rs`)

Modify the spawn path to redirect stdout/stderr to files:

```rust
pub fn spawn_with_logs(
    &mut self,
    node: &str,
    cmd: &[&str],
    log_dir: Option<&Path>,
) -> Result<u32> {
    let ns = self.namespace_for(node)?;
    let log_dir = log_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| state::lab_dir(self.name()).unwrap().join("logs"));

    std::fs::create_dir_all(&log_dir)?;

    let cmd_name = Path::new(cmd[0]).file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown");

    // Pre-create log files, spawn will write to them
    let stdout_path = log_dir.join(format!("{node}-{cmd_name}-pending.stdout"));
    let stderr_path = log_dir.join(format!("{node}-{cmd_name}-pending.stderr"));

    let stdout_file = File::create(&stdout_path)?;
    let stderr_file = File::create(&stderr_path)?;

    let pid = namespace::spawn_with_etc_and_io(ns, cmd, stdout_file, stderr_file)?;

    // Rename with actual PID
    let final_stdout = log_dir.join(format!("{node}-{cmd_name}-{pid}.stdout"));
    let final_stderr = log_dir.join(format!("{node}-{cmd_name}-{pid}.stderr"));
    std::fs::rename(&stdout_path, &final_stdout)?;
    std::fs::rename(&stderr_path, &final_stderr)?;

    self.pids.push((node.to_string(), pid));
    self.log_paths.insert(pid, LogPaths {
        stdout: final_stdout,
        stderr: final_stderr,
    });
    Ok(pid)
}
```

### Step 3: `logs` CLI command (`bins/lab/src/main.rs`)

```rust
/// Show logs for a background process.
Logs {
    /// Lab name.
    lab: String,

    /// Process ID.
    #[arg(long)]
    pid: Option<u32>,

    /// Show stderr instead of stdout.
    #[arg(long)]
    stderr: bool,

    /// Follow log output (like tail -f).
    #[arg(long, short)]
    follow: bool,

    /// Show last N lines.
    #[arg(long)]
    tail: Option<usize>,
},
```

Handler reads the log file path from state and prints content. For `--follow`, use
a file watcher or `tail -f` subprocess.

### Step 4: `spawn --log-dir` flag

Add `--log-dir` to the `Spawn` CLI command (from Plan 132):

```rust
/// Directory for stdout/stderr log files (default: lab state dir).
#[arg(long)]
log_dir: Option<PathBuf>,
```

### Step 5: Deploy-time log capture (`deploy.rs`)

In step 16 (spawn background processes), use the new `spawn_with_logs` path instead
of the current fire-and-forget spawn.

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_spawn_creates_log_files` | integration.rs | Log files exist after spawn |
| `test_logs_contain_output` | integration.rs | Process stdout captured in file |
| `test_logs_stderr_separate` | integration.rs | stderr goes to .stderr file |
| `test_logs_cli_reads_output` | integration.rs | `nlink-lab logs` prints content |
| `test_logs_tail_limit` | integration.rs | `--tail 5` shows last 5 lines |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `state.rs` | +15 | `LogPaths` struct, field on `LabState` |
| `running.rs` | +45 | `spawn_with_logs()` + log_paths storage |
| `deploy.rs` | +10 | Log dir creation + use spawn_with_logs |
| `main.rs` | +50 | `Logs` command + `--log-dir` on Spawn |
| Tests | +40 | 5 test functions |
| **Total** | ~160 | |
