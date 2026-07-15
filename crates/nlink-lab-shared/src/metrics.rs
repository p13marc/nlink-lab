//! Metrics types for live lab monitoring.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A point-in-time snapshot of all node metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub lab_name: String,
    pub timestamp: u64,
    pub nodes: HashMap<String, NodeMetrics>,
}

/// Metrics for a single node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub interfaces: Vec<InterfaceMetrics>,
    pub issues: Vec<String>,
    /// Top TCP flows by goodput in this node's namespace, attributed to
    /// the owning process where resolvable (Plan 160 / nlink 0.24
    /// sockdiag). Empty for container nodes and when no flow moved data
    /// between the last two collector ticks. `#[serde(default)]` keeps
    /// the snapshot wire-compatible with backends that predate the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sockets: Vec<SocketRateMetric>,
}

/// One TCP flow's goodput over the last collector interval, attributed
/// to a process. Plain data (no nlink dependency) — the backend
/// collector fills it from nlink's `SocketRateTracker` +
/// `SocketOwnerMap`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketRateMetric {
    /// Owning process command name, or `"-"` when unresolved (a
    /// short-lived or other-user process the `/proc` walk couldn't see).
    pub comm: String,
    /// Owning PID, when resolved.
    pub pid: Option<u32>,
    /// Local `ip:port`.
    pub local: String,
    /// Remote `ip:port`.
    pub remote: String,
    /// Transmit goodput (application bytes/second the peer acked).
    pub tx_bytes_per_sec: u64,
    /// Receive goodput (application bytes/second).
    pub rx_bytes_per_sec: u64,
    /// Retransmission overhead: Δbytes_retrans / Δbytes_sent.
    pub retrans_ratio: f64,
}

/// Metrics for a single interface.
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

/// Format a rate in bits per second as a human-readable string.
pub fn format_rate(bps: u64) -> String {
    match bps {
        0 => "0".to_string(),
        b if b < 1_000 => format!("{b} bps"),
        b if b < 1_000_000 => format!("{:.1} Kbps", b as f64 / 1_000.0),
        b if b < 1_000_000_000 => format!("{:.1} Mbps", b as f64 / 1_000_000.0),
        b => format!("{:.1} Gbps", b as f64 / 1_000_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_rate() {
        assert_eq!(format_rate(0), "0");
        assert_eq!(format_rate(500), "500 bps");
        assert_eq!(format_rate(1_500), "1.5 Kbps");
        assert_eq!(format_rate(45_200_000), "45.2 Mbps");
        assert_eq!(format_rate(1_000_000_000), "1.0 Gbps");
    }

    /// The `sockets` field is `#[serde(default)]`, so a snapshot from a
    /// backend that predates it (no `sockets` key) still deserializes.
    #[test]
    fn node_metrics_deserializes_without_sockets_field() {
        let json = r#"{"interfaces":[],"issues":[]}"#;
        let nm: NodeMetrics = serde_json::from_str(json).unwrap();
        assert!(nm.sockets.is_empty());
    }

    /// An empty `sockets` vec is omitted from the serialized form
    /// (`skip_serializing_if`), keeping the common no-flows case compact.
    #[test]
    fn empty_sockets_are_not_serialized() {
        let nm = NodeMetrics {
            interfaces: vec![],
            issues: vec![],
            sockets: vec![],
        };
        let json = serde_json::to_string(&nm).unwrap();
        assert!(!json.contains("sockets"), "sockets should be elided: {json}");
    }
}
