# Writing a test harness around nlink-lab

This guide is for people building Rust (or shell) test harnesses on
top of `nlink-lab`. The reference consumer is the `des-test-harness`
team's setup: ~30 `#[ignore]`-gated Layer-2 scenarios driving
3-machine topologies through deploy → spawn → impair → capture →
destroy cycles. The patterns here are theirs, with our notes on what
works and what to avoid.

If you're new to nlink-lab itself, read [`USER_GUIDE.md`](USER_GUIDE.md)
first. This guide assumes familiarity with `deploy`, `spawn`,
`impair`, `capture`, and `destroy`.

---

## Process & namespace model

> Read [`ARCHITECTURE.md` § "Process & namespace model"][arch-ns]
> first. The short version:
>
> - Spawned processes enter only `CLONE_NEWNET` (and `CLONE_NEWNS`
>   if `dns hosts`). **No `CLONE_NEWPID`** — host PID == namespace PID.
> - Spawned processes run as the caller's UID. `check_root` enforces
>   root, so they're root.
> - `/proc/<pid>/{stat,status,cmdline,comm}` is world-readable from
>   the host. `/proc/<pid>/fd/` is mode 0700 owned by root — readable
>   only as root or via `nlink-lab proc-stat`.

For sampling resource usage of a spawned process, **prefer
`nlink-lab proc-stat`** (Plan 157 PR C) over hand-rolled `/proc`
parsing. It routes through nlink-lab's root context, so the
`/proc/<pid>/fd/` permission gymnastics go away:

```bash
nlink-lab proc-stat des-3m site_a 292086 --json
nlink-lab proc-stat des-3m site_a 292086 --json --watch 5  # NDJSON, every 5s
```

[arch-ns]: ARCHITECTURE.md#process--namespace-model

---

## Spawn ordering & readiness

Use **`--wait-log <REGEX>`** as the default readiness primitive.
Sleeps are a anti-pattern — they're either too short (flaky) or too
long (slow tests).

```bash
nlink-lab spawn $LAB site_a --json \
    --wait-log '^STARTED$' --wait-timeout 10 \
    -- /usr/local/bin/zenohd --config $cfg
```

For services that don't emit a "ready" log line, use one of the
structural probes (Plan 157 PR G):

- `--wait-port <PORT>` — reads `/proc/<pid>/net/tcp{,6}`, returns
  when there's a `LISTEN` row matching the port. Doesn't actually
  connect — works for non-routable binds and avoids logged
  connection-refused noise.
- `--wait-fd-stable <SECS>` — heuristic; returns when the open-fd
  count hasn't changed for SECS seconds. Use as a last resort.

Probes AND-compose; specify multiple:

```bash
nlink-lab spawn $LAB site_a --json \
    --wait-log '^STARTED$' --wait-port 7447 --wait-timeout 10 \
    -- zenohd
```

For `--wait-tcp` vs `--wait-port`: `--wait-tcp <IP:PORT>` does a
real `connect(2)` from inside the namespace. Stricter (proves the
service accepts connections) but logs refused attempts on the
target side. `--wait-port` is silent. Pick by what the target
service tolerates.

### Dependency chains

Spawn services in dependency order, each blocking on the previous
one's readiness. Example pattern from `des-test-harness`:

```rust
// 1. Zenoh broker first.
let zenohd = lab.spawn("router")
    .args(&["zenohd", "--config", "/etc/zenoh.conf"])
    .wait_log("^STARTED$")
    .spawn_json()?;

// 2. Mediator depends on zenohd being up.
let mediator = lab.spawn("site_a")
    .args(&["./Mediator", "--config", "config.xcf"])
    .workdir("/work")
    .wait_log("Connected to broker")
    .spawn_json()?;

// 3. Discovery depends on mediator.
let discovery = lab.spawn("site_a")
    .args(&["des_discovery"])
    .wait_port(15987)
    .spawn_json()?;
```

(The Rust shape above is the harness's own thin wrapper around
`nlink-lab spawn --json`. nlink-lab itself doesn't ship a Rust
client — see [§ "Things we don't ship"](#things-we-dont-ship).)

---

## Capture endpoint selection

Capture-on-failure is the harness's most leveraged debugging tool —
it costs nothing on success and tells you everything on failure.

### Where to put captures

| Endpoint | Catches |
|----------|---------|
| `<node>:lo`     | Intra-process IPC on that node (every Zenoh subscription, every Unix-socket exchange). |
| `<node>:eth0`   | Cross-node traffic for the link to that interface. |
| `<router>:eth*` | Inter-site traffic. The router's interfaces are the choke points. |

Pick the *minimum* set that lets you reconstruct the failure. Two
captures per scenario is usually enough.

### Loopback dedup

Captures on `lo` see every packet **twice** — once outgoing, once
incoming. Use `--dedupe-loopback` (Plan 156 PR H) to drop the
outgoing copies at the kernel:

```bash
nlink-lab capture $LAB site_a:lo --dedupe-loopback -w site_a-lo.pcap
```

Without the flag, your downstream processing has to dedupe.
`--dedupe-loopback` does it once at capture time.

### Long soaks

For multi-hour scenarios, use rotation (Plan 157 PR F):

```bash
nlink-lab capture $LAB router:eth0 -w router.pcap \
    --max-size 100M --keep 5
```

Rotated segments (`router.pcap.1`, `router.pcap.2`, ...) are each
complete pcaps. Use `mergecap` if you need the union; otherwise
read them per-segment.

### Capture lifetime

Per the round-3 PR A fix, captures survive SIGTERM cleanly — every
segment up to the last fully-written packet is preserved. Captures
also survive lab `Drop` (the harness team relies on this for
failure-bundle fold-in). Don't bother wrapping them in `KeepAlive`
constructs.

---

## Failure-mode debugging

When a scenario fails:

1. **Don't destroy yet.** The lab's state is the most detailed
   evidence you have.
2. **`nlink-lab status --json $LAB`** — topology + per-node
   addresses + `host_resources` (mgmt bridge, declared subnets).
3. **`nlink-lab ps --json $LAB`** — every spawned PID with
   `alive: bool` and log paths. Pass `--alive-only` for liveness
   polling.
4. **`nlink-lab proc-stat $LAB <node> <pid>`** — resource snapshot
   of the suspected-stuck process. `--watch 1` while you reproduce
   for an NDJSON timeline.
5. **`nlink-lab logs $LAB --pid <pid> [--stderr] [--follow]`** —
   per-process captured stdout/stderr.
6. **`nlink-lab impair --show --json $LAB`** — current netem state
   per endpoint. Lets you assert "the partition I asked for is
   actually live" without grepping `tc qdisc show`.

### Capture-on-failure pattern

The `#[lab_test]` macro's `capture = true` mode (Plan 154) preserves
pcaps to `target/lab_test_captures/<test>-<pid>/` on test failure
and discards on success. For non-`lab_test` harnesses, the pattern
is:

```rust
let captures = vec![
    Capture::on(&lab, "router:eth0", &artefacts.join("router-eth0.pcap"))?,
    Capture::on(&lab, "site_a:lo", &artefacts.join("site-a-lo.pcap"))?,
];
let result = run_scenario(&mut lab).await;
if result.is_err() {
    eprintln!("HARNESS_ARTEFACT_DIR={}", artefacts.display());
    // Don't destroy on failure — let the user inspect.
    std::mem::forget(captures);
    return Err(result.unwrap_err());
}
// Success — drop captures (their pcap files remain) and destroy.
```

---

## Cleanup discipline

### Lab Drop

Wrap the lab in a guard that calls `destroy()` on `Drop`. nlink-lab
itself doesn't impose this — the API is `let lab = topo.deploy()
.await?; ...; lab.destroy().await?;` — but harnesses uniformly
wrap because tests panic.

### Mid-deploy panic

If `deploy()` panics or returns an error, the internal `Cleanup`
guard rolls back: kills spawned processes, removes DNS injections,
deletes namespaces, frees subnet-pool entries. State is *not*
written. After a panic-mid-deploy, run `nlink-lab destroy --orphans`
to reap any kernel state that escaped (rare; usually rollback is
clean).

### Stale state from crashed runs

`nlink-lab status --scan` reports both **orphans** (host resources
without state files — typically from a SIGKILL'd deploy) and
**stale** labs (state files claiming namespaces no longer present —
typically from a reboot or WSL restart). Both are cleanable:

```bash
nlink-lab destroy --orphans       # reap orphan kernel state
nlink-lab destroy <stale-lab>     # remove stale state file
```

CI runs that use `--unique` lab names get away with a periodic
`destroy --orphans` sweep at job start.

---

## Concurrency

### Per-host parallel-lab limit

Parallel deploys are safe up to host capacity (memory, file
descriptors, kernel data structures). The known coordination points:

- **`/etc/hosts`** is mutated under a global flock (Plan 157 PR D)
  — concurrent deploys with `dns hosts` mode serialise rather than
  race.
- **Subnet pool** (`subnet auto/N`) is flock-protected.
- Per-lab state files use per-lab flocks; they're independent.
- mac80211_hwsim is a single host-wide module; parallel Wi-Fi labs
  share its radio pool.

In practice we've validated 8 parallel deploys of `simple.nll` on a
12-core machine. For higher fan-out, configure your test runner's
test-group cap empirically:

```toml
# .config/nextest.toml
[test-groups]
netns = { max-threads = 8 }

[[profile.default.overrides]]
filter = 'package(des-test-harness) and test(/^plan_d_/)'
test-group = 'netns'
```

### Subnet collisions

Use `subnet auto/24` (Plan 157 PR E) in network blocks instead of
hard-coding. The pool guarantees parallel labs get non-colliding
subnets.

```nll
network lan_a {
  members [router:eth0, site_a:eth0]
  subnet auto/24    # resolved at deploy time
}
```

---

## Things we don't ship

For honesty about scope:

- **A Rust client library**. `nlink-lab spawn --json` + a small
  in-tree wrapper is the right boundary. A client library would
  couple your harness to nlink-lab internals; we'd rather you
  build the wrapper you need.
- **In-process embedding**. Same reason. The CLI is the API.
- **A GUI / TUI**. `nlink-lab status --json` + `tail -f` covers
  every observability use case we've seen.
- **Container-based labs as default**. nlink-lab is namespace-first.
  Container nodes work but aren't the focus; if you need container
  orchestration, containerlab is the better tool.

---

## Cross-references

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — namespace model, deploy
  sequence, contributor on-ramp.
- [`USER_GUIDE.md`](USER_GUIDE.md) — guided 60-min walkthrough of
  the engine itself.
- [`docs/json-schemas/`](json-schemas/) — JSON Schemas for
  high-traffic shapes (`deploy`, `status`, `spawn`, `ps`,
  `proc-stat`, `impair --show`).
- [`docs/cookbook/`](cookbook/) — copy-paste recipes per use case.
- [`CHANGELOG.md`](../CHANGELOG.md) — per-release notes.

---

*This guide started as a Plan 157 PR I skeleton. Contributions from
heavy consumers welcome — the more concrete the better. Open a PR
or file an issue.*
