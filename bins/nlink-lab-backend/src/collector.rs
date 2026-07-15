//! Metrics collector — gathers live stats from all lab nodes.

use std::collections::HashMap;
use std::time::Instant;

use nlink::netlink::{Connection, SockDiag, namespace};
use nlink::sockdiag::{SocketFilter, SocketOwnerMap, SocketRateTracker};
use nlink_lab::RunningLab;
use nlink_lab_shared::messages::{LabEvent, LabEventKind};
use nlink_lab_shared::metrics::{
    InterfaceMetrics, MetricsSnapshot, NodeMetrics, SocketRateMetric,
};

/// Top TCP flows (by goodput) reported per node.
const TOP_FLOWS: usize = 5;

pub struct MetricsCollector {
    /// Previous interface states: node -> iface -> state string.
    prev_states: HashMap<String, HashMap<String, String>>,
    /// Per-node cookie-keyed TCP goodput trackers (nlink 0.24). Kept
    /// across ticks so `ingest` can diff consecutive dumps; the first
    /// tick for a node only establishes the baseline.
    socket_trackers: HashMap<String, SocketRateTracker>,
}

impl MetricsCollector {
    pub fn new(_lab: &RunningLab) -> Self {
        Self {
            prev_states: HashMap::new(),
            socket_trackers: HashMap::new(),
        }
    }

    /// Dump TCP sockets in `ns_name`, diff against the node's tracker,
    /// and return the top flows by goodput attributed to their owning
    /// process. Best-effort: any error yields an empty vec (a node's
    /// socket view must never fail the whole snapshot).
    async fn collect_sockets(
        &mut self,
        node: &str,
        ns_name: &str,
        owners: &SocketOwnerMap,
    ) -> Vec<SocketRateMetric> {
        let conn: Connection<SockDiag> = match namespace::connection_for(ns_name) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("sockdiag connection for '{node}' failed: {e}");
                return Vec::new();
            }
        };
        // TCP byte counters ride in TCP_INFO — it must be requested.
        let snapshot = match conn.query(&SocketFilter::tcp().with_tcp_info().build()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("sockdiag query for '{node}' failed: {e}");
                return Vec::new();
            }
        };
        let inet: Vec<_> = snapshot.iter().filter_map(|s| s.as_inet()).collect();
        // cookie -> (inode, local, remote) join keys for this snapshot.
        let keys: HashMap<u64, (u32, String, String)> = inet
            .iter()
            .map(|s| (s.cookie, (s.inode, s.local.to_string(), s.remote.to_string())))
            .collect();

        let tracker = self.socket_trackers.entry(node.to_string()).or_default();
        let mut rates = tracker.ingest(inet.iter().copied(), Instant::now());
        rates.sort_by_key(|r| std::cmp::Reverse(r.tx_goodput_bps + r.rx_goodput_bps));

        rates
            .iter()
            .take(TOP_FLOWS)
            .map(|r| {
                let (inode, local, remote) = keys
                    .get(&r.cookie)
                    .cloned()
                    .unwrap_or((0, String::new(), String::new()));
                let owner = owners.resolve(inode).first();
                SocketRateMetric {
                    comm: owner.map_or_else(|| "-".to_string(), |p| p.comm.clone()),
                    pid: owner.map(|p| p.pid as u32),
                    local,
                    remote,
                    tx_bytes_per_sec: r.tx_goodput_bps,
                    rx_bytes_per_sec: r.rx_goodput_bps,
                    retrans_ratio: r.retrans_ratio,
                }
            })
            .collect()
    }

    /// Collect a snapshot and detect interface state change events.
    pub async fn snapshot(
        &mut self,
        lab: &RunningLab,
    ) -> Result<(MetricsSnapshot, Vec<LabEvent>), nlink_lab::Error> {
        let diagnostics = lab.diagnose(None).await?;
        let mut nodes = HashMap::new();
        let mut events = Vec::new();
        let lab_name = lab.name().to_string();
        let timestamp = crate::now_unix();

        // One amortized `/proc` walk joins socket inodes to owning
        // processes for every node this tick (inodes are global, so a
        // single scan serves all namespaces).
        let socket_owners = SocketOwnerMap::scan();

        for diag in &diagnostics {
            let prev_node = self.prev_states.entry(diag.node.clone()).or_default();
            let mut iface_metrics = Vec::new();

            for iface in &diag.interfaces {
                let state_str = iface.state.to_string();

                // Detect state changes
                if let Some(prev_state) = prev_node.get(&iface.name)
                    && *prev_state != state_str
                {
                    let kind = if state_str == "up" {
                        LabEventKind::InterfaceUp {
                            node: diag.node.clone(),
                            interface: iface.name.clone(),
                        }
                    } else {
                        LabEventKind::InterfaceDown {
                            node: diag.node.clone(),
                            interface: iface.name.clone(),
                        }
                    };
                    events.push(LabEvent {
                        lab_name: lab_name.clone(),
                        timestamp,
                        kind,
                    });
                }
                prev_node.insert(iface.name.clone(), state_str.clone());

                iface_metrics.push(InterfaceMetrics {
                    name: iface.name.clone(),
                    state: state_str,
                    rx_bps: iface.rates.rx_bps,
                    tx_bps: iface.rates.tx_bps,
                    rx_pps: iface.rates.rx_pps,
                    tx_pps: iface.rates.tx_pps,
                    rx_errors: iface.stats.rx_errors(),
                    tx_errors: iface.stats.tx_errors(),
                    rx_dropped: iface.stats.rx_dropped(),
                    tx_dropped: iface.stats.tx_dropped(),
                    tc_drops: iface.tc.as_ref().map_or(0, |tc| tc.drops),
                    tc_qlen: iface.tc.as_ref().map_or(0, |tc| tc.qlen),
                });
            }

            let issues: Vec<String> = diag.issues.iter().map(|i| i.to_string()).collect();

            // Per-process TCP goodput for bare-namespace nodes; container
            // nodes (no entry in the namespace map) are skipped.
            let sockets = match lab.namespace_name_of(&diag.node) {
                Some(ns_name) => {
                    let ns_name = ns_name.to_string();
                    self.collect_sockets(&diag.node, &ns_name, &socket_owners)
                        .await
                }
                None => Vec::new(),
            };

            nodes.insert(
                diag.node.clone(),
                NodeMetrics {
                    interfaces: iface_metrics,
                    issues,
                    sockets,
                },
            );
        }

        let snapshot = MetricsSnapshot {
            lab_name,
            timestamp,
            nodes,
        };

        Ok((snapshot, events))
    }
}
