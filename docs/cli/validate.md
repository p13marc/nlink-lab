# `nlink-lab validate`

Parse and validate a topology file without touching the kernel.

## Usage

```text
nlink-lab validate [OPTIONS] <TOPOLOGY>
```

## Description

Runs the parser and validator pipeline that `deploy` would, then
exits. Useful as a syntax check, a CI gate, or to inspect the
resolved IP addresses an auto-assigned subnet would produce.

`validate` does not require root. Safe to run anywhere.

The validator runs ~20 rules covering CIDR validity, endpoint
format (`node:iface`), dangling node references, profile
references, interface uniqueness, VLAN range, impairment
endpoints, rate-limit endpoints, route configuration, interface
name length, WireGuard peer keys, VRF table uniqueness, duplicate
link endpoints, and warning-level rules for unique IPs, MTU
consistency, route reachability, and unreferenced nodes.

## Arguments

| Argument | Description |
|----------|-------------|
| `<TOPOLOGY>` | Path to a `.nll` file. |

## Options

| Flag | Description |
|------|-------------|
| `--set KEY=VALUE` | Override a `param` declaration. Repeatable. Validates the same way `deploy` would after substitution. |
| `--show-ips` | Print the resolved IP address for every interface, including subnet-auto-assigned ones. |
| `--json` | Emit a structured report: `{ valid: true, issues: [...] }`. CI-friendly. |
| `-v`, `--verbose` | Print parser progress and lower-stage steps. |
| `-q`, `--quiet` | Suppress all non-error output (exit code is the only signal). |

## Examples

### Quick syntax check

```bash
nlink-lab validate examples/multi-site.nll
```

Output:

```text
Topology "infra" is valid
  Nodes:       16
  Links:       5
  Profiles:    5
  Networks:    7
  Impairments: 0
  Rate limits: 0
```

### See what addresses would be assigned

```bash
nlink-lab validate --show-ips examples/spine-leaf.nll
```

Useful when a topology uses subnet auto-assignment and you want to
know which `.x` each node will land on before deploying.

### CI gate

```bash
# Fail the build on any validator error or warning.
for f in topology/*.nll; do
  nlink-lab validate --quiet "$f" || exit 1
done
```

### Validate with parameter sweep

```bash
for delay in 5ms 50ms 500ms; do
  nlink-lab validate --set delay=$delay --quiet wan.nll || exit 1
done
```

### JSON output for tooling

```bash
nlink-lab validate --json examples/wan-impairment.nll | jq '.issues[]'
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Topology is valid (warnings allowed) |
| 1 | Validation failed |
| 2 | File not found or unreadable |

Note: warnings do not affect the exit code. To make warnings fatal,
parse the JSON output and check `issues[?].severity`.

## See also

- [`render`](render.md) — produce the post-lower flat NLL / Dot / ASCII
- [`deploy`](deploy.md) — `validate` runs as the first step
