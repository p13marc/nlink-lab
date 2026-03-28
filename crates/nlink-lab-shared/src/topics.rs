//! Zenoh key expression helpers for nlink-lab.
//!
//! All topics follow the format: `nlink-lab/{lab-name}/{category}/...`

/// Topology state (published on startup and changes).
pub fn topology(lab: &str) -> String {
    format!("nlink-lab/{lab}/topology")
}

/// Backend health/liveness status.
pub fn health(lab: &str) -> String {
    format!("nlink-lab/{lab}/health")
}

/// Per-interface metrics (high frequency, best-effort).
pub fn metrics_iface(lab: &str, node: &str, iface: &str) -> String {
    format!("nlink-lab/{lab}/metrics/{node}/{iface}")
}

/// Full metrics snapshot (all nodes, periodic).
pub fn metrics_snapshot(lab: &str) -> String {
    format!("nlink-lab/{lab}/metrics/snapshot")
}

/// Lab events (interface state changes, process exits).
pub fn events(lab: &str) -> String {
    format!("nlink-lab/{lab}/events")
}

/// RPC: execute command in a node.
pub fn rpc_exec(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/exec")
}

/// RPC: modify impairment on an interface.
pub fn rpc_impairment(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/impairment")
}

/// RPC: get lab status summary.
pub fn rpc_status(lab: &str) -> String {
    format!("nlink-lab/{lab}/rpc/status")
}

/// Wildcard: subscribe to all labs' topology updates.
pub fn all_topologies() -> &'static str {
    "nlink-lab/*/topology"
}

/// Wildcard: subscribe to all labs' health status.
pub fn all_health() -> &'static str {
    "nlink-lab/*/health"
}

/// Extract the lab name from a topic key expression.
///
/// Returns `None` if the key doesn't match the expected format.
pub fn extract_lab_name(key_expr: &str) -> Option<&str> {
    key_expr.strip_prefix("nlink-lab/")?.split('/').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_helpers() {
        assert_eq!(topology("dc"), "nlink-lab/dc/topology");
        assert_eq!(health("dc"), "nlink-lab/dc/health");
        assert_eq!(
            metrics_iface("dc", "spine1", "eth0"),
            "nlink-lab/dc/metrics/spine1/eth0"
        );
        assert_eq!(rpc_exec("dc"), "nlink-lab/dc/rpc/exec");
    }

    #[test]
    fn test_extract_lab_name() {
        assert_eq!(
            extract_lab_name("nlink-lab/dc-east/topology"),
            Some("dc-east")
        );
        assert_eq!(
            extract_lab_name("nlink-lab/my-lab/metrics/spine1/eth0"),
            Some("my-lab")
        );
        assert_eq!(extract_lab_name("other/prefix"), None);
    }
}
