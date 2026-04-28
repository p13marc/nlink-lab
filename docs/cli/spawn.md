# `nlink-lab spawn`

Spawn a long-lived background process inside a lab node.

## Usage

```text
nlink-lab spawn [OPTIONS] <LAB> <NODE> -- <CMD>...
```

## Description

Like [`exec`](exec.md), but the process runs in the background and
its lifetime is tracked by the lab. nlink-lab records the PID,
captures stdout and stderr to log files, and lists the process via
[`ps`](ps.md). Killed via [`kill`](kill.md) or wiped on
[`destroy`](destroy.md).

Use `spawn` for services (HTTP servers, daemons, simulators);
use `exec` for one-shot commands (ping, curl, scripted tests).

## Arguments

| Argument | Description |
|----------|-------------|
| `<LAB>` | Lab name. |
| `<NODE>` | Node name. |
| `<CMD>...` | Command and arguments. |

## Options

| Flag | Description |
|------|-------------|
| `--env KEY=VALUE` | Set environment variables. Repeatable. |
| `--workdir DIR` | Working directory before `exec()`. |
| `--log-dir DIR` | Where to capture stdout/stderr. Default: `~/.nlink-lab/<lab>/logs/`. |
| `--wait-tcp HOST:PORT` | After spawn, block until a TCP connect succeeds. Useful for "service ready" gating. The probe runs inside the node's namespace — `127.0.0.1:PORT` only matches loopback-bound services. |
| `--wait-timeout SECS` | Timeout for `--wait-tcp` (default 30s). |
| `--json` | Print `{pid, log_path}` as JSON instead of human text. |

## Examples

### Spawn an HTTP server and wait for readiness

```bash
sudo nlink-lab spawn lab server \
  --wait-tcp 0.0.0.0:8080 \
  -- /usr/bin/my-http-server --port 8080

# By the time spawn returns, port 8080 accepts connections.
sudo nlink-lab exec lab client -- curl -fsS http://server:8080/
```

### Spawn with a config and a working directory

```bash
sudo nlink-lab spawn lab worker \
  --workdir /tmp/work \
  --env LOG_LEVEL=info \
  --env CONFIG=/etc/myapp.toml \
  -- /usr/bin/myapp
```

### Capture logs to a custom dir for CI artifact upload

```bash
sudo nlink-lab spawn lab svc --log-dir /tmp/lab-logs -- /usr/bin/svc
# CI step later: tar /tmp/lab-logs and upload as artifact.
```

### List spawned processes

```bash
nlink-lab ps lab
```

### Tail a spawned process's logs

```bash
nlink-lab logs lab --pid 12345 --follow
```

`logs` doesn't require root.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Process spawned (and `--wait-tcp` succeeded if specified) |
| 1 | Bad arguments |
| 2 | Lab or node not found |
| 4 | `--wait-tcp` timed out |
| 5 | Insufficient capabilities |

## See also

- [`exec`](exec.md) — one-shot commands
- [`ps`](ps.md), [`kill`](kill.md), [`logs`](logs.md)
- [`wait-for`](wait-for.md) — block on a condition without spawning
