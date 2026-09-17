//! Metrics collector — gathers live stats from all lab nodes.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use nlink::netlink::{Connection, SockDiag, namespace};
use nlink::sockdiag::{SocketFilter, SocketOwnerMap, SocketRateTracker};
use nlink_lab::RunningLab;
use nlink_lab::deploy::NsRef;
use nlink_lab_shared::WIRE_VERSION;
use nlink_lab_shared::messages::{LabEvent, LabEventKind};
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics, SocketRateMetric};

/// Top TCP flows (by goodput) reported per node.
const TOP_FLOWS: usize = 5;

/// The cumulative link counters a rate is differenced from, as read on
/// one tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IfaceCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_pkts: u64,
    pub tx_pkts: u64,
}

/// Per-second rates over the interval between two samples, in the units
/// [`InterfaceMetrics`] documents: **bits** per second for bytes,
/// packets per second for packets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IfaceRates {
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub rx_pps: u64,
    pub tx_pps: u64,
}

/// Rates between two counter samples `elapsed_ms` apart.
///
/// Returns all-zero — the same "no reading yet" value a first sample
/// gets — when the interval is degenerate or any counter stepped
/// backwards. A backwards step means the interface was recreated (its
/// counters restarted at zero), so the delta across that boundary is
/// not a rate; reporting `0` is a smaller lie than reporting a
/// `saturating_sub`'d one, and the caller re-baselines.
///
/// This lives here, in the collector, rather than in nlink's
/// `Diagnostics`, because `RunningLab::diagnose` builds a fresh
/// `Diagnostics` per call — so its own `prev_stats` is always empty and
/// every `LinkRates` it returns is the default (#133).
pub fn rates_between(prev: IfaceCounters, now: IfaceCounters, elapsed_ms: u64) -> IfaceRates {
    if elapsed_ms == 0
        || now.rx_bytes < prev.rx_bytes
        || now.tx_bytes < prev.tx_bytes
        || now.rx_pkts < prev.rx_pkts
        || now.tx_pkts < prev.tx_pkts
    {
        return IfaceRates::default();
    }
    let per_sec = |delta: u64| -> u64 { delta.saturating_mul(1_000) / elapsed_ms };
    IfaceRates {
        rx_bps: per_sec((now.rx_bytes - prev.rx_bytes).saturating_mul(8)),
        tx_bps: per_sec((now.tx_bytes - prev.tx_bytes).saturating_mul(8)),
        rx_pps: per_sec(now.rx_pkts - prev.rx_pkts),
        tx_pps: per_sec(now.tx_pkts - prev.tx_pkts),
    }
}

pub struct MetricsCollector {
    /// Previous interface states: node -> iface -> state string.
    prev_states: HashMap<String, HashMap<String, String>>,
    /// Previous cumulative link counters: node -> iface -> (read at,
    /// counters). This is where interface rates come from — see
    /// [`rates_between`] and #133. Entries for interfaces that vanish
    /// are pruned each tick so a destroyed node leaks nothing.
    prev_counters: HashMap<String, HashMap<String, (Instant, IfaceCounters)>>,
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
            prev_counters: HashMap::new(),
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
        ns: &NsRef,
        owners: &SocketOwnerMap,
    ) -> Vec<SocketRateMetric> {
        // A bare namespace by name, a container by its init pid: both are
        // a network namespace sockdiag can be opened in.
        let conn: Result<Connection<SockDiag>, _> = match ns {
            NsRef::Named { name } => namespace::connection_for(name),
            NsRef::Container { pid, .. } => namespace::connection_for_pid(*pid),
            NsRef::Root => namespace::connection_for_path("/proc/self/ns/net"),
        };
        let conn = match conn {
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
                let exit_code = reap_exit_code(proc_info.pid).or(proc_info.exit_code);
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
            let counters_node = self.prev_counters.entry(diag.node.clone()).or_default();
            let mut seen_ifaces: HashSet<&str> = HashSet::new();
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

                // Rates, differenced here rather than taken from
                // `iface.rates` — those are always `LinkRates::default()`
                // because `RunningLab::diagnose` builds a fresh
                // `Diagnostics` per call, so the runner that would hold
                // the previous sample is dropped before the next one
                // (#133).
                let now = Instant::now();
                let counters = IfaceCounters {
                    rx_bytes: iface.stats.rx_bytes(),
                    tx_bytes: iface.stats.tx_bytes(),
                    rx_pkts: iface.stats.rx_packets(),
                    tx_pkts: iface.stats.tx_packets(),
                };
                let rates = match counters_node.get(&iface.name) {
                    Some((prev_at, prev)) => rates_between(
                        *prev,
                        counters,
                        now.duration_since(*prev_at).as_millis() as u64,
                    ),
                    // First sample for this interface: the baseline, no
                    // reading yet. Same contract the flow rates have.
                    None => IfaceRates::default(),
                };
                counters_node.insert(iface.name.clone(), (now, counters));
                seen_ifaces.insert(iface.name.as_str());

                iface_metrics.push(InterfaceMetrics {
                    name: iface.name.clone(),
                    state: state_str,
                    rx_bps: rates.rx_bps,
                    tx_bps: rates.tx_bps,
                    rx_pps: rates.rx_pps,
                    tx_pps: rates.tx_pps,
                    rx_bytes: counters.rx_bytes,
                    tx_bytes: counters.tx_bytes,
                    rx_pkts: counters.rx_pkts,
                    tx_pkts: counters.tx_pkts,
                    rx_errors: iface.stats.rx_errors(),
                    tx_errors: iface.stats.tx_errors(),
                    rx_dropped: iface.stats.rx_dropped(),
                    tx_dropped: iface.stats.tx_dropped(),
                    tc_drops: iface.tc.as_ref().map_or(0, |tc| tc.drops),
                    tc_qlen: iface.tc.as_ref().map_or(0, |tc| tc.qlen),
                });
            }
            // Drop baselines for interfaces that are gone, so a node
            // whose veths were removed does not keep them forever.
            counters_node.retain(|name, _| seen_ifaces.contains(name.as_str()));

            let issues: Vec<String> = diag.issues.iter().map(|i| i.to_string()).collect();

            // Per-process TCP goodput, for bare-namespace and container
            // nodes alike (a container's namespace is its init pid's).
            let sockets = match lab.ns_resolver_of(&diag.node) {
                Some(ns) => self.collect_sockets(&diag.node, &ns, &socket_owners).await,
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
        // 10s: the loop returns on the first success; the budget only has
        // to outlast a starved runner, not a fast one.
        for _ in 0..1000 {
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

    /// The first sample for an interface has nothing to difference
    /// against, so it reports no rate rather than a rate off a zero
    /// baseline (which would read as "everything since boot arrived in
    /// this tick").
    #[test]
    fn rates_between_is_zero_without_a_previous_sample() {
        let now = IfaceCounters {
            rx_bytes: 1_000_000,
            tx_bytes: 2_000_000,
            rx_pkts: 900,
            tx_pkts: 800,
        };
        // A caller with no baseline passes none at all; this asserts the
        // degenerate-interval guard, which is the other way in.
        assert_eq!(
            rates_between(IfaceCounters::default(), now, 0),
            IfaceRates::default()
        );
    }

    /// Bytes become **bits** per second; packets stay packets.
    #[test]
    fn rates_between_converts_bytes_to_bits() {
        let prev = IfaceCounters {
            rx_bytes: 1_000,
            tx_bytes: 2_000,
            rx_pkts: 10,
            tx_pkts: 20,
        };
        let now = IfaceCounters {
            rx_bytes: 1_000 + 12_500,
            tx_bytes: 2_000 + 25_000,
            rx_pkts: 10 + 50,
            tx_pkts: 20 + 100,
        };
        // 12_500 bytes in 100 ms = 125_000 B/s = 1_000_000 bit/s.
        let r = rates_between(prev, now, 100);
        assert_eq!(r.rx_bps, 1_000_000);
        assert_eq!(r.tx_bps, 2_000_000);
        assert_eq!(r.rx_pps, 500);
        assert_eq!(r.tx_pps, 1_000);
    }

    /// A counter that stepped backwards means the interface was
    /// recreated. The interval spans the restart, so it is not
    /// measurable — report no rate rather than a `saturating_sub`'d one,
    /// which would silently read as an idle tick.
    #[test]
    fn rates_between_is_zero_across_a_counter_reset() {
        let prev = IfaceCounters {
            rx_bytes: 25_353_219,
            tx_bytes: 10_000,
            rx_pkts: 5_000,
            tx_pkts: 4_000,
        };
        let now = IfaceCounters {
            rx_bytes: 3_445,
            tx_bytes: 12_000,
            rx_pkts: 9,
            tx_pkts: 4_100,
        };
        assert_eq!(rates_between(prev, now, 10_000), IfaceRates::default());
    }

    /// An unchanged interface over a real interval reports zero — which
    /// is a genuine reading, not the "no reading" of the cases above.
    #[test]
    fn rates_between_reports_zero_for_an_idle_interface() {
        let c = IfaceCounters {
            rx_bytes: 42,
            tx_bytes: 42,
            rx_pkts: 1,
            tx_pkts: 1,
        };
        assert_eq!(rates_between(c, c, 5_000), IfaceRates::default());
    }
}
