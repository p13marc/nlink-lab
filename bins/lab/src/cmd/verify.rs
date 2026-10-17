//! `nlink-lab verify` — drift check with an exit code (issue #58).

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology, require_root};
use crate::output::{DryRunReport, EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Compare the live lab against this topology file (`.nll` or
    /// `.nlz`) instead of the topology it runs: a CI gate on what `apply
    /// FILE` would change.
    #[arg(long = "file", value_name = "FILE")]
    pub topology: Option<PathBuf>,

    /// Set a `param` value for --file (repeatable): --set key=value.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,
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
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let mut desired = match topology {
        Some(path) if crate::cmd::common::is_archive(&path) => {
            let scratch =
                std::env::temp_dir().join(format!("nlink-lab-verify-{}", std::process::id()));
            let topo = crate::cmd::common::archive_topology(&path, &scratch, false);
            let _ = std::fs::remove_dir_all(&scratch);
            topo?
        }
        Some(path) => parse_topology(&path, &params)?,
        None => running.topology().clone(),
    };
    // a lab deployed with `--suffix`/`--unique`: its file's name plus that
    if desired.lab.name != lab
        && let Some(sfx) = nlink_lab::state::source_of(&lab).and_then(|s| s.suffix)
        && format!("{}-{sfx}", desired.lab.name) == lab
    {
        desired.lab.add_suffix(&sfx);
    }
    if desired.lab.name != lab {
        return Err(nlink_lab::Error::invalid_argument(format!(
            "topology is for lab {:?}, not {lab:?}",
            desired.lab.name
        )));
    }
    let layered = nlink_lab::compute_layered_diff(&running, &desired).await?;
    let plan = nlink_lab::apply_plan(&running, &desired)?;
    // and the plan's other work, as `apply` would do it: a change
    // that only re-creates a container verified clean (#209)
    let (removals, plan_ops) = crate::cmd::common::plan_lists(&plan);
    let report = DryRunReport::new(&lab, &layered, &removals).with_ops(&plan_ops);
    let drift = !report.no_op;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if drift {
        println!("Drift detected for lab {lab:?}:");
        crate::cmd::common::print_drift(&layered, &removals, &plan_ops);
        println!("{} change(s) needed to converge", report.change_count);
    } else if !ctx.quiet {
        println!("Lab {lab:?} matches its topology (no drift).");
    }
    if drift {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
