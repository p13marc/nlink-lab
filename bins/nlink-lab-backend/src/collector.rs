//! Metrics collector — gathers live stats from all lab nodes.

use std::collections::HashMap;

use nlink_lab::RunningLab;
use nlink_lab_shared::messages::{LabEvent, LabEventKind};
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};

pub struct MetricsCollector {
    /// Previous interface states: node -> iface -> state string.
    prev_states: HashMap<String, HashMap<String, String>>,
}

impl MetricsCollector {
    pub fn new(_lab: &RunningLab) -> Self {
        Self {
            prev_states: HashMap::new(),
        }
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

        for diag in &diagnostics {
            let prev_node = self.prev_states.entry(diag.node.clone()).or_default();
            let mut iface_metrics = Vec::new();

            for iface in &diag.interfaces {
                let state_str = iface.state.to_string();

                // Detect state changes
                if let Some(prev_state) = prev_node.get(&iface.name)
                    && *prev_state != state_str {
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

            nodes.insert(
                diag.node.clone(),
                NodeMetrics {
                    interfaces: iface_metrics,
                    issues,
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
