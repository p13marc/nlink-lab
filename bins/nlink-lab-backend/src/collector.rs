//! Metrics collector — gathers live stats from all lab nodes.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use nlink::netlink::{Connection, SockDiag, namespace};
use nlink::sockdiag::{SocketFilter, SocketOwnerMap, SocketRateTracker};
use nlink_lab::RunningLab;
use nlink_lab_shared::WIRE_VERSION;
use nlink_lab_shared::messages::{LabEvent, LabEventKind};
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics, SocketRateMetric};

/// Top TCP flows (by goodput) reported per node.
const TOP_FLOWS: usize = 5;

pub struct MetricsCollector {
    /// Previous interface states: node -> iface -> state string.
    prev_states: HashMap<String, HashMap<String, String>>,
    /// Per-node cookie-keyed TCP goodput trackers (nlink 0.24). Kept
    /// across ticks so `ingest` can diff consecutive dumps; the first
    /// tick for a node only establishes the baseline.
    socket_trackers: HashMap<String, SocketRateTracker>,
    /// Tracked background PIDs that were alive at the previous tick.
    /// `None` until the first tick establishes the baseline, so a
    /// process that was already dead when the daemon started never
    /// fires a `ProcessExited`.
    alive_pids: Option<HashSet<u32>>,
}

impl MetricsCollector {
    pub fn new(_lab: &RunningLab) -> Self {
        Self {
            prev_states: HashMap::new(),
            socket_trackers: HashMap::new(),
            alive_pids: None,
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
        let snapshot = match conn
            .query(&SocketFilter::tcp().with_tcp_info().build())
            .await
        {
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
            .map(|s| {
                (
                    s.cookie,
                    (s.inode, s.local.to_string(), s.remote.to_string()),
                )
            })
            .collect();

        let tracker = self.socket_trackers.entry(node.to_string()).or_default();
        let mut rates = tracker.ingest(inet.iter().copied(), Instant::now());
        rates.sort_by_key(|r| std::cmp::Reverse(r.tx_goodput_bps + r.rx_goodput_bps));

        rates
            .iter()
            .take(TOP_FLOWS)
            .map(|r| {
                let (inode, local, remote) =
                    keys.get(&r.cookie)
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

    /// Diff the lab's tracked PIDs against the previous tick and emit a
    /// `ProcessExited` for each one that was alive and is not any more.
    fn process_events(&mut self, lab: &RunningLab, timestamp: u64) -> Vec<LabEvent> {
        let status = lab.process_status();
        let now_alive: HashSet<u32> = status.iter().filter(|p| p.alive).map(|p| p.pid).collect();

        let mut events = Vec::new();
        if let Some(prev) = &self.alive_pids {
            for proc_info in status.iter().filter(|p| !p.alive && prev.contains(&p.pid)) {
                let exit_code = reap_exit_code(proc_info.pid);
                tracing::info!(
                    node = proc_info.node,
                    pid = proc_info.pid,
                    exit_code = ?exit_code,
                    "tracked process exited"
                );
                events.push(LabEvent::new(
                    lab.name(),
                    timestamp,
                    LabEventKind::ProcessExited {
                        node: proc_info.node.clone(),
                        pid: proc_info.pid,
                        exit_code,
                    },
                ));
            }
        }
        self.alive_pids = Some(now_alive);
        events
    }

    /// Collect a snapshot and detect interface state change and process
    /// exit events.
    pub async fn snapshot(
        &mut self,
        lab: &RunningLab,
    ) -> Result<(MetricsSnapshot, Vec<LabEvent>), nlink_lab::Error> {
        let diagnostics = lab.diagnose(None).await?;
        let mut nodes = HashMap::new();
        let lab_name = lab.name().to_string();
        let timestamp = crate::now_unix();
        let mut events = self.process_events(lab, timestamp);

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
                    events.push(LabEvent::new(&lab_name, timestamp, kind));
                }
                prev_node.insert(iface.name.clone(), state_str.clone());

                iface_metrics.push(InterfaceMetrics {
                    name: iface.name.clone(),
                    state: state_str,
                    rx_bps: iface.rates.rx_bps(),
                    tx_bps: iface.rates.tx_bps(),
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
            wire_version: WIRE_VERSION,
            lab_name,
            timestamp,
            nodes,
        };

        Ok((snapshot, events))
    }
}

/// Best-effort exit status for a tracked process that just died.
///
/// Only the parent can collect it: when the daemon runs inline in the
/// process that spawned the lab (`deploy --daemon`) the child is a zombie
/// nobody has waited on, and `waitpid(WNOHANG)` reaps it and yields the
/// status. In the standalone binary `waitpid` fails with `ECHILD` and
/// this returns `None`. Signal deaths are reported shell-style as
/// `128 + signo`.
fn reap_exit_code(pid: u32) -> Option<i32> {
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid with a valid out-pointer and WNOHANG never blocks
    // and only affects a child of this process.
    let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    if rc != pid as libc::pid_t {
        return None;
    }
    if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Some(128 + libc::WTERMSIG(status))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child we spawned and never waited on is reaped with its status.
    // The test reaps by hand through `reap_exit_code` (that is the point);
    // a `wait()` would steal the status it is checking for.
    #[allow(clippy::zombie_processes)]
    #[test]
    fn reap_exit_code_reads_own_child() {
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .expect("spawn sh");
        let pid = child.id();
        // Poll until the zombie is collectable (never wait() ourselves —
        // that would reap it first).
        let mut code = None;
        for _ in 0..200 {
            code = reap_exit_code(pid);
            if code.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(code, Some(3));
    }

    /// A PID that is not our child yields `None` rather than an error.
    #[test]
    fn reap_exit_code_none_for_foreign_pid() {
        assert_eq!(reap_exit_code(1), None);
    }
}
