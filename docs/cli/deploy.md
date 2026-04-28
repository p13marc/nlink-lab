# `nlink-lab deploy`

Bring a topology defined in an NLL file up as a running lab.

## Usage

```text
nlink-lab deploy [OPTIONS] <TOPOLOGY>
```

## Description

Parses the NLL file, runs the validator, and executes the 18-step
deployment sequence: namespaces → bridges → veths → addresses →
routes → sysctls → nftables → impairments → rate limits → DNS →
spawned processes → validation. State is written to
`~/.nlink-lab/<lab-name>/`.

`deploy` requires either root, SUID install, or
`CAP_NET_ADMIN`+`CAP_SYS_ADMIN`. Some features need additional caps
— `CAP_DAC_OVERRIDE` for DNS injection, `CAP_SYS_MODULE` for Wi-Fi.

If a lab with the same name already exists, `deploy` errors with
exit code 3 unless `--force` is passed.

## Arguments

| Argument | Description |
|----------|-------------|
| `<TOPOLOGY>` | Path to a `.nll` file. Relative paths resolve against the current working directory. |

## Options

| Flag | Description |
|------|-------------|
| `--dry-run` | Parse + validate + render the deployment plan; don't touch the kernel. Exits 0 on success. |
| `--force` | Destroy the existing lab with the same name before deploying. |
| `--skip-validate` | Skip post-deploy `validate { }` block assertions. The static topology validator still runs. |
| `--set KEY=VALUE` | Override an NLL `param`. Repeatable. Values are passed as strings; the param's declared type does the cast. |
| `--unique` | Append `-pid<PID>` to the lab name. Useful for concurrent test labs that share an NLL. |
| `--suffix STR` | Append a fixed suffix to the lab name. Mutually exclusive with `--unique`. |
| `--daemon` | Start the Zenoh metrics daemon after deploy completes. |
| `--json` | Emit machine-parseable JSON: lab name, namespaces, addresses, exit status. |
| `-v`, `--verbose` | Print every deployment step (the 18-step trace). |
| `-q`, `--quiet` | Suppress all output except errors. |

## Examples

### Deploy a topology

```bash
sudo nlink-lab deploy examples/simple.nll
```

### Override parameters at deploy time

```bash
sudo nlink-lab deploy wan.nll --set wan_delay=50ms --set wan_loss=0.1%
```

### Concurrent test labs from one NLL

```bash
# In CI: each test process gets its own lab.
sudo nlink-lab deploy --unique tests/scenarios/leader-election.nll
# → "leader-election-pid12345"

# Or with a fixed suffix you control:
sudo nlink-lab deploy --suffix run42 tests/scenarios/leader-election.nll
# → "leader-election-run42"
```

### CI: parameterized deploy with JSON output

```bash
sudo nlink-lab deploy --json --set delay=20ms wan.nll \
  | jq '.namespaces[].name'
```

### Plan the deployment without touching the kernel

```bash
nlink-lab deploy --dry-run examples/satellite-mesh.nll
```

`--dry-run` runs the validator and reports anything that would block
deploy. Doesn't require root.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Parse or validation failure (no kernel state changed) |
| 2 | Deploy failed; partial state may be present (run `destroy` to clean up) |
| 3 | Lab already exists and `--force` was not passed |
| 4 | Lock contention (another deploy or destroy is in progress for this lab name) |
| 5 | Insufficient capabilities (need `CAP_NET_ADMIN`+`CAP_SYS_ADMIN`) |

## State

Deploy writes:

- `~/.nlink-lab/<lab>/state.json` — namespace names, container IDs,
  spawned PIDs, addresses
- `~/.nlink-lab/<lab>/topology.toml` — rendered (post-loop, post-import)
  topology

These files are read by `destroy`, `apply`, `inspect`, `exec`, and
all other lab-aware commands. Removing them by hand leaves orphan
namespaces; use `nlink-lab destroy --orphans` to reap them.

## See also

- [`destroy`](destroy.md) — tear a deployed lab down
- [`apply`](apply.md) — reconcile a running lab to an edited NLL
- [`validate`](validate.md) — parse + validate without deploying
- [`status`](status.md) — list running labs
