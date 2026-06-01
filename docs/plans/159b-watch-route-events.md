# Plan 159b — `nlink-lab watch` covering RTNETLINK + nftables

**Date:** 2026-05-31
**Status:** Proposed (supersedes Plan 158d's nftables-only design)
**Effort:** Medium (3 days)
**Priority:** P2 — net-new feature; demand is "low until there
is, then high". The 158d arc was deferred to "ship if asked";
now that 0.19 lifts the RTNETLINK gap, the *full* watch story
is cheap enough to ship proactively.

---

## TL;DR

Plan 158d shipped a watch design for nftables drift only —
0.18's `Connection<Route>` had no `subscribe`/`events` surface.
0.19 ships full Route resync:

- `Connection<Route>::subscribe_all_with_resync(factory) -> BorrowedResyncStream<'_>`
- `Connection<Route>::into_events_with_resync(factory) -> OwnedResyncStream`
- `NetworkEvent` typed enum with 18 RTNETLINK variants
- `ResyncedEvent<T>` wrapper carrying live events / resync markers / resynced state
- Shared `ResyncStream` implementation (Plan 191 + Plan 195 upstream)

Plan 159b ships a single `nlink-lab watch <lab>` command that
subscribes to BOTH nftables and RTNETLINK on every node in the
lab, and emits a unified event stream. The user gets a tail of
every drift in their lab — firewall changes, route changes,
link adds/removes, address changes, neighbor entries, qdisc
mutations — in real time.

```bash
sudo nlink-lab watch my-lab
[my-lab/router] NewLink     eth1 up mtu 1500
[my-lab/router] NewAddress  10.0.1.1/24 on eth1
[my-lab/router] NewRule     filter input tcp dport 80 accept
[my-lab/server] NewRoute    default via 10.0.0.1
[my-lab/server] DelRule     filter output icmp drop
^C
```

`--json` emits one NDJSON record per event for piping to `jq`,
matching the 158d Phase 2 spec.

---

## Audit — what 0.19 ships for RTNETLINK events (citations to `/home/mpardo/git/rip/`)

### `Connection<Route>` subscribe + events

`crates/nlink/src/netlink/route_resync.rs:147..174`:

```rust
impl Connection<Route> {
    pub async fn into_events_with_resync(
        self,
        factory: ConnectionFactory<Route>,
    ) -> Result<OwnedResyncStream> { … }

    pub async fn subscribe_all_with_resync(
        &self,
        factory: ConnectionFactory<Route>,
    ) -> Result<BorrowedResyncStream<'_>> { … }
}
```

`ConnectionFactory<Route>` (line ~95 same file) — a closure
type the resync wrapper invokes on every ENOBUFS overflow to
build a fresh connection that re-dumps state. We construct one
with a closure over the namespace name + `connection_for`.

### `NetworkEvent` typed enum

`crates/nlink/src/netlink/events.rs:42..91` — 18 typed
variants:

- `NewLink(LinkMessage)` / `DelLink(LinkMessage)`
- `NewAddress(AddressMessage)` / `DelAddress(AddressMessage)`
- `NewRoute(RouteMessage)` / `DelRoute(RouteMessage)`
- `NewNeighbor(NeighborMessage)` / `DelNeighbor(NeighborMessage)`
- `NewFdb(FdbEntry)` / `DelFdb(FdbEntry)`
- `NewQdisc(TcMessage)` / `DelQdisc(TcMessage)`
- `NewClass(TcMessage)` / `DelClass(TcMessage)`
- `NewFilter(TcMessage)` / `DelFilter(TcMessage)`
- `NewAction(TcMessage)` / `DelAction(TcMessage)`

### `ResyncedEvent<T>` + `ResyncMarker`

`crates/nlink/src/netlink/resync.rs:88..107`:

```rust
pub enum ResyncedEvent<T> {
    Event(T),                   // live multicast frame
    Marker(ResyncMarker),       // ResyncStart / ResyncEnd
    Resynced(T),                // replay item from snapshot
}
```

On normal multicast: stream yields `Event(NewLink(...))` etc.
On ENOBUFS: yields `Marker(ResyncStart)`, then every current
state item as `Resynced(NewLink(...))`, then `Marker(ResyncEnd)`,
then resumes live `Event(...)`.

Watch CLI maps `Event` → display; `Resynced` → silenced (or
"-r" prefix) by default; `Marker` → "RESYNC" diagnostic.

### `Connection<Nftables>` subscribe (already in 0.18)

`crates/nlink/src/netlink/nftables/resync.rs:188`:

```rust
pub async fn subscribe_all_with_resync(
    &self,
    factory: ConnectionFactory<Nftables>,
) -> Result<BorrowedResyncStream<'_, NftablesEvent>> { … }
```

Same shape, different event type. The watch CLI runs both
streams concurrently per node.

### `ResyncStreamExt` combinators (Plan 195)

`crates/nlink/src/netlink/resync.rs` exposes a `StreamExt`-style
trait with adapters:

- `.only_events()` — strip `Resynced` and `Marker`, keep only
  live events
- `.with_resync_log(level)` — log resync markers at the given
  tracing level, pass through events
- `.coalesce_resyncs(window: Duration)` — collapse rapid back-
  to-back resyncs (rare but possible under heavy churn)

Watch uses `.with_resync_log(Level::INFO)` by default; the
`--include-snapshot` flag opts out so the user sees `Resynced`
events as well.

---

## What changes — file-by-file

### `bins/lab/src/main.rs`

Add a new `Watch` subcommand:

```rust
#[derive(Subcommand)]
enum Cmd {
    // … existing variants …
    /// Tail a lab's nftables + RTNETLINK drift events
    Watch {
        /// Lab name (required, positional)
        lab: String,
        /// Restrict to a single node (default: all nodes)
        #[arg(long, value_name = "NAME")]
        node: Option<String>,
        /// Event family — route, nftables, or both
        #[arg(long, value_enum, default_value_t = WatchFamily::Both)]
        family: WatchFamily,
        /// NDJSON output (one event per line)
        #[arg(long)]
        json: bool,
        /// Include snapshot replay frames after ENOBUFS resyncs
        #[arg(long)]
        include_snapshot: bool,
        /// Restrict events by regex match on the rendered event line
        #[arg(long, value_name = "PATTERN")]
        filter: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum WatchFamily { Route, Nftables, Both }
```

The handler resolves the running lab, opens per-node
`Connection<Route>` + `Connection<Nftables>` connections,
subscribes to both, multiplexes the streams onto a single
mpsc, and writes to stdout.

### `crates/nlink-lab/src/lib.rs`

Re-export the new public types from `running.rs`:

```rust
pub use running::{RunningLab, WatchEvent, WatchFamily};
```

### `crates/nlink-lab/src/running.rs`

Add `RunningLab::events(family) -> impl Stream<Item = WatchEvent>`:

```rust
pub enum WatchFamily {
    Route,
    Nftables,
    Both,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WatchEvent {
    pub node: String,
    pub timestamp: time::OffsetDateTime,
    pub family: WatchFamily,  // Route or Nftables (never Both)
    pub kind: WatchEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resync: Option<ResyncMarker>,  // None on live events
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEventKind {
    NewLink { name: String, mtu: u32, oper_state: String, ifindex: u32 },
    DelLink { name: String, ifindex: u32 },
    NewAddress { ifindex: u32, cidr: String },
    DelAddress { ifindex: u32, cidr: String },
    NewRoute { dest: String, gateway: Option<String>, oif: Option<String> },
    DelRoute { dest: String, gateway: Option<String>, oif: Option<String> },
    NewRule { table: String, chain: String, rule: String },
    DelRule { table: String, chain: String, rule: String },
    NewChain { table: String, chain: String },
    DelChain { table: String, chain: String },
    NewTable { table: String, family: String },
    DelTable { table: String, family: String },
    // … neighbor / qdisc / filter / class / action variants
    // … nftables: NewSet/DelSet/NewElement/DelElement
    Other { raw: String },  // catch-all for variants we don't render typed
}

impl RunningLab {
    pub fn events(&self, family: WatchFamily) -> impl Stream<Item = Result<WatchEvent>> + '_ {
        // see implementation sketch below
    }

    pub fn events_filtered(
        &self,
        family: WatchFamily,
        node_filter: Option<&str>,
        include_snapshot: bool,
    ) -> impl Stream<Item = Result<WatchEvent>> + '_ { … }
}
```

**Why per-event type instead of forwarding `NetworkEvent`
directly?** `LinkMessage` / `AddressMessage` etc. don't derive
`Serialize` (they're the raw kernel-message types — fixed
shape, byte-level layout via zerocopy). The watch CLI needs to
JSON-serialize events, so we lift the relevant fields out into
`WatchEventKind`. Same approach Plan 158d Phase 2 spec'd.

### Implementation sketch — `events` body

```rust
pub fn events(&self, family: WatchFamily) -> impl Stream<Item = Result<WatchEvent>> + '_ {
    use futures_util::stream::{self, StreamExt};

    let nodes: Vec<_> = self.state.node_handles.iter().collect();
    let streams = nodes.into_iter().flat_map(|(node_name, handle)| {
        let mut streams: Vec<BoxStream<'_, Result<WatchEvent>>> = Vec::new();

        if matches!(family, WatchFamily::Route | WatchFamily::Both) {
            let node_name = node_name.clone();
            let ns_name = handle.namespace_name().to_owned();
            let factory = make_route_factory(&ns_name);
            let s = async_stream::try_stream! {
                let conn = handle.connection::<Route>()?;
                let mut stream = conn.into_events_with_resync(factory).await?;
                while let Some(item) = stream.next().await {
                    let resynced = item?;
                    let ev = lift_route_event(&node_name, resynced)?;
                    yield ev;
                }
            };
            streams.push(Box::pin(s));
        }

        if matches!(family, WatchFamily::Nftables | WatchFamily::Both) {
            // mirror — opens Connection<Nftables>::subscribe_all_with_resync
        }

        streams
    });

    stream::select_all(streams).boxed()
}
```

`select_all` multiplexes per-node streams concurrently;
`async_stream::try_stream!` keeps the connection alive for the
stream's lifetime. The `ConnectionFactory<Route>` closure for
ENOBUFS recovery captures the namespace name and calls
`namespace::connection_for(&ns_name)`.

### `crates/nlink-lab/src/running.rs` — helpers

```rust
fn lift_route_event(
    node_name: &str,
    item: ResyncedEvent<NetworkEvent>,
) -> Result<WatchEvent> {
    match item {
        ResyncedEvent::Event(ev) | ResyncedEvent::Resynced(ev) => {
            let resync = matches!(item, ResyncedEvent::Resynced(_))
                .then_some(/* synthetic snapshot marker */);
            Ok(WatchEvent {
                node: node_name.to_owned(),
                timestamp: OffsetDateTime::now_utc(),
                family: WatchFamily::Route,
                kind: lift_route_kind(ev)?,
                resync: None,
            })
        }
        ResyncedEvent::Marker(m) => {
            Ok(WatchEvent {
                node: node_name.to_owned(),
                timestamp: OffsetDateTime::now_utc(),
                family: WatchFamily::Route,
                kind: WatchEventKind::Other { raw: format!("[RESYNC {:?}]", m) },
                resync: Some(m),
            })
        }
    }
}

fn lift_route_kind(ev: NetworkEvent) -> Result<WatchEventKind> {
    match ev {
        NetworkEvent::NewLink(lm) => Ok(WatchEventKind::NewLink {
            name: lm.name().unwrap_or("?").to_owned(),
            mtu: lm.mtu().unwrap_or(0),
            oper_state: format!("{:?}", lm.oper_state()),
            ifindex: lm.ifindex(),
        }),
        // … 17 other variants …
    }
}
```

Same pattern for `NftablesEvent`. Both lift functions live next
to `events` in `running.rs`.

### `bins/lab/src/main.rs` — `Cmd::Watch` handler

```rust
Cmd::Watch { lab, node, family, json, include_snapshot, filter } => {
    let running = RunningLab::load(&lab).await?;
    let pattern = filter.as_deref().map(regex::Regex::new).transpose()?;
    let mut stream = std::pin::pin!(running.events_filtered(
        family,
        node.as_deref(),
        include_snapshot,
    ));

    while let Some(ev) = stream.next().await {
        let ev = ev?;
        let line = if json {
            serde_json::to_string(&ev)?
        } else {
            render_event_line(&ev)
        };
        if let Some(re) = &pattern && !re.is_match(&line) {
            continue;
        }
        println!("{line}");
    }
    Ok(())
}
```

`render_event_line` is a thin formatter:
`[<lab>/<node>] <Kind> <details>`. NDJSON path uses the same
`WatchEvent` shape directly.

### `crates/nlink-lab/Cargo.toml`

Adds:

- `async-stream = "0.3"` for the `try_stream!` macro
- `futures-util = "0.3"` for `select_all` + `StreamExt`
- `regex = "1"` only in `bins/lab` (already a workspace dep)

`tokio-stream` is already in the workspace via nlink itself.

### `CLAUDE.md`

Add `Watch` to the CLI command list. Document the
`--family route|nftables|both` and `--include-snapshot` flags.

### `docs/NLINK_LAB.md`

Document the watch command as a P3 feature; cite Plan 159b.

---

## Phases

### Phase 1 — `RunningLab::events` library API + `Cmd::Watch` (route family only)

1. Wire `Connection<Route>::into_events_with_resync` through a
   per-node stream.
2. Define `WatchEvent` + `WatchEventKind` + `WatchFamily` types
   with `serde::Serialize`.
3. Write `lift_route_event` + `lift_route_kind`. Cover the 6
   most common variants (NewLink, DelLink, NewAddress,
   DelAddress, NewRoute, DelRoute); the others render via
   `Other { raw: format!("{:?}", ev) }` until Phase 3.
4. CLI handler for `Cmd::Watch` with `--family route` only.
5. Integration test (root-gated): deploy a topology, spawn the
   watch task, in a separate task call
   `LabNamespace::set_link_up(...)`, assert the watch stream
   yields a `NewLink` with the right name within 1 second.
6. Integration test: assert ENOBUFS recovery — flood the kernel
   with rapid link adds/dels, assert the stream emits a
   `ResyncStart` / `ResyncEnd` pair without dropping the
   subscription.

### Phase 2 — nftables family + select_all multiplexing

1. Add `Connection<Nftables>::subscribe_all_with_resync` to
   the per-node stream fan-out.
2. Write `lift_nft_event` for the 10 `NftablesEvent` variants.
3. Wire `--family nftables` and `--family both`.
4. Integration test: deploy a topology with a firewall;
   `nft add rule ... -t inet filter input tcp dport 8080 accept`
   from outside `apply`; assert the watch stream yields a
   `NewRule` event.
5. Integration test: both families subscribed; deploy creates
   both a link and a firewall rule; assert both events come
   through on the same stream.

### Phase 3 — type-richness, filter, JSON, snapshot inclusion

1. Fill in the remaining `WatchEventKind` variants
   (Neighbor / Qdisc / Filter / Class / Action / Set /
   Element). Render each typed.
2. Wire `--json` (NDJSON output via `serde_json::to_string`).
3. Wire `--filter <regex>` post-render filter.
4. Wire `--include-snapshot` (default-off; when on, replay
   frames render with a `[snapshot]` prefix).
5. Wire `--node <name>` filter (in-stream, not post-stream — so
   we don't even subscribe to other nodes).
6. Tests:
   - `watch_json_emits_ndjson_per_event` — one record per line,
     each parsable as `WatchEvent`.
   - `watch_filter_drops_non_matching` — pattern `^.*tcp.*$`
     filters out non-tcp lines.
   - `watch_include_snapshot_emits_resynced_frames` — assert
     snapshot replay frames carry the `[snapshot]` prefix.
   - `watch_node_filter_subscribes_one_node_only` — opens only
     one `Connection<Route>` (proves the filter is pre-
     subscription).

---

## Concurrency + correctness considerations

### One connection per (node, family)

Each subscription opens its own `Connection<P>` because:

- `into_events_with_resync` consumes `self` (owned form is
  necessary for `'static + Send` so the stream can move
  between tasks).
- The 0.19 F1 lock (post-cycle audit) serializes concurrent
  ops on a shared `Arc<Connection>`, but we don't need shared
  — one stream per connection is the simpler model.
- Per-namespace + per-family scales to ~32 connections for a
  16-node lab (well below kernel limits).

### `select_all` ordering

`select_all` polls all streams in a fair manner. Event
ordering across nodes is NOT guaranteed (kernel multicasts
arrive when they arrive). The `WatchEvent::timestamp` field
gives the user a hint; per-node order is preserved because
each `Connection<P>` writes its socket buffer in order.

### Graceful shutdown

Ctrl-C: dropping the stream drops every `Connection<P>`,
which closes the netlink socket; kernel removes the
subscription. No cleanup needed.

For background daemon use (Plan 159b doesn't ship a daemon
mode; that's a future feature), we'd use `tokio::signal::ctrl_c`
to drive shutdown explicitly.

### Backpressure

If the user redirects to a slow consumer (`watch ... | jq |
tee log`), `println!` blocks on stdout. The per-node stream
buffers up to the socket's `SO_RCVBUF` (~256 KB by default),
then the kernel drops frames → next read sees ENOBUFS → resync
fires. The resync wrapper means we never get a permanent miss;
worst case we re-replay the snapshot. Document this in the CLI
help.

### Connection-factory namespace capture

`ConnectionFactory<Route>` is `Arc<dyn Fn() -> Result<Connection<Route>> + Send + Sync>`. We capture the namespace name (owned String) inside the closure:

```rust
fn make_route_factory(ns: &str) -> ConnectionFactory<Route> {
    let ns = ns.to_owned();
    Arc::new(move || namespace::connection_for(&ns))
}
```

This is correct only if `namespace::connection_for(&ns)`
resolves the namespace by name. For `LabNamespace` (fd-based),
we'd need an adapter — but
`crates/nlink-lab/src/namespace.rs::node_handle.namespace_name()`
already exposes the name; pass it through.

---

## Test plan

### Unit tests (no root)

- `watch_event_kind_serialize_round_trip` — assert
  `serde_json::to_value(&kind)` produces the documented JSON
  shape for each `WatchEventKind` variant.
- `lift_route_kind_new_link_extracts_name_mtu_ifindex` — given
  a synthetic `LinkMessage`, assert the lifted kind has the
  right fields.
- `lift_route_kind_other_for_unknown_variant` — falls back to
  `Other { raw }` for variants we don't render typed (Phase 1
  smoke; gets retired in Phase 3 when all variants are typed).

### Root-gated integration tests (`tests/integration.rs`)

- `watch_route_new_link_observed` — deploy a topology, start
  watch stream in a tokio task, in another task add a dummy
  link, assert the watch stream yields the right `NewLink`
  within 1 second.
- `watch_route_addr_observed` — similar for `NewAddress`.
- `watch_route_del_route_observed` — similar for `DelRoute`.
- `watch_nft_new_rule_observed` — deploy a topology with an
  nftables rule, then `nft add rule ...` outside `apply`,
  assert `NewRule` event.
- `watch_both_families_concurrent` — both families subscribed;
  one event of each, both delivered.
- `watch_node_filter_subscribes_only_filtered_node` — set
  `--node router`, perform actions on `server`, assert the
  stream does NOT yield events for `server` (validates
  pre-subscription filter).
- `watch_filter_drops_non_matching` — `--filter "tcp"`
  drops non-matching events.
- `watch_json_ndjson_format` — `--json`; assert every line
  parses as a `WatchEvent`.
- `watch_enobufs_resync_recovers` — stress kernel with rapid
  link adds; assert `ResyncStart`/`ResyncEnd` pair shows up;
  assert subsequent live events continue to arrive.
- `watch_includes_snapshot_when_flagged` — with
  `--include-snapshot`, assert resync frames are rendered
  (vs default-off, which silences them).
- `watch_destroy_during_subscribe_cleans_up` — start watch,
  destroy the lab namespace; assert the stream ends cleanly
  (no panic).

### CI integration

Add `nlink-lab watch` to the smoke-test matrix. Run the
`watch_*` tests in the existing root-gated test pass.

---

## CLI documentation

```text
nlink-lab watch <lab> [OPTIONS]

Tail nftables and RTNETLINK drift events for a running lab.
Subscribes to every node in the lab and emits one event per
mutation. Useful for spotting hand-edits that bypass
`nlink-lab apply`.

ARGUMENTS:
  <lab>                 Lab name

OPTIONS:
      --node <NAME>     Restrict to a single node
      --family <F>      route | nftables | both [default: both]
      --json            Emit NDJSON (one event per line) for
                        piping to jq
      --include-snapshot
                        Show resync replay frames after
                        ENOBUFS recoveries (default: silence them)
      --filter <REGEX>  Only show events whose rendered line
                        matches REGEX

EXAMPLES:
  # Tail every drift on every node, human-readable
  sudo nlink-lab watch my-lab

  # Just nftables changes on the router
  sudo nlink-lab watch my-lab --node router --family nftables

  # NDJSON to a file for later analysis
  sudo nlink-lab watch my-lab --json > drift.ndjson

  # Only TCP-related events
  sudo nlink-lab watch my-lab --filter 'tcp.*dport'

EXIT CODES:
  0  stream ended cleanly (lab destroyed, Ctrl-C)
  1  initial connection failed (lab not running, permission
     denied)
  2  parse error in --filter regex
```

---

## Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| stdout backpressure starves stream → ENOBUFS → spurious resync | Medium — user sees a fat snapshot dump if they're piping to a slow sink | Medium under heavy load | Document the resync wrapper as expected behavior; `--filter` does post-render, so consider an upstream `RawEvent` pre-render shape to reduce stdout volume if it bites |
| `lift_route_kind` panics on a `NetworkEvent` field we don't expect | High if it happens | Low — we use `unwrap_or` defaults | Phase 1 tests; `Other { raw }` fallback for unhandled variants |
| `select_all` polls one stream more than others, starves the slow one | Low — `select_all` is fair by spec | Low | Tokio's implementation is fair; document if we observe drift |
| Test flakiness on the "watch stream observes X within 1 second" tests | Medium | Medium — kernel multicast timing is wall-clock-dependent | Bump timeout to 5s in CI; use `tokio::time::timeout` not raw sleep |
| `regex` crate adds binary size | Low | High (regex is heavyweight) | `regex-lite` is the lighter alternative; default to `regex` since clap already pulls it; if size matters, switch later |
| Watch on a node that no longer exists (destroyed mid-watch) | Low — `select_all` drops the stream cleanly | Low | Per-stream error handling propagates the error; user sees one disconnect message but the rest of the lab keeps tailing |

---

## Out of scope

- **WireGuard drift detection** — `Connection<Wireguard>` has
  no event subscription in 0.19 (GENL families don't
  multicast typed events). `WireguardWatcher` (Plan 199
  upstream) does polling-based per-interface state diff, which
  is a different shape entirely. Future `lab watch --wg` could
  use a polling task; out of scope for 159b.
- **Daemon / persistent watch** — `nlink-lab watch` runs in
  foreground only. A future feature could log to a file or
  publish to Zenoh; not 159b's scope.
- **Replay from log** — `nlink-lab watch --replay drift.ndjson`
  to reload a captured stream for offline analysis. Useful for
  CI but not 159b.
- **Filter by event kind** — `--kind NewLink,DelLink` would
  let the user pre-filter without regex. Defer — `--filter
  "NewLink|DelLink"` covers it.
- **Multi-lab watch** — `nlink-lab watch lab1,lab2,lab3`. One
  lab per invocation; user can run multiple terminals.
- **Diff against deployed config** — "show me only events that
  represent drift from `apply`'s declared state". Plan 158d
  Phase 3 spec'd this; defer to Plan 159b Phase 4 or a
  follow-up.

---

## Success criteria

- [ ] `nlink-lab watch <lab>` emits a tail of every nftables +
  RTNETLINK mutation across the lab.
- [ ] `--json` emits valid NDJSON (each line `serde_json::from_str`'able).
- [ ] `--family route` / `--family nftables` correctly filters.
- [ ] `--node <name>` opens only one set of connections (not all-then-filter).
- [ ] `--filter <regex>` works post-render.
- [ ] ENOBUFS recovery is transparent — user sees a brief
  `[RESYNC]` line, then events continue.
- [ ] Ctrl-C cleanly closes every connection.
- [ ] Integration tests pass in CI.
- [ ] CLI help documents every flag.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md)
- Plan 158d (superseded by 159b; the plan file was removed when
  this superseded it. The original nftables-only design lives on
  in git history if you ever want to compare shapes)
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  — item #15 closure cited
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `crates/nlink/src/netlink/route_resync.rs` — `subscribe_all_with_resync`
  - `crates/nlink/src/netlink/resync.rs` — `ResyncedEvent`, `ResyncMarker`, combinators
  - `crates/nlink/src/netlink/events.rs` — `NetworkEvent` enum
  - `crates/nlink/src/netlink/nftables/resync.rs` — nftables side
