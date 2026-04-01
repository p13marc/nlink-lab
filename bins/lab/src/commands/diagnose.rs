use crate::util::check_root;

pub(crate) async fn run(lab: String, node: Option<String>, json: bool) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    let results = running.diagnose(node.as_deref()).await?;
    if json {
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|diag| {
                serde_json::json!({
                    "node": diag.node,
                    "interfaces": diag.interfaces.iter().map(|iface| {
                        serde_json::json!({
                            "name": iface.name,
                            "state": format!("{:?}", iface.state),
                            "mtu": iface.mtu,
                            "rx_bytes": iface.stats.rx_bytes(),
                            "tx_bytes": iface.stats.tx_bytes(),
                            "issues": iface.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                    "issues": diag.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        for diag in &results {
            println!("── {} ──", diag.node);
            for iface in &diag.interfaces {
                let status = if iface.issues.is_empty() {
                    "OK"
                } else {
                    "WARN"
                };
                println!(
                    "  [{status:<4}] {:<12} state={:<6} mtu={:<5} rx={} tx={}",
                    iface.name,
                    format!("{:?}", iface.state),
                    iface.mtu.unwrap_or(0),
                    iface.stats.rx_bytes(),
                    iface.stats.tx_bytes(),
                );
                for issue in &iface.issues {
                    println!("         {issue}");
                }
            }
            for issue in &diag.issues {
                println!("  [WARN] {issue}");
            }
        }
    }
    Ok(())
}
