# Plan 158d — `nlink-lab watch` via nftables multicast

**Date:** 2026-05-27 (rewritten 2026-05-29 — nlink 0.18 lands
`subscribe_all_with_resync` + `NewSet/DelSet` variants,
collapsing the per-thread snapshot scaffolding)
**Status:** Proposed (PR D of the Plan 158 arc — optional)
**Effort:** Medium (2–3 days now that the resync helper exists
upstream)
**Priority:** P3 — power-user feature; no concrete user
request yet, but the underlying nlink primitives just
landed and the data flow is clean.

---

## TL;DR

nlink 0.16 shipped `Connection<Nftables>::subscribe` +
typed `NftablesEvent` enum + `events_with_resync` for
ENOBUFS-tolerant streaming. nlink-lab can use these to
provide a push-driven topology-drift monitor:

```bash
sudo nlink-lab watch my-lab
# router  NewRule (input) tcp dport 80 accept
# server  DelRule (output) icmp drop
# ^C
```

Three concrete features fall out of this primitive:

1. **CLI `nlink-lab watch <lab>`** — human-friendly /
   NDJSON live event tail. Per-node task running in each
   namespace.
2. **Library `RunningLab::nftables_events()`** — Stream of
   `(node, NftablesEvent)` for downstream test helpers
   (e.g. `#[lab_test]` could assert "after my apply, no
   `DelRule` event fires on node X").
3. **Backend Zenoh wiring** — `bins/nlink-lab-backend`
   gains a `nft.event/<lab>/<node>` publication. Powers a
   live drift indicator in `bins/topoviewer`.

This is the largest of the Plan 158 PRs (PR D). It is also
the only one without a concrete user request; ship A/B/C
first and gate D on whether anyone asks for it.

---

## Audit — nlink 0.18 primitives (citations to `/home/mpardo/git/rip/`)

- `NftablesEvent` enum — **10 typed variants** now (the
  original 8 + `NewSet(SetInfo)`/`DelSet(SetInfo)` shipped
  in 0.18 Plan 185 for resync completeness) at
  `crates/nlink/src/netlink/nftables/events.rs:71`. All are
  `#[non_exhaustive]`.
- `NftablesGroup::All` resolves to kernel multicast group
  `NFNLGRP_NFTABLES (= 7)`.
- **The big simplification** — nlink 0.18 ships two new
  helpers on `Connection<Nftables>`:
  - `Connection<Nftables>::subscribe_all_with_resync(factory)`
    — borrowed form, holds `&mut self`.
  - `Connection<Nftables>::into_events_with_resync(factory)`
    — **owned form**, returns `'static + Send` —
    `tokio::spawn`-friendly. **This is what we want.**
  - Both return
    `Stream<Item = Result<ResyncedEvent<NftablesEvent>>>`.
  - The `factory` parameter is a `ConnectionFactory<Nftables>`
    (generic alias at the crate root —
    `nlink::ConnectionFactory<P>`) that opens a fresh
    `Connection<Nftables>` on demand for the snapshot
    re-dump after ENOBUFS. nlink handles the snapshot enumeration
    (tables / chains / rules / flowtables / sets) and the
    `Resynced(...)` replay internally.
- Recipe: `/home/mpardo/git/rip/docs/recipes/nftables-watch-with-resync.md`.
- Per-namespace multi-subscribe pattern: one
  `Connection<Nftables>` per namespace, opened via
  `nlink::netlink::namespace::connection_for(name)`.
  `subscribe_all` takes `&mut self` and is sync; the
  resync helpers consume the connection.
- **No built-in table-name filter** on the event stream.
  Filtering is still consumer-side.

---

## Goals

1. **`nlink-lab watch <lab>`** subcommand emits a live
   event stream from every node in the lab, multiplexed
   onto stdout. Human-readable by default, NDJSON via
   `--json`.
2. **`--node <name>` and `--table <name>`** filters
   constrain the stream client-side.
3. **ENOBUFS recovery is automatic** — on a multicast
   overflow the stream emits a `--- resync start ---`
   marker, replays the current nft state via a fresh
   snapshot dump, emits `--- resync end ---`, and
   resumes live events. Implemented via
   `events_with_resync` so the lab user can trust the
   stream is complete.
4. **Library `RunningLab::nftables_events()`** returns a
   `Stream<Item = Result<NodeNftablesEvent>>` for
   programmatic consumers.
5. **`bins/nlink-lab-backend` adds a Zenoh publisher** on
   `nft.event/<lab>/<node>` carrying NDJSON-encoded
   events.
6. **`bins/topoviewer` consumes the new key expression**
   and shows a 1s-faded "edit" pulse on the affected
   node — visual real-time drift indicator. (Optional,
   ship-able as a separate follow-up.)

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│  RunningLab                                      │
│  ┌─────────────────────────────────────────────┐ │
│  │  spawn per-node thread (setns + LocalSet)   │ │
│  │  ┌───────────────────────────────────────┐  │ │
│  │  │ enter ns_for(node)                    │  │ │
│  │  │ let mut conn =                        │  │ │
│  │  │   namespace::connection_for(ns)?;     │  │ │
│  │  │ conn.subscribe_all()?;                │  │ │
│  │  │ let ns = ns_name.clone();             │  │ │
│  │  │ let stream = conn                     │  │ │
│  │  │   .into_events_with_resync(           │  │ │
│  │  │     move || { let ns = ns.clone();    │  │ │
│  │  │       Box::pin(async move {           │  │ │
│  │  │         namespace::connection_for(&ns)│  │ │
│  │  │       })                              │  │ │
│  │  │     })?;                              │  │ │
│  │  │ stream.map(|ev| (node, ev))           │  │ │
│  │  │       .forward(tx)                    │  │ │
│  │  └───────────────────────────────────────┘  │ │
│  └────────────────────────┬────────────────────┘ │
│                           │ mpsc::Receiver       │
│                           ▼                      │
│  ┌─────────────────────────────────────────────┐ │
│  │  ReceiverStream → Stream<NodeNftEvent>      │ │
│  └─────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘
        │                       │
        ▼                       ▼
  CLI render loop          Backend Zenoh publisher
```

Key constraints:

- **One `Connection<Nftables>` per node.** Each lives on
  its own thread (because the namespace `setns()` call
  is thread-local). Per-thread task pool sized to
  `min(node_count, num_cpus)` so a 100-node lab doesn't
  spawn 100 threads.
- **Snapshot for resync is now nlink's job.** The
  `into_events_with_resync(factory)` helper handles the
  fresh-connection-on-ENOBUFS dance internally — we hand
  it a closure that re-opens a `Connection<Nftables>` in
  the right namespace, and it does the rest. **~150 LOC
  of plumbing the original plan called for is gone.**
- **Backpressure.** mpsc channel capacity = 1024 events.
  If consumers fall behind, sender drops and emits a
  `OverflowWarn(dropped: u64)` event the consumer can
  surface. (We're already on a best-effort channel
  from the kernel — `ENOBUFS` is the kernel's version
  of the same.)

---

## CLI design

```text
sudo nlink-lab watch <LAB> [OPTIONS]

OPTIONS:
    --node <NAME>          Watch only this node (repeatable)
    --table <NAME>         Only events on this nft table (repeatable)
    --kind <KIND>          Only events of this kind (rule|chain|table|flowtable)
                           (repeatable, default = all)
    --json                 NDJSON output (one event per line)
    --duration <SECS>      Auto-exit after N seconds
    --include-initial      On startup, dump current state as a
                           Resynced(...) batch before live events
                           begin (uses the same resync path)
    --quiet                Suppress the lab-info banner
```

### Human-readable output

```text
2026-05-27T14:23:45.221Z  router      NewRule    inet/nlink-lab/input    tcp dport 80 accept    [comment "nlink-lab:fw:input:0"]
2026-05-27T14:23:45.222Z  router      DelRule    inet/nlink-lab/input    tcp dport 22 accept    [comment "nlink-lab:fw:input:1"]
2026-05-27T14:23:47.108Z  server      NewChain   inet/nlink-lab/output   filter
2026-05-27T14:24:01.945Z  router      --- resync start (ENOBUFS) ---
2026-05-27T14:24:01.951Z  router      Resynced   inet/nlink-lab/input    tcp dport 80 accept
2026-05-27T14:24:01.952Z  router      Resynced   inet/nlink-lab/input    tcp dport 443 accept
2026-05-27T14:24:01.953Z  router      --- resync end ---
```

### NDJSON output

```json
{"ts":"2026-05-27T14:23:45.221Z","node":"router","kind":"NewRule","table":"nlink-lab","family":"inet","chain":"input","rule_handle":7,"comment":"nlink-lab:fw:input:0","expression":"tcp dport 80 accept"}
{"ts":"2026-05-27T14:23:47.108Z","node":"server","kind":"NewChain","table":"nlink-lab","family":"inet","chain":"output","hook":"output"}
{"ts":"2026-05-27T14:24:01.945Z","node":"router","kind":"ResyncStart","reason":"ENOBUFS"}
{"ts":"2026-05-27T14:24:01.951Z","node":"router","kind":"Resynced","table":"nlink-lab","family":"inet","chain":"input","rule_handle":7,"expression":"tcp dport 80 accept"}
{"ts":"2026-05-27T14:24:01.953Z","node":"router","kind":"ResyncEnd"}
```

Schema lives at `docs/json-schemas/watch-event.schema.json`
alongside the other published schemas. Round-tripped
through `serde_json` + asserted by a unit test.

---

## Library API

```rust
// In crates/nlink-lab/src/running.rs

/// One event published by the kernel's nftables multicast
/// group, annotated with the node it was observed on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeNftablesEvent {
    pub node: String,
    pub timestamp: time::OffsetDateTime,
    pub event: NftablesEventKind,
}

/// Lab-flavored mirror of the nlink event enum, plus
/// the resync markers. We don't expose `nlink::netlink::...`
/// types through our public surface (Plan 152 convention).
#[derive(Debug, Clone, serde::Serialize)]
pub enum NftablesEventKind {
    NewTable      { family: Family, name: String },
    DelTable      { family: Family, name: String },
    NewChain      { family: Family, table: String, chain: String,
                    hook: Option<String>, policy: Option<String> },
    DelChain      { family: Family, table: String, chain: String },
    NewRule       { family: Family, table: String, chain: String,
                    handle: u64, comment: Option<String>,
                    expression: Option<String> },
    DelRule       { family: Family, table: String, chain: String,
                    handle: u64, comment: Option<String> },
    NewFlowtable  { family: Family, table: String, name: String },
    DelFlowtable  { family: Family, table: String, name: String },
    NewSet        { family: Family, table: String, name: String },
    DelSet        { family: Family, table: String, name: String },
    /// Emitted before a resync replay following ENOBUFS.
    ResyncStart   { reason: String },
    /// One item per snapshot frame during a resync.
    Resynced      { /* same fields as NewRule */ … },
    /// Emitted when the resync replay finishes.
    ResyncEnd,
    /// Emitted when the in-process mpsc channel overflowed.
    /// `dropped` counts how many events were lost.
    OverflowWarn  { dropped: u64 },
}

impl RunningLab {
    /// Subscribe to nftables multicast events on every node
    /// in this lab. Returns a single stream multiplexing all
    /// nodes. The stream lives as long as the lab is loaded;
    /// dropping it tears down every per-node subscription.
    ///
    /// Pass `nodes = None` to subscribe to every node;
    /// pass `Some(&["router", "server"])` to subscribe to a
    /// subset.
    pub fn nftables_events(
        &self,
        nodes: Option<&[&str]>,
    ) -> impl Stream<Item = Result<NodeNftablesEvent>> + 'static;
}
```

The stream is `'static` because per-node subscriptions own
their own `Connection<Nftables>` and forward into an mpsc
channel; the returned `ReceiverStream` doesn't borrow from
`self`.

---

## Phases

### Phase 1 — Library plumbing (1.5 days)

#### 1.1 `running.rs` — per-node task spawn

In `crates/nlink-lab/src/running.rs`:

```rust
fn spawn_node_watcher(
    node: String,
    ns_name: String,
    tx: mpsc::Sender<Result<NodeNftablesEvent>>,
) -> Result<std::thread::JoinHandle<()>> {
    // Spawn a *thread* (not a task) for the namespace pin.
    // setns is thread-local; the subsequent Connection must
    // live on that thread.
    let handle = std::thread::Builder::new()
        .name(format!("nft-watch-{node}"))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            runtime.block_on(async move {
                let mut conn = match nlink::netlink::namespace::connection_for(&ns_name) {
                    Ok(c) => c,
                    Err(e) => { let _ = tx.send(Err(e.into())).await; return; }
                };
                if let Err(e) = conn.subscribe_all() {
                    let _ = tx.send(Err(e.into())).await;
                    return;
                }

                // nlink 0.18: into_events_with_resync handles the
                // ENOBUFS-recovery dance entirely. We just hand it a
                // closure that opens a fresh Connection<Nftables> in
                // the right namespace; nlink dumps + emits Resynced
                // items + restarts live forwarding internally.
                let ns_for_factory = ns_name.clone();
                let stream = match conn.into_events_with_resync(
                    move || {
                        let ns = ns_for_factory.clone();
                        Box::pin(async move {
                            nlink::netlink::namespace::connection_for::<nlink::Nftables>(&ns)
                        })
                    },
                ) {
                    Ok(s) => s,
                    Err(e) => { let _ = tx.send(Err(e.into())).await; return; }
                };

                let mut stream = stream;
                while let Some(item) = stream.next().await {
                    let event = match item {
                        Ok(ResyncedEvent::Event(e))    => NftablesEventKind::from_nlink(e),
                        Ok(ResyncedEvent::Resynced(e)) => NftablesEventKind::resynced_from(e),
                        Ok(ResyncedEvent::Marker(ResyncMarker::ResyncStart)) =>
                            NftablesEventKind::ResyncStart { reason: "ENOBUFS".into() },
                        Ok(ResyncedEvent::Marker(ResyncMarker::ResyncEnd))   =>
                            NftablesEventKind::ResyncEnd,
                        Err(e) => { let _ = tx.send(Err(e.into())).await; continue; }
                    };
                    let wrapped = NodeNftablesEvent {
                        node: node.clone(),
                        timestamp: time::OffsetDateTime::now_utc(),
                        event,
                    };
                    if tx.send(Ok(wrapped)).await.is_err() {
                        // Consumer dropped — exit the watcher.
                        break;
                    }
                }
            });
        }).map_err(|e| Error::deploy_failed(format!("spawn watch thread: {e}")))?;
    Ok(handle)
}
```

Lifecycle concerns:

- A watcher thread must exit when (a) the consumer drops
  the returned stream, or (b) the lab is destroyed.
- (a) is handled by `tx.send(...).await.is_err()` —
  closed channel signals consumer-drop.
- (b) requires registering each `JoinHandle` somewhere
  `RunningLab::destroy` can find. Simplest: a
  `Mutex<Vec<JoinHandle<()>>>` field on `RunningLab`,
  drained on destroy. JoinHandles use a `Drop` impl that
  signals a per-thread `AtomicBool` shutdown flag the
  watcher loop checks each iteration.

**Key simplification vs. the original plan.** The owned
`into_events_with_resync(factory)` helper:

- Internally opens a fresh `Connection<Nftables>` on
  ENOBUFS and runs the snapshot.
- Knows how to enumerate tables / chains / rules /
  flowtables / sets (since 0.18, `NftablesEvent::NewSet`
  is in the resync replay too).
- Returns a `'static + Send` stream that's safe to forward
  across thread boundaries.

We don't need the ~80 LOC of `snapshot_dump(...)` helper
the original plan called for, nor the per-namespace
`Connection<Nftables>` factory boilerplate — both are
upstream concerns now.

#### 1.2 Tests

| Test | Description | Gated |
|------|-------------|-------|
| `nftables_event_kind_round_trip` | Construct each `NftablesEventKind` variant, serialize via `serde_json`, deserialize, assert byte-equivalence. | none |
| `nftables_events_emits_newrule` | Deploy 1-node lab. Subscribe via `lab.nftables_events(None)`. Run `lab.exec("a", "nft", &["add", "rule", …])`. Assert a `NewRule` event arrives in < 2s. | root |
| `nftables_events_emits_delrule` | Same but for delete. | root |
| `nftables_events_multi_node` | Deploy 3-node lab. Subscribe with `Some(&["a", "b"])` (skip node `c`). Edit ruleset on all three. Assert exactly nodes `a` and `b` produce events. | root |
| `nftables_events_resync_marker` | Deploy 1-node lab. Subscribe. Inject 10k rules rapidly inside the lab to force ENOBUFS. Assert at least one `ResyncStart` + matching `ResyncEnd` is emitted, and the stream resumes thereafter. | root, slow |

### Phase 2 — CLI subcommand (1 day)

#### 2.1 `bins/lab/src/main.rs`

Add `Commands::Watch { … }` with the option set described
above, plus a `run_watch` async dispatch handler. Render
loop:

```rust
async fn run_watch(
    lab: RunningLab,
    cli: WatchOpts,
    stdout: &mut impl AsyncWrite + Unpin,
) -> Result<()> {
    let nodes_arg: Option<Vec<&str>> =
        cli.node.as_ref().map(|v| v.iter().map(String::as_str).collect());
    let mut stream = lab.nftables_events(nodes_arg.as_deref());

    let deadline = cli.duration.map(|s| Instant::now() + Duration::from_secs_f64(s));

    while let Some(item) = stream.next().await {
        if let Some(dl) = deadline && Instant::now() >= dl { break; }
        let event = item?;
        if !filter_match(&cli, &event) { continue; }
        match cli.json {
            true  => write_ndjson(stdout, &event).await?,
            false => write_human(stdout, &event).await?,
        }
        stdout.flush().await?;
    }
    Ok(())
}
```

#### 2.2 Documentation

- New page `docs/cli/watch.md` mirroring the
  `docs/cli/capture.md` style: usage, options, output
  examples, "see also" pointing at `capture`,
  `inspect`, `diagnose`.
- New cookbook recipe
  `docs/cookbook/nft-drift-detection.md`: deploy a lab,
  background `nlink-lab watch`, edit rules via
  `nft -f` inside a node, observe the live tail.

#### 2.3 JSON schema

`docs/json-schemas/watch-event.schema.json` — referenced
by the CLI's `--json` output. Schema covers every
`NftablesEventKind` variant. Test:

```rust
#[test]
fn watch_event_ndjson_matches_schema() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/json-schemas/watch-event.schema.json"))
            .unwrap();
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    for ev in sample_events() {
        let v = serde_json::to_value(&ev).unwrap();
        assert!(validator.is_valid(&v), "bad: {v}");
    }
}
```

(Pulls in `jsonschema` crate as `dev-dependency` only —
no runtime cost.)

### Phase 3 — Backend Zenoh publisher (1 day, optional)

In `bins/nlink-lab-backend/src/collector.rs` (currently a
stub at 97 LOC), wire the event stream into a Zenoh
publisher:

```rust
pub async fn run_watch_publisher(
    lab: &RunningLab,
    session: zenoh::Session,
) -> anyhow::Result<()> {
    let mut stream = lab.nftables_events(None);
    while let Some(ev) = stream.next().await {
        let ev = ev?;
        let key = format!("nft.event/{}/{}", lab.name(), ev.node);
        let payload = serde_json::to_vec(&ev)?;
        session.put(&key, payload).await?;
    }
    Ok(())
}
```

Wire into `bins/nlink-lab-backend/src/main.rs` so
`--watch-nftables` (new flag) starts the publisher.

### Phase 4 — topoviewer drift indicator (1 day, optional)

In `bins/topoviewer/src/app.rs` (the iced GUI), subscribe
to `nft.event/<lab>/*` and trigger a 1-second-faded
pulse animation on the affected node circle. Pure
visual; doesn't change topology state.

This is the icing — skip if topoviewer isn't actively
used. Document as a follow-up.

---

## Tests summary

| Phase | Test | Gated | Effort |
|-------|------|-------|--------|
| 1 | `nftables_event_kind_round_trip` | none | unit |
| 1 | `nftables_events_emits_newrule` | root | int |
| 1 | `nftables_events_emits_delrule` | root | int |
| 1 | `nftables_events_multi_node` | root | int |
| 1 | `nftables_events_resync_marker` | root, slow | int |
| 2 | `watch_event_ndjson_matches_schema` | none | unit |
| 2 | `watch_cli_human_renders_newrule` | root | int |
| 2 | `watch_cli_json_emits_one_line_per_event` | root | int |
| 3 | `backend_watch_publishes_zenoh_key` | root, requires running zenohd | int (slow) |

The "slow" tag is for tests that take > 5 s — these run
in a separate integration job that doesn't block PR
merging (mirror the convention nlink-lab already uses
for `wait_for_log_line_times_out_*`).

---

## Acceptance

- `sudo nlink-lab watch <lab>` runs in a terminal and
  prints one human-friendly line per nftables event on any
  node.
- `--json` produces NDJSON validating against
  `docs/json-schemas/watch-event.schema.json`.
- `--node`, `--table`, `--kind` filters work and are
  individually testable.
- ENOBUFS recovery is automatic — verified by the slow
  integration test that floods rule operations.
- Library `RunningLab::nftables_events()` is documented +
  has a `#[lab_test]`-style example in the cookbook.
- Tearing down the lab (`nlink-lab destroy`) joins all
  watcher threads cleanly — no leaks reported by
  `valgrind --tool=helgrind` or `RUST_BACKTRACE` on
  panic.
- CHANGELOG entry under `[Unreleased] → Added`:
  > New `nlink-lab watch <LAB>` subcommand and
  > `RunningLab::nftables_events()` library API stream
  > typed nftables events from every node in a lab.
  > ENOBUFS recovery is automatic. NDJSON output
  > documented at `docs/json-schemas/watch-event.schema.json`.

---

## Out of scope

- **Conntrack event subscription.** nlink 0.16 also ships
  conntrack multicast; we could stream
  `(node, ConntrackEvent)` similarly. But conntrack
  events come at line rate; nlink-lab's debug use case is
  more "what's the firewall doing" than "what's flowing
  through" — the latter is what `capture` is for.
- **Route / link multicast events.** Same shape applies —
  add later if a user asks. RTNETLINK multicast is one
  more `Connection<Route>` subscribe call.
- **Filtering at the kernel side.** The kernel doesn't
  filter nft multicast by table — all consumers receive
  all groups they subscribe to. Client-side filtering is
  the only option.
- **Cross-namespace event ordering.** We multiplex per-
  node mpsc senders into one receiver — ordering is
  fair-but-not-strict-FIFO across nodes. Same-node
  ordering is preserved. A test asserts this.
- **Persisting events to disk.** Out of scope for a
  watch primitive; `--json | tee /path/to/log` is the
  composition story.
- **Backpressure beyond mpsc bound.** If consumers are
  slower than the kernel by a factor > 1024 events, we
  drop and emit `OverflowWarn`. No flow control upstream
  to the kernel (the kernel uses ENOBUFS for that, which
  we already handle).

---

## Files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | `nlink = "0.17"` bump (shared with 158a/b/c). |
| `crates/nlink-lab/src/running.rs` | New `nftables_events()` method + helper types + watcher-thread scaffolding. ~+200 LOC. |
| `crates/nlink-lab/src/lib.rs` | Re-export `NodeNftablesEvent` + `NftablesEventKind` + `Family`. |
| `crates/nlink-lab/src/error.rs` | Optionally new `Error::Watch { node, detail }` variant. |
| `bins/lab/src/main.rs` | New `Commands::Watch` + `run_watch` dispatcher. ~+150 LOC. |
| `docs/cli/watch.md` | New page. |
| `docs/cookbook/nft-drift-detection.md` | New recipe. |
| `docs/json-schemas/watch-event.schema.json` | New JSON schema. |
| `bins/nlink-lab-backend/src/collector.rs` | (Phase 3) Wire `run_watch_publisher`. ~+50 LOC. |
| `bins/nlink-lab-backend/src/main.rs` | (Phase 3) New `--watch-nftables` flag. |
| `bins/topoviewer/src/app.rs` | (Phase 4) Drift-pulse animation on event. ~+80 LOC. |
| `crates/nlink-lab/tests/integration.rs` | 5+ new `#[lab_test]` integration tests. |
| `crates/nlink-lab/Cargo.toml` | New dev-dep `jsonschema = "0.18"` (or current) for the schema-validation unit test. |
| `CHANGELOG.md` | New entry under `[Unreleased] → Added`. |
