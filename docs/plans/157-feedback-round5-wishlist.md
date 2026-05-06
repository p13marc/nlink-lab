# Plan 157: Round-5 Wishlist (`des-test-harness`)

**Date:** 2026-05-06
**Status:** Drafted; not yet started.
**Effort:** Mixed — six features, one investigation, four doc items.
The cheap docs ship in one PR; the rest are individually-reverable.
**Priority:** P1 — §1.2 parallel-lab safety is a real reliability
concern; §2.2 unblocks ~150 LOC of fragile `/proc` parsing in the
harness; the rest is leverage.

---

## Source

- `nlink-lab-wishlist-2026-05-06.md` (forward-looking wishlist from
  the `des-test-harness` team — same team that drove rounds 1–4).
- All claims independently verified against `master` at commit
  `3c76fe4` (the 0.3.1 release).

## Decisions at a glance

| § | Item | Type | Pri | Decision | PR |
|---|------|------|-----|----------|----|
| 1.1 | Document namespace + UID model | doc | P1 | **Take** — and correct the reporter's `CLONE_NEWPID` assumption (we don't use it) | A |
| 1.2 | Parallel-lab reliability | invest. + fix | P1 | **Investigate, then fix** | D |
| 2.1 | `spawn --json` returns `host_pid` + `ns_pid` | UX | P1 | **Take, with caveat**: emit `host_pid` as alias for `pid`; emit `ns_pid` only after a NEWPID-introducing change (currently equals host_pid) | B |
| 2.2 | `nlink-lab proc-stat` primitive | feature | P1 | **Take** — high leverage | C |
| 2.3 | Capture rotation / size cap | feature | P2 | **Take** | F |
| 2.4 | `--wait-fd-stable` + `--wait-port` | feature | P2 | **Take both** | G |
| 2.5 | Per-lab subnet allocator | feature | P2 | **Take** — depends on §1.2 fix landing first | E |
| 2.6 | Loopback dedupe at capture | feature | P3 | **Take** — netring already supports it (`ignore_outgoing`); ~30 LoC | H |
| 3.1 | Harness best-practices doc | doc | P1 | **Take** — write skeleton, accept their PR for the body | I |
| 3.2 | Schema link from each `--help` | doc | P2 | **Take** | A |
| 3.3 | CHANGELOG link from README | doc | P3 | **Take** — `CHANGELOG.md` already exists, just under-discovered | A |

**Important factual correction**: the reporter believes
`CLONE_NEWPID` is in use. It isn't. Verified at
`/home/mpardo/git/nlink/crates/nlink/src/netlink/namespace.rs:405`:
the only `setns` call is `CLONE_NEWNET`. The only `unshare` call
elsewhere in that file is `CLONE_NEWNS` (mount namespace, gated on
`/etc/netns/<ns>/` overlays existing). No NEWPID, no NEWUTS, no
NEWUSER, no NEWIPC. **host PID == ns PID** for every spawned
process. Several wishlist items are downstream of this confusion —
PR A's docs land first to defuse it.

---

## PR A — Docs sweep (§1.1, §3.2, §3.3) [P1, ~2 hours]

### Items bundled

Three pure-doc tasks, one PR:

#### §1.1 — Namespace + UID model

Add a "Process & namespace model" section to
`docs/ARCHITECTURE.md` (the existing contributor on-ramp). Direct
answers to the reporter's four questions:

> ## Process & namespace model
>
> ### Namespaces
>
> A process spawned by `nlink-lab spawn` (or `nlink-lab exec`) into a
> bare namespace node enters exactly one Linux namespace via
> `setns(2)`:
>
> | Flag             | Active? | Notes |
> |------------------|---------|-------|
> | `CLONE_NEWNET`   | always  | Network ns — the whole point of nlink-lab. |
> | `CLONE_NEWNS`    | sometimes | Only when `dns hosts` (or any `/etc/netns/<ns>/` overlay) is configured for the lab. Used for the `/etc/hosts` bind-mount. |
> | `CLONE_NEWPID`   | **no**  | PIDs are shared with the host. `host_pid == ns_pid`. |
> | `CLONE_NEWUTS`   | no      | |
> | `CLONE_NEWIPC`   | no      | |
> | `CLONE_NEWUSER`  | no      | |
>
> Container nodes go through docker/podman and follow that
> runtime's namespace conventions; the rules above apply only to
> bare namespace nodes.
>
> ### UID
>
> `nlink-lab` enforces root via `check_root` before any deploy /
> exec / spawn that touches netlink. Spawned processes inherit the
> caller's UID — which is always root in practice. There is no
> userns mapping; root in the namespace is root on the host.
>
> ### `/proc` visibility from the host
>
> Without `CLONE_NEWPID`, the host's `/proc` shows every spawned
> process. Permissions are the standard kernel rules — they do
> *not* depend on the namespace:
>
> | Path                       | Readable from host non-root? |
> |----------------------------|------------------------------|
> | `/proc/<pid>/stat`         | Yes (mode 0444) |
> | `/proc/<pid>/status`       | Yes (mode 0444) |
> | `/proc/<pid>/cmdline`      | Yes (mode 0444) |
> | `/proc/<pid>/fd/`          | **No** — mode 0700, owned by the spawned process's UID (root). |
>
> If you need to read `fd/` from a non-root host shell, use
> `nlink-lab proc-stat` (which routes the read through nlink-lab's
> own root context), or `sudo ls /proc/<pid>/fd`. Don't try to chase
> the UID mismatch in user code.
>
> ### Host PID vs namespace PID
>
> Because we don't use `CLONE_NEWPID`, **the two are equal**.
> `nlink-lab spawn --json`'s `pid` field (and the `host_pid` alias
> introduced in 0.4.0) is the only PID. `/proc/<pid>/...` works
> identically from inside the namespace and from the host —
> permissions are the only variable.

This subsection is the source of truth for §2.1 (which becomes a
near-noop) and a prerequisite for §2.2 (which uses the same model).

#### §3.2 — Schema link from each `--help`

Append a JSON Schema URL to each subcommand's clap doc-comment. Six
subcommands, one URL each:

```rust
/// Deploy a lab from a topology file.
///
/// JSON OUTPUT (with `--json`):
///   { "name": str, "nodes": int, ... }
/// Schema: docs/json-schemas/deploy.schema.json
Deploy { ... }
```

Apply to: `Deploy`, `Status`, `Spawn`, `Ps`, `Impair` (`--show`).
Use repo-relative paths — the GitHub-hosted URL would couple the
help text to the canonical repo URL, which is not what we want for
forks. The canonical doc-comment update from PR D (round-3) already
mentions schemas; this just adds the per-subcommand line.

#### §3.3 — CHANGELOG link from README

`CHANGELOG.md` already exists (Keep-a-Changelog format, updated
every release). Reporter says they didn't know. One-line README
addition:

```md
## Releases

See [`CHANGELOG.md`](CHANGELOG.md) for per-release notes. Current
release: [0.3.1](https://github.com/p13marc/nlink-lab/releases/tag/0.3.1).
```

Place under the existing top-level README structure.

### Files touched

- `docs/ARCHITECTURE.md` (new section)
- `bins/lab/src/main.rs` (six clap doc-comment updates)
- `README.md` (one section)

### Risks

None. Pure docs.

---

## PR B — `spawn --json` PID-clarity (§2.1) [P1, ~30 min]

### Decision

Reporter wants `host_pid` and `ns_pid` as separate fields. Without
`CLONE_NEWPID`, **they are always equal**. Adding `ns_pid: <same
value>` would mislead consumers into thinking the namespace has its
own PID space.

The right action is to:

1. Add `host_pid` as an explicit alias for `pid` in the JSON output.
   Reporter's confusion — "is `pid` the host or ns PID?" — is
   resolved by the explicit name.
2. **Do not** add `ns_pid` today. PR A's doc explains why.
3. If a future change introduces `CLONE_NEWPID` (none planned),
   add `ns_pid` then.

### Implementation

`bins/lab/src/main.rs`, `Spawn` match arm — add `host_pid` alongside
`pid`:

```rust
println!(
    "{}",
    serde_json::json!({
        "pid": pid,
        "host_pid": pid,   // alias — see ARCHITECTURE.md "Process & namespace model"
        "node": node,
        "command": cmd.join(" "),
    })
);
```

Same change in the `Ps` match arm and in the
`docs/json-schemas/spawn.schema.json` and `ps.schema.json` files
(add `host_pid` as an optional field with a description that points
at the doc).

### Tests

- Update `json_schemas_parse` test (already runs).
- Add a CLI smoke test invoking `cargo run -- spawn --json` and
  asserting `host_pid == pid` in the output. (Skipped if not root.)

### Risks

- We're adding a redundant field. Future-self may want to remove it
  if it's confusing. Mitigation: doc-comment on the schema explains
  it's an explicit alias for backwards compat.

### Files touched

- `bins/lab/src/main.rs` (two match arms)
- `crates/nlink-lab/src/running.rs` (`ProcessInfo` adds
  `#[serde(rename = "host_pid")] alias_pid: Option<u32>` — skipped
  when serializing today since `pid` already covers it; the field is
  redundant and the JSON output adds it explicitly in main.rs)
- `docs/json-schemas/spawn.schema.json`, `ps.schema.json`

### Effort

30 min including tests.

---

## PR C — `nlink-lab proc-stat` primitive (§2.2) [P1, ~1.5 days]

### Design

```
nlink-lab proc-stat <LAB> <NODE> <PID> [--json] [--watch <SECS>]
```

Reads `/proc/<pid>/{stat,status}` and counts entries in
`/proc/<pid>/fd/` from inside the target namespace via
`nlink-lab exec`. Returns parsed structured output:

```json
{
  "host_pid": 292086,
  "command": "des_discovery",
  "uid": 0,
  "rss_kb": 45660,
  "vsz_kb": 218472,
  "fd_count": 24,
  "cpu_user_ticks": 142,
  "cpu_kernel_ticks": 17,
  "started_at_unix_micros": 1714942234000000,
  "state": "S"
}
```

Field semantics:

- `host_pid` — same as ns_pid (no NEWPID); explicit name for
  consumers parsing programmatically.
- `command` — from `/proc/<pid>/comm` (16-byte limit; for the full
  argv use `/proc/<pid>/cmdline`).
- `uid` — first column of `Uid:` in `/proc/<pid>/status`.
- `rss_kb` — `VmRSS` in `/proc/<pid>/status` (kB).
- `vsz_kb` — `VmSize` in `/proc/<pid>/status` (kB).
- `fd_count` — number of entries in `/proc/<pid>/fd/`. Counted by
  `ls /proc/<pid>/fd | wc -l` exec'd in the namespace (root context,
  permitted).
- `cpu_user_ticks`, `cpu_kernel_ticks` — fields 14, 15 of
  `/proc/<pid>/stat` (clock ticks since boot; consumers convert via
  `_SC_CLK_TCK`).
- `started_at_unix_micros` — derived from field 22 of
  `/proc/<pid>/stat` (start time, jiffies since boot) plus `BTIME`
  from `/proc/stat`. Computed inside the ns to match the kernel the
  process is on.
- `state` — single character from `/proc/<pid>/stat` field 3
  (`R`/`S`/`D`/`Z`/`T`/etc.).

### Why exec inside the namespace

Without NEWPID this is technically unnecessary — we could read
host-side `/proc/<pid>/...` directly. But:

- Consumers with mount-namespaced labs (when `dns hosts` is on)
  may have a private mount view; the in-ns `/proc` reflects it.
- Future-proof: if `CLONE_NEWPID` is ever added, the in-ns read
  stays correct.
- Permission model: we exec as root inside the namespace, so
  `/proc/<pid>/fd/` (root-owned 0700) is readable.

The per-call cost is one `nsenter` + one short-lived process. For a
test harness that polls every few seconds, this is fine.

### `--watch <SECS>`

Streaming mode: emits one record per interval to stdout, one JSON
object per line (NDJSON). Stops on Ctrl-C. Implementation: tokio
loop with `tokio::time::interval`. The default poll loop is plain;
no need for inotify.

### Library API

```rust
impl RunningLab {
    /// Sample resource usage of a tracked process. Reads
    /// /proc/<pid>/{stat,status,fd} from inside the target node's
    /// namespace via `exec`.
    ///
    /// `pid` is the host PID (same as ns PID — see
    /// `docs/ARCHITECTURE.md` "Process & namespace model").
    pub fn proc_stat(&self, node: &str, pid: u32) -> Result<ProcStat>;
}

pub struct ProcStat { /* fields as above */ }
```

### Implementation

Pure parser in a new module `crates/nlink-lab/src/proc_stat.rs`:

- `parse_status(text: &str) -> StatusFields` — extracts
  `VmRSS`/`VmSize`/`Uid`.
- `parse_stat(text: &str) -> StatFields` — splits the
  parens-delimited comm field, extracts ticks + start time.
- `read_btime() -> u64` — for ticks-to-unix conversion.

`RunningLab::proc_stat` orchestrates the three exec calls and
combines into a `ProcStat`.

### Tests

- Pure-parser unit tests on hand-crafted `/proc/<pid>/stat` and
  `/proc/<pid>/status` strings (8–10 cases including kernel quirks
  like comm with parens: `(my (weird) comm)`).
- Integration test (root-only): spawn `sleep 30`, call `proc_stat`,
  assert `command == "sleep"`, `rss_kb > 0`, `state == "S"`.
- CLI smoke test for `--json` output shape against the schema.

### Schema

`docs/json-schemas/proc-stat.schema.json` — added to
`json_schemas_parse` test list.

### Files touched

- `crates/nlink-lab/src/proc_stat.rs` (new)
- `crates/nlink-lab/src/lib.rs` (export `proc_stat::ProcStat`)
- `crates/nlink-lab/src/running.rs` (`proc_stat` method)
- `bins/lab/src/main.rs` (clap variant + match arm)
- `crates/nlink-lab/tests/integration.rs` (one new test)
- `docs/json-schemas/proc-stat.schema.json` (new)
- `CHANGELOG.md`

### Risks

- `comm` with parens needs careful parsing. Kernel docs are clear:
  the comm is everything between the *last* `(` and the *next-to-last*
  `)`. We use `rfind(')')` and slice from `find('(')`.
- `/proc/<pid>/cmdline` zero-byte separators — we don't expose this
  (just use `comm`); but worth a comment on why.

### Effort

~1.5 days: 0.5d for parser + tests, 0.5d for CLI plumbing + schema,
0.5d for `--watch` and integration test.

---

## PR D — Parallel-lab reliability (§1.2) [P1, 1–3 days]

### Phase A: Reproduce (~½ day)

Write a parallel-deploy stress test in
`crates/nlink-lab/tests/stress.rs`:

```rust
#[lab_test]   // root-only, gated
async fn N_labs_in_parallel_all_succeed() {
    const N: usize = 8;
    let topo_text = include_str!("../../../examples/simple.nll");
    let mut handles = Vec::new();
    for i in 0..N {
        handles.push(tokio::spawn(async move {
            let mut topo = parser::parse(topo_text)?;
            topo.lab.name = format!("stress-{i}-{}", std::process::id());
            topo.deploy().await
        }));
    }
    let mut errs = Vec::new();
    let mut labs = Vec::new();
    for h in handles {
        match h.await.unwrap() {
            Ok(lab) => labs.push(lab),
            Err(e) => errs.push(e),
        }
    }
    // Cleanup
    for lab in labs { let _ = lab.destroy().await; }
    assert!(errs.is_empty(), "{} of {} labs failed: {errs:?}", errs.len(), N);
}
```

Variants:
- with `dns hosts` mode
- with `mgmt host-reachable`
- without either (baseline)

### Phase B: Diagnose (~½ day)

Run the stress test under `strace -f -e openat,renameat,flock` and
look for the failure-mode signature. Hypotheses, ordered by my
prior:

1. **`/etc/hosts` race** (highest prior). `crates/nlink-lab/src/dns.rs:91`
   `inject_hosts` reads `/etc/hosts`, splices a managed section,
   atomic-renames. **No flock**. Two concurrent deploys with
   `dns hosts` mode race; the loser overwrites the winner's section.
   Symptom would be: deploy succeeds, but DNS resolution between
   nodes fails on the loser. May not match the reporter's failure
   mode (which is "deploy fails 2/4"), but worth eliminating.
2. **Mgmt bridge name collision**. `nl{hash8}` — djb2 over the lab
   name. With `--unique` adding the PID suffix, collision over 30
   parallel labs is ~5e-8 — negligible. *Probably not the cause.*
3. **Veth peer name collision** (`np{hash8}{idx}`). Same
   probability. Probably not.
4. **Concurrent `ip netns add`**. The kernel serialises this via a
   global mutex; concurrent calls work but slowly. Failures only
   under extreme load.
5. **Mgmt host-reachable bridge**. When `mgmt host-reachable` is
   set, deploy creates a host-side bridge and assigns IPs. Two
   parallel deploys may collide on bridge IP allocation if subnets
   overlap.
6. **Deploy step ordering race**. Step 4 (create bridges) and step
   5 (create veths spanning ns) are not serialised across labs.
   The kernel itself is supposed to be safe but bugs exist.

### Phase C: Fix (~1–2 days, depending on root cause)

- For (1) `/etc/hosts` race: wrap `inject_hosts` and the matching
  `remove_hosts` in a flock on a sentinel file
  (`/var/lock/nlink-lab-hosts.lock` or
  `$XDG_STATE_HOME/nlink-lab/hosts.lock`). Same pattern as
  `state::lock` for per-lab locks.
- For (2)/(3) hash collisions: increase hash width if needed (we
  have 5 chars to spare in the 15-char ifname budget — `nl{hash10}`
  drops collision probability to ~1e-10 for 100 labs). Probably
  unnecessary.
- For deploy-step races: identify and serialise specific kernel
  ops via a workspace-wide async mutex.
- If the failure is genuinely host-capacity (not a bug): document
  the per-host cap, expose it as `nlink-lab status --json`'s
  per-lab info, and surface in `--help`.

### Phase D: Bonus per-lab info in `status --json`

Reporter asks for "subnet, bridge interfaces, ports bound" per lab
in `nlink-lab status --json <lab>`. We already emit the topology +
addresses; add:

```json
{
  ...
  "host_resources": {
    "mgmt_bridge": "nl04ab1234",
    "veth_peers": ["nm04ab12340", "nm04ab12341", "nm04ab12342"],
    "subnets": ["10.0.0.0/24", "10.1.0.0/24"]
  }
}
```

Pure data assembly from existing state — no new netlink calls.

### Acceptance

```bash
# This must succeed on a host with reasonable capacity:
sudo cargo test -p nlink-lab --test stress -- N_labs_in_parallel_all_succeed
```

### Files touched

- `crates/nlink-lab/tests/stress.rs` (new file, gated)
- `crates/nlink-lab/src/dns.rs` (add lock if /etc/hosts is the
  cause)
- `crates/nlink-lab/src/state.rs` (lock helper if needed in dns.rs)
- `bins/lab/src/main.rs` (Phase D status enrichment)
- `docs/json-schemas/status-lab.schema.json` (new — for the
  per-lab status shape, if we don't have one yet)
- `CHANGELOG.md`

### Risks

- Investigation may uncover a deeper kernel-level limit. If so,
  the fix becomes "document the cap" — still actionable, just less
  ambitious.
- The /etc/hosts lock is global state; we need to handle the case
  where a stale lock file blocks future runs (use `flock` not
  `mkdir`-based locking).

### Effort

1–3 days, depending on root cause.

---

## PR E — Per-lab subnet allocator (§2.5) [P2, 1–2 days]

### NLL syntax

```nll
node site_a {
    subnet auto/24
}
network lan {
    subnet auto/24
    members [...]
}
```

Or a compact pool form:

```nll
pool labs 10.0.0.0/8 /24
network lan {
    subnet from labs
    members [...]
}
```

Pick the simpler `auto/24` form; `pool` is already supported for
loopback addresses (`lo pool name`) in the existing parser.

### Allocator

Pool: `10.0.0.0/8` (RFC1918 — 65536 /24 subnets available).

State coordination via a shared file
`$XDG_STATE_HOME/nlink-lab/subnet-pool.json`:

```json
{
  "in_use": {
    "10.0.0.0/24": "lab-foo",
    "10.0.1.0/24": "lab-bar"
  },
  "next_offset": 2
}
```

Acquire `flock` on this file during deploy. Allocate from
`next_offset`; on conflict (existing in_use), increment and retry.
On lab destroy, prune the file.

### Lifecycle

- Deploy: lock pool → for each `subnet auto/<prefix>` placeholder,
  allocate → record in lab state → write back pool → unlock.
- Destroy: lock pool → remove this lab's entries → write back →
  unlock.
- `destroy --orphans`: walk lab state files, prune unreachable
  entries from the pool.

### Implementation

- `crates/nlink-lab/src/subnet_pool.rs` (new) — pool management.
- AST: extend `Subnet` to support `auto` keyword + prefix.
- Lower: replace `auto/N` with a real `Subnet` after consulting the
  pool.
- Deploy: pool acquire happens before validation; pool record
  written to state.

### Tests

- Pure unit tests on the allocator: 1000 labs allocated and freed,
  no leaks.
- Integration test: two parallel deploys both with `auto/24` — both
  succeed with non-colliding subnets.

### Files touched

- `crates/nlink-lab/src/parser/nll/{ast.rs,parser.rs,lower.rs}` —
  `auto` keyword.
- `crates/nlink-lab/src/subnet_pool.rs` (new).
- `crates/nlink-lab/src/state.rs` — extend `LabState` to track
  allocated subnets for cleanup.
- `crates/nlink-lab/src/deploy.rs` — call into pool during deploy.
- `bins/lab/src/main.rs` — `destroy --orphans` prunes the pool.
- `docs/NLL_DSL_DESIGN.md` — document the syntax.
- `docs/json-schemas/state.schema.json` (if we have one — else N/A).
- `CHANGELOG.md`

### Risks

- Pool file corruption if a deploy crashes mid-write. Mitigation:
  same atomic-write pattern as `state::save` (temp + rename).
- Orphan pool entries from crashed deploys. Mitigation: `destroy
  --orphans` sweeps based on actual lab presence.

### Dependency

Should land *after* PR D — if parallel labs aren't safe at the
deploy level, the allocator can't help.

### Effort

~1.5 days.

---

## PR F — Capture rotation (§2.3) [P2, 1–2 days]

### CLI

```
nlink-lab capture <LAB> <ENDPOINT> --output <PATH> \
    [--max-size <BYTES>] [--rotate <SECS>] [--keep <N>]
```

Flags:
- `--max-size 100M` — rotate when current segment exceeds N bytes.
  Suffixes: `K`, `M`, `G`.
- `--rotate 600s` — rotate every N seconds (alternative to size).
- `--keep 5` — retain the most recent N segments. Older segments
  are deleted on rotation. Default: unlimited.

### Naming

Segments: `<base>.pcap`, `<base>.pcap.1`, `<base>.pcap.2`, ...
Rotation:
1. Delete `<base>.pcap.<keep>` if it exists.
2. Rename `<base>.pcap.<i>` → `<base>.pcap.<i+1>` for `i = keep-1
   .. 1`.
3. Rename `<base>.pcap` → `<base>.pcap.1`.
4. Open new `<base>.pcap`, write fresh global header.

### Index

Optional `<base>.pcap.idx` JSON file:

```json
[
  { "file": "stress.pcap.2", "started_at": "2026-05-06T14:30:00Z", "size_bytes": 100000000 },
  { "file": "stress.pcap.1", "started_at": "2026-05-06T14:35:00Z", "size_bytes": 100000000 },
  { "file": "stress.pcap",   "started_at": "2026-05-06T14:40:00Z", "size_bytes":  42000000 }
]
```

Rewritten on each rotation.

### Implementation

Wrap `PcapWriter<W>` in a `RotatingPcapWriter` that:

- Tracks bytes written since last rotation.
- Holds the base path + rotation policy.
- On each `write_packet`: increments counter, checks threshold,
  rotates if needed.
- Rotation: drop the inner File, rename, open new File, reconstruct
  the inner `PcapWriter` (which emits the global header).

The unbuffered writes from the round-3 PR A fix mean rotation is
clean — no buffered bytes lost on file swap.

### Tests

- Unit test: `RotatingPcapWriter` with mock filesystem, write 250 MB
  with `max_size=100M`, assert three segments exist with correct
  sizes.
- Integration test: 30-second capture with `--max-size 1M --keep 3`,
  assert only `.pcap`, `.pcap.1`, `.pcap.2` exist after.

### Files touched

- `crates/nlink-lab/src/capture.rs` (new `RotatingPcapWriter`)
- `bins/lab/src/main.rs` (Capture clap variant)
- `crates/nlink-lab/tests/integration.rs` (one new test)
- `docs/cookbook/long-soak-capture.md` (new — usage example)
- `CHANGELOG.md`

### Risks

- File-rename atomicity on the filesystem. POSIX rename is atomic
  within a single filesystem; cross-fs would fail. Document that
  the output path must be on a single filesystem.
- Segments are individually valid pcaps but not unioned — consumers
  need a tool to concatenate or process per-segment. Note in the
  cookbook recipe.

### Effort

1.5 days.

---

## PR G — `--wait-fd-stable` and `--wait-port` (§2.4) [P2, 1 day total]

### Sub-feature 1: `spawn --wait-port [HOST:]PORT`

Mirror of `--wait-tcp`, but probes `/proc/<pid>/net/tcp` for a
listening socket on the given port — *no actual connect*. Useful
when:

- The service binds early but doesn't accept the test's connection
  fast enough (e.g. SSL handshake during cold start).
- Connecting would have side effects (logged failed-connection
  attempts).
- The service binds to a non-routable address that the host can't
  reach.

Implementation:

- Library: `RunningLab::wait_for_port(pid, port, timeout, interval)`.
- Reads `/proc/<pid>/net/tcp` and `/proc/<pid>/net/tcp6` via
  `nlink-lab exec`, parses the local-address column, looks for the
  port in `LISTEN` state (column `st = 0A`).
- CLI: `--wait-port` flag on `Spawn`, parser for `[HOST:]PORT` (host
  optional, defaults to "any").

### Sub-feature 2: `spawn --wait-fd-stable [--stable-for SECS]`

Heuristic readiness probe: returns when `/proc/<pid>/fd` count
hasn't changed for `--stable-for` (default: 500ms). Useful for
binaries that don't emit a "ready" log line and don't open a
listening port.

Document the heuristic clearly: "*This is a guess.* A process may
open more files later in its lifecycle. Prefer `--wait-log` or
`--wait-port` when possible."

Implementation: same exec-into-ns pattern, poll fd count, track in
a small state machine.

### Tests

- Integration test for `--wait-port`: spawn `python3 -m
  http.server 8080`, wait-port 8080, assert success in <2s.
- Integration test for `--wait-fd-stable`: spawn `bash -c 'sleep
  10'`, wait-fd-stable 200ms, assert success well before 10s.

### Files touched

- `crates/nlink-lab/src/running.rs` (two new methods)
- `bins/lab/src/main.rs` (two flags on `Spawn`)
- `crates/nlink-lab/tests/integration.rs` (two tests)
- `CHANGELOG.md`

### Effort

~½ day each, ~1 day total.

---

## PR H — Loopback dedup at capture (§2.6) [P3, ~½ day]

### What

Add `--dedupe-loopback` flag on `nlink-lab capture` that drops
outgoing packets when capturing on `lo`. The kernel BPF tap on
loopback emits each packet twice (once with `PACKET_OUTGOING`, once
with `PACKET_HOST`); the second is the receive-side copy that
matches what consumers care about.

### Implementation

netring 0.2.0 already exposes `Capture::builder().ignore_outgoing()`
(uses the `PACKET_IGNORE_OUTGOING` socket option, kernel ≥ 4.20).
Wire a CLI flag through to it:

```rust
let mut builder = Capture::builder()
    .interface(&config.interface)
    .profile(config.profile)
    .snap_len(config.snap_len);
if config.dedupe_loopback {
    builder = builder.ignore_outgoing(true);
}
```

CLI: `--dedupe-loopback` flag on `Capture`.

Default: false (preserve current "every packet, both directions"
behavior — some consumers want this).

Documentation: cookbook recipe at
`docs/cookbook/loopback-capture.md` explaining the duplication
phenomenon and when to use the flag.

### Tests

- Unit test on `CaptureConfig` field plumbing.
- Integration test: deploy a node, run `nlink-lab capture lo
  --dedupe-loopback` while exec'ing `ping -c 3 127.0.0.1`, assert
  the resulting pcap has 6 packets (3 echo request + 3 echo
  reply), not 12.

### Files touched

- `crates/nlink-lab/src/capture.rs` (`CaptureConfig::dedupe_loopback`
  field, plumb to netring builder)
- `bins/lab/src/main.rs` (Capture clap variant + match arm)
- `crates/nlink-lab/tests/integration.rs` (one new test, root-gated)
- `docs/cookbook/loopback-capture.md` (new)
- `CHANGELOG.md`

### Effort

~½ day.

---

## PR I — Harness best-practices doc (§3.1) [P1, ~3 hours us, plus their PR]

### Approach

Two-phase:

1. **We write a skeleton** — section headers and one or two
   concrete examples per section, drawn from the existing
   USER_GUIDE + ARCHITECTURE + cookbook.
2. **They contribute the body** — they offered to upstream their
   accumulated tribal knowledge. We accept their PR.

### Skeleton (`docs/HARNESS_GUIDE.md`)

Section headers per their list:

```
# Writing a test harness around nlink-lab

## Spawn ordering & dependencies
- Use --wait-log or --wait-port, not sleeps.
- Example: zenohd → mediator → discovery (depends-on chain).

## Capture endpoint selection
- Where traffic appears (lo vs eth, intra-ns vs inter-ns).
- Recipe: capture on `lo` for intra-process IPC.
- Recipe: capture on `eth0` for cross-node traffic.

## Failure-mode debugging
- nlink-lab status --json LAB
- nlink-lab ps --json --alive-only LAB
- nlink-lab logs --pid <pid> --follow

## Cleanup discipline
- Lab Drop → destroy.
- Mid-deploy panic → state file written before any external
  resource → `destroy --force` cleans up.
- `destroy --orphans` for stale resources.

## Reading /proc from spawned processes
- See ARCHITECTURE.md "Process & namespace model".
- Use `nlink-lab proc-stat` for resource sampling.
- Use `nlink-lab exec NODE -- cat /proc/<pid>/...` for raw access.

## Concurrency
- Per-host parallel-lab limit (per Plan 156 / Plan 157 PR D).
- nextest test-groups for capping concurrency.
```

### Files touched

- `docs/HARNESS_GUIDE.md` (new, skeleton)
- `README.md` (link to HARNESS_GUIDE)
- `CHANGELOG.md`

### Effort

3 hours for the skeleton + integration into the docs structure.
Their PR is review-only on our side.

---

## Suggested PR sequence

1. **PR A** (docs sweep) — ships first, defuses the
   `CLONE_NEWPID` confusion, makes everything downstream easier.
   ~2 hours.
2. **PR B** (`spawn --json` host_pid) — trivial, slots in
   alongside PR A's docs. ~30 min.
3. **PR D** (parallel-lab investigation) — start in parallel with
   PRs A+B since investigation can run in the background. 1–3 d.
4. **PR C** (`proc-stat`) — high-value standalone feature. 1.5 d.
5. **PR H** (lo dedup) — quick win, slot anywhere. ½ d.
6. **PR G** (`--wait-port` + `--wait-fd-stable`) — independent. 1 d.
7. **PR F** (capture rotation) — independent, 1.5 d.
8. **PR E** (subnet allocator) — depends on PR D landing. 1.5 d.
9. **PR I** (harness guide) — accept their PR, write skeleton if
   needed. 3 h us, plus their work.

Each PR is independently reverable. Total: ~7–10 days of focused
work plus the variable PR D investigation.

---

## Risk summary

| PR | Risk | Mitigation |
|----|------|------------|
| A | None | — |
| B | Redundant field may confuse | Schema + doc-comment explain it's an explicit alias |
| C | comm-with-parens parse | rfind(')') from end, slice cleanly |
| D | Investigation may not yield a fix; cap is the result | Document the cap explicitly; expose it via status |
| E | Pool corruption from crash | Atomic write + orphan sweep |
| F | Cross-fs rename | Document single-fs requirement |
| G | `--wait-fd-stable` is heuristic | Explicit doc note; preferred alternatives listed |
| H | Some consumers want both directions | Flag is opt-in; default unchanged |
| I | Their PR may diverge from our style | Provide skeleton; reviewers steer in PR |

---

## Test surface added

| Test | PR | Type |
|------|----|------|
| `json_schemas_parse` updated to include new schemas | A, B, C, D | unit |
| `proc_stat::tests::*` (~10 cases) | C | unit |
| `proc_stat_returns_live_data` (root-only) | C | integration |
| `N_labs_in_parallel_all_succeed` (root-only, gated) | D | integration |
| `subnet_pool::tests::*` | E | unit |
| `parallel_subnet_auto_allocation` (root-only) | E | integration |
| `RotatingPcapWriter` unit tests | F | unit |
| `capture_rotation_keeps_only_n_segments` (root-only) | F | integration |
| `wait_for_port_returns_when_listening` (root-only) | G | integration |
| `wait_for_fd_stable_returns_after_quiesce` (root-only) | G | integration |
| `loopback_dedupe_halves_capture` (root-only) | H | integration |

---

## Acceptance criteria (from reporter, condensed)

- [ ] **PR A**: `docs/ARCHITECTURE.md` has the namespace model
      section. `nlink-lab spawn --help` references the spawn
      schema. README links the CHANGELOG.
- [ ] **PR B**: `nlink-lab spawn --json` returns `host_pid` in
      addition to `pid`. Same for `ps --json`.
- [ ] **PR C**: `nlink-lab proc-stat LAB NODE PID --json` returns
      structured output matching the schema. `--watch 5s` emits one
      record per 5s until Ctrl-C.
- [ ] **PR D**: 8 parallel deploys of `simple.nll` all succeed.
      `nlink-lab status --json LAB` shows `host_resources`.
- [ ] **PR E**: `node site_a { subnet auto/24 }` allocates a
      non-colliding /24 across parallel deploys.
- [ ] **PR F**: `nlink-lab capture LAB EP --max-size 1M --keep 3
      -w foo.pcap` produces `foo.pcap`, `foo.pcap.1`, `foo.pcap.2`,
      with the oldest deleted on rotation.
- [ ] **PR G**: `--wait-port 8080` and `--wait-fd-stable` block
      spawn until the respective signal fires.
- [ ] **PR H**: capture on `lo` with `--dedupe-loopback` produces
      half the packets of the same capture without the flag.
- [ ] **PR I**: `docs/HARNESS_GUIDE.md` exists and links from
      README.

---

## Notes on coordinating with the harness team

They've offered to write the body of §3.1 (PR I) and to re-validate
the full release within ~2 hours. Same pattern as rounds 0.2 → 0.3.
Once PRs A+B+C land, they can probably revert the `~150 LOC of
fragile /proc parsing` they mentioned and switch to `proc-stat`.

If PR D's investigation surfaces a hard parallel-lab cap, surface
it in `--help` and in the HARNESS_GUIDE so they can configure
nextest test-groups deterministically.
