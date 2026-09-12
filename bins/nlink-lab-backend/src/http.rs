//! HTTP / OpenMetrics endpoint next to the zenoh surface (issue #64).
//!
//! A deliberately tiny HTTP/1.1 responder on tokio — four `GET` routes,
//! no framework: `curl`, Prometheus and Grafana get the same data the
//! zenoh publishers carry, without a zenoh client.
//!
//! - `GET /metrics` — OpenMetrics text (interface counters, socket
//!   rates, health) for a Prometheus scrape
//! - `GET /api/v1/snapshot` — the latest [`MetricsSnapshot`] as JSON
//! - `GET /api/v1/health` — the latest [`HealthStatus`] as JSON
//! - `GET /api/v1/topology` — the deployed `Topology` as JSON

use std::sync::Arc;

use nlink_lab_shared::messages::HealthStatus;
use nlink_lab_shared::metrics::MetricsSnapshot;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

/// What the endpoint serves; the backend loop updates it on every tick.
#[derive(Default)]
pub struct State {
    pub snapshot: Option<MetricsSnapshot>,
    pub health: Option<HealthStatus>,
    pub topology: serde_json::Value,
}

pub type Shared = Arc<RwLock<State>>;

/// Bind `addr` and serve until the returned task is aborted.
pub async fn spawn(
    addr: std::net::SocketAddr,
    state: Shared,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "HTTP endpoint listening (/metrics, /api/v1/*)");
    Ok(tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(stream, state).await {
                    tracing::debug!("http connection: {e}");
                }
            });
        }
    }))
}

async fn handle(mut stream: tokio::net::TcpStream, state: Shared) -> std::io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = path.split('?').next().unwrap_or("/");
    let (status, content_type, body) = if method != "GET" {
        (
            "405 Method Not Allowed",
            "text/plain",
            "GET only\n".to_string(),
        )
    } else {
        match path {
            "/metrics" => (
                "200 OK",
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
                openmetrics(&*state.read().await),
            ),
            "/api/v1/snapshot" => json_or_404(state.read().await.snapshot.as_ref()),
            "/api/v1/health" => json_or_404(state.read().await.health.as_ref()),
            "/api/v1/topology" => (
                "200 OK",
                "application/json",
                serde_json::to_string_pretty(&state.read().await.topology).unwrap_or_default(),
            ),
            "/" => (
                "200 OK",
                "text/plain",
                "nlink-lab backend: /metrics, /api/v1/snapshot, /api/v1/health, /api/v1/topology\n"
                    .to_string(),
            ),
            _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
        }
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn json_or_404<T: serde::Serialize>(v: Option<&T>) -> (&'static str, &'static str, String) {
    match v {
        Some(v) => (
            "200 OK",
            "application/json",
            serde_json::to_string_pretty(v).unwrap_or_default(),
        ),
        None => ("404 Not Found", "text/plain", "no sample yet\n".to_string()),
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

type Getter = fn(&nlink_lab_shared::metrics::InterfaceMetrics) -> u64;

/// OpenMetrics rendering of the latest snapshot + health. Deterministic
/// (nodes and interfaces sorted) so it can be diffed in tests.
pub fn openmetrics(state: &State) -> String {
    let mut out = String::new();
    if let Some(h) = &state.health {
        let lab = esc(&h.lab_name);
        for (name, help, value) in [
            (
                "nlink_lab_nodes",
                "Nodes in the topology.",
                h.node_count as u64,
            ),
            (
                "nlink_lab_namespaces",
                "Network namespaces created.",
                h.namespace_count as u64,
            ),
            (
                "nlink_lab_containers",
                "Container nodes.",
                h.container_count as u64,
            ),
            (
                "nlink_lab_processes",
                "Tracked background processes.",
                h.pid_count as u64,
            ),
            (
                "nlink_lab_backend_uptime_seconds",
                "Backend uptime.",
                h.uptime_secs,
            ),
        ] {
            out += &format!(
                "# TYPE {name} gauge\n# HELP {name} {help}\n{name}{{lab=\"{lab}\"}} {value}\n"
            );
        }
    }
    if let Some(s) = &state.snapshot {
        let lab = esc(&s.lab_name);
        let gauges: [(&str, &str, Getter); 10] = [
            (
                "nlink_lab_iface_rx_bytes_per_second",
                "Receive throughput.",
                |m| m.rx_bps,
            ),
            (
                "nlink_lab_iface_tx_bytes_per_second",
                "Transmit throughput.",
                |m| m.tx_bps,
            ),
            (
                "nlink_lab_iface_rx_packets_per_second",
                "Receive packet rate.",
                |m| m.rx_pps,
            ),
            (
                "nlink_lab_iface_tx_packets_per_second",
                "Transmit packet rate.",
                |m| m.tx_pps,
            ),
            ("nlink_lab_iface_rx_errors_total", "Receive errors.", |m| {
                m.rx_errors
            }),
            ("nlink_lab_iface_tx_errors_total", "Transmit errors.", |m| {
                m.tx_errors
            }),
            ("nlink_lab_iface_rx_dropped_total", "Receive drops.", |m| {
                m.rx_dropped
            }),
            ("nlink_lab_iface_tx_dropped_total", "Transmit drops.", |m| {
                m.tx_dropped
            }),
            ("nlink_lab_iface_tc_drops_total", "Qdisc drops.", |m| {
                m.tc_drops
            }),
            ("nlink_lab_iface_tc_qlen", "Qdisc queue length.", |m| {
                u64::from(m.tc_qlen)
            }),
        ];
        let mut nodes: Vec<(&String, &nlink_lab_shared::metrics::NodeMetrics)> =
            s.nodes.iter().collect();
        nodes.sort_by(|a, b| a.0.cmp(b.0));
        for (name, help, get) in gauges {
            let kind = if name.ends_with("_total") {
                "counter"
            } else {
                "gauge"
            };
            let metric = if kind == "counter" {
                name.trim_end_matches("_total")
            } else {
                name
            };
            out += &format!("# TYPE {metric} {kind}\n# HELP {metric} {help}\n");
            for (node, nm) in &nodes {
                let mut ifaces: Vec<&nlink_lab_shared::metrics::InterfaceMetrics> =
                    nm.interfaces.iter().collect();
                ifaces.sort_by(|a, b| a.name.cmp(&b.name));
                for m in ifaces {
                    out += &format!(
                        "{name}{{lab=\"{lab}\",node=\"{}\",iface=\"{}\",state=\"{}\"}} {}\n",
                        esc(node),
                        esc(&m.name),
                        esc(&m.state),
                        get(m)
                    );
                }
            }
        }
        out += "# TYPE nlink_lab_socket_tx_bytes_per_second gauge\n# HELP nlink_lab_socket_tx_bytes_per_second Per-flow transmit rate (top flows).\n";
        for (node, nm) in &nodes {
            for f in &nm.sockets {
                out += &format!(
                    "nlink_lab_socket_tx_bytes_per_second{{lab=\"{lab}\",node=\"{}\",comm=\"{}\",local=\"{}\",remote=\"{}\"}} {}\n",
                    esc(node),
                    esc(&f.comm),
                    esc(&f.local),
                    esc(&f.remote),
                    f.tx_bytes_per_sec
                );
            }
        }
        out += "# TYPE nlink_lab_socket_rx_bytes_per_second gauge\n# HELP nlink_lab_socket_rx_bytes_per_second Per-flow receive rate (top flows).\n";
        for (node, nm) in &nodes {
            for f in &nm.sockets {
                out += &format!(
                    "nlink_lab_socket_rx_bytes_per_second{{lab=\"{lab}\",node=\"{}\",comm=\"{}\",local=\"{}\",remote=\"{}\"}} {}\n",
                    esc(node),
                    esc(&f.comm),
                    esc(&f.local),
                    esc(&f.remote),
                    f.rx_bytes_per_sec
                );
            }
        }
        out += "# TYPE nlink_lab_node_issues gauge\n# HELP nlink_lab_node_issues Diagnostic issues reported for the node.\n";
        for (node, nm) in &nodes {
            out += &format!(
                "nlink_lab_node_issues{{lab=\"{lab}\",node=\"{}\"}} {}\n",
                esc(node),
                nm.issues.len()
            );
        }
        out += &format!(
            "# TYPE nlink_lab_snapshot_timestamp_seconds gauge\n# HELP nlink_lab_snapshot_timestamp_seconds Unix time of the last sample.\nnlink_lab_snapshot_timestamp_seconds{{lab=\"{lab}\"}} {}\n",
            s.timestamp
        );
    }
    out += "# EOF\n";
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab_shared::metrics::{InterfaceMetrics, NodeMetrics};

    fn sample() -> State {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            "r1".to_string(),
            NodeMetrics {
                interfaces: vec![InterfaceMetrics {
                    name: "eth0".into(),
                    state: "up".into(),
                    rx_bps: 10,
                    tx_bps: 20,
                    rx_pps: 1,
                    tx_pps: 2,
                    rx_errors: 0,
                    tx_errors: 0,
                    rx_dropped: 3,
                    tx_dropped: 0,
                    tc_drops: 4,
                    tc_qlen: 5,
                }],
                issues: vec!["mtu mismatch".into()],
                sockets: vec![],
            },
        );
        State {
            snapshot: Some(MetricsSnapshot {
                wire_version: 1,
                lab_name: "l\"ab".into(),
                timestamp: 1700000000,
                nodes,
            }),
            health: Some(HealthStatus {
                wire_version: 1,
                lab_name: "l\"ab".into(),
                timestamp: 1700000000,
                node_count: 1,
                namespace_count: 1,
                container_count: 0,
                pid_count: 2,
                uptime_secs: 9,
            }),
            topology: serde_json::json!({}),
        }
    }

    #[test]
    fn openmetrics_is_well_formed_and_escaped() {
        let text = openmetrics(&sample());
        assert!(text.ends_with("# EOF\n"));
        assert!(
            text.contains("nlink_lab_nodes{lab=\"l\\\"ab\"} 1\n"),
            "{text}"
        );
        assert!(
            text.contains("# TYPE nlink_lab_iface_rx_dropped counter\n"),
            "{text}"
        );
        assert!(text.contains("nlink_lab_iface_rx_dropped_total{lab=\"l\\\"ab\",node=\"r1\",iface=\"eth0\",state=\"up\"} 3\n"), "{text}");
        assert!(text.contains("nlink_lab_iface_tc_qlen{lab=\"l\\\"ab\",node=\"r1\",iface=\"eth0\",state=\"up\"} 5\n"), "{text}");
        assert!(
            text.contains("nlink_lab_node_issues{lab=\"l\\\"ab\",node=\"r1\"} 1\n"),
            "{text}"
        );
        assert_eq!(text, openmetrics(&sample()), "deterministic");
    }

    #[tokio::test]
    async fn http_routes_answer() {
        let state: Shared = Arc::new(RwLock::new(sample()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let task = spawn(addr, state).await.unwrap();
        async fn get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            let text = String::from_utf8(buf).unwrap();
            let (head, body) = text.split_once("\r\n\r\n").unwrap();
            (head.lines().next().unwrap().to_string(), body.to_string())
        }
        let (status, body) = get(addr, "/metrics").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(body.ends_with("# EOF\n"));
        let (status, body) = get(addr, "/api/v1/health").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["node_count"], 1);
        let (status, _) = get(addr, "/api/v1/snapshot?x=1").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let (status, _) = get(addr, "/nope").await;
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        task.abort();
    }
}
