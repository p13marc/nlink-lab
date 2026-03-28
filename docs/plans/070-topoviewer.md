# Plan 070: TopoViewer — Native Topology Visualizer

**Priority:** High
**Effort:** 5-7 days
**Depends on:** Plan 071 (backend daemon provides live data via Zenoh)
**Target:** `bins/topoviewer/` (new crate)

## Summary

A native GUI application built with Rust + Iced that renders lab topologies as
interactive node-link diagrams. Connects to the nlink-lab backend daemon (plan 071)
via **Zenoh** for live status — or renders static topology files with zero privileges.

The GUI always runs as a **regular unprivileged user**. It has **no nlink dependency**
and never touches netlink directly. All privileged operations are requested via
Zenoh query/reply to the backend daemon.

Architecture follows the same pattern as [tcgui](https://github.com/p13marc/tcgui):
Zenoh subscribers for streaming state, Zenoh `get()` for query/reply operations,
shared types from `nlink-lab-shared`.

## Modes of Operation

| Mode | Privileges | Data Source | Features |
|------|-----------|-------------|----------|
| **Static** | None | `.nll` file | Layout, pan/zoom, select, sidebar, export |
| **Live** | None | Zenoh (backend daemon) | Static + live metrics, status colors, bandwidth, exec |

## Crate Structure

```
bins/topoviewer/
  Cargo.toml
  src/
    main.rs             # Entry point, CLI args
    app.rs              # Iced Application impl
    canvas.rs           # Canvas Program impl (draw nodes, links, labels)
    layout.rs           # Force-directed layout algorithm
    theme.rs            # Colors, fonts, sizes (dark/light)
    zenoh_client.rs     # Zenoh session, subscriptions, queries
    sidebar.rs          # Detail panel for selected node/link
```

## Dependencies

```toml
[dependencies]
iced = { version = "0.13", features = ["canvas", "tokio"] }
nlink-lab = { workspace = true }           # parser + types only
nlink-lab-shared = { workspace = true }    # Zenoh message types + topics
zenoh = { version = "1.5", features = ["unstable"] }
zenoh-ext = "1.5"
tokio = { workspace = true }
clap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

Note: **no `nlink` dependency** — the GUI never talks to netlink.

## Zenoh Integration

### Subscriptions (receive streaming data)

The GUI subscribes to backend pub/sub topics using wildcard patterns:

```rust
// Subscribe to all labs (or filter by --lab flag)
let topo_sub = session
    .declare_subscriber("nlink-lab/*/topology")
    .history(HistoryConfig::default().detect_late_publishers())
    .await?;

let metrics_sub = session
    .declare_subscriber(&format!("nlink-lab/{lab}/metrics/snapshot"))
    .history(HistoryConfig::default().detect_late_publishers())
    .await?;

let events_sub = session
    .declare_subscriber(&format!("nlink-lab/{lab}/events"))
    .history(HistoryConfig::default())
    .recovery(RecoveryConfig::default().heartbeat())
    .await?;

let health_sub = session
    .declare_subscriber("nlink-lab/*/health")
    .history(HistoryConfig::default().detect_late_publishers())
    .await?;
```

Late-joiner history means the GUI immediately receives the latest topology and
metrics snapshot when it connects, even if the backend published them minutes ago.

### Queries (request operations)

For exec and impairment changes, the GUI uses Zenoh `get()`:

```rust
// Execute a command in a node
let request = ExecRequest { node, cmd, args };
let replies = session
    .get(topics::rpc_exec(lab_name))
    .payload(serde_json::to_string(&request)?)
    .await?;

while let Ok(reply) = replies.recv_async().await {
    let response: ExecResponse = serde_json::from_str(...)?;
    // Update UI with result
}
```

### Iced Integration

Zenoh events are bridged to Iced messages via a subscription:

```rust
fn subscription(&self) -> Subscription<Message> {
    Subscription::batch([
        // Zenoh event stream → Iced messages
        iced::subscription::channel(
            std::any::TypeId::of::<ZenohBridge>(),
            100,
            |output| zenoh_event_loop(self.zenoh_session.clone(), output),
        ),
    ])
}
```

The `zenoh_event_loop` runs in a background task, receiving from Zenoh subscribers
and forwarding as Iced messages:

```rust
async fn zenoh_event_loop(
    session: Arc<Session>,
    mut output: mpsc::Sender<Message>,
) -> ! {
    let topo_sub = session.declare_subscriber("nlink-lab/*/topology").await.unwrap();
    let metrics_sub = session.declare_subscriber("nlink-lab/*/metrics/snapshot").await.unwrap();

    loop {
        tokio::select! {
            Ok(sample) = topo_sub.recv_async() => {
                let update: TopologyUpdate = serde_json::from_str(...).unwrap();
                output.send(Message::TopologyReceived(update)).await.ok();
            }
            Ok(sample) = metrics_sub.recv_async() => {
                let snapshot: MetricsSnapshot = serde_json::from_str(...).unwrap();
                output.send(Message::MetricsReceived(snapshot)).await.ok();
            }
        }
    }
}
```

## Application Design

### State

```rust
struct TopoViewer {
    // Data
    topology: Option<Topology>,
    lab_name: Option<String>,
    node_positions: HashMap<String, Point>,
    metrics: HashMap<String, NodeMetrics>,
    health: Option<HealthStatus>,

    // Zenoh
    zenoh_session: Option<Arc<zenoh::Session>>,

    // UI state
    selected_node: Option<String>,
    selected_link: Option<usize>,
    camera: Camera,
    dragging: Option<String>,
    canvas_cache: canvas::Cache,
    dark_mode: bool,
    show_addresses: bool,
    show_metrics: bool,
}

struct Camera {
    offset: Vector,
    scale: f32,
}
```

### Messages

```rust
enum Message {
    // Zenoh data
    TopologyReceived(TopologyUpdate),
    MetricsReceived(MetricsSnapshot),
    HealthReceived(HealthStatus),
    EventReceived(LabEvent),

    // Canvas interaction
    NodeClicked(String),
    NodeDragged(String, Point),
    NodeReleased,
    LinkClicked(usize),
    BackgroundClicked,

    // UI controls
    ToggleAddresses,
    ToggleMetrics,
    ToggleDarkMode,
    ZoomIn,
    ZoomOut,
    FitToScreen,
    ExportPng,
}
```

### Canvas Rendering

1. **Links** as lines between node centers
   - Gray = no metrics, green = traffic flowing, red = errors/drops
   - Label: addresses, live bandwidth (from MetricsSnapshot)
   - Thicker stroke for higher bandwidth

2. **Nodes** as rounded rectangles
   - Blue = namespace, purple = container
   - Green border = all healthy, yellow = warnings, red = errors
   - Badge count for active issues

3. **Selection sidebar**
   - Node details: interfaces, addresses, routes, live stats
   - Link details: endpoints, impairments, bandwidth
   - Exec button: run command in selected node (via Zenoh query)

### Force-Directed Layout

```rust
struct LayoutEngine {
    positions: HashMap<String, Point>,
    velocities: HashMap<String, Vector>,
}

impl LayoutEngine {
    fn new(topology: &Topology) -> Self;
    fn step(&mut self, topology: &Topology);
    fn run(&mut self, topology: &Topology, max_iters: usize);
}
```

## CLI

```
nlink-lab-topoviewer [OPTIONS] [TOPOLOGY_FILE]

Arguments:
  [TOPOLOGY_FILE]  Path to .nll file (static mode)

Options:
  -l, --lab <NAME>            Connect to running lab via Zenoh (live mode)
  --dark                      Use dark theme
  --zenoh-connect <ENDPOINT>  Connect to Zenoh endpoint
  --zenoh-mode <MODE>         Zenoh mode: peer (default), client
```

## Multi-Lab Discovery

When launched without `--lab`, the GUI subscribes to `nlink-lab/*/health` and
auto-discovers all running backends. A lab selector dropdown lets the user switch
between labs.

```
┌─────────────────────────────────────────────────────────┐
│  TopoViewer              Labs: [dc-east ▼] [dc-west]   │
│                                                         │
│    ┌─────┐          ┌─────┐          ┌─────┐           │
│    │spine│──────────│spine│          │     │           │
│    │  1  │    ╲     │  2  │          │ ... │           │
│    └──┬──┘     ╲    └──┬──┘          │     │           │
│       │         ╲      │             └─────┘           │
│    ┌──┴──┐    ┌──┴──┐                                  │
│    │leaf │    │leaf │     Node: leaf1                   │
│    │  1  │    │  2  │     Interfaces:                   │
│    └──┬──┘    └──┬──┘       eth1: UP  45.2 Mbps ↓↑     │
│       │          │          eth3: UP   1.2 Mbps ↓↑ ⚠   │
│    ┌──┴──┐    ┌──┴──┐     Issues:                      │
│    │srv1 │    │srv2 │       eth3: 12 qdisc drops       │
│    └─────┘    └─────┘                                   │
└─────────────────────────────────────────────────────────┘
```

## Implementation Order

### Phase 1: Static Viewer (days 1-3)

1. Create `bins/topoviewer/` crate with Iced dependency
2. Implement `layout.rs` — force-directed algorithm
3. Implement `canvas.rs` — draw nodes as boxes, links as lines
4. Implement `app.rs` — load .nll topology, render canvas
5. Pan and zoom via mouse drag/scroll
6. Click to select node/link, show details in sidebar

### Phase 2: Live Metrics via Zenoh (days 4-5)

7. Implement `zenoh_client.rs` — session, subscriptions, queries
8. Bridge Zenoh events to Iced messages via subscription
9. Color-code interfaces by status
10. Show live bandwidth on links from MetricsSnapshot
11. Show issue badges on nodes
12. Multi-lab discovery via health topic

### Phase 3: Polish (days 6-7)

13. Dark/light theme support
14. Node dragging to reposition
15. Fit-to-screen button
16. Export as PNG/SVG
17. Keyboard shortcuts (Ctrl+R refresh, Escape deselect, +/- zoom)
18. Exec panel: run commands in selected node via Zenoh query

## Progress

### Phase 1: Static Viewer

- [x] Create `bins/topoviewer/` crate
- [x] Force-directed layout engine
- [x] Canvas rendering (nodes, links, labels)
- [x] Pan and zoom (scroll + drag)
- [x] Detail sidebar (node info, metrics)
- [x] Load topology from .nll file

### Phase 2: Live Metrics via Zenoh

- [x] Zenoh client (session, subscriptions)
- [x] Bridge Zenoh → Iced messages via Subscription
- [x] Link color-coding (green=traffic, red=errors, gray=idle)
- [x] Live bandwidth in sidebar
- [x] Issue badges on nodes
- [x] Multi-lab discovery

### Phase 3: Polish

- [x] Dark theme (default)
- [x] Node dragging to reposition
- [x] Fit-to-screen button
- [x] Export PNG (via screenshot + png crate)
- [x] Keyboard shortcuts (Esc, +/-, f, a, m, e)
- [x] Exec panel via Zenoh query
