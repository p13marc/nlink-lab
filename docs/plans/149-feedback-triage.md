# Plan 149: External Feedback Triage + nlink 0.13.0 Upgrade

**Date:** 2026-04-20
**Status:** Implemented 2026-04-21 — all confirmed fixes + nlink 0.13.0 upgrade landed
**Effort:** Medium (2–3 days if done as one batch; or shippable as 3 independent PRs)
**Priority:** P1 — two high-severity correctness bugs block common workflows

## Status — Landed

| # | Fix | Commit | Tests |
|---|-----|--------|-------|
| 1 | shell nsenter `--net=<path>` | df44ac9 | unit: `nsenter_shell_args_uses_equals_form` |
| 2 | Bridge peer-name hash (`np{hash8}{idx}`) | df44ac9 | unit × 4 in `types::name_hash_tests`; integration: `deploy_networks_with_shared_prefix` |
| 3 | Veth EEXIST names the mgmt peer | df44ac9 | covered by #2 integration |
| 4 | `destroy --orphans` | 6510369 | unit × 4 on `classify_orphans` |
| 5 | `status --scan` | 6510369 | same classifier |
| 6 | Streaming `exec` | 479bfc4 | integration: `exec_attached_forwards_exit_code` |
| 7 | `logs --pid --follow` | 118eb5a | unit × 2 on `tail_follow_to` |
| – | nlink 0.13.0 upgrade | a065243 | existing tests |

Follow-ups noted but deferred (see below): bridge-name truncation
collision (related to #2, not in scope for this plan); features #9–13.

---

## Context

External team submitted `nlink-lab-feedback.md` after a single session bringing
up a 3-node topology with two isolated LANs. Report flags 8 bugs/UX issues and
5 feature/doc suggestions. Independently, `nlink` 0.13.0 is out (we pin
`0.12.2`) with typed rate/percent wrappers that affect a couple of call sites.

All claims were verified against the current code. Line numbers in the
feedback report are accurate except for claim 3 (error message has drifted —
see below).

## Decisions

- **Do in this plan:** every confirmed bug + orphan cleanup + streaming exec.
  These are small, high-leverage, and the feedback is accurate.
- **Do in this plan but separately landable:** nlink 0.13.0 upgrade. Purely
  mechanical, unrelated to the feedback, worth bundling so we don't carry a
  stale dep.
- **Defer / out of scope:** features #9 (`attach`), #10 (tmux helper), #11
  (deploy telemetry). Useful but larger design work. File as follow-ups if
  agreed.
- **Skip:** claim 8 (`check_root` SUID). Verified not broken — `geteuid()` is
  the right call and the fallback reads `CapEff`. No action.

---

## Confirmed Bugs — Fixes

### Fix 1 — `shell` nsenter argv (HIGH)

**Location:** `bins/lab/src/main.rs:1383-1389`
**Current:**
```rust
.args(["--net", &ns_path, "--", &shell])
```
**Problem:** `nsenter` parses bare `--net` as "enter target's netns" and
expects a target (PID/file) from another flag. `ns_path` is then treated as
the command to run, so it errors immediately with `neither filename nor
target pid supplied for ns/net`. The `shell` branch is broken for every
namespace node (the container branch above is fine — it uses `docker exec`).

**Fix:** pass `--net=<path>` as a single argv entry.
```rust
.args([&format!("--net={ns_path}"), "--", &shell])
```

**Alternative considered:** use `ip netns exec`. Rejected — nsenter is more
direct and the fix is a one-char change.

**Tests:**
- New integration test in `crates/nlink-lab/tests/` (or a new
  `tests/cli_shell.rs`) that deploys a trivial lab, spawns
  `nlink-lab shell <lab> <node> -c "echo ok"`-style non-interactive command,
  and asserts exit 0 + expected stdout. Requires root, so gate on
  `#[cfg(feature = "integration")]` like existing integration tests.

### Fix 2 — Peer-name collision on 4-char prefix (HIGH)

**Location:** `crates/nlink-lab/src/deploy.rs:410-415`
**Current:**
```rust
let peer_name = format!("br{}p{}", net_name.chars().take(4).collect::<String>(), k);
```
**Problem:** Truncating `net_name` to 4 chars collapses `lan_a`/`lan_b` (and
every similar pair) to `brlan_p0`. The second veth `add_link` hits EEXIST in
the mgmt ns. The same hashing approach already exists for lab-level mgmt
names at `crates/nlink-lab/src/types.rs:293` (`mgmt_peer_name`:
`nm{hash8}{idx}`).

**Fix:**

1. Add `Network::peer_name(lab_name, net_name, idx)` in `types.rs` — returns
   `np{hash8}{idx}`, where `hash8` is DJB2 of `net_name` via the existing
   `name_hash_str`. Fits 15 chars for idx up to 99999. Deterministic per
   `(lab, network, index)`.
2. Replace the `format!` at `deploy.rs:410` with the new helper.
3. Mirror this for `force_cleanup` so orphaned per-network peers can be
   reaped — currently `force_cleanup` only reaps mgmt peers (`nm…`) and the
   mgmt bridge (`nl…`). Add a sweep for `np…` prefixes in
   `bins/lab/src/main.rs:2541-2563`.

**Validator rule (optional, low cost):** add a rule in `validator.rs` that
rejects topologies where two networks' generated peer-name prefixes collide
— defence in depth against a future refactor that re-introduces truncation.
Since the fix uses a hash, real-world collision probability is negligible,
so this is mostly symbolic; skip unless trivial.

**Separate related concern (noted during implementation):** the bridge
name itself (`deploy.rs:367`, `"{prefix}-{net_name}"` truncated to 15
chars) has the same class of collision risk. With a long lab prefix
(≥11 chars) two networks sharing a prefix collapse to the same bridge
name. Not hit by the reporter's short `des-3m` lab. Mitigation for a
follow-up: apply the same `network_peer_name_for`-style hash
(`nb{hash8}` or similar) and migrate `force_cleanup`/`status --scan`
accordingly. Deferred from this plan to keep the fix focused and the
bridge-name scheme debuggable from `ip link` output.

**Tests:**
- `test_network_peer_name_unique_across_networks` — builds a `Topology` with
  networks `lan_a`/`lan_b`/`lan_c` (same 4-char prefix), asserts all peer
  names unique.
- Integration test deploying such a topology and asserting success (was the
  exact failure in the report).

### Fix 3 — Veth EEXIST error misattribution (MEDIUM)

**Location:** `crates/nlink-lab/src/deploy.rs:421-428`
**Current:**
```rust
Error::deploy_failed(format!(
    "failed to create veth for network '{net_name}' member '{member}': {e}"
))
```
**Note:** The report cites `ep.iface` being in the message; current code
uses `member` (full `node:iface`). The underlying gripe still stands: when
EEXIST comes from the auto-generated peer name, the message never names
`peer_name`.

**Fix:** on `add_link` error, probe both names and include in the message.
```rust
.map_err(|e| {
    // On EEXIST, probe which name actually collided
    let detail = format!(
        "(member iface '{}' in '{}', peer iface '{}' in mgmt ns)",
        ep.iface, ep.node, peer_name
    );
    Error::deploy_failed(format!(
        "failed to create veth for network '{net_name}' member '{member}' {detail}: {e}"
    ))
})
```
Optionally, when `e` is EEXIST, try `namespace::link_exists(ns, name)` (or a
shell-fallback `ip link show dev X`) on each candidate and append which side
collided. This is more code for a diagnostic path — include it only if
straightforward via existing nlink APIs; otherwise ship just the name hint.

**Tests:** none strictly required — the fix is user-visible only in error
paths and is covered indirectly by Fix 2 tests.

### Fix 4 — `destroy --all --force` misses orphans (MEDIUM)

**Location:** `bins/lab/src/main.rs:800-822`
**Problem:** `--all` walks `RunningLab::list()` which reads only
`~/.nlink-lab/*/state.json`. If deploy crashed before writing state (e.g.
because of Fix 2's EEXIST), mgmt bridges and veths leak silently and
`destroy --all` reports "No running labs."

**Fix:** after the state-driven loop, also scan the host for lab-owned
resources with no state file:

1. Enumerate interfaces matching `^nl[0-9a-f]{8}$` (mgmt bridges) via
   `ip -o link show` or an nlink `get_links()` filter.
2. For each such bridge, reverse the hash is not possible, so treat the
   bridge name itself as the cleanup key. Drive `force_cleanup` by bridge
   name rather than lab name: already-parameterised as
   `mgmt_bridge_name_for(lab)` → just take the bridge name directly.
3. Also sweep `nm{hash8}*` veth peers and `np{hash8}*` (after Fix 2)
   network peers whose hash doesn't match a known-running lab.
4. Netns without state are already enumerable via `ip netns list` — filter
   for unknown prefixes by comparing to `RunningLab::list()` names.

**New subcommand recommended:** `nlink-lab destroy --orphans` (or
`nlink-lab cleanup`) — keeps the orphan-scan behaviour explicit rather than
overloading `--all --force`. Implementation would be:
```rust
Commands::Destroy { name: None, force: true, all: true, orphans: true } => {
    // Run normal --all --force first, then scan-and-reap orphans
}
```
The detection logic lives in one helper (`find_orphans()`) shared with
`status --scan` (Fix 5).

**Tests:** gated integration test that simulates a crashed deploy by
manually creating a `nl{hash}` bridge + `nm{hash}0` veth with no state
file, runs `destroy --orphans`, asserts both are gone.

### Fix 5 — `status` silent about orphans (LOW)

**Location:** `bins/lab/src/main.rs:858-874`
**Fix:** add `--scan` flag. When set, runs the Fix 4 orphan detection and
reports `{bridges, veths, netns}` that look like lab state but have no
matching `state.json`. Without the flag, behaviour is unchanged.

When `--scan` finds nothing, silent. When it finds orphans, print a
`Orphans detected:` block after the normal table and suggest
`nlink-lab destroy --orphans`.

Zero-orphan fast path should be genuinely fast (one `ip -o link show` and
one `ip netns list` — already cheap).

### Fix 6 — `exec` buffers stdio (MEDIUM)

**Location:** `bins/lab/src/main.rs:977-990`, `running.rs:201-216`
**Problem:** Non-JSON path calls `running.exec()` which runs to completion,
captures stdout/stderr, then prints. Unusable for live-output commands
(`ping`, `tail -f`, services).

**Fix:** add a streaming path. Two options:

**Option A (preferred):** new `RunningLab::exec_attached(node, cmd, args)`
that spawns with `Stdio::inherit()` for all three streams and returns the
exit code. CLI uses this on the non-JSON path; JSON path still uses the
buffered `exec()`.

**Option B:** add an `--attach` flag and keep current behaviour default.
Safer (no behavioural change for scripts that rely on captured output via
stdout piping), but only if anything currently relies on that.

Claim: nothing currently relies on `exec` buffering the whole output
because callers can pipe directly. Recommend **Option A** — default to
streaming, which is the Unix-shell expectation. Scripts that want a
structured capture use `--json`.

**Note on implementation:** the namespace-side entry point today is
`namespace::spawn_output_with_etc` (buffering). For streaming we need an
nlink entry point that inherits stdio, or shell out via `nsenter
--net=/var/run/netns/{ns} -- cmd …` (the same one-shot pattern as Fix 1).
The `nsenter` shell-out is simpler and consistent with Fix 1; prefer that
for `exec_attached`.

**Tests:** run `exec <lab> <node> -- sh -c 'printf foo; sleep 1; printf bar'`
and verify the test harness sees `foo` before the 1s sleep elapses (tail
-F pattern). Non-trivial to assert from Rust without pty hoops — minimum
test is exit-code propagation; document the behaviour in the README.

### Fix 7 — `logs --follow --pid` ignored (LOW)

**Location:** `bins/lab/src/main.rs:1987-2016`
**Current:** `--pid` branch reads the log file once with `read_to_string`,
then returns. `follow` is dropped.

**Fix:** when `follow` is set on the `--pid` path, implement `tail -F`
semantics: read existing content, then loop on `File::seek(End)` + sleep
(or `notify` crate) printing new lines until interrupted. Handle file
rotation/truncation minimally — if `metadata().len()` drops below the last
read position, reopen. Honour `--tail` for the initial dump.

Keep logic in a small helper `tail_follow(path: &Path, tail: Option<usize>)`
so it's reusable for future log sources.

**Tests:** unit test the tail-follow helper against a temp file; the
combination with `--pid` exercises it indirectly.

---

## nlink 0.13.0 Upgrade (separate commit, same plan)

0.13.0 introduces typed `Rate`/`Percent` wrappers and removes the
string/float-based setter shape on `NetemConfig` / `RateLimiter`. No API
help for the feedback bugs but keeps the dep current and aligns with
nlink's direction of strongly-typed builders (matches `GUIDELINES.md` §1).

**Changes needed:**

| File | Current | After |
|------|---------|-------|
| `Cargo.toml` workspace dep | `nlink = "0.12.2"` | `nlink = "0.13.0"` |
| `crates/nlink-lab/src/deploy.rs:1236-1246` | `RateLimiter::egress(&str)` | `.egress(Rate::bits_per_sec(parse_rate_bps(s)?))` |
| `crates/nlink-lab/src/deploy.rs:2888-2905` (`build_netem`) | `.loss(f64)`, `.rate(u64)` | `.loss(Percent::new(...)?)`, `.rate(Rate::bits_per_sec(...))` |
| `crates/nlink-lab/src/builder.rs:868` | `.loss("0.1%")` | same wrapper |
| `crates/nlink-lab/src/helpers.rs:89,107` | `parse_percent → f64`, `parse_rate_bps → u64` | Keep internal signatures for now; construct typed wrappers at call sites. (Changing helper signatures would touch more call sites; defer if not needed.) |

No behaviour change. Single `cargo check` pass should surface any missed
sites. Commit independently so bisects on the feedback fixes don't touch
external dep.

---

## Out-of-Scope / Follow-up Candidates

Filed for later; not in this plan:

- **#9 `nlink-lab attach`** — mostly subsumed by Fix 6's streaming `exec`.
  A no-command `attach` = `shell`. Revisit once Fix 1/6 land and we see
  whether a separate verb adds anything.
- **#10 Multi-pane tmux helper** — nice, but UX-only and easy to prototype
  outside the codebase. Defer.
- **#11 Deploy progress telemetry** — legitimately useful but non-trivial.
  Propose a separate plan (`150-deploy-telemetry.md`) that adds structured
  step events (stdout JSON events under `--json`, human-readable progress
  under `--verbose`) and hooks orphan-cleanup into post-failure behaviour.
- **#13 Naming/length docs** — low-effort docs update. If Fix 2 lands, add
  a "Generated interface names" section to `docs/NLINK_LAB.md` documenting
  the `nl/nm/np` prefixes + hash scheme and the 15-char budget. Include in
  this plan only if the naming helper gets refactored.

---

## File Changes Summary

| File | Change | Fix |
|------|--------|-----|
| `bins/lab/src/main.rs:1384` | `--net=` single argv | #1 |
| `crates/nlink-lab/src/types.rs` (~293) | Add `Network::peer_name` helper | #2 |
| `crates/nlink-lab/src/deploy.rs:410` | Use new helper | #2 |
| `crates/nlink-lab/src/deploy.rs:424` | Error message includes peer name | #3 |
| `bins/lab/src/main.rs:2541` | Extend `force_cleanup` to sweep `np…` | #2, #4 |
| `bins/lab/src/main.rs:800-822` | `--all` scans for orphans; new `--orphans` flag | #4 |
| `bins/lab/src/main.rs:858-874` | `status --scan` | #5 |
| `crates/nlink-lab/src/running.rs` | Add `exec_attached` | #6 |
| `bins/lab/src/main.rs:977-994` | Use `exec_attached` on non-JSON path | #6 |
| `bins/lab/src/main.rs:1998-2016` | Tail-follow for `--pid --follow` | #7 |
| `Cargo.toml` | nlink `0.13.0` | upgrade |
| `crates/nlink-lab/src/deploy.rs:1236,2888` | Typed `Rate`/`Percent` wrappers | upgrade |
| `crates/nlink-lab/src/builder.rs:868` | Typed wrappers | upgrade |

## Test Surface Additions

| Test | Covers |
|------|--------|
| `tests/cli_shell.rs` (integration, root-only) | #1 |
| `test_network_peer_name_unique` (unit) | #2 |
| `tests/integration_prefix_collision.rs` (integration, root-only) | #2 |
| `tests/orphan_cleanup.rs` (integration, root-only) | #4 |
| `test_tail_follow` (unit) | #7 |

## Suggested Commit Sequence

1. **Fix #1 (shell nsenter)** — one-line, ship first, unblocks immediate
   use.
2. **Fix #2 + #3 (peer-name hashing, error message)** — related; one commit.
3. **Fix #4 + #5 (orphan detection + `status --scan` + `--orphans`)** —
   share the discovery helper; one commit.
4. **Fix #6 (streaming exec)** — independent.
5. **Fix #7 (`logs --follow --pid`)** — independent.
6. **nlink 0.13.0 upgrade** — independent; land last so earlier fixes don't
   collide with dep churn.

Each commit is a candidate PR on its own. The reporter offered to
contribute patches for #1 and #2 — if they send them, prefer accepting
theirs for those two fixes to keep contribution momentum.

## Risks

- **Fix 2 renames mgmt-namespace interfaces.** Any deployed lab from
  before the change can't be cleaned up by the new-code `force_cleanup`
  because the bridge-peer name scheme changed. Mitigation: have the new
  `force_cleanup` sweep both old (`br{prefix4}p{idx}`) and new
  (`np{hash8}{idx}`) patterns for one release. Remove old pattern in the
  following release.
- **Fix 6 behavioural change.** `nlink-lab exec` without `--json` no
  longer returns captured stdout — scripts doing `output=$(nlink-lab exec
  …)` still work because stdout is still stdout, just unbuffered. Still,
  call out in CHANGELOG.
- **nlink 0.13.0** — if typed wrappers expose any hidden panics (e.g.
  `Percent::new` rejects >100), audit `parse_percent` behaviour to match.
  Percent is already clamped by `parse_percent` per the existing test at
  `helpers.rs:302`, so should be fine.
