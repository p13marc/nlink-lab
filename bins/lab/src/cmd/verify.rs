//! `nlink-lab verify` — drift check with an exit code (issue #58).

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology, require_root};
use crate::output::{DryRunReport, EXIT_VALIDATION, print_layered, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Compare the live lab against this topology file instead of the
    /// one it was deployed from (what `apply` would converge to).
    #[arg(long, value_name = "FILE")]
    pub topology: Option<PathBuf>,

    /// Set a `param` value for --topology (repeatable): --set key=value.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
}

/// Exit 0 when the kernel state of every node matches the topology, 2
/// on drift (the report is printed either way), 1 on error. Unlike
/// `apply --check` the drift itself is not an error envelope: the JSON
/// report on stdout is the whole answer.
pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        topology,
        params,
    } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let desired = match topology {
        Some(path) => parse_topology(&path, &params)?,
        None => running.topology().clone(),
    };
    if desired.lab.name != lab {
        return Err(nlink_lab::Error::invalid_topology(format!(
            "topology is for lab {:?}, not {lab:?}",
            desired.lab.name
        )));
    }
    let layered = nlink_lab::compute_layered_diff(&running, &desired).await?;
    let removals: Vec<String> = nlink_lab::apply_plan(&running, &desired)?
        .ops
        .iter()
        .filter(|o| o.is_removal())
        .map(|o| o.describe())
        .collect();
    let report = DryRunReport::new(&lab, &layered, &removals);
    let drift = !report.no_op;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if drift {
        println!("Drift detected for lab {lab:?}:");
        print_layered(&layered, &removals);
        println!("{} change(s) needed to converge", report.change_count);
    } else if !ctx.quiet {
        println!("Lab {lab:?} matches its topology (no drift).");
    }
    if drift {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
