use crate::util::now_unix;
use nlink_lab_shared::{messages::*, topics};
use std::time::{Duration, Instant};

pub(crate) async fn run_daemon_inline(lab: &nlink_lab::RunningLab) -> nlink_lab::Result<()> {
    let zenoh_config = zenoh::Config::default();
    let session = zenoh::open(zenoh_config).await.map_err(|e| {
        nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
    })?;

    let lab_name = lab.name().to_string();
    let start_time = Instant::now();

    let topo_publisher = session
        .declare_publisher(topics::topology(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;
    let health_publisher = session
        .declare_publisher(topics::health(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;
    let snapshot_publisher = session
        .declare_publisher(topics::metrics_snapshot(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;

    let exec_queryable = session
        .declare_queryable(topics::rpc_exec(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("queryable: {e}")))?;
    let status_queryable = session
        .declare_queryable(topics::rpc_status(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("queryable: {e}")))?;

    // Publish initial topology
    let topo_json = serde_json::to_string(lab.topology())?;
    let topo_update = TopologyUpdate {
        lab_name: lab_name.clone(),
        timestamp: now_unix(),
        node_count: lab.topology().nodes.len(),
        link_count: lab.topology().links.len(),
        topology_json: topo_json,
    };
    topo_publisher
        .put(serde_json::to_vec(&topo_update).unwrap())
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publish: {e}")))?;

    let _token = session
        .liveliness()
        .declare_token(topics::health(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("liveliness: {e}")))?;

    eprintln!("Backend daemon running (Ctrl-C to stop)");

    let mut health_interval = tokio::time::interval(Duration::from_secs(10));
    let mut metrics_interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                if let Ok(diags) = lab.diagnose(None).await {
                    let snapshot = diags_to_snapshot(&lab_name, &diags);
                    // Per-interface metrics
                    for (node_name, node_metrics) in &snapshot.nodes {
                        for iface in &node_metrics.interfaces {
                            let topic = topics::metrics_iface(&lab_name, node_name, &iface.name);
                            if let Ok(json) = serde_json::to_vec(iface) {
                                let _ = session.put(&topic, json).await;
                            }
                        }
                    }
                    // Full snapshot
                    if let Ok(json) = serde_json::to_vec(&snapshot) {
                        let _ = snapshot_publisher.put(json).await;
                    }
                }
            }
            _ = health_interval.tick() => {
                let status = HealthStatus {
                    lab_name: lab_name.clone(),
                    timestamp: now_unix(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: 0,
                    pid_count: lab.process_status().len(),
                    uptime_secs: start_time.elapsed().as_secs(),
                };
                if let Ok(json) = serde_json::to_vec(&status) {
                    let _ = health_publisher.put(json).await;
                }
            }
            Ok(query) = exec_queryable.recv_async() => {
                if let Some(payload) = query.payload()
                    && let Ok(req) = serde_json::from_slice::<ExecRequest>(&payload.to_bytes()) {
                        let args: Vec<&str> = req.args.iter().map(|s| s.as_str()).collect();
                        let resp = match lab.exec(&req.node, &req.cmd, &args) {
                            Ok(output) => ExecResponse {
                                success: output.exit_code == 0,
                                exit_code: output.exit_code,
                                stdout: output.stdout,
                                stderr: output.stderr,
                            },
                            Err(e) => ExecResponse {
                                success: false,
                                exit_code: -1,
                                stdout: String::new(),
                                stderr: e.to_string(),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&resp) {
                            let _ = query.reply(topics::rpc_exec(&lab_name), json).await;
                        }
                    }
            }
            Ok(query) = status_queryable.recv_async() => {
                let resp = StatusResponse {
                    lab_name: lab_name.clone(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: 0,
                    uptime_secs: start_time.elapsed().as_secs(),
                    nodes: lab.node_names().map(|s| s.to_string()).collect(),
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = query.reply(topics::rpc_status(&lab_name), json).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nShutting down daemon");
                break;
            }
        }
    }

    Ok(())
}

fn diags_to_snapshot(
    lab_name: &str,
    diags: &[nlink_lab::NodeDiagnostic],
) -> nlink_lab_shared::metrics::MetricsSnapshot {
    use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};
    let mut nodes = std::collections::HashMap::new();
    for diag in diags {
        let iface_metrics: Vec<InterfaceMetrics> = diag
            .interfaces
            .iter()
            .map(|iface| InterfaceMetrics {
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
            })
            .collect();
        let issues: Vec<String> = diag.issues.iter().map(|i| i.to_string()).collect();
        nodes.insert(
            diag.node.clone(),
            NodeMetrics {
                interfaces: iface_metrics,
                issues,
            },
        );
    }
    MetricsSnapshot {
        lab_name: lab_name.to_string(),
        timestamp: now_unix(),
        nodes,
    }
}
