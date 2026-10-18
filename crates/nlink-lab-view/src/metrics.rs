//! The metrics a view shows, from the snapshots the daemon sends: each
//! interface's rate in each direction and a short history of them, a
//! "heat" of errors and drops that rises when they happen and fades when
//! they stop, and whether the samples stopped coming.

use std::collections::{BTreeMap, VecDeque};

use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot};

/// Samples kept per interface.
pub const HISTORY: usize = 120;
/// What the heat keeps of itself per sample once errors stop.
const COOLING: f32 = 0.7;

/// One interface's series.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IfaceSeries {
    /// Bits per second, oldest first.
    pub rx_bps: VecDeque<u64>,
    pub tx_bps: VecDeque<u64>,
    pub state: String,
    /// 0 to 1: 1 when errors or drops grew at the last sample.
    pub heat: f32,
    /// The last counters, to tell growth.
    last_bad: Option<u64>,
}

impl IfaceSeries {
    /// The latest rates: `(rx, tx)` in bits per second.
    pub fn rate(&self) -> (u64, u64) {
        (
            self.rx_bps.back().copied().unwrap_or(0),
            self.tx_bps.back().copied().unwrap_or(0),
        )
    }

    fn push(&mut self, m: &InterfaceMetrics) {
        for (series, value) in [(&mut self.rx_bps, m.rx_bps), (&mut self.tx_bps, m.tx_bps)] {
            if series.len() == HISTORY {
                series.pop_front();
            }
            series.push_back(value);
        }
        self.state = m.state.clone();
        let bad = m.rx_errors + m.tx_errors + m.rx_dropped + m.tx_dropped + m.tc_drops;
        let grew = self.last_bad.is_some_and(|last| bad > last);
        self.heat = if grew { 1.0 } else { self.heat * COOLING };
        if self.heat < 0.05 {
            self.heat = 0.0;
        }
        self.last_bad = Some(bad);
    }
}

/// The metrics of one lab.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetricsStore {
    series: BTreeMap<(String, String), IfaceSeries>,
    /// When the last snapshot came, in the caller's milliseconds.
    last_at: Option<u64>,
}

impl MetricsStore {
    /// Take a snapshot in, received at `now_ms`. Interfaces it no longer
    /// has are dropped.
    pub fn ingest(&mut self, snapshot: &MetricsSnapshot, now_ms: u64) {
        let mut seen = Vec::new();
        for (node, metrics) in &snapshot.nodes {
            for iface in &metrics.interfaces {
                let key = (node.clone(), iface.name.clone());
                self.series.entry(key.clone()).or_default().push(iface);
                seen.push(key);
            }
        }
        self.series.retain(|k, _| seen.contains(k));
        self.last_at = Some(now_ms);
    }

    pub fn series(&self, node: &str, iface: &str) -> Option<&IfaceSeries> {
        self.series.get(&(node.to_string(), iface.to_string()))
    }

    /// The latest rates of `node:iface`, `(rx, tx)` in bits per second.
    pub fn rate(&self, node: &str, iface: &str) -> Option<(u64, u64)> {
        self.series(node, iface).map(IfaceSeries::rate)
    }

    /// The summed rates of a node's interfaces.
    pub fn node_rate(&self, node: &str) -> (u64, u64) {
        self.series
            .iter()
            .filter(|((n, _), _)| n == node)
            .map(|(_, s)| s.rate())
            .fold((0, 0), |(r, t), (rx, tx)| (r + rx, t + tx))
    }

    /// The hottest of a node's interfaces.
    pub fn node_heat(&self, node: &str) -> f32 {
        self.series
            .iter()
            .filter(|((n, _), _)| n == node)
            .map(|(_, s)| s.heat)
            .fold(0.0, f32::max)
    }

    /// Whether the samples stopped: none for three intervals.
    pub fn is_stale(&self, now_ms: u64, interval_ms: u64) -> bool {
        self.last_at
            .is_none_or(|at| now_ms.saturating_sub(at) > interval_ms.max(250) * 3)
    }

    /// Whether it has had a snapshot.
    pub fn has_data(&self) -> bool {
        self.last_at.is_some()
    }
}

/// How busy a rate is, 0 to 1: a log scale from 1 kbit/s to 10 Gbit/s
/// without a capacity, its share of the capacity with one.
pub fn utilisation(bps: u64, capacity_bps: Option<u64>) -> f32 {
    match capacity_bps.filter(|c| *c > 0) {
        Some(cap) => (bps as f32 / cap as f32).clamp(0.0, 1.0),
        None if bps < 1_000 => 0.0,
        None => ((bps as f32).log10() - 3.0) / 7.0,
    }
    .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab_shared::metrics::NodeMetrics;

    fn snap(rx: u64, errors: u64) -> MetricsSnapshot {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            "a".to_string(),
            NodeMetrics {
                interfaces: vec![InterfaceMetrics {
                    name: "eth0".into(),
                    state: "up".into(),
                    rx_bps: rx,
                    tx_bps: rx / 2,
                    rx_errors: errors,
                    ..Default::default()
                }],
                issues: vec![],
                sockets: vec![],
            },
        );
        MetricsSnapshot {
            wire_version: 1,
            lab_name: "l".into(),
            timestamp: 0,
            nodes,
        }
    }

    #[test]
    fn rates_history_and_heat() {
        let mut m = MetricsStore::default();
        assert!(m.is_stale(0, 1000));
        m.ingest(&snap(1000, 0), 0);
        m.ingest(&snap(2000, 0), 1000);
        assert_eq!(m.rate("a", "eth0"), Some((2000, 1000)));
        assert_eq!(m.series("a", "eth0").unwrap().rx_bps.len(), 2);
        assert_eq!(m.node_rate("a"), (2000, 1000));
        // errors grow: hot; then they stop: it fades away
        m.ingest(&snap(2000, 5), 2000);
        assert_eq!(m.node_heat("a"), 1.0);
        for i in 0..10 {
            m.ingest(&snap(2000, 5), 3000 + i * 1000);
        }
        assert_eq!(m.node_heat("a"), 0.0);
        assert!(!m.is_stale(13_000, 1000));
        assert!(m.is_stale(20_000, 1000));
    }

    #[test]
    fn history_is_bounded_and_gone_interfaces_go() {
        let mut m = MetricsStore::default();
        for i in 0..(HISTORY as u64 + 10) {
            m.ingest(&snap(i, 0), i);
        }
        assert_eq!(m.series("a", "eth0").unwrap().rx_bps.len(), HISTORY);
        let empty = MetricsSnapshot {
            wire_version: 1,
            lab_name: "l".into(),
            timestamp: 0,
            nodes: Default::default(),
        };
        m.ingest(&empty, 999);
        assert!(m.series("a", "eth0").is_none());
    }

    #[test]
    fn utilisation_scales() {
        assert_eq!(utilisation(0, None), 0.0);
        assert!((utilisation(10_000_000_000, None) - 1.0).abs() < 1e-6);
        assert!((utilisation(50, Some(100)) - 0.5).abs() < 1e-6);
        assert_eq!(utilisation(500, Some(100)), 1.0);
    }
}
