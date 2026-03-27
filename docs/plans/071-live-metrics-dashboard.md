# Plan 071: Live Metrics Dashboard

**Priority:** High
**Effort:** 3-4 days
**Target:** `crates/nlink-lab/src/metrics.rs` (new), `bins/lab/src/main.rs`

## Summary

Add a `nlink-lab metrics` CLI command that streams live per-interface and
per-link metrics to the terminal in a TUI-like display. Uses nlink's
`Diagnostics::scan()` API with periodic sampling for rate calculations.

This is the headless counterpart to the TopoViewer GUI — useful in SSH
sessions, CI pipelines, and scripted monitoring.

## Architecture

```
crates/nlink-lab/src/
  metrics.rs            # NEW: MetricsCollector, MetricsSnapshot, time-series

bins/lab/src/
  main.rs               # Add Metrics command
```

## nlink Diagnostics API (what we consume)

Each `Diagnostics::scan()` call returns:

```
DiagnosticReport
├── interfaces: Vec<InterfaceDiag>
│   ├── name, ifindex, state, mtu, flags
│   ├── stats: LinkStats { rx_bytes, tx_bytes, rx_packets, tx_packets,
│   │                      rx_errors, tx_errors, rx_dropped, tx_dropped }
│   ├── rates: LinkRates { rx_bps, tx_bps, rx_pps, tx_pps }
│   ├── tc: Option<TcDiag> { qdisc, drops, overlimits, backlog, qlen,
│   │                         rate_bps, rate_pps, bytes, packets }
│   └── issues: Vec<Issue>
├── routes: RouteDiag { ipv4_count, ipv6_count, has_default_v4/v6 }
└── issues: Vec<Issue> { severity, category, message }
```

Rates are automatically calculated by the Diagnostics module between
consecutive `scan()` calls (it caches previous stats internally).

## Design

### MetricsCollector

```rust
/// Periodically collects diagnostics from all lab nodes.
pub struct MetricsCollector {
    lab: RunningLab,
    interval: Duration,
    history: VecDeque<MetricsSnapshot>,
    max_history: usize,
}

/// A single point-in-time snapshot of all node metrics.
pub struct MetricsSnapshot {
    pub timestamp: Instant,
    pub nodes: HashMap<String, NodeMetrics>,
}

/// Metrics for a single node.
pub struct NodeMetrics {
    pub interfaces: Vec<InterfaceMetrics>,
    pub issues: Vec<Issue>,
}

/// Metrics for a single interface.
pub struct InterfaceMetrics {
    pub name: String,
    pub state: OperState,
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
    pub tc_backlog: u32,
}

impl MetricsCollector {
    pub fn new(lab: RunningLab, interval: Duration) -> Self;

    /// Take a single snapshot (calls scan() on all nodes).
    pub async fn snapshot(&mut self) -> Result<MetricsSnapshot>;

    /// Stream snapshots at the configured interval.
    pub fn stream(&mut self) -> impl Stream<Item = Result<MetricsSnapshot>>;
}
```

### CLI Command

```
nlink-lab metrics <lab> [OPTIONS]

Options:
  -i, --interval <SEC>    Refresh interval in seconds (default: 2)
  -n, --node <NODE>       Filter to specific node
  -f, --format <FMT>      Output format: table (default), json, csv
  -c, --count <N>         Number of samples then exit (default: infinite)
  --no-header             Omit header row (for scripting)
  --interfaces-only       Show only interface metrics, skip routes/issues
```

### Output Formats

**Table (default) — refreshing terminal display:**

```
lab: datacenter-sim  |  nodes: 6  |  refresh: 2s  |  sample: #5

NODE         IFACE     STATE    RX rate    TX rate    RX pps   TX pps   ERRORS  DROPS
─────────────────────────────────────────────────────────────────────────────────────
spine1       eth1      UP       45.2 Mbps  45.1 Mbps  38.2k   37.9k      0      0
spine1       eth2      UP       22.8 Mbps  22.9 Mbps  19.1k   19.2k      0      0
leaf1        eth1      UP       45.1 Mbps  45.2 Mbps  37.9k   38.2k      0      0
leaf1        eth3      UP        1.2 Mbps   0.8 Mbps   1.0k    0.7k      0     12 ⚠
server1      eth0      UP        0.8 Mbps   1.2 Mbps   0.7k    1.0k      0      0

ISSUES:
  [WARN] leaf1:eth3 — qdisc drops detected (12 drops, netem delay=10ms)
```

**JSON — one object per sample, for piping:**

```json
{
  "timestamp": "2026-03-27T14:30:00Z",
  "nodes": {
    "spine1": {
      "interfaces": [
        {
          "name": "eth1",
          "state": "Up",
          "rx_bps": 45200000,
          "tx_bps": 45100000,
          "rx_errors": 0,
          "tx_errors": 0
        }
      ]
    }
  }
}
```

**CSV — for import into spreadsheets/analysis tools:**

```csv
timestamp,node,interface,state,rx_bps,tx_bps,rx_pps,tx_pps,errors,drops
2026-03-27T14:30:00Z,spine1,eth1,Up,45200000,45100000,38200,37900,0,0
```

### Integration with TopoViewer

The `MetricsCollector` is shared between the CLI metrics command and the
TopoViewer GUI. The TopoViewer uses `MetricsCollector::snapshot()` in its
subscription loop. The CLI uses `MetricsCollector::stream()` for continuous
output.

### Rate Formatting Helper

```rust
fn format_rate(bps: u64) -> String {
    match bps {
        0 => "0".to_string(),
        b if b < 1_000 => format!("{b} bps"),
        b if b < 1_000_000 => format!("{:.1} Kbps", b as f64 / 1_000.0),
        b if b < 1_000_000_000 => format!("{:.1} Mbps", b as f64 / 1_000_000.0),
        b => format!("{:.1} Gbps", b as f64 / 1_000_000_000.0),
    }
}
```

## Implementation Order

### Phase 1: Core Metrics (days 1-2)

1. Create `crates/nlink-lab/src/metrics.rs` with `MetricsCollector`
2. Implement `snapshot()` using `RunningLab::diagnose()`
3. Implement `stream()` using tokio interval
4. Add `Metrics` command to CLI with table output
5. Rate formatting helper

### Phase 2: Output Formats (day 2-3)

6. JSON output (`--format json`)
7. CSV output (`--format csv`)
8. Node filtering (`--node`)
9. Sample count limit (`--count`)

### Phase 3: Polish (day 3-4)

10. Terminal clearing for refreshing table display
11. Issue summary at bottom of table
12. Color output (green/yellow/red for status)
13. Register `metrics` module in lib.rs and re-export types

## Progress

### Phase 1: Core Metrics

- [ ] Create `metrics.rs` with `MetricsCollector`
- [ ] Implement `snapshot()` from diagnostics
- [ ] Implement `stream()` with tokio interval
- [ ] Add `Metrics` CLI command with table output
- [ ] Rate formatting helper

### Phase 2: Output Formats

- [ ] JSON output
- [ ] CSV output
- [ ] Node filtering
- [ ] Sample count limit

### Phase 3: Polish

- [ ] Refreshing terminal display
- [ ] Issue summary
- [ ] Color output (ANSI)
- [ ] Module registration and exports
