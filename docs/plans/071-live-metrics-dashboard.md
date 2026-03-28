# Plan 071: Backend Daemon, Metrics & CLI Dashboard

**Priority:** High
**Effort:** 5-7 days
**Target:** `crates/nlink-lab-shared/` (new), `bins/nlink-lab-backend/` (new), `bins/lab/src/main.rs`

## Summary

Add a privileged backend daemon (`nlink-lab-backend`) that runs as root (or with
`CAP_NET_ADMIN`), collects live metrics from all lab nodes, and exposes them via
**Zenoh** pub/sub and query/reply. Unprivileged clients (the GUI from plan 070,
the `nlink-lab metrics` CLI, or external tools) connect via Zenoh to receive
streaming metrics, query topology, and issue commands.

Architecture follows the same pattern as [tcgui](https://github.com/p13marc/tcgui):
shared types crate, Zenoh AdvancedPublisher with history for state, queryables
for mutating operations.

## Architecture

```
┌───────────────────────────────────────────────────────────────┐
│                  Unprivileged clients                         │
│                                                               │
│  nlink-lab metrics    TopoViewer GUI     External tools       │
│  (CLI, table/json)    (Iced, plan 070)   (zenoh-cli, jq)     │
│                                                               │
│  Zenoh subscribers         Zenoh get()                        │
│  (pub/sub streams)         (query/reply)                      │
└──────────┬────────────────────┬──────────────────┬────────────┘
           │       Zenoh (TCP / UDP / TLS / QUIC)  │
┌──────────▼────────────────────▼──────────────────▼────────────┐
│              nlink-lab-backend (CAP_NET_ADMIN)                 │
│                                                               │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐│
│  │ Publishers    │  │ Queryables   │  │ Background tasks     ││
│  │ - metrics     │  │ - exec       │  │ - MetricsCollector   ││
│  │ - topology    │  │ - impairment │  │   (periodic scan)    ││
│  │ - health      │  │ - deploy     │  │ - Netlink events     ││
│  │ - events      │  │ - destroy    │  │   (interface state)  ││
│  └──────┬───────┘  └──────┬───────┘  └──────────┬───────────┘│
│         └──────────────────┼─────────────────────┘            │
│                     RunningLab → nlink (netlink)              │
└───────────────────────────────────────────────────────────────┘
```

## Crate Structure

```
crates/nlink-lab-shared/       # NEW: Shared types between backend & clients
  Cargo.toml                   # depends on serde, serde_json
  src/
    lib.rs                     # Re-exports
    topics.rs                  # Zenoh key expression helpers
    messages.rs                # All pub/sub + query/reply message types
    metrics.rs                 # MetricsSnapshot, NodeMetrics, InterfaceMetrics

bins/nlink-lab-backend/        # NEW: Privileged backend daemon
  Cargo.toml                   # depends on zenoh, zenoh-ext, nlink-lab, nlink, tokio
  src/
    main.rs                    # Entry point, Zenoh session, event loop
    collector.rs               # MetricsCollector (periodic diagnostics scan)
    handlers.rs                # Queryable handlers (exec, impairment, deploy)

bins/lab/src/main.rs           # Existing CLI — add `daemon`, `metrics` commands
```

## Dependencies

```toml
# crates/nlink-lab-shared/Cargo.toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }

# bins/nlink-lab-backend/Cargo.toml
[dependencies]
zenoh = { version = "1.5", features = ["unstable"] }
zenoh-ext = "1.5"
nlink-lab = { workspace = true }
nlink-lab-shared = { workspace = true }
nlink = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
clap = { workspace = true }

# bins/lab/Cargo.toml — add for metrics CLI
zenoh = { version = "1.5", features = ["unstable"] }
nlink-lab-shared = { workspace = true }
```

## Zenoh Topics

All topics use format: `nlink-lab/{lab-name}/{category}/...`

### Pub/Sub (backend publishes, clients subscribe)

| Topic | Message Type | QoS | History | Description |
|-------|-------------|-----|---------|-------------|
| `nlink-lab/{lab}/topology` | `TopologyUpdate` | Reliable | Keep Last 1 | Full topology (on startup + changes) |
| `nlink-lab/{lab}/health` | `HealthStatus` | Reliable | Keep Last 1 | Backend alive, node/link counts |
| `nlink-lab/{lab}/metrics/{node}/{iface}` | `InterfaceMetrics` | Best Effort | None | Per-interface bandwidth, errors (high freq) |
| `nlink-lab/{lab}/metrics/snapshot` | `MetricsSnapshot` | Reliable | Keep Last 1 | Full snapshot all nodes (periodic) |
| `nlink-lab/{lab}/events` | `LabEvent` | Reliable | Keep Last 10 | Interface state changes, process exits |

### Query/Reply (clients query, backend replies)

| Topic | Request | Response | Description |
|-------|---------|----------|-------------|
| `nlink-lab/{lab}/rpc/exec` | `ExecRequest` | `ExecResponse` | Execute command in node |
| `nlink-lab/{lab}/rpc/impairment` | `ImpairmentRequest` | `ImpairmentResponse` | Modify TC impairment |
| `nlink-lab/{lab}/rpc/status` | `StatusRequest` | `StatusResponse` | Lab status summary |

### Topic Helpers (in nlink-lab-shared)

```rust
// crates/nlink-lab-shared/src/topics.rs

pub fn topology(lab: &str) -> String {
    format!("nlink-lab/{lab}/topology")
}

pub fn health(lab: &str) -> String {
    format!("nlink-lab/{lab}/health")
}

pub fn metrics_iface(lab: &str, node: &str, iface: &str) -> String {
    format!("nlink-lab/{lab}/metrics/{node}/{iface}")
}

pub fn metrics_snapshot(lab: &str) -> String {
    format!("nlink-lab/{lab}/metrics/snapshot")
}

pub fn events(lab: &str) -> String {
    format!("nlink-lab/{lab}/events")
}

pub fn rpc_exec(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/exec")
}

pub fn rpc_impairment(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/impairment")
}

pub fn rpc_status(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/status")
}

/// Extract lab name from a topic key expression.
pub fn extract_lab_name(key_expr: &str) -> Option<&str> {
    key_expr.strip_prefix("nlink-lab/")?.split('/').next()
}
```

## Shared Message Types

```rust
// crates/nlink-lab-shared/src/messages.rs

use serde::{Serialize, Deserialize};

// ─── Pub/Sub messages ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyUpdate {
    pub lab_name: String,
    pub topology: nlink_lab::Topology,  // re-exported, already Serialize
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub lab_name: String,
    pub node_count: usize,
    pub namespace_count: usize,
    pub container_count: usize,
    pub pid_count: usize,
    pub uptime_secs: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabEvent {
    pub lab_name: String,
    pub kind: LabEventKind,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LabEventKind {
    InterfaceUp { node: String, interface: String },
    InterfaceDown { node: String, interface: String },
    ProcessExited { node: String, pid: u32, exit_code: i32 },
}

// ─── Query/Reply messages ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub node: String,
    pub cmd: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentRequest {
    pub node: String,
    pub interface: String,
    pub delay: Option<String>,
    pub jitter: Option<String>,
    pub loss: Option<String>,
    pub corrupt: Option<String>,
    pub reorder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub lab_name: String,
    pub node_count: usize,
    pub namespace_count: usize,
    pub container_count: usize,
    pub uptime_secs: u64,
}
```

```rust
// crates/nlink-lab-shared/src/metrics.rs

use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub lab_name: String,
    pub timestamp: u64,
    pub nodes: HashMap<String, NodeMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub interfaces: Vec<InterfaceMetrics>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceMetrics {
    pub name: String,
    pub state: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub rx_pps: u64,
    pub tx_pps: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub tc_drops: u64,
    pub tc_qlen: u32,
}
```

## Backend Daemon

### Main Loop

```rust
// bins/nlink-lab-backend/src/main.rs

async fn run(lab_name: &str, zenoh_config: zenoh::Config, interval: Duration) -> Result<()> {
    let lab = RunningLab::load(lab_name)?;
    let session = zenoh::open(zenoh_config).await?;

    // ── Publishers (with history for late joiners) ──

    let topo_publisher = session
        .declare_publisher(topics::topology(lab_name))
        .cache(CacheConfig::default().max_samples(1))
        .publisher_detection()
        .await?;

    let health_publisher = session
        .declare_publisher(topics::health(lab_name))
        .cache(CacheConfig::default().max_samples(1))
        .await?;

    let snapshot_publisher = session
        .declare_publisher(topics::metrics_snapshot(lab_name))
        .cache(CacheConfig::default().max_samples(1))
        .await?;

    // ── Queryables ──

    let exec_queryable = session
        .declare_queryable(topics::rpc_exec(lab_name))
        .await?;

    let impair_queryable = session
        .declare_queryable(topics::rpc_impairment(lab_name))
        .await?;

    // ── Publish initial topology ──
    topo_publisher.put(serde_json::to_string(&TopologyUpdate {
        lab_name: lab_name.into(),
        topology: lab.topology().clone(),
        timestamp: now(),
    })?).await?;

    // ── Liveliness token ──
    let _token = session.liveliness()
        .declare_token(topics::health(lab_name))
        .await?;

    // ── Main event loop ──
    let mut collector = MetricsCollector::new(&lab, interval);
    let mut health_interval = tokio::time::interval(Duration::from_secs(10));
    let mut metrics_interval = tokio::time::interval(interval);

    loop {
        tokio::select! {
            // Periodic metrics collection
            _ = metrics_interval.tick() => {
                if let Ok(snapshot) = collector.snapshot().await {
                    let json = serde_json::to_string(&snapshot)?;
                    snapshot_publisher.put(json).await?;
                }
            }

            // Periodic health heartbeat
            _ = health_interval.tick() => {
                let status = HealthStatus { /* ... */ };
                health_publisher.put(serde_json::to_string(&status)?).await?;
            }

            // Handle exec queries
            Ok(query) = exec_queryable.recv_async() => {
                handle_exec_query(query, &lab).await;
            }

            // Handle impairment queries
            Ok(query) = impair_queryable.recv_async() => {
                handle_impairment_query(query, &lab).await;
            }
        }
    }
}
```

### MetricsCollector

```rust
// bins/nlink-lab-backend/src/collector.rs

pub struct MetricsCollector {
    lab: RunningLab,
    interval: Duration,
}

impl MetricsCollector {
    pub fn new(lab: &RunningLab, interval: Duration) -> Self;

    /// Collect a full snapshot from all nodes via RunningLab::diagnose().
    pub async fn snapshot(&mut self) -> Result<MetricsSnapshot>;
}
```

## CLI Commands

### `nlink-lab daemon`

```
nlink-lab daemon <lab> [OPTIONS]

Start the Zenoh backend for a running lab.

Options:
  -i, --interval <SEC>        Metrics collection interval (default: 2)
  --zenoh-mode <MODE>         Zenoh mode: peer (default), client
  --zenoh-listen <ENDPOINT>   Zenoh listen endpoint (default: tcp/0.0.0.0:7447)
  --zenoh-connect <ENDPOINT>  Connect to Zenoh router
  --foreground                Run in foreground (default: daemonize)
```

Requires root or `CAP_NET_ADMIN`. Typically started after deploy:

```bash
sudo nlink-lab deploy datacenter.nll
sudo nlink-lab daemon datacenter &

# Now any user can:
nlink-lab metrics datacenter
nlink-lab-topoviewer --lab datacenter
```

### `nlink-lab metrics`

```
nlink-lab metrics <lab> [OPTIONS]

Stream live metrics from the backend via Zenoh (no root required).

Options:
  -n, --node <NODE>       Filter to specific node
  -f, --format <FMT>      Output: table (default), json, csv
  -c, --count <N>         Number of samples then exit
  --zenoh-connect <EP>    Connect to specific Zenoh endpoint
```

Subscribes to `nlink-lab/{lab}/metrics/snapshot` — **no root required**.

### Table Output

```
lab: datacenter  |  nodes: 6  |  refresh: 2s  |  sample: #5

NODE         IFACE     STATE    RX rate    TX rate    ERRORS  DROPS
────────────────────────────────────────────────────────────────────
spine1       eth1      UP       45.2 Mbps  45.1 Mbps     0      0
spine1       eth2      UP       22.8 Mbps  22.9 Mbps     0      0
leaf1        eth3      UP        1.2 Mbps   0.8 Mbps     0     12 ⚠

ISSUES:
  [WARN] leaf1:eth3 — qdisc drops detected (12 drops)
```

## Multi-Lab & Remote Support

Because Zenoh handles discovery and routing, this architecture natively supports:

```
┌──────────────────────┐    ┌──────────────────────┐
│  Lab "dc-east"       │    │  Lab "dc-west"       │
│  backend on host-a   │    │  backend on host-b   │
└──────────┬───────────┘    └──────────┬───────────┘
           │      Zenoh mesh           │
           └──────────┬────────────────┘
                      │
              ┌───────▼──────────┐
              │  Frontend / CLI  │
              │  (any machine)   │
              └──────────────────┘
```

Topics are lab-namespaced (`nlink-lab/dc-east/...`, `nlink-lab/dc-west/...`),
so one client can observe multiple labs from different machines.

## Implementation Order

### Phase 1: Shared Types + Daemon Core (days 1-2)

1. Create `crates/nlink-lab-shared/` crate with message types and topic helpers
2. Create `bins/nlink-lab-backend/` with Zenoh session setup
3. Publish topology and health on startup
4. Handle `Ping`/`Status` queryables
5. Add `nlink-lab daemon` CLI command

### Phase 2: Metrics Collector (days 2-3)

6. Implement `MetricsCollector` using `RunningLab::diagnose()`
7. Publish `MetricsSnapshot` at configured interval
8. Publish per-interface metrics to individual topics
9. Rate formatting helper

### Phase 3: CLI Metrics Client (days 3-4)

10. Add `nlink-lab metrics` CLI command — Zenoh subscriber
11. Table output with terminal clearing refresh
12. JSON and CSV output formats
13. Node filtering + count limit

### Phase 4: Mutating Operations (days 5-6)

14. Handle `ExecRequest` queryable
15. Handle `ImpairmentRequest` queryable
16. `--daemon` flag on `nlink-lab deploy` to auto-start backend
17. Graceful shutdown on SIGTERM

### Phase 5: Polish (day 7)

18. Zenoh config CLI flags (mode, listen, connect)
19. Liveliness token for backend discovery
20. Lab event publishing (interface state changes)

## Progress

### Phase 1: Shared Types + Daemon Core
- [x] Create `crates/nlink-lab-shared/` with messages + topics
- [x] Create `bins/nlink-lab-backend/` with Zenoh session
- [x] Publish topology + health on startup
- [x] Handle `Status` queryable
- [x] `nlink-lab daemon` CLI command

### Phase 2: Metrics Collector
- [x] `MetricsCollector` from diagnostics
- [x] Publish `MetricsSnapshot` periodically
- [ ] Per-interface metrics publishing
- [x] Rate formatting helper

### Phase 3: CLI Metrics Client
- [x] `nlink-lab metrics` command (Zenoh subscriber)
- [x] Table output with refresh
- [x] JSON output
- [x] Node filtering + count limit

### Phase 4: Mutating Operations
- [x] Handle `ExecRequest`
- [x] Handle `ImpairmentRequest`
- [ ] `--daemon` flag on deploy
- [x] Graceful shutdown

### Phase 5: Polish
- [x] Zenoh config CLI flags
- [x] Liveliness token
- [ ] Lab event publishing
