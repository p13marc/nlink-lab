//! `nlink-lab metrics`.

use crate::ctx::Ctx;

/// `metrics --format` values.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricsFormat {
    Table,
    Json,
}

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,

    /// Filter to specific node.
    #[arg(short, long)]
    pub node: Option<String>,

    /// Output format (`--json` selects json too).
    #[arg(short, long, value_enum, default_value_t = MetricsFormat::Table)]
    pub format: MetricsFormat,

    /// Number of samples then exit.
    #[arg(short, long)]
    pub count: Option<usize>,

    /// Zenoh connect endpoint.
    #[arg(long)]
    pub zenoh_connect: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        format: fmt,
        count,
        zenoh_connect,
    } = args;
    let mut zenoh_config = zenoh::Config::default();
    if let Some(connect) = &zenoh_connect {
        zenoh_config
            .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}")))?;
    }

    let session = zenoh::open(zenoh_config).await.map_err(|e| {
        nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
    })?;

    let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
    let subscriber = session
        .declare_subscriber(&topic)
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("subscribe to '{topic}': {e}")))?;

    eprintln!("Subscribing to metrics for lab '{lab}'... (Ctrl-C to stop)");

    let mut samples = 0usize;
    loop {
        tokio::select! {
            Ok(sample) = subscriber.recv_async() => {
                let payload = sample.payload().to_bytes();
                if let Ok(snapshot) = serde_json::from_slice::<nlink_lab_shared::metrics::MetricsSnapshot>(&payload) {
                    samples += 1;

                    if fmt == MetricsFormat::Json || ctx.json {
                        println!("{}", serde_json::to_string(&snapshot).unwrap_or_default());
                    } else {
                        // Clear screen for table mode
                        print!("\x1B[2J\x1B[H");
                        println!(
                            "lab: {}  |  nodes: {}  |  sample: #{}",
                            snapshot.lab_name,
                            snapshot.nodes.len(),
                            samples,
                        );
                        println!();
                        println!(
                            "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>8}",
                            "NODE", "IFACE", "STATE", "RX rate", "TX rate", "ERRORS", "DROPS"
                        );
                        println!("{}", "─".repeat(78));

                        let mut node_names: Vec<&String> = snapshot.nodes.keys().collect();
                        node_names.sort();
                        for node_name in node_names {
                            if let Some(filter) = &node
                                && node_name != filter { continue; }
                            let metrics = &snapshot.nodes[node_name];
                            for iface in &metrics.interfaces {
                                let errors = iface.rx_errors + iface.tx_errors;
                                let drops = iface.rx_dropped + iface.tx_dropped + iface.tc_drops;
                                let drop_warn = if drops > 0 { " !" } else { "" };
                                println!(
                                    "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>7}{}",
                                    node_name,
                                    iface.name,
                                    iface.state,
                                    nlink_lab_shared::metrics::format_rate(iface.rx_bps),
                                    nlink_lab_shared::metrics::format_rate(iface.tx_bps),
                                    errors,
                                    drops,
                                    drop_warn,
                                );
                            }
                            for issue in &metrics.issues {
                                println!("  [WARN] {node_name}: {issue}");
                            }
                            // Top TCP flows by goodput (nlink 0.24 sockdiag),
                            // published by the backend collector.
                            for s in &metrics.sockets {
                                println!(
                                    "  {:<20} {} -> {}  tx {}  rx {}{}",
                                    format!("{} (pid {})", s.comm, s.pid.map_or_else(|| "-".to_string(), |p| p.to_string())),
                                    s.local,
                                    s.remote,
                                    nlink_lab_shared::metrics::format_rate(s.tx_bytes_per_sec * 8),
                                    nlink_lab_shared::metrics::format_rate(s.rx_bytes_per_sec * 8),
                                    if s.retrans_ratio > 0.0 {
                                        format!("  retr {:.1}%", s.retrans_ratio * 100.0)
                                    } else {
                                        String::new()
                                    },
                                );
                            }
                        }
                    }

                    if let Some(max) = count
                        && samples >= max {
                            break;
                        }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }
    Ok(())
}
