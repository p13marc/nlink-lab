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
}
