# CLI reference

Every `nlink-lab` subcommand has a reference page below. Pages are
organized by what you'd typically reach for; the alphabetical
listing at the bottom catches the long tail.

For a high-level walkthrough, start with the
[user guide](../USER_GUIDE.md) instead.

## Lifecycle

The day-to-day commands.

| Command | What |
|---------|------|
| [`deploy`](deploy.md) | Bring a topology up |
| [`destroy`](destroy.md) | Tear a topology down |
| [`validate`](validate.md) | Parse + validate without deploying |
| [`apply`](apply.md) | Reconcile a running lab to an edited NLL |
| [`status`](status.md) | List running labs and their nodes |

## Interaction

Run things inside a lab.

| Command | What |
|---------|------|
| [`exec`](exec.md) | Run a one-shot command in a node |
| [`spawn`](spawn.md) | Run a long-lived background process |
| [`shell`](shell.md) | Open an interactive shell in a node |
| [`wait-for`](wait-for.md) | Block until a TCP port / file / exec passes |
| [`logs`](logs.md) | Tail logs of spawned processes or container nodes |
| [`ps`](ps.md) | List spawned processes |
| [`kill`](kill.md) | Kill a spawned process |
| [`stats`](stats.md) | Live CPU/memory for container nodes |
| [`restart`](restart.md) | Restart a container node |

## Inspection

What's actually running.

| Command | What |
|---------|------|
| [`ip`](ip.md) | Show node IP addresses |
| [`inspect`](inspect.md) | Full lab overview (nodes, links, addresses) |
| [`render`](render.md) | Lower an NLL to flat NLL / JSON / Dot / ASCII |
| [`graph`](graph.md) | Topology graph (Dot) |
| [`diff`](diff.md) | Diff two NLL topologies |
| [`diagnose`](diagnose.md) | Per-lab health checks |
| [`metrics`](metrics.md) | Zenoh metrics export |
| [`containers`](containers.md) | List container nodes |

## Network ops

Things you'd reach for in a chaos test.

| Command | What |
|---------|------|
| [`capture`](capture.md) | Packet capture via netring (writes pcap) |
| [`impair`](impair.md) | Apply a runtime impairment (without redeploy) |

## Project / system

| Command | What |
|---------|------|
| [`init`](init.md) | Generate a new NLL from a template |
| [`test`](test.md) | Deploy → validate → destroy across one or more NLL files |
| [`export`](export.md) | Export a lab as plain text or a portable `.nlz` archive |
| [`import`](import.md) | Import a `.nlz` archive — verify, extract, deploy |
| [`pull`](pull.md) | Pre-pull container images for a topology |
| [`completions`](completions.md) | Generate shell completions |
| [`daemon`](daemon.md) | Run the zenoh backend for a lab (metrics, RPC, events) |
| [`metrics`](metrics.md) | Stream live metrics published by `daemon` |
| [`watch`](watch.md) | Print RTNETLINK + nftables drift events for a lab |
| [`scenario`](scenario.md) | Run a `scenario` block from a deployed lab's topology |
| [`proc-stat`](proc-stat.md) | Resource snapshot of a spawned process |
| [`wait`](wait.md) | Block until a lab's state file exists (deploy finished) |

## Global flags

These are accepted by every subcommand:

| Flag | What |
|------|------|
| `--json` | Emit machine-parseable JSON instead of human text (errors become a JSON envelope on stderr) |
| `--verbose`, `-v` | Show deployment steps / tracing (`info` level) |
| `--quiet`, `-q` | Suppress non-error output |

`--skip-validate` belongs to `deploy` only (it drops the `validate {}`
assertions); `--set KEY=VALUE` is accepted by every command that reads a
topology (`deploy`, `apply`, `validate`, `render`, `graph`, `diff`,
`pull`, `test`, `export --archive`).

## Exit codes

One policy for every subcommand (the `--json` error envelope carries the
same value in `exit_code`):

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error (bad arguments, lab not found, kernel/netlink failure, lock held by another process) |
| 2 | Validation, assertion or drift failure: `validate`/`deploy` with topology errors, `deploy --strict`, `test`, `scenario`, `apply --check` |
| 124 | Timeout (`exec --timeout`, `wait-for`) |
| child's code | `exec` and `shell` pass the command's exit status through |
