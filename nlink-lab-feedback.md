# Feedback for `nlink-lab` — from a heavy-test-harness consumer

**Date**: 2026-04-28
**Source**: build of `tools/des-test-harness/` in the `des-rs`
workspace, which wraps `nlink-lab` to orchestrate distributed tests
of `des-discovery`.
**`nlink-lab` version under test**: `nlink-lab 0.1.0`
**Host**: Linux 6.17 (Ubuntu 24.04 derivative), x86_64.

This report is self-contained — it does not assume the receiving LLM
has any context about the harness or the consuming project. Each
finding includes a concrete reproduction.

---

## Table of contents

1. [Context — what the harness does and how it uses nlink-lab](#1-context)
2. [Bugs (one)](#2-bugs)
3. [Surprising behaviours that need fixing or documenting (three)](#3-surprising-behaviours)
4. [Feature requests (five)](#4-feature-requests)
5. [Documentation gaps (three)](#5-documentation-gaps)
6. [Things that work very well](#6-things-that-work-very-well)
7. [Suggested CLI/API additions in priority order](#7-suggested-cliapi-additions-in-priority-order)
8. [Notes on contributing fixes](#8-notes-on-contributing-fixes)

---

## 1. Context

### What the consumer is

`des-test-harness` is a Rust crate that orchestrates end-to-end
tests of a distributed pub/sub system across an `nlink-lab` topology
(routers + isolated LANs + per-node namespaces). A typical test:

1. `HarnessBuilder::new(scenario).topology("3-machine.nll").build()` →
   internally runs `nlink-lab deploy --unique --json --quiet
   <topo>`.
2. Builder also starts `tcpdump` captures via `nlink-lab spawn` for
   each `capture_link("router:eth0")` declaration.
3. Test then calls `Harness::spawn_zenohd / spawn_cpp_mediator /
   spawn_des_discovery / spawn_des_pub / spawn_des_sub` — each is
   `nlink-lab spawn --json [--env ...] [--workdir ...]
   [--wait-tcp ...] LAB NODE CMD...`.
4. Test calls `Harness::wait_for_exit(handle, timeout)` which polls
   `nlink-lab ps --json LAB`.
5. Test calls `Harness::impair_link("router:eth0", Impair::partition())`
   → `nlink-lab impair --partition LAB ENDPOINT`.
6. On `Drop`, harness:
   - Kills capture processes via `nlink-lab kill`.
   - On failure: copies per-process stdout/stderr from
     `~/.local/state/nlink-lab/labs/{lab}/logs/` into a failure
     bundle, emits `HARNESS_ARTEFACT_DIR=...` line.
   - Calls `nlink-lab destroy LAB --force --quiet`.

### Surface area touched

- `nlink-lab deploy --unique --json --quiet <topo>`
- `nlink-lab destroy --force --quiet <lab>`
- `nlink-lab status --json [<lab>]`
- `nlink-lab spawn --json [--env K=V] [--workdir D] [--wait-tcp HP] <lab> <node> <cmd>...`
- `nlink-lab kill --quiet <lab> <pid>`
- `nlink-lab ps --json <lab>`
- `nlink-lab capture --quiet -w <file> --snap-len <n> <lab> <endpoint>`
- `nlink-lab impair [--loss / --delay / --jitter / --partition / --clear] <lab> <endpoint>`
- `nlink-lab metrics` — known of, not yet used by harness.

### Observable state we depend on

- Per-process stdout/stderr files at
  `~/.local/state/nlink-lab/labs/{lab}/logs/{node}-{cmd}-{pid}.{stdout,stderr}`
  (we read these directly; see §3.3 for how this broke us).
- `nlink-lab status --json` returns a JSON array of objects with at
  least `{name, node_count, created_at}`.
- `nlink-lab spawn --json` returns `{command, node, pid}`.
- `nlink-lab ps --json` returns an array of
  `{node, pid, alive, stdout_log, stderr_log}` (see §3.2 for the
  `alive` gotcha).

The harness is one of the heaviest external consumers of nlink-lab so
far — every issue below was hit during normal use and works around in
upstream-friendly ways. Each section's "Suggested fix" is what we
think would let us delete the workaround.

---

## 2. Bugs

### 2.1 BUG · `nlink-lab capture` does not flush its pcap on signal

**Severity**: HIGH (blocks a documented use case of the tool;
silently produces unusable output).

**Symptom**: a pcap file written by `nlink-lab capture -w
<path>` has **0 bytes** if the capture process is terminated by
SIGTERM, SIGINT, or SIGKILL. Only natural exits (`--count N` or
`--duration N`) produce a usable pcap.

**Reproduction** (verified on `nlink-lab 0.1.0`, kernel 6.17,
Ubuntu 24.04):

```bash
# Deploy a fresh lab
$ nlink-lab deploy testing/topologies/three-machine.nll --unique --quiet
OK Lab "des-3m-310086" deployed in 73ms
$ LAB=des-3m-310086

# Background capture; ping in parallel to generate traffic
$ rm -f /tmp/h4-test.pcap
$ (timeout 3 nlink-lab capture --quiet -w /tmp/h4-test.pcap \
    --snap-len 256 $LAB router:eth0 &)
$ sleep 0.3 && nlink-lab exec $LAB router /usr/bin/ping -c 4 -W 1 10.1.0.2
4 packets transmitted, 4 received, 0% packet loss, time 3073ms

# Wait for `timeout 3` to send SIGTERM.
$ sleep 1
$ ls -la /tmp/h4-test.pcap
-rw-rw-r-- 1 root mpardo 0 Apr 28 16:30 /tmp/h4-test.pcap
#                       ^ 0 bytes, despite 4 packets traversing the interface
```

Same result with SIGINT (`kill -INT $PID`) and with `nlink-lab destroy`
(which sends SIGKILL).

**Comparison — works as expected**: `--count` natural exit:

```bash
$ (nlink-lab exec $LAB router /usr/bin/ping -c 100 -i 0.05 10.1.0.2 > /dev/null 2>&1 &)
$ nlink-lab capture --quiet -c 10 -w /tmp/h4-test.pcap --snap-len 256 $LAB router:eth0
$ ls -la /tmp/h4-test.pcap
-rw-rw-r-- 1 root mpardo 1168 Apr 28 16:31 /tmp/h4-test.pcap
#                       ^ 10 ICMP packets, ~110 bytes each
```

**Diagnosis**: looks like the netring backend buffers writes and the
SIGTERM handler doesn't trigger a final `flush()` + close on the
pcap file. The kernel destroying the file's writer on SIGKILL leaves
the buffered bytes in user-space, never flushed to disk.

**Workaround used**: the harness avoids `nlink-lab capture` entirely.
Instead it spawns `tcpdump -U -w <path> -i <iface>` *inside* the
node namespace via `nlink-lab spawn`. The `-U` flag (unbuffered, "as
soon as a packet is received, write to file") guarantees that even a
SIGKILL on lab destroy leaves a complete pcap. This works but
duplicates capture machinery already in nlink-lab.

**Suggested fix**:

```rust
// In whatever file owns nlink-lab capture's main loop:
use tokio::signal::unix::{signal, SignalKind};

async fn run_capture(args: CaptureArgs) -> Result<()> {
    let mut writer = pcap::Writer::open(&args.write)?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint  = signal(SignalKind::interrupt())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,
            _ = sigint.recv()  => break,
            result = capture.next_packet() => {
                match result {
                    Some(pkt) => writer.write(&pkt)?,
                    None => break,
                }
            }
        }
    }
    // CRITICAL: flush + close before returning.
    writer.flush()?;
    drop(writer);
    Ok(())
}
```

The receiving LLM should also check whether the pcap writer is
currently flushed *between packets* (i.e., per-packet writes instead
of buffered) — `tcpdump`'s `-U` flag toggles exactly this. If it's
buffered today, switching to per-packet flush is the most robust fix;
the SIGTERM handler then becomes a polish item.

**Acceptance criteria**:

```bash
# This should produce a non-empty pcap with the captured packets:
nlink-lab capture --quiet -w /tmp/cap.pcap LAB router:eth0 &
PID=$!
sleep 1; nlink-lab exec LAB router /usr/bin/ping -c 5 10.1.0.2
sleep 1; kill -TERM $PID; wait $PID 2>/dev/null
[ "$(stat -c %s /tmp/cap.pcap)" -gt 100 ] || { echo FAIL; exit 1; }
```

---

## 3. Surprising behaviours

### 3.1 SURPRISE · `--env KEY=VALUE` changes the spawned process's logfile basename

**Severity**: MEDIUM (silently breaks any consumer that reconstructs
log paths from argv[0] basename).

**Symptom**: when `nlink-lab spawn` is invoked with `--env`, the
created log file is named `{node}-env-{pid}.{stdout,stderr}` rather
than `{node}-{argv0_basename}-{pid}.{stdout,stderr}`. This breaks
consumers that compute the log path from the binary name.

**Reproduction**:

```bash
$ LAB=$(nlink-lab status --json | jq -r '.[0].name')

# Without --env: filename uses binary basename (good)
$ nlink-lab spawn --json $LAB site_a /usr/bin/sleep 5
{"command":"/usr/bin/sleep 5","node":"site_a","pid":345001}
$ ls ~/.local/state/nlink-lab/labs/$LAB/logs/ | grep 345001
site_a-sleep-345001.stderr
site_a-sleep-345001.stdout

# With --env: filename basename becomes "env" (surprising)
$ nlink-lab spawn --json --env FOO=bar $LAB site_a /usr/bin/sleep 5
{"command":"/usr/bin/sleep 5","node":"site_a","pid":345020}
$ ls ~/.local/state/nlink-lab/labs/$LAB/logs/ | grep 345020
site_a-env-345020.stderr
site_a-env-345020.stdout
#       ^^^ should be "sleep"
```

**Diagnosis**: nlink-lab is implementing `--env` by wrapping the
command as `env KEY=VALUE <cmd>`. The logfile basename is then
derived from argv[0] of the actual exec, which is `env`. The user's
binary name is lost.

**Workaround used**: post-spawn, the harness scans
`~/.local/state/nlink-lab/labs/{lab}/logs/` for any file ending in
`-{pid}.stdout` and uses whatever filename it finds. This works but
makes the harness's log-path resolution opaque to readers.

**Suggested fix**: spawn the binary directly with environment vars
set in the child's `posix_spawn`/`execve` envp, rather than wrapping
with `env`. Standard library:

```rust
// Today (presumably):
Command::new("env").args(&env_args).arg(cmd).args(cmd_args).spawn()?;

// Suggested:
let mut cmd = Command::new(user_cmd);
for (k, v) in &user_env { cmd.env(k, v); }
cmd.args(user_args).spawn()?;
```

The logfile then follows naturally from `user_cmd`'s basename.

**Acceptance criteria**:

```bash
LAB=...
PID=$(nlink-lab spawn --json --env FOO=bar $LAB site_a /usr/bin/sleep 5 \
       | jq -r .pid)
[ -f ~/.local/state/nlink-lab/labs/$LAB/logs/site_a-sleep-$PID.stdout ]
# i.e. logfile basename is "sleep", not "env".
```

### 3.2 SURPRISE · `nlink-lab ps --json` keeps exited entries with `alive: false`

**Severity**: LOW–MEDIUM (reasonable schema, but causes subtle bugs
in consumers that don't read the field).

**Symptom**: a process spawned via `nlink-lab spawn` and then
exited remains in `nlink-lab ps --json` output, with the `alive`
field flipped to `false`. A naive consumer that polls "is `pid`
in the list?" sees the entry forever.

**Reproduction**:

```bash
$ LAB=$(nlink-lab status --json | jq -r '.[0].name')
$ nlink-lab spawn --json $LAB router /usr/bin/sleep 1
{"command":"/usr/bin/sleep 1","node":"router","pid":357574}
$ sleep 0.3 && nlink-lab ps --json $LAB | jq '.[] | {pid,alive}'
{"pid":357574,"alive":true}
$ sleep 1.5 && nlink-lab ps --json $LAB | jq '.[] | {pid,alive}'
{"pid":357574,"alive":false}
#                       ^^^^^ stays in list forever after exit
```

**Diagnosis**: by design — letting consumers see post-mortem state
of their spawned processes. But the field's existence is undocumented
in the help text:

```
$ nlink-lab ps --help | grep -A 2 alive
# (no output)
```

So a naive consumer's first encounter with a long-running test that
spawns a quick child is: "my wait-loop never returned because
`.[] | select(.pid == X)` was always non-empty."

**Workaround used**: harness's `wait_for_exit` checks `entry.alive`
in addition to `entry.pid`.

**Suggested fix** (pick one):

a. **Add `--alive-only` flag** that filters in the engine. Trivial
   change; opt-in for compatibility.

b. **Document the `alive` field in the help text**:

   ```
   --json    Output JSON instead of human-readable text. Each entry
             includes {node, pid, alive: bool, stdout_log, stderr_log}.
             Note: exited processes remain in the list with alive=false
             until the lab is destroyed.
   ```

c. **Garbage-collect exited entries after N seconds**. Less
   compatible — would break consumers that scrape post-mortem state.
   Don't recommend.

We'd take (a) and (b) together.

**Acceptance criteria**:

- `nlink-lab ps --help` output mentions the `alive` field.
- (Optional) `nlink-lab ps --alive-only --json LAB` returns the
  same array filtered to `alive == true`.

### 3.3 SURPRISE · `nlink-lab destroy` exit code is fine on success but message goes to stdout

**Severity**: trivial polish.

**Symptom**: `nlink-lab destroy LAB` succeeds with a multi-line
human-readable message on stdout, which is fine for interactive use
but noisy in scripts that pipe through it.

```bash
$ nlink-lab destroy $LAB --force
Lab "des-3m-XYZ" destroyed:
  Nodes:       3
  Links:       0
  Processes:   1 killed
$ echo $?
0
```

**Workaround used**: `--quiet` flag (already exists, works as
expected). Just a doc note that it's the recommended flag for
scripted use.

**Suggested**: in the help text for `--quiet` (across all
subcommands), recommend it for scripted/automated use:

```
-q, --quiet   Quiet output (errors only). Recommended for scripts;
              the default human-readable output is intended for
              interactive shells.
```

---

## 4. Feature requests

### 4.1 FEATURE · `nlink-lab logs --pid PID --follow`

**Severity**: MEDIUM (existing feature works for containers but not
for spawned background processes).

**Status**: `nlink-lab logs LAB --pid PID` exists today and shows
stdout (or `--stderr`). `--follow` is documented as "container
only". Consumers who spawn background processes via `nlink-lab
spawn` have to `tail -F` the log file themselves.

**Use case**: the harness sometimes wants to watch a long-running
process for a specific log line ("[STARTED] tunnel established") to
proceed with the next step. Today that's a custom `tail -F`
parsing loop in the consumer.

**Suggested fix**: extend `--follow` to work for
`--pid` background processes too.

```
nlink-lab logs LAB --pid 12345 --follow [--stderr]
# tails the per-process log file until the process exits OR
# the lab is destroyed OR Ctrl-C.
```

Implementation: `tail -F`-style loop that re-opens the log file
whenever it disappears (handles log rotation / lab destroy
gracefully).

**Bonus**: `--json` output mode that emits one JSON object per
log line:

```
{"ts": "2026-04-28T15:00:01Z", "pid": 12345, "stream": "stdout", "line": "[STARTED] ..."}
```

### 4.2 FEATURE · `nlink-lab spawn --wait-log <regex>`

**Severity**: NICE.

**Use case**: today, `nlink-lab spawn --wait-tcp HP` blocks the
spawn until a TCP listener opens — perfect for binaries that have
a known port. But many binaries signal readiness via a log line,
not a port (e.g., a Mediator that doesn't open a port until a
client connects, or a controller that prints "ready" to stdout).

**Suggested**:

```
--wait-log <REGEX>      Wait for a log line matching REGEX before
                        returning from spawn.
--wait-log-stream <S>   Which stream to monitor: stdout | stderr | both.
                        [default: both]
```

Same `--wait-timeout` applies. If the timeout fires before the regex
matches, fail the spawn (and bubble that up via exit code so the
consumer can react).

### 4.3 FEATURE · `nlink-lab spawn --capture-link <ENDPOINT>`

**Severity**: NICE — would let consumers stop carrying their own
capture machinery.

Right now the harness has its own `tcpdump -U` capture (because of
§2.1). If §2.1 is fixed AND the spawn-side could attach a capture
declaration, we could simplify:

```
nlink-lab spawn ... --capture-link router:eth0 --capture-out /tmp/r.pcap
# The capture lifetime is bound to the spawn — when the spawn exits,
# the capture is signaled to stop and flush.
```

Equivalent to:

```
nlink-lab capture LAB router:eth0 -w /tmp/r.pcap &
CAP_PID=$!
nlink-lab spawn LAB ... --wait-for-exit  # if such existed
kill -TERM $CAP_PID
```

Lower priority than §2.1 (which is the real blocker).

### 4.4 FEATURE · `nlink-lab destroy --leak-on-failure`

**Severity**: NICE — would directly support the test harness's
"keep-alive on failure for debugging" pattern.

Today the harness implements this in user code: when a test fails,
the harness prints `HARNESS_KEEP_ALIVE: lab des-3m-XYZ preserved`
and skips the `destroy` call. Users then manually
`nlink-lab destroy des-3m-XYZ` after debugging.

**Suggested**: `nlink-lab` could have a "keep-on-failure" mode
where the deploy is annotated with a TTL or an external "kill
switch" file. Daemon (or `cron`-style sweeper) reaps stale labs.
This is broader than what the harness needs — file as discussion,
not action.

### 4.5 FEATURE · `nlink-lab inspect --json` returns capture / spawn metadata

**Severity**: NICE.

`nlink-lab inspect LAB` exists; we haven't probed its `--json` shape.
A scenario that survived a partial failure (e.g., 3 of 4 spawns
succeeded) would benefit from being able to query "what's currently
spawned in this lab" and "what captures are running" in one call.

Today: combine `nlink-lab ps --json` + manual capture tracking in
the consumer. Could be one call.

---

## 5. Documentation gaps

### 5.1 `--json` schema for every subcommand

The `--json` flag is widely supported but the output schema is
documented nowhere we could find. We discovered field names by
running each command and parsing the output. A short "JSON output
schema" appendix in the help text (or a man page) would save
consumers a probe round each.

Specifically observed:

| Command | Observed JSON shape |
|---|---|
| `status --json` | `[{name, node_count, created_at}, ...]` |
| `deploy --json` | `{lab, status, ...}` (we parse `lab` field) |
| `spawn --json` | `{command, node, pid}` |
| `ps --json` | `[{node, pid, alive, stdout_log, stderr_log}, ...]` |

Suggested: add a top-level note to the JSON flag description, or a
new `nlink-lab help --json-schema` command that emits all schemas at
once. A JSONSchema document committed to the repo would be ideal.

### 5.2 Per-process log path convention

The harness reads
`~/.local/state/nlink-lab/labs/{lab}/logs/{node}-{cmd_basename}-{pid}.{stdout,stderr}`
directly. This path scheme isn't in any help text. Suggested: a doc
note (in `nlink-lab spawn --help`) like:

```
LOGS:
  stdout/stderr are captured to per-process files at
  ~/.local/state/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}
  Use `nlink-lab logs <lab> --pid <pid>` to read them, or read the
  files directly. The path is stable and consumers can rely on it.
```

### 5.3 Invariants around lab name uniqueness

`--unique` appends a PID-derived suffix; the consumer needs to
discover the chosen lab name to destroy it later. Today we use
`status --json` after deploy to find the most-recently-created lab
matching our prefix. A clean way would be: `deploy --unique --json`
returns the chosen lab name in a stable field. We *think* it does —
but couldn't confirm because §5.1 leaves the schema undocumented.
If it does, please document; if it doesn't, please add it.

---

## 6. Things that work very well

For balance — these are the parts of nlink-lab the harness leans on
heavily and that have caused us zero trouble:

- **`deploy --unique`** — perfect for parallel test isolation. The
  PID suffix means concurrent test runs never collide on lab names.
- **Stable per-process log file paths** — even with §3.1's
  surprises, the path scheme is reliable enough that the harness's
  artefact bundler can mine logs without help from nlink-lab.
- **`spawn --workdir`** — solved an important problem cleanly (the
  C++ Mediator only finds its config via cwd).
- **`spawn --wait-tcp`** — fantastic. Removed several `sleep 2`s
  from the harness.
- **`exec` and `spawn`'s separation** — different lifetimes for
  foreground vs background, both with sane defaults.
- **`impair`'s knob set** — `--partition`, `--loss`, `--delay`,
  `--rate`, plus per-direction (`--in-*`, `--out-*`). Only `--clear`
  to undo. Idiomatic and matches what netem provides.
- **`destroy --force`** — survives partial state from a crashed test
  cleanly. Important for harness reliability.
- **`destroy --orphans`** — used for periodic cleanup; works.

---

## 7. Suggested CLI / API additions in priority order

Re-ranked by combined impact / effort:

| # | Item | § | Effort | Impact |
|---|---|---|---|---|
| 1 | Fix `capture` flush-on-signal | 2.1 | M | HIGH (blocks documented use) |
| 2 | Use `execve` env directly instead of wrapping `env` | 3.1 | S | MEDIUM (silent breakage) |
| 3 | Document the `alive` field in `ps --json` output | 3.2 | S | MEDIUM (hard-to-find footgun) |
| 4 | Document per-subcommand `--json` schemas | 5.1 | M | MEDIUM |
| 5 | `nlink-lab logs --pid --follow` for spawned processes | 4.1 | M | MEDIUM |
| 6 | `nlink-lab spawn --wait-log <regex>` | 4.2 | M | MEDIUM |
| 7 | Document the per-process log path convention | 5.2 | S | LOW |
| 8 | Document the `deploy --unique --json` return shape | 5.3 | S | LOW |
| 9 | `nlink-lab spawn --capture-link` integrated capture | 4.3 | L | LOW (after #1) |
| 10 | `nlink-lab destroy --leak-on-failure` (discussion) | 4.4 | L | LOW |

Items 1, 2, 3, 4 unlock the harness to shed three workarounds and
make its documentation cleaner. Everything else is incremental.

---

## 8. Notes on contributing fixes

If the receiving LLM is being asked to *implement* these in the
nlink-lab tree, here are some starting hints based on how the
external behaviour is shaped:

- **§2.1 (capture flush)**: the bug is most likely in whatever
  netring/pcap-writer calls `nlink-lab capture` makes. Look for
  buffered writes on the pcap file handle — switching to per-packet
  flush is the most robust fix; install a SIGTERM/SIGINT handler as a
  belt-and-braces measure.

- **§3.1 (`--env` wrapping)**: search for
  `Command::new("env")` or similar in the spawn implementation.
  Replacement should iterate the user's env-vars and call
  `cmd.env(k, v)` on the `Command` for the user's binary directly.

- **§3.2 (`alive` field doc)**: add to the clap derive doc-comment
  on the `--json` flag of `ps` (or to the `ps` subcommand's about
  text).

- **§4.1 (`logs --follow` for spawned)**: extend the existing
  `nlink-lab logs --pid PID` path. Open the log file with `seek`
  to end + `inotify`-watch (or polling fallback) for new lines.

- **§4.2 (`--wait-log <regex>`)**: similar to `--wait-tcp` but
  scans the log file. Probably wants a small state machine: read
  pre-existing content (in case the line was already emitted) +
  follow new lines until match.

We're happy to test any of these against the harness in this repo —
its `tests/smoke.rs` and `tests/reference_scenario.rs` already cover
deploy, spawn, kill, capture, impair, destroy in fairly demanding
combinations. Run with:

```
cargo nextest run -p des-test-harness --run-ignored ignored-only
```

Each smoke test exits in 1–3 s; the reference scenario takes ~26 s.
A regression in `capture` or `spawn` would fire `h4_capture_link_*`
or `h2_spawn_zenohd_*` immediately.

---

*End of report. Drafted by the assistant during the harness-build
session of 2026-04-28; revise freely.*
