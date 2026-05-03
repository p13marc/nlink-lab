# Plan 156: Round-4 Feedback (`des-test-harness`)

**Date:** 2026-05-03
**Status:** Drafted; not yet started.
**Effort:** Small — 1 HIGH bug (~10 LoC fix), 1 small feature, 1 doc-gap
feature. Three independent PRs.
**Priority:** P1 — Bug #1 silently breaks partition/heal cycles, the
worst kind of false-green test signal.

---

## Source

- `nlink-lab-feedback-2026-05-03.md` (round 4 from the harness team).
- All claims verified against `master` at commit `76c1305`. File:line
  citations accurate as of that commit.

## Decisions at a glance

| § | Item | Severity | Decision | PR |
|---|------|----------|----------|----|
| 1 | `impair --partition` no-op on second cycle | HIGH | **Fix** | A |
| 2 | `exec --timeout SECS` not implemented | LOW (feature) | **Implement** | B |
| 3 | `impair --show --json` output | NIT (doc gap) | **Implement** | C |

The reporter's positive notes confirm round-1/2/3 work landed cleanly.
Their `repeated_partition_cycles` scenario is currently passing only
because of a harness-side `--loss 100%` workaround in
`tools/des-test-harness/src/impair.rs`. Once Bug #1 is fixed, they'll
revert the workaround.

---

## PR A — Fix `impair --partition` no-op-after-clear (§1, P1)

### Diagnosis

Verified exactly as the reporter hypothesized. The bug is in three
lines of `running.rs` interacting:

1. **`partition()`** at `crates/nlink-lab/src/running.rs:883-907`
   short-circuits if the endpoint is in `saved_impairments`
   (line 885-887). On the *first* call, this is empty, so the function
   inserts a snapshot of the current impairment (line 897), installs
   100% loss (line 904), and persists state (line 905).

2. **`clear_impairment()`** at `running.rs:867-880` deletes the qdisc
   from the kernel (line 876) but does **not** remove the endpoint
   from `saved_impairments`, and does **not** call `save_state()`.

3. On the *second* `partition()`, the stale `saved_impairments` entry
   short-circuits (line 886) without touching the kernel. The CLI
   prints "Partitioned router:eth0" (because the function returned
   `Ok(())`), but the netem qdisc is never installed.

4. On the *second* `clear_impairment()`, the user-facing crash:
   `del_qdisc(...)` returns `nlink::Error::QdiscNotFound`, which
   propagates as `clear impairment on '<ep>': qdisc not found: root
   on ifindex 3`. (`running.rs:876-878`.)

The reporter's three suggested fixes map to:

1. Re-check kernel state before short-circuit. *Most defensive but
   over-engineered.*
2. Reset the flag in `clear_impairment`. *Minimum change.*
3. Make `--clear` idempotent on missing qdiscs. *Independent
   robustness improvement.*

### Decision: take (2) and (3) together

Together they cover both observable failure modes:

- (2) makes the `partition` short-circuit only fire when there's
  *real* prior partition state, fixing the silent no-op.
- (3) makes the `clear` failure path go away in the (now rare) cases
  where the kernel-side qdisc is already gone — e.g., a topology
  with no impairments declared, where the user calls `--clear` once
  on a fresh deploy. Today that errors with `QdiscNotFound` from
  `del_qdisc`. Reporter hit the same error path via the `--heal`
  → `clear_impairment` chain.

(1) (kernel-state re-check) we skip — costs an extra netlink
round-trip on every partition for a problem that (2) eliminates by
construction.

### Fix

**File 1**: `crates/nlink-lab/src/running.rs` — `clear_impairment`
becomes `&mut self` (so it can prune `saved_impairments`) and
becomes idempotent for `QdiscNotFound`:

```rust
pub async fn clear_impairment(&mut self, endpoint: &str) -> Result<()> {
    let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
        endpoint: endpoint.to_string(),
    })?;
    let ns_name = self.namespace_for(&ep.node)?;
    let conn: Connection<Route> = namespace::connection_for(ns_name)
        .map_err(|e| Error::deploy_failed(format!("connection for '{ns_name}': {e}")))?;

    // Idempotent: if the qdisc is already gone, that's fine — we're
    // converging to the same end state. Other errors propagate.
    match conn.del_qdisc(&ep.iface, nlink::TcHandle::ROOT).await {
        Ok(()) => {}
        Err(nlink::Error::QdiscNotFound { .. }) => {}
        Err(e) => {
            return Err(Error::deploy_failed(format!(
                "clear impairment on '{endpoint}': {e}"
            )));
        }
    }

    // Drop any stale "is partitioned" bookkeeping for this endpoint
    // and persist — otherwise a follow-up `partition()` would see the
    // entry and short-circuit without installing the qdisc.
    if self.saved_impairments.remove(endpoint).is_some() {
        self.save_state()?;
    }
    Ok(())
}
```

**File 2**: `bins/lab/src/main.rs:1594` — already passes `mut
running`, no change needed at the CLI side.

**File 3**: `crates/nlink-lab/src/deploy.rs:2509` (in `apply_diff`) —
`running` is already `&mut RunningLab`, no change.

**File 4**: `crates/nlink-lab/src/running.rs:916` — `heal()` already
calls `clear_impairment` on `&mut self`. No change.

`scenario.rs` has its own `clear_impairment` *helper function*
(`scenario.rs:230-239`, takes `&RunningLab`) — this is unrelated to
the method on `RunningLab` and uses the `let _ = del_qdisc(...)`
pattern (already idempotent). No change.

### Why `--loss 100%` works today

For the response back to the reporter: their `--loss 100%` workaround
goes through `set_impairment` (which uses `change_qdisc` then falls
back to `add_qdisc`) and never touches `saved_impairments`. The
short-circuit guard doesn't apply because the bookkeeping is
specific to the `partition`/`heal` pair.

### Tests

- **Unit test** in `crates/nlink-lab/src/running.rs::tests` — synthetic
  `RunningLab` with hand-rolled `saved_impairments`, call
  `clear_impairment`, assert the entry is removed. Hard because
  `clear_impairment` does real netlink work — let's not. Skip.
- **Integration test** (root-only, gated via `#[lab_test]`):
  `partition_clear_cycles_are_idempotent`. Deploy `simple.nll`, run
  4 partition+clear cycles on `host:eth0`, assert each `--partition`
  installs a real netem qdisc by exec'ing `tc qdisc show dev eth0`
  and grepping for `loss 100%`, and assert each `--clear` removes
  it. Mirrors the reporter's acceptance test exactly.
- **Integration test**: `clear_impairment_idempotent_on_fresh_deploy`.
  Deploy a topology with no impairments. Call `clear_impairment`
  twice on `host:eth0`. Both must succeed.

Both tests run under `sudo cargo test -p nlink-lab --test integration`.

### Risks

- Changing `clear_impairment` from `&self` to `&mut self` is a
  signature change. All callers in-tree already have `&mut`. External
  callers (none we know of) would need to pass `mut`. Low impact.
- `saved_impairments` previously could outlive `clear_impairment`.
  After the fix, it doesn't. If anything assumed the old behaviour
  (it shouldn't), it'd break. Best-faith risk: zero — the field is
  internal bookkeeping for partition/heal and isn't documented as a
  public source of truth.

---

## PR B — `nlink-lab exec --timeout SECS` (§2, NICE)

### Design

Mirror `coreutils timeout(1)`. New flag on the `Exec` clap variant:

```
--timeout <SECS>
    Maximum wall-clock time the command is allowed to run before
    being killed. Sends SIGTERM, then SIGKILL after a 1-second
    grace period. Exit code 124 on timeout (matches `timeout(1)`).
    Default: no timeout.
```

Composes with `--workdir`, `--env`, `--json`. The error path emits
`nlink-lab exec: command timed out after Ns` to stderr (so callers
can distinguish "the inner command exited 124" from "we timed
out").

### Implementation

**Library** (`crates/nlink-lab/src/running.rs`): the new field on
`ExecOpts<'a>`:

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecOpts<'a> {
    pub workdir: Option<&'a std::path::Path>,
    pub env: &'a [(&'a str, &'a str)],
    pub timeout: Option<std::time::Duration>,    // ← new
}
```

The body of `exec_with_opts` and `exec_attached_with_opts` adds a
timeout wrapper around the spawn-and-wait. For namespace nodes, the
existing `namespace::spawn_with_etc(...)` returns a `Child` — we
poll `child.try_wait()` against a deadline, send SIGTERM via
`libc::kill(pid, SIGTERM)`, sleep 1s, then SIGKILL if still alive.

For container nodes, `docker exec` / `podman exec` already supports
no per-call timeout, so we wrap the runtime command the same way:
spawn, poll `try_wait`, signal-and-kill on deadline.

A single helper `wait_with_timeout(child: Child, dur: Duration) ->
Result<ExitStatus>` handles both. Returns
`Error::Timeout(duration)` (new variant) when the kill fires.

For the *attached* path (`exec_attached_with_opts`), `Stdio::inherit`
plus `child.wait()` — same timeout pattern works.

**CLI** (`bins/lab/src/main.rs`): add the field to the `Exec` clap
variant, parse to `Option<Duration>`, pass via
`ExecOpts::timeout`. On `Error::Timeout`, exit 124 (matching
`coreutils timeout(1)`); print
`nlink-lab exec: command timed out after Ns` to stderr first.

**Error type** (`crates/nlink-lab/src/error.rs`): new variant
`#[error("command timed out after {0:?}")] Timeout(std::time::Duration)`.

### Tests

- **Integration test** (root-only): `exec_with_timeout_kills_long_running`.
  Run `sleep 5` with `--timeout 1`. Must return within ~2s with
  `Error::Timeout`. CLI smoke-test via the binary (since the test
  exercises both code paths).
- **Integration test**: `exec_under_timeout_returns_normally`.
  `echo hi` with `--timeout 5` returns exit 0 + correct stdout.

### Files touched

- `crates/nlink-lab/src/running.rs` — add `timeout` to `ExecOpts`,
  thread through `exec_with_opts` and `exec_attached_with_opts`.
- `crates/nlink-lab/src/error.rs` — `Timeout` variant.
- `bins/lab/src/main.rs` — `Exec` clap variant + match-arm
  threading + 124 exit code on `Error::Timeout`.
- `crates/nlink-lab/tests/integration.rs` — two new tests.
- `CHANGELOG.md` — entry.

### Risks

- Sending SIGTERM into a namespace from the host: `libc::kill(pid,
  SIGTERM)` works because the PID is in the host PID namespace
  (we're not using PID namespaces, just net namespaces). Already
  proven by `RunningLab::kill_process`.
- 1s grace between SIGTERM and SIGKILL is the conventional pattern;
  matches `timeout(1)` defaults. Leave configurable as a follow-up
  if anyone asks.

---

## PR C — `nlink-lab impair --show --json` (§3, NIT)

### Design

Today `impair --show` runs `tc qdisc show` per namespace and prints
the raw text under per-node headers (`bins/lab/src/main.rs:1572-1581`).
The reporter wants a parsed JSON view.

Reporter's suggested shape:

```json
{
  "lab": "des-3m",
  "endpoints": {
    "router:eth0": {
      "qdisc": "netem",
      "loss_pct": 100.0,
      "delay_ms": null,
      "jitter_ms": null,
      "rate_bps": null,
      "out_loss_pct": null,
      "in_loss_pct": null,
      "partition": true
    },
    "router:eth1": null
  }
}
```

We adopt their shape with two caveats:

- `out_*` / `in_*` directional fields. Today nlink-lab installs a
  netem qdisc on the *root* of each interface (egress only). The
  ingress shaping that round 1's `--in-*` flags reach is achieved by
  installing a netem on the *peer*'s egress. So per-endpoint there's
  one direction only; `out_*` is redundant with the top-level fields,
  and `in_*` requires looking at the peer. Plan: emit only the
  top-level (`loss_pct`, etc.); `out_*`/`in_*` we omit. If a consumer
  needs the directional split, the peer's row covers it.
- `partition: true` set when `endpoint` is in
  `saved_impairments` — that's the contract. The reporter's heuristic
  ("loss = 100% means partition") is *almost* right but treats a
  user-set `--loss 100%` the same as a partition; the
  `saved_impairments`-based answer is more precise.

### Implementation

**Parser** (`crates/nlink-lab/src/impair_parse.rs`, new file): given
the per-line `tc qdisc show` output, extract `(qdisc_kind,
delay_ms, jitter_ms, loss_pct, rate_bps)` per interface. Format is
stable across kernels we care about:

```
qdisc netem 801c: dev eth0 root refcnt 2 limit 1000 \
       delay 10ms 2ms loss 100% rate 1Mbit
qdisc noqueue 0: dev eth0 root refcnt 2
```

Fields appear in stable orders; missing fields just don't appear.
A small per-line tokeniser is enough — no `nom`/`winnow` dependency
needed. Pure function, fully unit-testable against captured `tc`
strings (~10 cases, including `noqueue`/none).

**CLI** (`bins/lab/src/main.rs:1572-1581`): when `--show` and `cli.json`,
walk all endpoints in the topology (so we naturally get a
"`endpoint: null`" row for impairment-free interfaces), exec `tc
qdisc show dev <iface>` per node × per interface, parse, build the
nested object, emit it.

For non-JSON: behaviour unchanged.

**Schema**: `docs/json-schemas/impair-show.schema.json`. Slot
alongside the five from PR D. `json_schemas_parse` test will catch
malformed schema in CI.

### Tests

- **Unit tests** in `impair_parse::tests`: parse 8–10 hand-crafted
  `tc qdisc show` strings (clean noqueue, netem with all fields,
  netem with delay only, htb without netem, multi-line junk we
  should ignore, etc.). All pure-function tests; no root needed.
- **Integration test** (root-only):
  `impair_show_json_emits_active_impairment`. Deploy with one
  endpoint having a declared impairment, run `impair --show
  --json`, parse the output, assert the endpoint shows `loss_pct:
  100.0`, `partition: true` after a `--partition`.

### Files touched

- `crates/nlink-lab/src/impair_parse.rs` — new (pure parser, ~60 LoC).
- `crates/nlink-lab/src/lib.rs` — export the parser if useful, or
  keep crate-internal.
- `bins/lab/src/main.rs:1572-1581` — JSON branch.
- `docs/json-schemas/impair-show.schema.json` — new.
- `docs/json-schemas/README.md` — add row.
- `bins/lab/src/main.rs::tests::json_schemas_parse` — add the new
  schema to the include list.

### Risks

- `tc` output format drift across kernels. We've targeted Linux 6.x
  which is what the reporter uses; older kernels emit slightly
  different field ordering. Acceptable today. If a user with an
  older kernel files a parser bug, harden the parser then.

---

## Suggested commit / PR sequence

1. **PR A** — partition cycles fix. Highest priority, smallest
   surface, ships first. Once it's in, the harness team can drop
   their `--loss 100%` workaround.
2. **PR C** — `--show --json`. Independent doc gap. Cheap.
3. **PR B** — `exec --timeout`. Independent feature. Touches
   `ExecOpts` again — pairs neatly with the round-3 PR B
   consolidation; same pattern.

Each PR is independently reviewable and revertable.

---

## Risk summary

| PR | Risk | Mitigation |
|----|------|------------|
| A | Signature change on `clear_impairment` | All in-tree callers already have `mut`; external none we know of. |
| B | SIGTERM→SIGKILL grace timing surprises a long-cleanup process | 1s grace matches `timeout(1)` convention; document if anyone complains. |
| C | `tc` output drift across kernels | Targets the kernel surface the reporter uses; revisit on bug report. |

---

## Test surface added

| Test | PR | Type |
|------|----|------|
| `partition_clear_cycles_are_idempotent` (root-only) | A | integration |
| `clear_impairment_idempotent_on_fresh_deploy` (root-only) | A | integration |
| `exec_with_timeout_kills_long_running` (root-only) | B | integration |
| `exec_under_timeout_returns_normally` (root-only) | B | integration |
| `impair_parse::tests::*` (~10 cases) | C | unit |
| `impair_show_json_emits_active_impairment` (root-only) | C | integration |
| schema-parse compile-time check (extended) | C | unit |

---

## Acceptance criteria (from reporter)

- [ ] **PR A**: the loop test from §1's "Acceptance test" section
      prints `ALL CYCLES OK` for 5 cycles. Today it prints `FAIL: no
      qdisc after --partition` on cycle 2.
- [ ] **PR B**: `nlink-lab exec --timeout 1 LAB NODE -- sleep 5`
      returns exit 124 within ~2s, and `nlink-lab exec --timeout 5
      LAB NODE -- /bin/echo hi` returns exit 0 + `hi`.
- [ ] **PR C**: `nlink-lab impair --show --json LAB | jq` returns
      structured output matching the schema; `null` for endpoints
      with no impairment.

---

## Notes on coordinating with the harness team

They've offered to revert their `--loss 100%` workaround once PR A
ships. Their `repeated_partition_cycles` scenario in
`tools/des-test-harness/tests/network_resilience.rs` will be the
acceptance test from their side — it currently passes only because
of the workaround.

If we want belt-and-suspenders for the response: include a one-line
loop snippet in the response markdown (the reporter's own
acceptance test) so they can verify the fix locally without
unwinding the workaround first.
