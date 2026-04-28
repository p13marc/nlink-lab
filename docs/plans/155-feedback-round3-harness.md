# Plan 155: Round-3 Feedback (`des-test-harness`)

**Date:** 2026-04-28
**Status:** Drafted; not yet started.
**Effort:** Medium — 1 P1 bug, 2 P2 surprises, 1 trivial polish, 4 features,
3 doc gaps. Splittable into 4 independent PRs.
**Priority:** P1 — `capture` flush-on-signal silently produces unusable
pcaps, blocking a documented use case.

---

## Source

- `nlink-lab-feedback.md` (third feedback report from the same harness
  team that submitted rounds 1 and 2 — all round-1 fixes verified, all
  round-2 fixes acknowledged as working).
- All claims independently verified against current `master` (commit
  `867a0bb`). File:line citations in this plan are accurate as of that
  commit.

## Decisions at a glance

| § | Item | Severity | Decision | PR |
|---|------|----------|----------|----|
| 2.1 | `capture` doesn't flush pcap on signal | HIGH | **Fix** | A |
| 3.1 | `--env` wraps with `env`, breaks logfile basename | MEDIUM | **Fix** | B |
| 3.2 | `ps --json` keeps exited entries forever | LOW–MED | **Fix (doc + opt-in flag)** | C |
| 3.3 | `destroy` non-quiet message on stdout | trivial | **Doc only** | D |
| 4.1 | `logs --pid --follow` "container only" | (already shipped) | **Doc fix only** | D |
| 4.2 | `spawn --wait-log <regex>` | NICE | **Implement** | E |
| 4.3 | `spawn --capture-link` integrated capture | NICE | **Defer** — depends on 2.1 + cross-cutting design | — |
| 4.4 | `destroy --leak-on-failure` / TTL | NICE | **Defer** — broader design discussion | — |
| 4.5 | `inspect --json` capture/spawn metadata | NICE | **Tentatively defer** — partially achievable today via `ps --json`; reassess after 4.3 | — |
| 5.1 | `--json` schema docs | docs | **Do** — partial coverage in PR D, full document as follow-up | D |
| 5.2 | Per-process log path doc | docs | **Do** | D |
| 5.3 | `deploy --unique --json` shape doc | docs | **Do — already returns `name`; doc only** | D |

**Verified-stale-claim note**: §4.1 (`logs --pid --follow`) was implemented
in round 1 (commit `118eb5a`). The reporter's belief that it's
"container only" comes from the stale doc-comment at
`bins/lab/src/main.rs:554-555` (`/// Stream logs (tail -f style, container only).`).
PR D fixes the doc-comment.

---

## PR A — Fix `capture` flush on signal (§2.1, P1)

### Diagnosis

Verified against current code:

- **Buffered writer**: `crates/nlink-lab/src/capture.rs:25-46` —
  `PcapWriter` wraps the file in `BufWriter<W>`. Per-packet writes at
  `capture.rs:215-216` go into the buffer; explicit `flush()` is only
  called once, at `capture.rs:230-232`, *after* the loop's natural exit.
- **Signal handler**: `bins/lab/src/main.rs:1709-1719` registers a
  handler for `SIGINT` only that flips a `static AtomicBool`. The
  capture loop checks the flag at `capture.rs:201-203`, breaks out, and
  then reaches the flush. So **`Ctrl-C` actually works today.**
- **What breaks**: SIGTERM has no handler, so the process terminates
  abruptly without reaching the flush. SIGKILL is uncatchable, same
  outcome. Result: 0-byte pcap.
- **Why the user thinks SIGINT also fails**: their reproducer at
  feedback §2.1 uses `timeout 3 nlink-lab capture …`. `timeout(1)`
  delivers **SIGTERM** by default, not SIGINT. So their report's
  "Same result with SIGINT" claim is consistent with our reading only
  if they tested SIGINT separately; the headline reproducer is
  actually a SIGTERM case.

### Fix

Two layers, both inexpensive:

**Layer 1 — flush per packet (matches `tcpdump -U`).**

Change `PcapWriter::write_packet` to flush the underlying writer after
each packet. Replace `BufWriter<W>` with the raw `W` (or keep
`BufWriter` but call `self.writer.flush()` at the end of
`write_packet`). The pcap writer is not in a hot path that benefits
from buffering — it captures events at human packet rates, not at
ring-buffer rates — so the latency cost is negligible.

Recommended approach: drop `BufWriter` entirely. Direct writes to the
underlying `W` are fine for this volume; eliminates the SIGTERM
flush-loss mode by design. If profiling later shows write syscall
overhead dominating, reintroduce a `BufWriter` *and* an explicit
flush-per-packet — same correctness, slightly fewer syscalls.

**Layer 2 — handle SIGTERM as well as SIGINT.**

`bins/lab/src/main.rs:1712-1719` — register SIGTERM with the same
handler:

```rust
unsafe {
    let h = handler as *const () as libc::sighandler_t;
    libc::signal(libc::SIGINT, h);
    libc::signal(libc::SIGTERM, h);
}
```

Even though Layer 1 makes Layer 2 redundant for correctness, the
SIGTERM handler also gives the capture loop a chance to print the
"X packets captured" summary line and exit 0 instead of being killed
mid-write. UX win for ~3 lines of code.

**Layer 3 (not implemented — note for later)**: SIGKILL is uncatchable.
Layer 1 (drop the buffer) is what guarantees data on SIGKILL too,
because once `write(2)` returns the kernel owns the bytes. Document in
the help text that "captures terminate with `Ctrl-C` (SIGINT) or any
catchable signal; SIGKILL leaves a complete pcap thanks to per-packet
writes."

### Tests

- **Unit test** in `crates/nlink-lab/src/capture.rs::tests`: synthetic
  `Cursor<Vec<u8>>` wrapped as `W`. Write the global header + a single
  packet via `write_packet`, then **without** calling `flush()`,
  inspect the underlying buffer — it must already contain the packet
  bytes. Covers the unbuffered-by-design property.
- **Integration test** (root-only, gated): deploy `examples/simple.nll`,
  start `nlink-lab capture` in a subprocess, send some traffic, send
  SIGTERM, wait, assert the pcap is non-empty and parses (use the
  `pcap-parser` crate or a manual header read — minimum: file size > 0
  and pcap magic bytes intact).
- **CLI smoke test** matching the reporter's acceptance criteria from
  feedback §2.1 verbatim. Goes in `bins/lab/tests/` if we have CLI
  tests there, otherwise as a shell script in `tests/scripts/`.

### Files touched

- `crates/nlink-lab/src/capture.rs` — drop `BufWriter`, add unit test.
- `bins/lab/src/main.rs:1712-1719` — register SIGTERM.
- `bins/lab/src/main.rs:553-555` — document signal behaviour in `Logs`
  (no, that's logs — for `Capture`, find the corresponding doc-comment
  on the `Capture` variant ~line 380s).

### Risks

- Removing `BufWriter` is a measurable syscall increase. Capture in
  this tool is intended for debugging, not high-throughput line-rate
  capture, so this is fine. If a future user files "capture drops at
  10 Gbps" we revisit.

---

## PR B — `--env` plumbing direct, not `env` wrapper (§3.1, P2)

### Diagnosis

Verified at `bins/lab/src/main.rs:988-995` (`Exec`) and
`bins/lab/src/main.rs:1063-1071` (`Spawn`):

```rust
let cmd = if env_vars.is_empty() {
    cmd
} else {
    let mut full = vec!["env".to_string()];
    full.extend(env_vars);
    full.extend(cmd);
    full
};
```

The CLI literally prepends `/usr/bin/env K=V K=V cmd args`. Then:

- `RunningLab::spawn_with_logs_in` derives `cmd_basename` from `cmd[0]`
  at `crates/nlink-lab/src/running.rs:328-331`. With the wrapper, that's
  `"env"`, not the user's binary. Hence
  `{node}-env-{pid}.{stdout,stderr}` filenames.
- For `exec`, the wrapper has no observable side effect (no log files);
  but harmonising both paths reduces special cases.

### Fix

Plumb env vars through the library API instead of wrapping at the CLI.

**Step 1 — library API.** In `crates/nlink-lab/src/running.rs`:

- Add a fourth `_in` variant signature (or extend the existing ones —
  see decision below):
  ```rust
  pub fn exec_in(
      &self, node: &str, cmd: &str, args: &[&str],
      workdir: Option<&Path>,
      env: &[(&str, &str)],   // <-- new
  ) -> Result<ExecOutput>
  ```
  Same for `exec_attached_in` and `spawn_with_logs_in`.

- **API decision**: append `env` rather than introduce yet another
  `_in_with_env` family. The current `exec` / `exec_in` pair was added
  in round 2 (commit `867a0bb`) precisely to avoid breaking the
  ~50 call sites of `exec()`. The pattern repeats: keep `exec`,
  `exec_in` thin wrappers that delegate to a single new
  `exec_with_opts(node, cmd, args, opts: ExecOpts)` where `ExecOpts`
  is a small POD with `workdir` and `env` (and a place for future
  options). This consolidates the wrapper sprawl.

  Rough shape:
  ```rust
  #[derive(Default, Debug)]
  pub struct ExecOpts<'a> {
      pub workdir: Option<&'a Path>,
      pub env: &'a [(&'a str, &'a str)],
  }
  pub fn exec_with_opts(
      &self, node: &str, cmd: &str, args: &[&str], opts: ExecOpts<'_>
  ) -> Result<ExecOutput>;
  // Existing methods become wrappers.
  ```

- For container nodes, env passes through as repeated `-e K=V` args to
  `docker exec` / `podman exec` — same place we just added `-w`.
  See `running.rs:198-211` for the docker-exec pattern.
- For namespace nodes, set on the `std::process::Command` via
  `command.env(k, v)` before `spawn_*_with_etc`. The pre_exec dance
  in nlink doesn't override the env from the parent `Command`, so
  this works as expected.

**Step 2 — CLI.** Drop the `env`-wrapping branch entirely in both
match arms. Parse `env_vars: Vec<String>` (each `K=V`) into pairs:

```rust
fn parse_env_pairs(env_vars: &[String]) -> Result<Vec<(String, String)>> {
    env_vars.iter().map(|s| {
        s.split_once('=').ok_or_else(||
            Error::invalid_topology(format!("invalid --env: {s:?} (expected K=V)"))
        ).map(|(k, v)| (k.to_string(), v.to_string()))
    }).collect()
}
```

Pass the resulting `&[(&str, &str)]` through to the library via
`ExecOpts::env`.

**Step 3 — tests.** Update the integration test for the existing
`exec_in_respects_workdir` to also verify env propagation, and add a
new test that the spawn log file basename matches `argv[0]`'s basename
even when env vars are set:

```rust
#[lab_test("examples/simple.nll")]
async fn spawn_logfile_basename_unaffected_by_env(lab: RunningLab) {
    // Spawn with --env, assert log path contains "sleep" not "env".
    let env: &[(&str, &str)] = &[("FOO", "bar")];
    let pid = lab.spawn_with_logs_with_opts("host", &["sleep", "0.1"],
        SpawnOpts { env, ..Default::default() })?;
    let (out, _) = lab.log_paths(pid).unwrap();
    assert!(out.contains("host-sleep-"),
        "expected 'sleep' in log path, got: {out}");
}
```

### Backwards compatibility

The `exec_with_opts` consolidation is additive — old methods stay as
wrappers, no caller break. Internally, the `spawn_with_logs_in` body
moves into a new `spawn_with_logs_with_opts`; existing `_in` is a
thin wrapper.

### Files touched

- `crates/nlink-lab/src/running.rs` — add `ExecOpts`, `SpawnOpts`,
  consolidate body of `exec_in` / `exec_attached_in` /
  `spawn_with_logs_in` into `_with_opts` versions; existing `_in` and
  zero-opt wrappers delegate.
- `bins/lab/src/main.rs` — replace the `env`-wrapping branch in `Exec`
  (~lines 987-995) and `Spawn` (~lines 1063-1071) with `parse_env_pairs`
  + `ExecOpts::env` plumbing.
- `crates/nlink-lab/tests/integration.rs` — env propagation test +
  log-basename test.

### Risks

- `Command::env(k, v)` does **not** clear the parent's environment by
  default. The `env -i K=V cmd` form (which the wrapper *does* support
  via `env`'s flags if a user wrote `--env -i`, though it'd be a
  surprise) is silently disabled by this fix. Doc-comment on `--env`
  should clarify that envs are *added* on top of the inherited
  environment; users who want a hermetic env need to use a wrapper
  themselves. (Today's behaviour is: any `env`-style flag in `--env`
  passes through. Specifically: `--env -i FOO=bar` would clear envs and
  set only FOO, because `env -i FOO=bar` is what gets exec'd. After
  this PR, that side channel disappears. This is the right call —
  `-i` was never a contract — but mention in CHANGELOG.)

---

## PR C — `ps --alive-only` + doc the `alive` field (§3.2, P2)

### Diagnosis

Verified at `crates/nlink-lab/src/running.rs:908-923`:

```rust
pub fn process_status(&self) -> Vec<ProcessInfo> {
    self.pids
        .iter()
        .map(|(node, pid)| {
            let alive = unsafe { libc::kill(*pid as i32, 0) } == 0;
            ...
        }).collect()
}
```

`self.pids: Vec<(String, u32)>` is never pruned (`running.rs:28`), so
exited PIDs remain forever, with `alive: false`.

CLI side at `bins/lab/src/main.rs:1600-1615`: emits all entries.
Subcommand has no doc-comment beyond `/// List processes running in a
lab.` (`bins/lab/src/main.rs:353`), and `--json` for `ps` is implicit
(via the global flag) and undocumented at the subcommand level.

### Fix

Two parts, neither contentious.

**Part 1 — Add `--alive-only` filter.**

`bins/lab/src/main.rs:354-357`:

```rust
/// List processes (alive and exited) tracked by `spawn` for a lab.
///
/// Exited processes remain in the listing with `alive: false` so that
/// post-mortem inspection (which log files? when did they exit?) is
/// possible. Use `--alive-only` to filter them out.
Ps {
    /// Lab name.
    lab: String,

    /// Hide processes whose tracked PID has exited (alive == false).
    #[arg(long)]
    alive_only: bool,
},
```

Match arm: filter `procs` after `process_status()` based on
`alive_only`. Trivial.

For the library API, add a sibling helper:

```rust
pub fn process_status_alive_only(&self) -> Vec<ProcessInfo> {
    self.process_status().into_iter().filter(|p| p.alive).collect()
}
```

(Or just let consumers filter — but the named helper makes intent
clear and is two lines.)

**Part 2 — Document `alive`.**

- Doc-comment on the `Ps` clap variant (above) explains the retention
  semantics.
- Doc-comment on `ProcessInfo` in `running.rs:50-65` already says
  `/// Whether the process is still alive.` — extend to also note
  retention:
  ```rust
  /// Whether the process is still alive (`kill(pid, 0)` returns 0).
  ///
  /// Note: tracked processes are *not* removed from the list when they
  /// exit. They remain with `alive == false` until the lab is destroyed
  /// or `state.json` is cleaned manually. Consumers polling for "is X
  /// still running?" must check this field, not just look up the PID.
  ```

### Tests

- Unit test in `bins/lab/src/main.rs::tests` is hard because filtering
  is a one-liner — but a small CLI-shape test that invokes
  `nlink-lab ps --help` and asserts `--alive-only` is in the output is
  worthwhile (catches accidental removal). Use `assert_cmd` if it's a
  dev-dep already, otherwise a small `Command::new(env!("CARGO_BIN_…"))`.
- Integration test (root-only): spawn a `sleep 0.05`, wait 0.5s,
  call `process_status_alive_only` — assert the entry is filtered out.

### Files touched

- `bins/lab/src/main.rs:354-357` — add `--alive-only` flag + doc.
- `bins/lab/src/main.rs:1600-1615` — apply filter in match arm.
- `crates/nlink-lab/src/running.rs:50-65` — extend `ProcessInfo`
  doc-comment.
- `crates/nlink-lab/src/running.rs:908-923` — add
  `process_status_alive_only`.
- `crates/nlink-lab/tests/integration.rs` — exit-detection test.

### Risks

None. `--alive-only` is purely additive; default behaviour unchanged.

---

## PR D — Documentation sweep (§3.3, §4.1 stale-doc, §5.1, §5.2, §5.3)

### Items

**§4.1 — Fix the stale doc-comment for `logs --follow`.**

`bins/lab/src/main.rs:553-556`:

```rust
/// Stream logs (tail -f style, container only).
#[arg(long)]
follow: bool,
```

Currently incorrect — round 1 (commit `118eb5a`) added `tail -F`
support for `--pid`. Replace with:

```rust
/// Stream logs in tail -F style — works for container nodes (via the
/// runtime) and for tracked background processes (via `--pid`).
/// Re-opens the file on rotation/truncation. Stops on Ctrl-C.
#[arg(long)]
follow: bool,
```

**§3.3 — `--quiet` recommendation in `destroy` help.**

`destroy` uses the global `--quiet` flag. The reporter's request is a
small wording add: "recommended for scripted use." Either add this to
the global `--quiet` doc-comment in the `Cli` struct, or to the
`destroy` `about` text. Prefer the global flag — applies to all
subcommands.

Find the global `quiet` flag definition (presumably on `struct Cli`
near the top of `main.rs`) and append to its doc-comment:

```rust
/// Suppress informational output (errors still go to stderr).
///
/// Recommended for scripted/automated use; the default human-readable
/// output is intended for interactive shells.
#[arg(short, long, global = true)]
quiet: bool,
```

**§5.2 — Per-process log path convention.**

In the `Spawn` clap variant doc-comment (currently `/// Spawn a
background process in a lab node.`, `bins/lab/src/main.rs:159`),
add a path note:

```rust
/// Spawn a background process in a lab node.
///
/// Stdout/stderr are captured to per-process log files at:
///   $XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}
/// (default $XDG_STATE_HOME = ~/.local/state). The path is stable —
/// consumers can read it directly, or use `nlink-lab logs <lab> --pid
/// <pid>`.
Spawn { ... }
```

Same note also worth adding to `nlink-lab logs --help` (specifically
the `--pid` flag's doc-comment).

**§5.3 — `deploy --json` returns `name` field.**

Verified: `bins/lab/src/main.rs:783-792` emits
`{"name": ..., "nodes": ..., "links": ..., "deploy_time_ms": ...}`. So
`--unique --json` already returns the chosen lab name in `.name`. Only
documentation needed: add a `JSON OUTPUT:` block to the `Deploy`
clap variant doc-comment listing the schema. Mirror to `spawn`,
`status`, `ps`, `inspect` while we're here.

**§5.1 — `--json` schema docs (full table).**

Two surfaces:

1. **Per-subcommand JSON OUTPUT block** in clap doc-comments for every
   subcommand that respects `--json`. This is what shows up in
   `--help`. Format:
   ```
   JSON OUTPUT (with --json):
       { "name": str, "nodes": int, "links": int, "deploy_time_ms": int }
   ```

2. **JSON Schema files** under `docs/json-schemas/`. One `.json` file
   per command (e.g. `deploy.schema.json`, `ps.schema.json`). Use
   draft-07 minimal — only the fields we contractually emit. The
   `schemars` crate is already a candidate; alternatively hand-write
   schemas (more honest but more work). Recommendation: hand-write
   four for the high-traffic ones (`deploy`, `status`, `spawn`, `ps`)
   in this PR; defer the rest.

   Add a top-level reference in `README.md` and in `CLAUDE.md` →
   "JSON output schemas live in `docs/json-schemas/`."

### Tests

- Snapshot test for clap `--help` output (`assert_cmd_apply` /
  `insta`) on the changed subcommands, so future doc-comment
  regressions are caught. If we don't already use `insta`, skip the
  snapshot test for now; doc accuracy is verified manually.
- For the JSON schema files, a small unit test that parses the
  schemas at compile time (`include_str!(...)` + `serde_json::from_str`
  → `Value`) so a malformed JSON Schema fails CI.

### Files touched

- `bins/lab/src/main.rs:553-556` — `--follow` doc-comment.
- `bins/lab/src/main.rs:159` — `Spawn` doc-comment with log path.
- `bins/lab/src/main.rs:138-150` (and similar) — Add `JSON OUTPUT`
  blocks to `deploy`, `status`, `spawn`, `ps`, `inspect`.
- `bins/lab/src/main.rs:<global Cli>` — `--quiet` doc.
- `docs/json-schemas/deploy.schema.json`, `status.schema.json`,
  `spawn.schema.json`, `ps.schema.json` — new files.
- `README.md` (or `CLAUDE.md`) — link to JSON schemas dir.

### Risks

None. Pure docs.

---

## PR E — `spawn --wait-log <regex>` (§4.2, NICE)

### Design

Mirror of `--wait-tcp`. The current `--wait-tcp` polls inside the
namespace via the same TCP-probe mechanism that `wait_for_tcp`
exposes. `--wait-log` watches the per-process log file the spawn
already creates, so no namespace-entry needed.

CLI:

```
--wait-log <REGEX>          Wait for a stdout/stderr line matching REGEX
                            before returning from spawn.
--wait-log-stream <S>       Stream to monitor: stdout | stderr | both.
                            [default: both]
```

Compose with `--wait-timeout` (already exists for `--wait-tcp`).

### Implementation

**Library**:

```rust
pub async fn wait_for_log_line(
    &self,
    pid: u32,
    pattern: &Regex,
    stream: LogStream,         // Stdout | Stderr | Both
    timeout: Duration,
    interval: Duration,
) -> Result<()>;
```

Tail-follow each watched log file from offset 0 (so a line emitted
*before* we started watching is matched — the spawn returned PID
fast, but the line could already be there by the time we set up the
watcher). Reuse the existing `tail_follow_to(path, start, out,
should_continue)` helper from `bins/lab/src/main.rs:2802` —
generalise it slightly to take a `for_each_line` callback instead of
writing to `W`, or just write into a `LineMatcher` that wraps
`Regex::is_match`.

A simpler MVP: open file, `read_to_string`, check; if no match,
poll metadata + read new bytes; check again; loop until match or
timeout. Skip `inotify` for now — the existing `tail_follow_to` polls
at 250ms and that's fine for spawn-readiness latency budgets.

**CLI** (`bins/lab/src/main.rs:1098+`, where `--wait-tcp` is handled
today):

```rust
if let Some(ref re) = wait_log {
    let pat = Regex::new(re).map_err(|e|
        nlink_lab::Error::invalid_topology(format!("invalid --wait-log regex: {e}")))?;
    running.wait_for_log_line(pid, &pat, wait_log_stream.into(), timeout, interval).await?;
    if !cli.quiet { eprintln!("ready"); }
}
```

Allow combining `--wait-tcp` and `--wait-log` (AND-semantics:
both must succeed) — useful for "wait for the listening socket *and*
'startup complete' line".

### Dependencies

- `regex` — already in the workspace (used by `validator.rs` and
  others). No new dep.

### Tests

- Unit test on the matcher: feed a stream of lines, assert match
  fires at the expected line.
- Integration test (root-only): spawn `bash -c 'echo readyish; sleep
  10'` with `--wait-log '^readyish$'`, assert spawn returns within
  ~500ms. Cancel.

### Files touched

- `crates/nlink-lab/src/running.rs` — `wait_for_log_line` +
  `LogStream` enum.
- `bins/lab/src/main.rs:175+` — add `wait_log`, `wait_log_stream`
  fields to `Spawn` clap variant.
- `bins/lab/src/main.rs:1098+` — wire into match arm; both
  `--wait-tcp` and `--wait-log` can be combined.
- `crates/nlink-lab/tests/integration.rs` — wait-log test.

### Risks

- If user supplies a regex that never matches (typo) and a long
  timeout, spawn blocks for the full timeout. Mitigation: clear error
  message including the regex source on timeout. No code change
  needed beyond a good `Error::deploy_failed("timeout waiting for log
  line matching '{re}' on PID {pid}")`.

---

## Deferred items (filed, not in this plan)

### §4.3 `spawn --capture-link <ENDPOINT>`

Genuinely useful — would let the harness drop its tcpdump-in-spawn
workaround once §2.1 is fixed. But the lifecycle binding (capture
exits when its tied spawn exits) requires either:

- Process group / `setpgid` + signal propagation, or
- A nlink-lab-side supervisor that watches the spawn's PID and
  signals the capture child on exit.

Either is a small engine. Cleaner to design it once we have user
demand for the integrated form (the workaround is not painful enough
to be P-anything). Re-file when 2 different consumers want it.

### §4.4 `destroy --leak-on-failure` / TTL labs

Cross-cutting. Touches deploy (TTL annotation), state schema (kill
switch file), daemon (sweeper). Belongs in its own design doc — file
as `docs/plans/152-lab-lifecycle-policies.md` if/when prioritised.
Not blocking the harness; their user-side keep-alive workaround is
stated to work fine.

### §4.5 `inspect --json` capture/spawn metadata

Today `nlink-lab status --json <lab>` emits topology + addresses
(see `main.rs:935-951`). `nlink-lab inspect` already exists — need to
verify what `--json` returns there. Likely the cleanest action is:

- Make `inspect --json` superset of `status --json <lab>` + `ps
  --json <lab>` + a new `captures` array (which is empty until §4.3
  lands or until we surface running captures explicitly).

Touches state schema (need to track running captures). Defer until
§4.3 lands so they can share the capture-tracking surface.

---

## Suggested commit / PR sequence

1. **PR A** — capture flush. Highest value; smallest surface; ships
   first so harness can drop their tcpdump-in-spawn workaround.
2. **PR D** — doc sweep. Cheap; corrects stale doc on `--follow`;
   eliminates §3.3 / §4.1 / §5.1 / §5.2 / §5.3 in one go.
3. **PR B** — `--env` plumbing. Refactors `RunningLab` API
   (introduces `ExecOpts` / `SpawnOpts`); land separately so it can be
   reverted if the consolidation surprises anyone.
4. **PR C** — `ps --alive-only`. Tiny; pair with PR D if convenient.
5. **PR E** — `spawn --wait-log`. Independent feature.

Each PR is independently reverable and individually verifiable.

---

## Risk summary

| PR | Risk | Mitigation |
|----|------|------------|
| A | Capture syscall rate increases (unbuffered) | Fine for current use; revisit if line-rate capture is requested. Document in capture help. |
| B | `--env -i` side-channel disappears | Document in CHANGELOG; was never contracted. |
| B | `RunningLab` API consolidation churn | Wrappers preserve all existing methods; no caller break. |
| C | None | — |
| D | None | — |
| E | Bad regex blocks for full timeout | Surface clear error on timeout. |

---

## Test surface added

| Test | PR | Type |
|------|----|------|
| `pcap_writer_unbuffered` (unit) | A | unit |
| `capture_sigterm_produces_pcap` (root-only) | A | integration |
| `spawn_logfile_basename_unaffected_by_env` (root-only) | B | integration |
| `exec_in_propagates_env` (root-only) | B | integration |
| `process_status_alive_only_filters_dead` (root-only) | C | integration |
| `wait_for_log_line_matches` (root-only) | E | integration |
| `wait_for_log_line_timeout` (root-only, fast) | E | integration |
| schema-parse compile-time check | D | unit |

---

## Acceptance criteria (from reporter, condensed)

- [ ] `nlink-lab capture -w f.pcap LAB ENDPOINT &; PID=$!; sleep 1;
      send-traffic; kill -TERM $PID; wait`. → `f.pcap` is non-empty
      and contains the captured packets. (PR A)
- [ ] `nlink-lab spawn --json --env FOO=bar LAB site_a /usr/bin/sleep 5
      | jq -r .pid` → log file is `…/site_a-sleep-<pid>.stdout` (not
      `site_a-env-<pid>.stdout`). (PR B)
- [ ] `nlink-lab ps --help` mentions the `alive` field and the
      retention semantics; `nlink-lab ps --alive-only --json LAB`
      filters dead entries. (PR C)
- [ ] `nlink-lab logs --help` for `--follow` no longer says "container
      only". (PR D)
- [ ] `nlink-lab spawn ... --wait-log '^READY$' -- prog` returns
      after `prog` prints `READY`. (PR E)
