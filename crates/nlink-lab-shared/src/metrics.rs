//! Metrics types for live lab monitoring.
//!
//! Every field is `#[serde(default)]`; see the crate docs for the wire
//! compatibility contract.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::WIRE_VERSION;

/// A point-in-time snapshot of all node metrics.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetricsSnapshot {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub lab_name: String,
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub nodes: HashMap<String, NodeMetrics>,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            wire_version: WIRE_VERSION,
            lab_name: String::new(),
            timestamp: 0,
            nodes: HashMap::new(),
        }
    }
}

/// Metrics for a single node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeMetrics {
    #[serde(default)]
    pub interfaces: Vec<InterfaceMetrics>,
    #[serde(default)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SocketRateMetric {
    /// Owning process command name, or `"-"` when unresolved (a
    /// short-lived or other-user process the `/proc` walk couldn't see).
    #[serde(default)]
    pub comm: String,
    /// Owning PID, when resolved.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Local `ip:port`.
    #[serde(default)]
    pub local: String,
    /// Remote `ip:port`.
    #[serde(default)]
    pub remote: String,
    /// Transmit goodput (application bytes/second the peer acked).
    #[serde(default)]
    pub tx_bytes_per_sec: u64,
    /// Receive goodput (application bytes/second).
    #[serde(default)]
    pub rx_bytes_per_sec: u64,
    /// Retransmission overhead: Δbytes_retrans / Δbytes_sent.
    #[serde(default)]
    pub retrans_ratio: f64,
}

/// Metrics for a single interface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InterfaceMetrics {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    /// Receive rate in **bits** per second — the unit [`format_rate`]
    /// expects. nlink's `LinkRates` counts bytes, so these are populated
    /// from its `rx_bps()`/`tx_bps()` accessors, which convert.
    #[serde(default)]
    pub rx_bps: u64,
    /// Transmit rate in **bits** per second. See [`Self::rx_bps`].
    #[serde(default)]
    pub tx_bps: u64,
    #[serde(default)]
    pub rx_pps: u64,
    #[serde(default)]
    pub tx_pps: u64,
    #[serde(default)]
    pub rx_errors: u64,
    #[serde(default)]
    pub tx_errors: u64,
    #[serde(default)]
    pub rx_dropped: u64,
    #[serde(default)]
    pub tx_dropped: u64,
    #[serde(default)]
    pub tc_drops: u64,
    #[serde(default)]
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
        let nm = NodeMetrics::default();
        let json = serde_json::to_string(&nm).unwrap();
        assert!(
            !json.contains("sockets"),
            "sockets should be elided: {json}"
        );
    }

    /// A snapshot with every field missing still decodes; a sender that
    /// predates versioning reads as `wire_version == 0`.
    #[test]
    fn snapshot_deserializes_from_empty_document() {
        let snap: MetricsSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(snap.wire_version, 0);
        assert!(snap.nodes.is_empty());

        let im: InterfaceMetrics = serde_json::from_str(r#"{"name":"eth0"}"#).unwrap();
        assert_eq!(im.name, "eth0");
        assert_eq!(im.rx_bps, 0);

        let sm: SocketRateMetric = serde_json::from_str("{}").unwrap();
        assert!(sm.pid.is_none());
    }

    /// Fields a newer backend adds are ignored by an older client.
    #[test]
    fn snapshot_ignores_unknown_fields() {
        let json = r#"{"wire_version":99,"lab_name":"l","nodes":{"a":{"interfaces":[{"name":"eth0","rx_bps":8,"future_counter":1}],"gpu":[]}},"future":{"x":1}}"#;
        let snap: MetricsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.wire_version, 99);
        assert_eq!(snap.nodes["a"].interfaces[0].rx_bps, 8);
    }

    #[test]
    fn snapshot_default_stamps_current_version() {
        let snap = MetricsSnapshot {
            lab_name: "l".into(),
            ..Default::default()
        };
        assert_eq!(snap.wire_version, WIRE_VERSION);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains(&format!("\"wire_version\":{WIRE_VERSION}")));
    }
}
