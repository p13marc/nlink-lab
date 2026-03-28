//! Metrics collector — gathers live stats from all lab nodes.

use std::collections::HashMap;

use nlink_lab::RunningLab;
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};

pub struct MetricsCollector;

impl MetricsCollector {
    pub fn new(_lab: &RunningLab) -> Self {
        Self
    }

    pub async fn snapshot(
        &mut self,
        lab: &RunningLab,
    ) -> Result<MetricsSnapshot, nlink_lab::Error> {
        let diagnostics = lab.diagnose(None).await?;
        let mut nodes = HashMap::new();

        for diag in &diagnostics {
            let mut iface_metrics = Vec::new();

            for iface in &diag.interfaces {
                iface_metrics.push(InterfaceMetrics {
                    name: iface.name.clone(),
                    state: format!("{:?}", iface.state),
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

            nodes.insert(
                diag.node.clone(),
                NodeMetrics {
                    interfaces: iface_metrics,
                    issues,
                },
            );
        }

        Ok(MetricsSnapshot {
            lab_name: lab.name().to_string(),
            timestamp: crate::now_unix(),
            nodes,
        })
    }
}
