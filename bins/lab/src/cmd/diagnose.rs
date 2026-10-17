//! `nlink-lab diagnose`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Node name (omit to diagnose all).
    #[arg(add = crate::ctx::node_completer())]
    pub node: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let results = running.diagnose(node.as_deref()).await?;
    // nlink lists an interface's issues under it and again in the
    // report's own list: the node level shows the others only, and each
    // issue its severity (#226).
    fn node_issues(diag: &nlink_lab::NodeDiagnostic) -> Vec<&nlink::netlink::diagnostics::Issue> {
        diag.issues
            .iter()
            .filter(|i| i.interface.is_none())
            .collect()
    }
    if ctx.json {
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(nlink_lab::NodeDiagnostic::to_json)
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        for diag in &results {
            println!("── {} ──", diag.node);
            for iface in &diag.interfaces {
                let status = iface
                    .issues
                    .iter()
                    .map(|i| i.severity)
                    .max()
                    .map_or_else(|| "OK".to_string(), |s| s.to_string());
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
            for issue in node_issues(diag) {
                println!("  [{:<4}] {issue}", issue.severity);
            }
        }
    }
    Ok(())
}
