//! `nlink-lab restore`.

use crate::ctx::{Ctx, require_root, yellow};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Snapshot name (see `nlink-lab snapshot <lab> --list`).
    pub name: String,

    /// Show the plan without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, name, dry_run } = args;
    let snapshot = nlink_lab::state::snapshot_load(&lab, &name)?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;

    if dry_run {
        let plan = nlink_lab::apply_plan(&running, &snapshot.topology)?;
        let ops: Vec<String> = plan.ops.iter().map(|o| o.describe()).collect();
        let live: Vec<&String> = snapshot.state.live_impairments.keys().collect();
        let parts: Vec<&String> = snapshot.state.saved_impairments.keys().collect();
        if ctx.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "lab": lab,
                    "snapshot": snapshot.meta,
                    "ops": ops,
                    "restore_impairments": live,
                    "restore_partitions": parts,
                    "dry_run": true,
                }))?
            );
        } else {
            println!("Restore {lab:?} to snapshot {name:?} (dry run):");
            if ops.is_empty() {
                println!("  topology: no changes");
            }
            for op in &ops {
                println!("  {op}");
            }
            for ep in &live {
                println!("  runtime impairment on {ep}");
            }
            for ep in &parts {
                println!("  partition {ep}");
            }
        }
        return Ok(());
    }

    require_root()?;
    let result = snapshot.topology.validate();
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }
    result.bail()?;
    let report = nlink_lab::restore(&mut running, &snapshot).await?;
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "lab": lab,
                "snapshot": snapshot.meta,
                "ops": report.ops,
                "removed": report.removed,
                "applied": report.applied,
                "restored_impairments": snapshot.state.live_impairments.len(),
                "restored_partitions": snapshot.state.saved_impairments.len(),
            }))?
        );
    } else if !ctx.quiet {
        for op in &report.applied {
            println!("  {op}");
        }
        println!(
            "Restored lab {lab:?} to snapshot {name:?}: {} op(s), {} runtime impairment(s), {} partition(s)",
            report.ops,
            snapshot.state.live_impairments.len(),
            snapshot.state.saved_impairments.len()
        );
    }
    Ok(())
}
