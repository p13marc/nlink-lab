# `nlink-lab destroy`

Tear down a running lab and remove its kernel state.

## Usage

```text
nlink-lab destroy [OPTIONS] [NAME]
nlink-lab destroy --all [OPTIONS]
nlink-lab destroy --orphans [OPTIONS]
```

## Description

Reverses what `deploy` built: kills spawned processes, removes
container nodes, deletes namespaces, removes bridges and veth
pairs, removes the state directory at `~/.nlink-lab/<name>/`.

Three modes:

- **`destroy <name>`** — destroy one named lab.
- **`destroy --all`** — destroy every lab listed by `status`.
- **`destroy --orphans`** — reap mgmt bridges, veths, and namespaces
  whose state file is missing (a deploy crashed or the state
  directory was deleted manually). Doesn't require a name.

## Arguments

| Argument | Description |
|----------|-------------|
| `[NAME]` | Lab name. Omitted when `--all` or `--orphans` is given. |

## Options

| Flag | Description |
|------|-------------|
| `--force` | Continue cleanup even if some resources are already gone. Useful when a previous destroy failed mid-way. |
| `--all` | Destroy every running lab. Mutually exclusive with `<NAME>`. |
| `--orphans` | Reap host resources (namespaces, bridges, veths) that don't have a state file. Combinable with `--all`. |
| `--json` | Emit JSON listing what was destroyed and what failed. |
| `-v`, `--verbose` | Print each cleanup step. |
| `-q`, `--quiet` | Suppress non-error output. |

## Examples

### Tear down a lab

```bash
sudo nlink-lab destroy simple
```

### Destroy every running lab

```bash
sudo nlink-lab destroy --all
```

### Reap orphaned resources after a crashed deploy

```bash
nlink-lab status --scan         # list orphans without removing
sudo nlink-lab destroy --orphans
```

### CI cleanup that doesn't fail on missing state

```bash
sudo nlink-lab destroy --force --all || true
```

`--force` makes destroy idempotent: missing namespaces or bridges
are warnings, not errors.

## What gets destroyed

| Resource | Destroyed |
|----------|-----------|
| Network namespaces | yes |
| Bridges | yes |
| veth pairs | yes (deleted with one side; kernel removes the pair) |
| Container nodes | yes (`docker rm -f` / `podman rm -f`) |
| Spawned processes | yes (SIGKILL) |
| /etc/hosts injections | yes |
| Wi-Fi `mac80211_hwsim` radios | yes |
| State directory `~/.nlink-lab/<lab>/` | yes |

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success (or `--force` swallowed expected errors) |
| 1 | Bad arguments (e.g. both `<NAME>` and `--all`) |
| 2 | Some resources failed to clean up; partial state may remain |
| 3 | Lab not found |
| 4 | Lock contention |

## See also

- [`deploy`](deploy.md)
- [`status`](status.md) — `--scan` flag lists orphans without destroying
