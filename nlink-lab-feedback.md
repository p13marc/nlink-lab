# nlink-lab — feedback report

Based on a single session bringing up a 3-node topology (`des-3m`) with two
isolated LANs, this is a list of issues hit and suggestions for fixes and
improvements. File/line references are against a local checkout of nlink-lab
and nlink at the versions installed as `/usr/local/bin/nlink-lab`.

## Bugs

### 1. `nlink-lab shell` is broken (high)

```
$ nlink-lab shell des-3m router
nsenter: neither filename nor target pid supplied for ns/net
```

**Root cause** — `bins/lab/src/main.rs:1383-1389`:

```rust
let ns_path = format!("/var/run/netns/{ns}");
let status = std::process::Command::new("nsenter")
    .args(["--net", &ns_path, "--", &shell])
```

`--net` and `ns_path` are passed as two separate argv entries. `nsenter`
interprets bare `--net` as "enter the target's net namespace" and expects
the target (PID or file) from another flag — none is given, so it errors.

**Fix** — pass a single argument:

```rust
.args([&format!("--net={ns_path}"), "--", &shell])
```

**Reproducer** — any successfully-deployed namespace node.

### 2. Peer-name collision when two network names share a 4-character prefix (high)

Deploy failure with a misleading error:

```
error: deploy failed: failed to create veth for network 'lan_b'
member 'router:eth1': add_link(eth1, kind=veth): File exists (os error 17)
```

The message blames `eth1` in the router ns, but `eth1` does not exist
anywhere on the host, in any netns, or in the just-created router
namespace. The actual collision is in the mgmt namespace.

**Root cause** — `crates/nlink-lab/src/deploy.rs:410`:

```rust
let peer_name = format!("br{}p{}", net_name.chars().take(4).collect::<String>(), k);
```

The mgmt-side peer interface name uses only the first 4 characters of the
network name. Two networks named `lan_a` and `lan_b` both produce
`brlan_p0` / `brlan_p1`, so the second veth pair's peer end collides in
mgmt ns. The rtnetlink error carries only EEXIST and no name, and the
caller formats the message around the primary (`ep.iface` = `eth1`),
producing the misdirection.

**Suggested fixes** (any one, but 1 + 3 preferred):

1. Generate peer names from a short hash of the network name, same way
   the root-ns mgmt bridge is named (`mgmt_bridge_name_for`). This makes
   the constraint disappear entirely.
2. Use the full network name (truncated to fit 15 chars after the
   `br…p{idx}` suffix), and validate at topology-parse time that no two
   networks collapse to the same peer-name prefix.
3. Improve the error message: when `add_link` returns EEXIST on a veth
   pair, probe both `name` and `peer_name` in their target namespaces and
   report which one is the culprit.

Once fix 1 or 2 is in, a validator rule at parse-time to reject
pre-collision topologies would prevent regressions.

### 3. Error attribution for veth EEXIST is misleading (medium)

See #2 above. Even independent of the peer-name bug, any future EEXIST on
a veth pair will produce the same wrong-side blame. The deploy error
format at `deploy.rs:424-428` should distinguish which end collided.

## UX / observability

### 4. `nlink-lab destroy --all --force` does not find orphans

```
$ nlink-lab status             # says "no running labs"
$ nlink-lab destroy --all --force
No running labs.               # but mgmt bridge / veths still exist
$ nlink-lab destroy des-3m --force
deleted mgmt bridge 'nlf886648e'   # finally cleaned up
```

**Root cause** — `bins/lab/src/main.rs:800-822`: `--all` iterates
`RunningLab::list()`, which only enumerates labs with a persisted
`state.json`. If a deploy crashes before state is written (e.g., due to
bug #2), the mgmt bridge and any half-made interfaces leak silently,
and `--all` can't see them.

**Fix suggestion** — make `destroy --all --force` also scan the host for
orphaned resources using the well-known pattern (`nl{hash}` bridges and
`nm{hash}…` veths, matching the same hashing scheme as `force_cleanup`),
and offer to clean labs whose state is gone. Equivalently, a new
`nlink-lab cleanup` or `destroy --orphans` subcommand.

### 5. `nlink-lab status` is silent about orphan state

Related to #4: `status` only reads the state file. Recommend either:

- a `--scan` flag that walks the host for `nl{hash}` bridges / `{prefix}-*`
  netns and reports unknown-to-state resources, or
- always include a "orphans detected" line if any are found.

### 6. `nlink-lab exec` buffers stdio — can't be used for long-running services

`exec` captures full stdout/stderr and prints them only after the command
exits (`bins/lab/src/main.rs:985-990`). This makes it useless for running
a service in the foreground with live output, which is the common case
for manual testing (zenohd, a broker, a mediator).

**Fix suggestion** — add `exec --attach` (or `--tty` / `--interactive`)
that `stdin/out/err.inherit()`s like `shell` intends to, and propagates
the child's exit code.

### 7. `nlink-lab logs --follow` only works for container nodes

`spawn`ed processes write to `<state-dir>/<pid>.{out,err}`, but
`nlink-lab logs --pid <pid> --follow` rejects the combination — `--follow`
is container-only. Users have to find the log file path and `tail -f` it
manually. Supporting `--follow` for spawned-process logs would remove
this friction.

### 8. `check_root` warning is noisy and not actionable when installed SUID

`bins/lab/src/main.rs:2497-2515` warns about missing caps/SUID even when
the binary is SUID root. The `geteuid() != 0` check fires before the
SUID has been applied to the process's euid? Actually verify — if the
binary is `-rwsr-xr-x` root:root, `geteuid()` returns 0 inside the
process, so the warning shouldn't fire. If it is firing somewhere, it's
worth a look. (Didn't reproduce in this session; noting for awareness.)

## Feature suggestions

### 9. `nlink-lab attach` — stream-friendly exec

Equivalent to fixed `shell` but parameterised: `nlink-lab attach <lab>
<node> [-- cmd…]`. With no command, drops into the configured shell;
with a command, inherits stdio. Would make documenting manual test runs
dramatically simpler than the current "open seven terminals and
`sudo nsenter` into each" workflow.

### 10. Built-in split-window helper for manual multi-process tests

For the common manual-testing case (N processes across M nodes, each
wanting its own live terminal), a helper that prints or launches a
tmux/zellij session with one pane per component would be valuable. Even
a `--print-tmux` flag that emits a ready-to-paste tmux command list would
be a big UX win.

### 11. Deploy progress / partial-failure telemetry

A deploy that fails mid-way (bug #2 is a good example) leaves resources
behind, doesn't tell you which step failed, and doesn't print what it
already created. A verbose mode that emits a structured log of each step
(`creating netns des-3m-router` … `creating veth eth1/brlan_p0 — ERR`)
would make orphan cleanup and bug triage much easier.

### 12. Topology validator — pre-flight peer-name collision check

Once bug #2 is fixed for the truncation case, the same validator should
still run and reject topologies whose network names, after truncation,
collapse onto each other — as a defence against future regressions.

### 13. Document the 15-character ifname constraint and naming rules

The constraints around Linux interface names (15 chars, kernel-refused
characters) affect network names, node names, and derived bridge/peer
names. A section in the docs listing exactly what names are derived from
which fields — with length budgets and examples — would save a lot of
"why does my topology refuse to deploy" debugging.

## Summary

| # | Area                      | Severity | Type    |
|---|---------------------------|----------|---------|
| 1 | `shell` subcommand        | high     | bug     |
| 2 | Peer-name collision       | high     | bug     |
| 3 | Veth EEXIST attribution   | medium   | bug     |
| 4 | `destroy --all --force`   | medium   | UX      |
| 5 | `status` misses orphans   | low      | UX      |
| 6 | `exec` buffers stdio      | medium   | UX      |
| 7 | `logs --follow` for PID   | low      | UX      |
| 8 | `check_root` vs SUID      | low      | polish  |
| 9 | `attach` subcommand       | —        | feature |
|10 | Multi-pane test helper    | —        | feature |
|11 | Deploy telemetry          | —        | feature |
|12 | Naming validator          | —        | feature |
|13 | Naming/length docs        | —        | docs    |

Happy to contribute patches for the two high-severity bugs (shell
`--net=` and peer-name hashing) if that's useful.
