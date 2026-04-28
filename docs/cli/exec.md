# `nlink-lab exec`

Run a one-shot command inside a deployed lab node.

## Usage

```text
nlink-lab exec [OPTIONS] <LAB> <NODE> -- <CMD>...
```

## Description

Runs a command in the network namespace (or container) of the given
node. Stdio streams live by default — good for `ping`, `tail -f`,
interactive services. Pass `--json` for buffered output with
structured exit code, stdout, stderr, and duration.

`exec` requires the same caps as `deploy`: root, SUID, or
`CAP_NET_ADMIN`+`CAP_SYS_ADMIN`.

For long-lived background processes that should survive across
shells, use [`spawn`](spawn.md) instead — it tracks PIDs and
captures stdout/stderr to log files.

## Arguments

| Argument | Description |
|----------|-------------|
| `<LAB>` | Lab name (created by `deploy`). |
| `<NODE>` | Node name within the lab. |
| `<CMD>...` | Command and arguments. The `--` separator is recommended to disambiguate flags. |

## Options

| Flag | Description |
|------|-------------|
| `--env KEY=VALUE` | Set environment variables. Repeatable. |
| `--workdir DIR` | Working directory. For namespace nodes this is `chdir()` on the host filesystem; for container nodes it's passed as `-w <path>` to docker/podman. |
| `--json` | Buffer output and emit `{exit_code, stdout, stderr, duration_ms}`. Streams are not visible until the command completes. |
| `-v`, `--verbose` | Print the resolved namespace path or container ID before running. |
| `-q`, `--quiet` | Suppress non-error output (the exec'd command's stdio still shows). |

## Examples

### Run a one-shot command

```bash
sudo nlink-lab exec simple host -- ping -c 3 router
```

### Stream long-running output

```bash
sudo nlink-lab exec simple router -- tail -f /var/log/syslog
```

Press Ctrl-C to detach (the remote process is killed).

### Capture structured output for assertion

```bash
RESULT=$(sudo nlink-lab exec --json simple client -- curl -fsS http://server:8080/health)
echo "$RESULT" | jq -e '.exit_code == 0 and (.stdout | contains("ok"))'
```

### Run with environment variables

```bash
sudo nlink-lab exec simple worker \
  --env LOG_LEVEL=debug \
  --env CONFIG=/etc/myapp.toml \
  -- /usr/bin/myapp --once
```

### Run in a specific working directory

```bash
sudo nlink-lab exec simple builder \
  --workdir /tmp/build \
  -- cargo test --release
```

### Run a shell pipeline

```bash
sudo nlink-lab exec simple host -- sh -c 'ip route | grep default'
```

`exec` doesn't expand shell metacharacters itself; wrap pipelines
in `sh -c '...'` if needed.

### Resolve a node's address dynamically

```bash
SERVER_IP=$(nlink-lab ip simple server --iface eth0)
sudo nlink-lab exec simple client -- curl http://$SERVER_IP:8080/
```

`ip` doesn't require root.

## Exit codes

`exec`'s exit code is the exit code of the spawned command:

| Code | Meaning |
|------|---------|
| (passthrough) | The exit code of the command run inside the node |
| 1 | Could not start the command (binary not found, etc.) |
| 2 | Lab or node not found |
| 5 | Insufficient capabilities |

If you need to distinguish "the command exited with 1" from "exec
itself failed," use `--json`.

## See also

- [`spawn`](spawn.md) — long-lived background processes
- [`shell`](shell.md) — interactive shell in a node
- [`wait-for`](wait-for.md) — block until a condition holds (TCP / file / exec)
- [`ip`](ip.md) — resolve node addresses for use in `exec`
