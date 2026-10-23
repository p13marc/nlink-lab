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
| [`snapshot`](snapshot.md) | Checkpoints of a running lab: `save`, `ls`, `rm`, `restore` |
| [`apply`](apply.md) | Reconcile a running lab to an edited NLL (default: its recorded source) |
| [`ls`](ls.md) | List the labs on this host (`--scan`: orphans and stale labs) |
| [`status`](status.md) | One lab: nodes, addresses, containers, links, impairments, processes |
| [`archive`](archive.md) | Portable `.nlz` archives: `create`, `extract`, `inspect` |

## Interaction

Run things inside a lab.

| Command | What |
|---------|------|
| [`exec`](exec.md) | Run a one-shot command in a node |
| [`spawn`](spawn.md) | Run a long-lived background process |
| [`shell`](shell.md) | Open an interactive shell in a node |
| [`wait`](wait.md) | Block until a lab is deployed, or a TCP port / file / exec / log line in a node |
| [`logs`](logs.md) | Tail logs of spawned processes or container nodes |
| [`ps`](ps.md) | List spawned processes |
| [`kill`](kill.md) | Kill a spawned process |
| [`stats`](stats.md) | Live CPU/memory for container nodes |
| [`node`](node.md) | Add, remove or restart a node |
| [`link`](link.md) | Add, remove or change a link (MTU, qdisc) |

## Inspection

What's actually running.

| Command | What |
|---------|------|
| [`ip`](ip.md) | Show node IP addresses |
| [`conntrack`](conntrack.md) | List a lab's tracked flows, or delete them (`--flush`) |
| [`render`](render.md) | Lower an NLL to flat NLL / JSON, or a diagram (`--diagram dot\|mermaid\|ascii`) |
| [`diff`](diff.md) | Diff two NLL topologies |
| [`diagnose`](diagnose.md) | Per-lab health checks |
| [`verify`](verify.md) | Check a running lab for drift against its topology |
| [`top`](top.md) | Live TUI: nodes, interfaces, flows |
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
| [`export`](export.md) | A running lab's topology as TOML, NLL or JSON |
| [`pull`](pull.md) | Pre-pull container images for a topology |
| [`completions`](completions.md) | Generate shell completions |
| [`lsp`](lsp.md) | Language server for `.nll` files (stdio) |
| [`scenario`](scenario.md) | Run a `scenario` block from a deployed lab's topology |
| [`benchmark`](benchmark.md) | Run a `benchmark` block from a deployed lab's topology |
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
topology (`deploy`, `apply`, `validate`, `render`, `diff`,
`pull`, `test`, `verify --file`, `archive create`).

Under `--json`, a failure is printed on stderr as the daemon API's error
document (`docs/API.md`): `code`, `message`, `chain`, `exit_code`, and
when they apply `errno`, `ext_ack`, `netns`, `hint`, `issues` (a parse
error is one issue with rule `parse` and `location` `file:line:column`)
and `nlink` (what nlink says about a failed netlink call).

## Progress

Long operations — `deploy`, `apply`, `destroy`, `snapshot restore`,
`test`, `scenario`, the waits of `spawn` and `wait`, `pull` — show
what they are doing, on stderr:

- on a terminal, one live line (the stage, the op being done, what is
  being waited for, elapsed time) and a kept `✓` line per finished
  stage;
- on a pipe or in a CI log, one line per finished stage, wait and
  warning, each after the time since the start, and a heartbeat every
  10 s while nothing else happens;
- with `--format ndjson`, every event as a JSON line on stdout
  (`{"schema_version": 1, "item": "op", "event": {…}}`, the shape of
  [the API's `OpEvent`](../API.md#progress)), then the result's line;
- nothing with `--json` (one document) or `--quiet`.

Ctrl-C cancels a deploy, apply or restore: it stops before its next
step, or in the middle of a wait, and rolls back what it did (exit 130).
A second Ctrl-C a moment later quits at once. `destroy` does not stop
halfway; Ctrl-C only says so.

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
| 2 | A negative result: validation, assertion or drift failure — `validate`/`deploy` with topology errors, `deploy --strict`, `test`, `scenario`, `benchmark`, `verify` |
| 64 | Usage error: a command line that does not parse, or a removed command (which says what replaced it) |
| 124 | Timeout (`exec --timeout`, `wait`) |
| 130 | Interrupted or cancelled |
| child's code | `exec` and `shell` pass the command's exit status through |
