# CLI reference

Every `nlink-lab` subcommand has a reference page below, grouped by what
you'd typically reach for.

For a high-level walkthrough, start with the
[user guide](../USER_GUIDE.md) instead.

## Lifecycle

The day-to-day commands.

| Command | What |
|---------|------|
| [`deploy`](deploy.md) | Bring a topology up |
| [`destroy`](destroy.md) | Tear a topology down |
| [`use`](use.md) | Select the current lab that lab-scoped commands act on |
| [`validate`](validate.md) | Parse + validate without deploying |
| [`edit`](edit.md) | Edit a running lab in place (nodes, links, impairments) |
| [`snapshot`](snapshot.md) | Checkpoint a running lab |
| [`restore`](restore.md) | Restore a lab from a checkpoint |
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
| [`verify`](verify.md) | Check a running lab for drift against its topology |
| [`metrics`](metrics.md) | Zenoh metrics export |
| [`top`](top.md) | Live TUI: nodes, interfaces, flows |
| [`containers`](containers.md) | List container nodes |
| [`events`](events.md) | Lifecycle event log (deploy, impair, spawn, drift) |

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
| [`fmt`](fmt.md) | Format NLL files (`--check` for CI, `-w` in place) |
| [`lint`](lint.md) | Style and portability advice for a topology |
| [`doctor`](doctor.md) | Check this host for everything nlink-lab needs |
| [`test`](test.md) | Deploy → validate → destroy across one or more NLL files |
| [`export`](export.md) | Export a lab as plain text or a portable `.nlz` archive |
| [`import`](import.md) | Import a `.nlz` archive — verify, extract, deploy |
| [`pull`](pull.md) | Pre-pull container images for a topology |
| [`completions`](completions.md) | Generate shell completions |
| [`lsp`](lsp.md) | Language server for `.nll` files (stdio) |
| [`daemon`](daemon.md) | Run the zenoh backend for a lab (metrics, RPC, events) |
| [`metrics`](metrics.md) | Stream live metrics published by `daemon` |
| [`watch`](watch.md) | Print RTNETLINK + nftables drift events for a lab |
| [`scenario`](scenario.md) | Run a `scenario` block from a deployed lab's topology |
| [`benchmark`](benchmark.md) | Run a `benchmark` block from a deployed lab's topology |
| [`proc-stat`](proc-stat.md) | Resource snapshot of a spawned process |
| [`wait`](wait.md) | Block until a lab's state file exists (deploy finished) |

## Global flags

These are accepted by every subcommand:

| Flag | What |
|------|------|
| `--format text\|json\|ndjson` | `text` for people; `json`, one pretty document; `ndjson`, one compact document per line (what streams always print). JSON documents carry a `schema_version`; errors become the API's error document on stderr (see below) |
| `--json` | Shorthand for `--format json` |
| `--color auto\|always\|never` | Colour the output: `auto` colours a stream that is a terminal, unless `NO_COLOR` is set (`CLICOLOR` and `CLICOLOR_FORCE` are honoured too). Decided per stream |
| `-v`, `-vv` | Show deployment steps and `info` logs; `-vv` adds `debug` |
| `--quiet`, `-q` | Suppress non-error output; list commands (`ps`) print only ids |

Commands that act on a running lab (`exec`, `ps`, `impair`, `capture`,
…) take `-L LAB`; without it the lab is `$NLINK_LAB`, else the one
[`use`](use.md) selected, else the only running lab. Completion offers
that lab's nodes and endpoints.

`--skip-validate` belongs to `deploy` only (it drops the `validate {}`
assertions); `--set KEY=VALUE` is accepted by every command that reads a
topology (`deploy`, `apply`, `validate`, `render`, `graph`, `diff`,
`pull`, `test`, `export --archive`).

Under `--json`, a failure is printed on stderr as the daemon API's error
document (`docs/API.md`): `code`, `message`, `chain`, `exit_code`, and
when they apply `errno`, `ext_ack`, `netns`, `hint`, `issues` (a parse
error is one issue with rule `parse` and `location` `file:line:column`)
and `nlink` (what nlink says about a failed netlink call).

## Values

- **Durations** carry a unit: `500ms`, `30s`, `1.5m`, `2h` (`ns`, `us`,
  `ms`, `s`, `m`, `h`). A bare number is refused with a hint: `--timeout
  30` read seconds in one command and milliseconds in another.
- **`KEY=VALUE`** for `--set` and `--env`; **`NODE:IFACE`** for
  endpoints. A malformed value is a usage error (exit 64) before
  anything runs.
- **Short flags mean one thing everywhere**: `-c` count, `-e` env,
  `-f` follow, `-i` interval, `-o` output, `-s` signal, `-t` timeout,
  `-w` write in place, plus the global `-q`, `-v`, `-h`, `-V`. Anything
  else is long only.

## Exit codes

One policy for every subcommand (the `--json` error document carries the
same value in `exit_code`):

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error (lab not found, kernel/netlink failure, lock held by another process) |
| 2 | A negative result: validation, assertion or drift failure — `validate`/`deploy` with topology errors, `deploy --strict`, `test`, `scenario`, `benchmark`, `apply --check` |
| 64 | Usage error: a command line that does not parse, or a removed command (which says what replaced it) |
| 124 | Timeout (`exec --timeout`, `wait-for`) |
| 130 | Interrupted or cancelled |
| child's code | `exec` and `shell` pass the command's exit status through |
