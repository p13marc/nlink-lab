pub(crate) async fn run(
    lab: String,
    node: Option<String>,
    fmt: String,
    count: Option<usize>,
    zenoh_connect: Option<String>,
) -> nlink_lab::Result<()> {
    let mut zenoh_config = zenoh::Config::default();
    if let Some(connect) = &zenoh_connect {
        zenoh_config
            .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
            .map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}"))
            })?;
    }

    let session = zenoh::open(zenoh_config).await.map_err(|e| {
        nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
    })?;

    let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
    let subscriber = session.declare_subscriber(&topic).await.map_err(|e| {
        nlink_lab::Error::deploy_failed(format!("subscribe to '{topic}': {e}"))
    })?;

    eprintln!("Subscribing to metrics for lab '{lab}'... (Ctrl-C to stop)");

    let mut samples = 0usize;
    loop {
        tokio::select! {
            Ok(sample) = subscriber.recv_async() => {
                let payload = sample.payload().to_bytes();
                if let Ok(snapshot) = serde_json::from_slice::<nlink_lab_shared::metrics::MetricsSnapshot>(&payload) {
                    samples += 1;

                    if fmt == "json" {
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
