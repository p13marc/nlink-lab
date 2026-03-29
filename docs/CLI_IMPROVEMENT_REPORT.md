# CLI Quality-of-Life Improvement Report

Analysis of `nlink-lab-cli` — 22 commands, 35 issues found, organized by
impact.

## Current Command Inventory

| Command | Purpose | JSON | Timing | Root |
|---------|---------|------|--------|------|
| `deploy` | Create lab from .nll file | -- | Yes | Yes |
| `apply` | Hot-reload topology changes | -- | Yes | Yes |
| `destroy` | Tear down lab | -- | No | Yes |
| `status` | Show running labs / lab details | Yes | No | No |
| `exec` | Run command in lab node | -- | No | Yes |
| `validate` | Check topology without deploying | -- | No | No |
| `impair` | Modify runtime impairments | -- | No | Yes |
| `graph` | Print DOT graph | -- | No | No |
| `render` | Expand and print topology | Yes | No | No |
| `ps` | List background processes | Yes | No | No |
| `kill` | Kill background process | -- | No | Yes |
| `diagnose` | Run network diagnostics | **No** | No | Yes |
| `capture` | Packet capture (tcpdump) | -- | No | Yes |
| `wait` | Wait for lab ready | -- | No | No |
| `diff` | Compare two topologies | -- | No | No |
| `export` | Export lab topology | -- | No | No |
| `completions` | Generate shell completions | -- | -- | No |
| `daemon` | Start Zenoh backend | -- | No | Yes |
| `metrics` | Stream live metrics | -- | No | No |
| `init` | Create from template | -- | No | No |

## Priority 1: Must Fix

### 1.1 `diagnose` missing `--json` support

Every other inspection command supports `--json`. Diagnose doesn't.
Users can't parse diagnostic output programmatically.

```rust
// Current: raw text only
// Fix: check json flag, serialize DiagReport
if json {
    println!("{}", serde_json::to_string_pretty(&results)?);
} else {
    // existing text output
}
```

### 1.2 `exec` doesn't validate node exists

If you typo the node name, you get a confusing nlink error instead of
a helpful message listing available nodes.

```
$ nlink-lab exec mylab typo-node -- ip addr
Error: node not found: typo-node

# Better:
Error: node 'typo-node' not found in lab 'mylab'
Available nodes: router, host1, host2
```

### 1.3 `destroy` shows minimal feedback

Just says "X namespaces removed". Should show what was cleaned up:

```
# Current:
Lab "mylab" destroyed (5 namespaces removed)

# Better:
Lab "mylab" destroyed:
  Nodes:       5 namespaces removed
  Containers:  2 stopped and removed
  Processes:   3 killed
  Links:       8 (auto-cleaned)
  State:       ~/.local/state/nlink-lab/labs/mylab/ removed
```

### 1.4 `status` (specific lab) lacks detail

Shows comma-separated node names. Should be a proper table:

```
# Current:
Lab: mylab
Nodes: 5
Links: 8
  router, host1, host2, host3, host4

# Better:
Lab: mylab
Created: 2026-03-29 14:32:10 UTC
Nodes: 5
Links: 8
Impairments: 2

  NODE      TYPE        IMAGE              INTERFACES
  router    namespace   --                 eth0, eth1, eth2
  host1     namespace   --                 eth0
  web       container   nginx:alpine       eth0
  db        container   postgres:16        eth0
  cache     container   redis:7            eth0
```

## Priority 2: Should Fix

### 2.1 `wait` has no progress feedback

Silently polls every 250ms. User doesn't know if it's working:

```
# Current: (silence for 30 seconds, then timeout or success)

# Better:
Waiting for lab 'mylab'... ready (3.2s)
# or
Waiting for lab 'mylab'... timeout after 30s
```

### 2.2 `deploy` doesn't show breakdown timing

Shows total time but no phases. For a 10-second deploy, user can't
tell what's slow:

```
# Current:
Lab "dc" deployed in 8s

# Better:
Lab "dc" deployed in 8.2s
  Parse:       0.05s
  Validate:    0.12s
  Namespaces:  0.8s
  Links:       2.1s
  Addresses:   1.3s
  Routes:      0.5s
  Firewall:    0.2s
  Impairments: 0.1s
  Assertions:  3.0s (2 pass, 0 fail)
```

### 2.3 `impair --show` outputs raw tc

Shows raw `tc qdisc show` output which is hard to read. Should parse
into a table:

```
# Current:
qdisc netem 8001: dev eth0 root refcnt 2 limit 1000 delay 10ms ...

# Better:
  ENDPOINT      DELAY   JITTER  LOSS    RATE
  router:eth0   10ms    2ms     0.1%    100mbit
  router:eth1   --      --      --      --
```

### 2.4 Missing `destroy --all`

No way to destroy all labs at once. Users must list then destroy each:

```bash
# Current workaround:
nlink-lab status --json | jq -r '.[].name' | xargs -I{} sudo nlink-lab destroy {}

# Better:
sudo nlink-lab destroy --all
```

### 2.5 `apply` doesn't show what changed

Dry-run shows the diff, but actual apply just says "applied". Should
show a summary:

```
# Current:
Applied changes to 'mylab' in 2.1s

# Better:
Applied changes to 'mylab' in 2.1s:
  Added:    node host3, link router:eth2--host3:eth0
  Removed:  impairment on router:eth1
  Changed:  impairment on router:eth0 (delay 10ms → 50ms)
```

### 2.6 `capture` doesn't forward exit code

Failed captures (permission denied, interface not found) appear
successful to the shell because exit code isn't forwarded.

### 2.7 Missing `--verbose` / `--quiet` global flags

No way to control output verbosity. Some users want silence (scripts),
others want maximum detail (debugging).

```bash
nlink-lab deploy -q topology.nll    # silent, only errors
nlink-lab deploy -v topology.nll    # show each deployment step
```

## Priority 3: Nice to Have

### 3.1 `deploy` should suggest next steps

After deploy, new users don't know what to do:

```
Lab "mylab" deployed in 2.1s (3 nodes, 4 links)

Next steps:
  nlink-lab status mylab          # inspect lab details
  nlink-lab exec mylab router -- ip addr   # run commands
  nlink-lab diagnose mylab        # check connectivity
  nlink-lab destroy mylab         # tear down
```

### 3.2 Colored output

No color anywhere. Colored severity labels, status badges, and link
diagrams would improve readability:

```
  PASS  host1 can reach host2 (10.0.0.2)
  FAIL  host1 cannot reach host3 (10.0.1.2)
  WARN  node 'isolated' has no links
```

### 3.3 `graph` is redundant with `render --dot`

`graph` and `render --dot` do the same thing. Could deprecate `graph`
in favor of `render --dot`, or make `graph` open a viewer.

### 3.4 Missing `logs` command

No way to see background process stdout/stderr after deploy. Users
must re-exec to check:

```bash
# Missing:
nlink-lab logs mylab router    # show stdout/stderr from background procs
```

### 3.5 Missing `restart` command

To restart a lab, users must destroy + deploy. A restart command would
preserve state:

```bash
nlink-lab restart mylab
# equivalent to: destroy + deploy with same topology
```

### 3.6 Missing `attach` / `shell` command

`exec` requires specifying the full command. A shell shorthand:

```bash
nlink-lab shell mylab router
# equivalent to: nlink-lab exec mylab router -- /bin/sh
```

### 3.7 `init` should open editor

After creating a file from a template, offer to open it:

```
Created router.nll from template 'router'
Edit with: $EDITOR router.nll
```

### 3.8 Missing `inspect` command

Combine status + diagnose + impair --show into one comprehensive view:

```bash
nlink-lab inspect mylab
# Shows: lab info, node table, link table, impairments, diagnostics
```

## Priority 4: Container-Specific Features

The CLI has **zero container-specific commands**. Container nodes are
deployed and exec'd transparently, but there's no way to inspect or
manage the container layer directly.

### 4.1 Missing `containers` / `images` command

No way to list containers in a lab or see their status:

```bash
# Missing:
nlink-lab containers mylab

# Expected output:
  NODE    IMAGE            CONTAINER ID    STATUS     PID    CPU    MEMORY
  web     nginx:alpine     a1b2c3d4e5f6    running    4521   0.5    256m
  db      postgres:16      f6e5d4c3b2a1    running    4522   1      512m
  cache   redis:7          1a2b3c4d5e6f    running    4523   --     --
```

This would shell out to `docker/podman inspect` for live status (CPU,
memory usage, uptime) beyond what's in the state file.

### 4.2 Missing `logs` command for container processes

Background processes in containers write to stdout/stderr but there's
no way to retrieve those logs:

```bash
# Missing:
nlink-lab logs mylab web
nlink-lab logs mylab web --follow    # tail -f style
nlink-lab logs mylab web --tail 50   # last 50 lines

# Implementation: docker/podman logs <container_id>
```

For namespace nodes, `exec` with `journalctl` or reading log files works.
But container logs need `docker logs` / `podman logs`.

### 4.3 Missing `pull` command

No way to pre-pull images before deploying. Useful for air-gapped
environments or CI where you want to cache images:

```bash
nlink-lab pull topology.nll
# Parses topology, finds all image references, pulls each one
# Output: Pulled nginx:alpine (42MB), postgres:16 (380MB), redis:7 (35MB)
```

### 4.4 Missing `restart` for container nodes

Can't restart a single container node without destroying the whole lab:

```bash
nlink-lab restart mylab web
# Stops and starts the container, re-applies networking
```

### 4.5 `exec` should support `-it` interactive mode

Currently `exec` captures output and prints it. For interactive
commands (shells, debuggers), it should attach stdin/stdout directly:

```bash
# Current (non-interactive):
nlink-lab exec mylab web -- /bin/sh    # hangs or returns immediately

# Better:
nlink-lab exec -it mylab web -- /bin/sh    # interactive TTY
nlink-lab shell mylab web                  # shorthand for the above
```

Implementation: Use `docker exec -it` for containers,
`nsenter + exec` for namespaces.

### 4.6 `status` should show container health

When nodes have `healthcheck` defined, status should show health state:

```
  NODE    TYPE        IMAGE           HEALTH      UPTIME
  web     container   nginx:alpine    healthy     12m
  db      container   postgres:16     healthy     12m
  worker  container   myapp           unhealthy   12m
  router  namespace   --              --          12m
```

Implementation: For containers, run the healthcheck command and report
pass/fail. For namespaces, show `--`.

### 4.7 Missing container resource usage

No way to see actual CPU/memory usage of container nodes:

```bash
nlink-lab stats mylab

  NODE    CPU%    MEMORY      MEMORY%    NET I/O
  web     2.3%    45.2 MiB    17.6%      1.2 MB / 850 KB
  db      8.1%    198 MiB     38.7%      3.4 MB / 2.1 MB
  cache   0.5%    12.1 MiB    4.7%       500 KB / 200 KB
```

Implementation: `docker stats --no-stream` / `podman stats --no-stream`.

### 4.8 `deploy` should report image pull progress

When deploying with container nodes, image pulls are silent. Should
show download progress:

```
Pulling nginx:alpine... done (42MB, 3.2s)
Pulling postgres:16... done (380MB, 12.1s)
Creating nodes...
```

## Summary

| Priority | Count | Description |
|----------|-------|-------------|
| P1: Must fix | 4 | Broken/missing functionality |
| P2: Should fix | 7 | Significant UX improvements |
| P3: Nice to have | 8 | Polish and convenience |
| P4: Containers | 8 | Container-specific features |
| **Total** | **27** | |

## Recommended Implementation Order

**Phase A** (1 day): P1 items — diagnose JSON, exec node validation,
destroy detail, status table.

**Phase B** (1-2 days): P2 items — wait progress, deploy breakdown,
impair table, destroy --all, apply summary, capture exit code, verbose flag.

**Phase C** (1-2 days): P4 container items — containers command, logs
command, shell/exec -it, pull command, status health column.

**Phase D** (2-3 days): P3 items — colors, inspect command, deprecate
graph, next-step hints, stats command.
