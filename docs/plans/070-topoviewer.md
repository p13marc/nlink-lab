# Plan 070: TopoViewer — Native Topology Visualizer

**Priority:** High
**Effort:** 5-7 days
**Target:** `bins/topoviewer/` (new crate)

## Summary

A native GUI application built with Rust + Iced that renders lab topologies as
interactive node-link diagrams. Connects to running labs for live status
(interface state, metrics, issues) or renders static topology files.

Differentiator vs containerlab's TopoViewer: native performance, no browser
dependency, direct integration with nlink-lab's Rust API and nlink diagnostics.

## Architecture

```
bins/topoviewer/
  Cargo.toml          # depends on iced, nlink-lab, nlink, tokio
  src/
    main.rs           # Entry point, argument parsing
    app.rs            # Iced Application impl (state, update, view)
    canvas.rs         # Canvas Program impl (drawing nodes, links, labels)
    layout.rs         # Force-directed layout algorithm
    theme.rs          # Colors, fonts, sizes
    metrics.rs        # Background diagnostics poller
```

## Dependencies

```toml
[dependencies]
iced = { version = "0.13", features = ["canvas", "tokio"] }
nlink-lab = { workspace = true }
nlink = { workspace = true }
tokio = { workspace = true }
clap = { workspace = true }
```

## Design

### Application State

```rust
struct TopoViewer {
    // Data
    topology: Topology,
    lab_name: Option<String>,        // if connected to running lab
    node_positions: HashMap<String, Point>,
    diagnostics: HashMap<String, NodeDiagnostic>,  // live metrics per node

    // UI state
    selected_node: Option<String>,
    selected_link: Option<usize>,
    camera: Camera,                  // pan + zoom
    dragging: Option<String>,        // node being dragged
    canvas_cache: canvas::Cache,

    // Settings
    refresh_interval: Duration,
    show_addresses: bool,
    show_metrics: bool,
}

struct Camera {
    offset: Vector,    // pan offset
    scale: f32,        // zoom level (1.0 = 100%)
}
```

### Messages

```rust
enum Message {
    // Canvas interaction
    CanvasEvent(canvas::Event),
    NodeClicked(String),
    NodeDragged(String, Point),
    NodeReleased,
    LinkClicked(usize),
    BackgroundClicked,

    // Data
    DiagnosticsReceived(Vec<NodeDiagnostic>),
    TopologyLoaded(Topology),
    Tick,                           // periodic refresh

    // UI
    ToggleAddresses,
    ToggleMetrics,
    ZoomIn,
    ZoomOut,
    FitToScreen,
}
```

### Canvas Rendering

The canvas draws:

1. **Links** as lines between node centers
   - Gray = normal, green = traffic flowing, red = errors/drops
   - Label: addresses, impairment values, live bandwidth
   - Thicker stroke for higher bandwidth

2. **Nodes** as rounded rectangles
   - Blue = namespace node, purple = container node
   - Green border = all interfaces up, red = any interface down
   - Label: node name, image name (for containers)

3. **Selection panel** (right sidebar, not canvas)
   - Node: interfaces with addresses, routes, sysctls, live stats
   - Link: endpoints, addresses, impairments, bandwidth

### Layout Algorithm

**Force-directed layout** (Fruchterman-Reingold):

```rust
struct LayoutEngine {
    positions: HashMap<String, Point>,
    velocities: HashMap<String, Vector>,
    iterations: usize,
}

impl LayoutEngine {
    fn new(topology: &Topology) -> Self;

    /// Run one iteration of force-directed layout.
    fn step(&mut self, topology: &Topology) {
        // 1. Repulsive force between all node pairs (Coulomb's law)
        // 2. Attractive force along links (Hooke's law)
        // 3. Apply velocity with damping
        // 4. Clamp positions to bounds
    }

    /// Run layout until convergence or max iterations.
    fn run(&mut self, topology: &Topology, max_iters: usize);
}
```

Initial positions: arrange nodes in a circle, then run ~100 iterations of
force-directed layout. Users can drag nodes to override positions.

### Live Metrics

Background subscription polls diagnostics every N seconds:

```rust
fn subscription(&self) -> Subscription<Message> {
    if self.lab_name.is_some() {
        iced::time::every(self.refresh_interval)
            .map(|_| Message::Tick)
    } else {
        Subscription::none()
    }
}

fn update(&mut self, message: Message) -> Command<Message> {
    match message {
        Message::Tick => {
            let lab_name = self.lab_name.clone().unwrap();
            Command::perform(
                async move {
                    let lab = RunningLab::load(&lab_name).ok()?;
                    lab.diagnose(None).await.ok()
                },
                |result| Message::DiagnosticsReceived(result.unwrap_or_default()),
            )
        }
        Message::DiagnosticsReceived(diags) => {
            for d in diags {
                self.diagnostics.insert(d.node.clone(), d);
            }
            self.canvas_cache.clear();  // force redraw
            Command::none()
        }
        // ...
    }
}
```

### CLI

```
nlink-lab-topoviewer [OPTIONS] [TOPOLOGY_FILE]

Arguments:
  [TOPOLOGY_FILE]  Path to .toml or .nll topology file

Options:
  -l, --lab <NAME>     Connect to a running lab for live metrics
  -r, --refresh <SEC>  Metrics refresh interval (default: 2)
  --dark               Use dark theme
```

## Implementation Order

### Phase 1: Static Viewer (days 1-3)

1. Create `bins/topoviewer/` crate with Iced dependency
2. Implement `layout.rs` — force-directed algorithm
3. Implement `canvas.rs` — draw nodes as boxes, links as lines
4. Implement `app.rs` — load topology, render canvas
5. Pan and zoom via mouse drag/scroll
6. Click to select node/link, show details in sidebar

### Phase 2: Live Metrics (days 4-5)

7. Implement `metrics.rs` — background diagnostics poller
8. Add subscription for periodic refresh
9. Color-code interfaces by status (up/down/impaired)
10. Show live bandwidth on links
11. Show issue badges on nodes

### Phase 3: Polish (days 6-7)

12. Dark/light theme support
13. Node dragging to reposition
14. Fit-to-screen button
15. Export as PNG/SVG
16. Keyboard shortcuts (Ctrl+R refresh, Escape deselect, +/- zoom)

## Progress

### Phase 1: Static Viewer

- [ ] Create `bins/topoviewer/` crate
- [ ] Force-directed layout engine
- [ ] Canvas rendering (nodes, links, labels)
- [ ] Pan and zoom
- [ ] Click-to-select with detail sidebar
- [ ] Load topology from file or running lab

### Phase 2: Live Metrics

- [ ] Background diagnostics poller
- [ ] Interface status color-coding
- [ ] Live bandwidth display on links
- [ ] Issue badges on nodes

### Phase 3: Polish

- [ ] Dark/light theme
- [ ] Node dragging
- [ ] Fit-to-screen
- [ ] Export PNG/SVG
- [ ] Keyboard shortcuts
