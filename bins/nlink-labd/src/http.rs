//! Read-only HTTP (#270), off unless `[http] listen` is set: what
//! Prometheus, Grafana and `curl` read without a client of the API.
//!
//! - `GET /metrics` — OpenMetrics for every lab (`lab`, `node`, `iface`
//!   labels);
//! - `GET /api/v2/labs` — the labs;
//! - `GET /api/v2/labs/<lab>` — one lab, as `status --json` prints it;
//! - `GET /api/v2/labs/<lab>/topology` — its redacted topology;
//! - `GET /api/v2/labs/<lab>/metrics` — its latest metrics snapshot;
//! - `GET /api/v2/labs/<lab>/events` — its latest lifecycle events;
//! - `GET /api/v2/labs/<lab>/events/stream` — its lifecycle and runtime
//!   events as they come (server-sent events).
//!
//! It authenticates no one: everything it serves is what a viewer reads,
//! and it listens where it is told — loopback, unless a scraper elsewhere
//! needs it.

use std::sync::Arc;

use nlink_lab::CancelToken;
use nlink_lab_shared::api::{LabSummary, Notification, Request, Response, Topic};
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::hub::Hub;

/// Serve `listener` until `shutdown`.
pub(crate) async fn serve(listener: tokio::net::TcpListener, hub: Arc<Hub>, shutdown: CancelToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let hub = hub.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, hub).await {
                        tracing::debug!("http connection: {e}");
                    }
                });
            }
        }
    }
}

type Answer = (&'static str, &'static str, String);

fn json(value: &impl serde::Serialize) -> Answer {
    (
        "200 OK",
        "application/json",
        serde_json::to_string_pretty(value).unwrap_or_default(),
    )
}

fn not_found(what: &str) -> Answer {
    ("404 Not Found", "text/plain", format!("{what}\n"))
}

/// A read the library answers, off the event loop.
async fn read(request: Request) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        match rt.block_on(nlink_lab::internal::ops::read::handle(&request)) {
            Some(result) => result.map_err(|e| e.to_string()),
            None => Err("not a read".into()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn handle(mut stream: tokio::net::TcpStream, hub: Arc<Hub>) -> std::io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = path.split('?').next().unwrap_or("/");
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if method == "GET"
        && let ["api", "v2", "labs", lab, "events", "stream"] = segments.as_slice()
    {
        return serve_sse(stream, hub, lab.to_string()).await;
    }
    let (status, content_type, body) = if method != "GET" {
        (
            "405 Method Not Allowed",
            "text/plain",
            "GET only\n".to_string(),
        )
    } else {
        match segments.as_slice() {
            ["metrics"] => {
                let labs: Vec<(LabSummary, Option<MetricsSnapshot>)> = hub
                    .labs()
                    .into_iter()
                    .map(|l| {
                        let snapshot = hub.latest_metrics(&l.lab);
                        (l, snapshot)
                    })
                    .collect();
                (
                    "200 OK",
                    "application/openmetrics-text; version=1.0.0; charset=utf-8",
                    openmetrics(&labs),
                )
            }
            ["api", "v2", "labs"] => json(&serde_json::json!({ "labs": hub.labs() })),
            ["api", "v2", "labs", lab] => match read(Request::GetLab { lab: lab.to_string() }).await {
                Ok(Response::Lab { detail, .. }) => json(&detail),
                _ => not_found("no such lab"),
            },
            ["api", "v2", "labs", lab, "topology"] => {
                match read(Request::GetTopology {
                    lab: lab.to_string(),
                    redact: true,
                })
                .await
                {
                    Ok(Response::Topology { topology, .. }) => json(&topology),
                    _ => not_found("no such lab"),
                }
            }
            ["api", "v2", "labs", lab, "metrics"] => match hub.latest_metrics(lab) {
                Some(snapshot) => json(&snapshot),
                None => not_found("no sample yet"),
            },
            ["api", "v2", "labs", lab, "events"] => {
                match read(Request::ReadEvents {
                    lab: lab.to_string(),
                    since: None,
                    kinds: vec![],
                    limit: Some(500),
                })
                .await
                {
                    Ok(Response::Events { events, .. }) => json(&events),
                    _ => not_found("no such lab"),
                }
            }
            [""] => (
                "200 OK",
                "text/plain",
                "nlink-labd: /metrics, /api/v2/labs, /api/v2/labs/<lab>[/topology|/metrics|/events|/events/stream]\n"
                    .to_string(),
            ),
            _ => not_found("not found"),
        }
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// `text/event-stream`: every lifecycle and runtime event of `lab`, each
/// tagged with its `source`, until the client goes away.
async fn serve_sse(
    mut stream: tokio::net::TcpStream,
    hub: Arc<Hub>,
    lab: String,
) -> std::io::Result<()> {
    let mut notes = hub.subscribe_all();
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
        )
        .await?;
    loop {
        let note = match notes.recv().await {
            Ok(note) => note,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        if note.lab.as_deref() != Some(lab.as_str()) {
            continue;
        }
        let (source, mut event) = match (&note.topic, &note.notification) {
            (Topic::Lifecycle, Notification::Lifecycle { event, .. }) => {
                ("lifecycle", event.clone())
            }
            (Topic::Runtime, Notification::Runtime { event, .. }) => ("runtime", event.clone()),
            _ => continue,
        };
        if let Some(obj) = event.as_object_mut() {
            obj.insert("source".into(), source.into());
        }
        stream
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await?;
    }
    stream.shutdown().await
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

type Getter = fn(&InterfaceMetrics) -> u64;

/// The interface families: name, help, value.
const IFACE: [(&str, &str, Getter); 14] = [
    (
        "nlink_lab_iface_rx_bits_per_second",
        "Receive throughput.",
        |m| m.rx_bps,
    ),
    (
        "nlink_lab_iface_tx_bits_per_second",
        "Transmit throughput.",
        |m| m.tx_bps,
    ),
    (
        "nlink_lab_iface_rx_bytes_total",
        "Cumulative bytes received since the interface appeared.",
        |m| m.rx_bytes,
    ),
    (
        "nlink_lab_iface_tx_bytes_total",
        "Cumulative bytes transmitted since the interface appeared.",
        |m| m.tx_bytes,
    ),
    (
        "nlink_lab_iface_rx_packets_total",
        "Cumulative packets received since the interface appeared.",
        |m| m.rx_pkts,
    ),
    (
        "nlink_lab_iface_tx_packets_total",
        "Cumulative packets transmitted since the interface appeared.",
        |m| m.tx_pkts,
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

/// A metric family's header: once, whatever the labs.
fn family(out: &mut String, name: &str, help: &str) {
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
    out.push_str(&format!("# TYPE {metric} {kind}\n# HELP {metric} {help}\n"));
}

/// A snapshot's nodes and their interfaces, sorted: the output is stable.
fn sorted(s: &MetricsSnapshot) -> Vec<(&String, &NodeMetrics, Vec<&InterfaceMetrics>)> {
    let mut nodes: Vec<_> = s
        .nodes
        .iter()
        .map(|(name, nm)| {
            let mut ifaces: Vec<&InterfaceMetrics> = nm.interfaces.iter().collect();
            ifaces.sort_by(|a, b| a.name.cmp(&b.name));
            (name, nm, ifaces)
        })
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(b.0));
    nodes
}

/// OpenMetrics of every lab: each family once, every lab's samples under
/// it. Deterministic (labs, nodes and interfaces sorted).
pub fn openmetrics(labs: &[(LabSummary, Option<MetricsSnapshot>)]) -> String {
    let mut labs: Vec<&(LabSummary, Option<MetricsSnapshot>)> = labs.iter().collect();
    labs.sort_by(|a, b| a.0.lab.cmp(&b.0.lab));
    let mut out = String::new();
    type LabGauge = fn(&LabSummary) -> u64;
    let lab_gauges: [(&str, &str, LabGauge); 5] = [
        ("nlink_lab_nodes", "Nodes in the topology.", |l| {
            l.nodes as u64
        }),
        ("nlink_lab_containers", "Container nodes.", |l| {
            l.containers as u64
        }),
        (
            "nlink_lab_processes",
            "Tracked background processes alive.",
            |l| l.processes as u64,
        ),
        (
            "nlink_lab_impaired_endpoints",
            "Endpoints with an impairment.",
            |l| l.impaired as u64,
        ),
        (
            "nlink_lab_uptime_seconds",
            "Time since the lab was deployed.",
            |l| crate::uptime(&l.created_at),
        ),
    ];
    for (name, help, get) in lab_gauges {
        family(&mut out, name, help);
        for (lab, _) in &labs {
            out.push_str(&format!(
                "{name}{{lab=\"{}\"}} {}\n",
                esc(&lab.lab),
                get(lab)
            ));
        }
    }
    let snapshots: Vec<(&str, &MetricsSnapshot)> = labs
        .iter()
        .filter_map(|(l, s)| s.as_ref().map(|s| (l.lab.as_str(), s)))
        .collect();
    for (name, help, get) in IFACE {
        family(&mut out, name, help);
        for (lab, s) in &snapshots {
            for (node, _, ifaces) in sorted(s) {
                // no `state` label: a link going down changed the series
                // identity of its counters, which broke `rate()` (#218)
                for m in ifaces {
                    out.push_str(&format!(
                        "{name}{{lab=\"{}\",node=\"{}\",iface=\"{}\"}} {}\n",
                        esc(lab),
                        esc(node),
                        esc(&m.name),
                        get(m)
                    ));
                }
            }
        }
    }
    family(
        &mut out,
        "nlink_lab_iface_up",
        "1 when the interface's operational state is up.",
    );
    for (lab, s) in &snapshots {
        for (node, _, ifaces) in sorted(s) {
            for m in ifaces {
                out.push_str(&format!(
                    "nlink_lab_iface_up{{lab=\"{}\",node=\"{}\",iface=\"{}\"}} {}\n",
                    esc(lab),
                    esc(node),
                    esc(&m.name),
                    u8::from(m.state == "up")
                ));
            }
        }
    }
    type FlowRate = fn(&nlink_lab_shared::metrics::SocketRateMetric) -> u64;
    let flows: [(&str, &str, FlowRate); 2] = [
        (
            "nlink_lab_socket_tx_bytes_per_second",
            "Per-flow transmit rate (top flows).",
            |f| f.tx_bytes_per_sec,
        ),
        (
            "nlink_lab_socket_rx_bytes_per_second",
            "Per-flow receive rate (top flows).",
            |f| f.rx_bytes_per_sec,
        ),
    ];
    for (name, help, get) in flows {
        family(&mut out, name, help);
        for (lab, s) in &snapshots {
            for (node, nm, _) in sorted(s) {
                for f in &nm.sockets {
                    out.push_str(&format!(
                        "{name}{{lab=\"{}\",node=\"{}\",comm=\"{}\",local=\"{}\",remote=\"{}\"}} {}\n",
                        esc(lab),
                        esc(node),
                        esc(&f.comm),
                        esc(&f.local),
                        esc(&f.remote),
                        get(f)
                    ));
                }
            }
        }
    }
    family(
        &mut out,
        "nlink_lab_node_issues",
        "Diagnostic issues reported for the node.",
    );
    for (lab, s) in &snapshots {
        for (node, nm, _) in sorted(s) {
            out.push_str(&format!(
                "nlink_lab_node_issues{{lab=\"{}\",node=\"{}\"}} {}\n",
                esc(lab),
                esc(node),
                nm.issues.len()
            ));
        }
    }
    family(
        &mut out,
        "nlink_lab_snapshot_timestamp_seconds",
        "Unix time of the last sample.",
    );
    for (lab, s) in &snapshots {
        out.push_str(&format!(
            "nlink_lab_snapshot_timestamp_seconds{{lab=\"{}\"}} {}\n",
            esc(lab),
            s.timestamp
        ));
    }
    out.push_str("# EOF\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(lab: &str) -> MetricsSnapshot {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            "r1".to_string(),
            NodeMetrics {
                interfaces: vec![InterfaceMetrics {
                    name: "eth0".into(),
                    state: "up".into(),
                    rx_bps: 10,
                    tx_bps: 20,
                    rx_bytes: 6_000,
                    tc_qlen: 5,
                    ..Default::default()
                }],
                issues: vec!["mtu mismatch".into()],
                sockets: vec![],
            },
        );
        MetricsSnapshot {
            wire_version: 1,
            lab_name: lab.into(),
            timestamp: 1_700_000_000,
            nodes,
        }
    }

    #[test]
    fn every_family_once_with_every_labs_samples() {
        let lab = |name: &str| LabSummary {
            lab: name.into(),
            nodes: 2,
            ..Default::default()
        };
        let text = openmetrics(&[
            (lab("b\"q"), Some(snapshot("b\"q"))),
            (lab("a"), Some(snapshot("a"))),
            (lab("c"), None),
        ]);
        assert_eq!(
            text.matches("# TYPE nlink_lab_iface_rx_bits_per_second ")
                .count(),
            1
        );
        assert_eq!(
            text.matches("# TYPE nlink_lab_iface_rx_bytes counter")
                .count(),
            1
        );
        assert!(
            text.contains(
                "nlink_lab_iface_rx_bytes_total{lab=\"a\",node=\"r1\",iface=\"eth0\"} 6000"
            )
        );
        assert!(text.contains("nlink_lab_nodes{lab=\"c\"} 2"));
        // escaped, and sorted: lab a before lab b"q
        assert!(text.contains("lab=\"b\\\"q\""));
        let a = text.find("rx_bits_per_second{lab=\"a\"").unwrap();
        let b = text.find("rx_bits_per_second{lab=\"b").unwrap();
        assert!(a < b);
        assert!(text.ends_with("# EOF\n"));
    }
}
