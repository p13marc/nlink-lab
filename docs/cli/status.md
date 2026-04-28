# `nlink-lab status`

List running labs, or show details for a specific lab.

## Usage

```text
nlink-lab status [OPTIONS] [NAME]
```

## Description

Reads `~/.nlink-lab/*/state.json` to enumerate currently-running
labs. With a name, shows the lab's nodes, addresses, and spawned
processes. With `--scan`, also walks the host for orphan resources
(namespaces, mgmt bridges, veths) without a state file — these are
left over by a crashed deploy and can be reaped with
`destroy --orphans`.

`status` does not require root.

## Arguments

| Argument | Description |
|----------|-------------|
| `[NAME]` | Lab name. Omit to list all running labs. |

## Options

| Flag | Description |
|------|-------------|
| `--scan` | Also report host-level orphans without a state file. |
| `--json` | Machine-parseable output. |
| `-v`, `--verbose` | More detail per lab (full state, sysctls, captured PIDs). |

## Examples

### List all running labs

```bash
nlink-lab status
```

```text
NAME           NODES  PROCESSES  STARTED
satellite-mesh    12          0  2026-04-27T10:14:23Z
wan-test           4          2  2026-04-27T11:02:11Z
```

### Show details for a specific lab

```bash
nlink-lab status satellite-mesh
```

Includes the node table with namespace name and addresses.

### Scan for orphans

```bash
nlink-lab status --scan
```

```text
Orphan namespaces:
  satellite-mesh-sat0      (no state file)
  satellite-mesh-sat1      (no state file)
Orphan bridges:
  br-mesh                  (no state file)

Reap with: sudo nlink-lab destroy --orphans
```

`--scan` is read-only — it never modifies host state.

### CI gate: assert no labs are running

```bash
nlink-lab status --json | jq -e 'length == 0'
```

### CI gate: assert specific lab is up

```bash
nlink-lab status --json simple | jq -e '.nodes | length == 2' \
  || { echo "lab broken"; exit 1; }
```

## Output schema (`--json`)

```json
{
  "name": "satellite-mesh",
  "started_at": "2026-04-27T10:14:23Z",
  "nodes": [
    {"name": "sat0", "namespace": "satellite-mesh-sat0",
     "addresses": ["172.100.0.1/24"]},
    ...
  ],
  "processes": [
    {"pid": 12345, "node": "sat0", "cmd": "...",
     "log_path": "/home/.../sat0-12345.log"}
  ],
  "containers": []
}
```

`status --scan` adds an `orphans` array.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Listing succeeded |
| 1 | Bad arguments |
| 3 | Named lab not found |

## See also

- [`destroy`](destroy.md) `--orphans` to reap state-less resources
- [`inspect`](inspect.md) — fuller view including topology graph
- [`ps`](ps.md), [`logs`](logs.md), [`stats`](stats.md) — drill into a running lab
