//! nlink-lab-backend: Zenoh backend daemon for live lab monitoring.
//!
//! Runs as root (or CAP_NET_ADMIN), collects metrics from deployed labs,
//! and exposes them via Zenoh pub/sub and query/reply.

use std::time::{Duration, Instant};

use clap::Parser;
use tracing::{info, warn};

mod collector;
mod handlers;

#[derive(Parser)]
#[command(name = "nlink-lab-backend", about = "Zenoh backend daemon for nlink-lab")]
struct Cli {
    /// Lab name (must be deployed).
    lab: String,

    /// Metrics collection interval in seconds.
    #[arg(short, long, default_value = "2")]
    interval: u64,

    /// Zenoh mode: peer or client.
    #[arg(long, default_value = "peer")]
    zenoh_mode: String,

    /// Zenoh listen endpoint.
    #[arg(long)]
    zenoh_listen: Option<String>,

    /// Zenoh connect endpoint.
    #[arg(long)]
    zenoh_connect: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Load the running lab
    let lab = nlink_lab::RunningLab::load(&cli.lab)?;
    info!(
        lab = cli.lab,
        nodes = lab.namespace_count(),
        "loaded lab"
    );

    // Build Zenoh config
    let mut zenoh_config = zenoh::Config::default();
    match cli.zenoh_mode.as_str() {
        "client" => {
            zenoh_config
                .insert_json5("mode", r#""client""#)
                .map_err(|e| anyhow::anyhow!("bad zenoh config: {e}"))?;
        }
        _ => {} // peer is default
    }
    if let Some(listen) = &cli.zenoh_listen {
        zenoh_config
            .insert_json5("listen/endpoints", &format!(r#"["{listen}"]"#))
            .map_err(|e| anyhow::anyhow!("bad zenoh listen config: {e}"))?;
    }
    if let Some(connect) = &cli.zenoh_connect {
        zenoh_config
            .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
            .map_err(|e| anyhow::anyhow!("bad zenoh connect config: {e}"))?;
    }

    let session = zenoh::open(zenoh_config).await.map_err(|e| {
        anyhow::anyhow!("failed to open Zenoh session: {e}")
    })?;
    info!("Zenoh session opened");

    run(&session, lab, Duration::from_secs(cli.interval)).await
}

async fn run(
    session: &zenoh::Session,
    lab: nlink_lab::RunningLab,
    interval: Duration,
) -> anyhow::Result<()> {
    use nlink_lab_shared::{messages::*, topics};

    let lab_name = lab.name().to_string();
    let start_time = Instant::now();

    // ── Publishers ──────────────────────────────────────────
    let topo_publisher = session
        .declare_publisher(topics::topology(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("topology publisher: {e}"))?;

    let health_publisher = session
        .declare_publisher(topics::health(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("health publisher: {e}"))?;

    let snapshot_publisher = session
        .declare_publisher(topics::metrics_snapshot(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("snapshot publisher: {e}"))?;

    // ── Queryables ─────────────────────────────────────────
    let exec_queryable = session
        .declare_queryable(topics::rpc_exec(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("exec queryable: {e}"))?;

    let impair_queryable = session
        .declare_queryable(topics::rpc_impairment(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("impairment queryable: {e}"))?;

    let status_queryable = session
        .declare_queryable(topics::rpc_status(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("status queryable: {e}"))?;

    // ── Publish initial topology ───────────────────────────
    let topo = lab.topology();
    let topo_json = serde_json::to_string(topo)?;
    let topo_update = TopologyUpdate {
        lab_name: lab_name.clone(),
        timestamp: now_unix(),
        node_count: topo.nodes.len(),
        link_count: topo.links.len(),
        topology_json: topo_json,
    };
    topo_publisher
        .put(serde_json::to_vec(&topo_update)?)
        .await
        .map_err(|e| anyhow::anyhow!("publish topology: {e}"))?;
    info!("published initial topology");

    // ── Liveliness token ───────────────────────────────────
    let _token = session
        .liveliness()
        .declare_token(topics::health(&lab_name))
        .await
        .map_err(|e| anyhow::anyhow!("liveliness token: {e}"))?;

    // ── Main event loop ────────────────────────────────────
    let mut collector = collector::MetricsCollector::new(&lab);
    let mut health_interval = tokio::time::interval(Duration::from_secs(10));
    let mut metrics_interval = tokio::time::interval(interval);

    info!(lab = lab_name, "backend daemon running");

    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                match collector.snapshot(&lab).await {
                    Ok(snapshot) => {
                        if let Ok(json) = serde_json::to_vec(&snapshot) {
                            if let Err(e) = snapshot_publisher.put(json).await {
                                warn!("publish metrics: {e}");
                            }
                        }
                    }
                    Err(e) => warn!("metrics collection: {e}"),
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
                    if let Err(e) = health_publisher.put(json).await {
                        warn!("publish health: {e}");
                    }
                }
            }

            Ok(query) = exec_queryable.recv_async() => {
                handlers::handle_exec(&lab, query).await;
            }

            Ok(query) = impair_queryable.recv_async() => {
                handlers::handle_impairment(&lab, query).await;
            }

            Ok(query) = status_queryable.recv_async() => {
                let status = StatusResponse {
                    lab_name: lab_name.clone(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: 0,
                    uptime_secs: start_time.elapsed().as_secs(),
                    nodes: lab.node_names().map(|s| s.to_string()).collect(),
                };
                if let Ok(json) = serde_json::to_string(&status) {
                    if let Err(e) = query.reply(topics::rpc_status(&lab_name), json).await {
                        warn!("reply status: {e}");
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                break;
            }
        }
    }

    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
