//! `nlink-lab apply`.

use anstream::{print, println};
use std::path::PathBuf;
use std::time::Instant;

use crate::cmd::common;
use crate::ctx::{Ctx, parse_topology, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// The changed topology file (`.nll` or `.nlz`). Default: the file
    /// the lab was deployed from (or last applied), with its params.
    pub topology: Option<PathBuf>,

    /// The lab to apply it to. Default: the file's lab (with the suffix
    /// it was deployed with), else `$NLINK_LAB`, `nlink-lab use`, the
    /// only lab.
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Set a `param` value (repeatable): --set wan_delay=50ms. Default:
    /// the values the lab was deployed with, when the file is too.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,

    /// Show what would change without applying (`verify --file FILE`
    /// is the CI drift gate).
    #[arg(long)]
    pub dry_run: bool,

    /// Fail (exit 2) when any `validate { … }` assertion fails after
    /// the changes are applied. The lab stays as applied for inspection.
    #[arg(long)]
    pub strict: bool,

    /// Do not run the topology's `validate { … }` assertions after
    /// applying.
    #[arg(long)]
    pub skip_validate: bool,

    /// Drop the impairments set at runtime with `nlink-lab impair` and
    /// converge on the topology's `impair` declarations. Without this,
    /// a runtime impairment survives `apply` unless the topology changes
    /// that endpoint.
    #[arg(long)]
    pub reset_impairments: bool,

    /// Keep the tracked flows of the NAT rules this apply changes. Without
    /// it they are deleted once the apply is done, so a flow that began
    /// before (a UDP stream, an open connection) is translated by the new
    /// rules on its next packet rather than keep its old translation.
    #[arg(long)]
    pub keep_conntrack: bool,
}

/// The topology to converge on, and the source to record for the next
/// `apply`: `file` (or the lab's recorded source), parsed with `params`
/// (or the recorded ones), named like the running lab (its deploy
/// suffix added).
fn desired_topology(
    file: Option<PathBuf>,
    lab: &crate::ctx::LabArg,
    params: Vec<(String, String)>,
) -> nlink_lab::Result<(nlink_lab::Topology, nlink_lab::state::LabSource)> {
    let target = match (&file, &lab.lab) {
        (_, Some(lab)) => Some(lab.clone()),
        (None, None) => Some(lab.resolve()?),
        (Some(_), None) => None,
    };
    let recorded = target
        .as_deref()
        .and_then(nlink_lab::state::source_of)
        .unwrap_or_default();
    // a file named here is its own; the recorded one may be a daemon's
    // staged copy, its host files the client's (#285)
    let dir = if file.is_none() {
        recorded.dir.clone()
    } else {
        None
    };
    let (path, params) = match file {
        Some(path) => (path, params),
        None => {
            let lab = target.as_deref().unwrap_or_default();
            let path = recorded.path.clone().ok_or_else(|| {
                nlink_lab::Error::invalid_argument(format!(
                    "lab '{lab}' has no recorded topology file: `nlink-lab apply FILE`"
                ))
            })?;
            let params = if params.is_empty() {
                recorded.params.clone()
            } else {
                params
            };
            (PathBuf::from(path), params)
        }
    };
    let mut desired = if common::is_archive(&path) {
        let name = target.clone().unwrap_or_default();
        common::archive_topology(
            &path,
            &nlink_lab::state::state_dir(&name).join("source"),
            false,
        )?
    } else {
        parse_topology(&path, &params)?
    };
    if let Some(dir) = &dir {
        desired.source_dir = Some(dir.clone());
    }
    // a lab deployed with `--suffix`/`--unique` is named after its file
    // plus that suffix
    if let Some(target) = &target
        && desired.lab.name != *target
        && let Some(sfx) = &recorded.suffix
        && format!("{}-{sfx}", desired.lab.name) == *target
    {
        desired.lab.add_suffix(sfx);
    }
    if let Some(target) = &target
        && desired.lab.name != *target
    {
        return Err(nlink_lab::Error::invalid_argument(format!(
            "{} is the topology of lab '{}', not '{target}'",
            path.display(),
            desired.lab.name
        )));
    }
    let source = nlink_lab::state::LabSource {
        path: std::path::absolute(&path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        params,
        suffix: recorded.suffix,
        dir,
    };
    Ok((desired, source))
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        lab,
        params,
        dry_run,
        strict,
        skip_validate,
        reset_impairments,
        keep_conntrack,
    } = args;
    let apply_opts = nlink_lab::ApplyOptions {
        reset_impairments,
        keep_conntrack,
    };

    let (mut desired, source) = desired_topology(topology, &lab, params)?;
    if skip_validate {
        desired.assertions.clear();
    }
    common::check_topology(&desired)?;

    // Load current topology from running lab state
    let lab_name = &desired.lab.name.clone();
    if !nlink_lab::state::exists(lab_name) {
        return Err(nlink_lab::Error::NotFound {
            name: format!("{lab_name} (deploy first, then apply changes)"),
        });
    }
    let mut running = nlink_lab::RunningLab::load(lab_name)?;
    // `subnet auto` networks get the subnets the deploy allocated
    nlink_lab::subnet_pool::resolve_auto_subnets(running.topology(), &mut desired)?;
    let current = running.topology();

    let diff = nlink_lab::diff_topologies(current, &desired);

    // Plan 158f Phase 2 — when --dry-run is set,
    // compute the layered diff (topology + per-namespace
    // NetworkConfig + NftablesConfig) so the user sees the
    // full set of kernel changes apply would commit, not
    // just the lab-graph subset.
    //
    // `compute_layered_diff` walks every node and runs a
    // dump round-trip per (node, protocol family). For a
    // 50-node lab that's ~100 dumps; in practice ms-scale
    // on a quiet host. The cost only applies on
    // --dry-run path; normal apply doesn't pay it.
    let layered_view = if dry_run {
        Some(nlink_lab::compute_layered_diff_with(&running, &desired, &apply_opts).await?)
    } else {
        None
    };
    // Removal ops (deleted nodes/links/routes/…) come from the plan
    // diff: the layered diff only sees what the declarative layers
    // still declare, never what disappeared (#83).
    // …and so does the plan's other work: a container re-created because
    // its options changed is in neither the lab graph nor the layers.
    let (removals, plan_ops): (Vec<String>, Vec<String>) = if dry_run {
        common::plan_lists(&nlink_lab::apply_plan_with(
            &running,
            &desired,
            &apply_opts,
        )?)
    } else {
        (Vec::new(), Vec::new())
    };

    // JSON dry-run output for CI consumption.
    if ctx.json && dry_run {
        let layered = layered_view
            .as_ref()
            .expect("layered_view is Some when dry_run");
        let report =
            crate::output::DryRunReport::new(lab_name, layered, &removals).with_ops(&plan_ops);
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Non-JSON path. For --dry-run, render the
    // layered diff (richer than the lab-graph-only
    // TopologyDiff). For ordinary apply, the existing
    // TopologyDiff render is what gets printed.
    let layered_is_empty = layered_view.as_ref().map(|l| l.is_empty()).unwrap_or(true)
        && removals.is_empty()
        && plan_ops.is_empty();

    if (dry_run) && layered_is_empty {
        if !ctx.quiet {
            println!("No changes to apply.");
            if let Some(layered) = layered_view.as_ref().filter(|l| l.has_blocked()) {
                println!("nftables keeps what nlink cannot change:");
                crate::output::print_layered(layered, &[]);
            }
        }
        return Ok(());
    }
    // An unchanged topology can still leave the kernel to fix: a hand edit
    // `verify` reports, or what a newer nlink-lab plans differently for the
    // same file (a forward chain that now accepts). Look before saying so.
    let mut drift_view = None;
    if !dry_run && diff.is_empty() {
        require_root()?;
        let layered = nlink_lab::compute_layered_diff_with(&running, &desired, &apply_opts).await?;
        // A change only the plan sees (container options) is work too: it
        // used to be reported as "No changes to apply" and never applied.
        let plan = nlink_lab::apply_plan_with(&running, &desired, &apply_opts)?;
        let plan_has_work = plan.work().next().is_some() || plan.ops.iter().any(|o| o.is_removal());
        if layered.is_empty() && !plan_has_work {
            if ctx.json {
                // One JSON document on stdout, as after an apply (#177).
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "lab": lab_name,
                        "ops": 0,
                        "removed": 0,
                        "applied": [],
                        "duration_ms": 0,
                    })
                );
            } else if !ctx.quiet {
                println!("No changes to apply.");
            }
            return Ok(());
        }
        drift_view = Some(layered);
    }

    // Under `--json`, stdout carries the one JSON document printed after
    // the apply; the change summary is for humans (#177).
    if !ctx.quiet && !ctx.json {
        println!("Changes for lab '{lab_name}':");
        if let Some(layered) = &drift_view {
            println!("  (topology unchanged; the kernel has drifted from it)");
            common::print_drift(layered, &removals, &plan_ops);
            println!("{} change(s)", layered.change_count());
        } else if let Some(layered) = &layered_view {
            let n = common::print_drift(layered, &removals, &plan_ops);
            println!("{n} change(s)");
        } else {
            print!("{diff}");
            println!("{} change(s)", diff.change_count());
        }
    }

    if dry_run {
        if !ctx.quiet {
            println!("\n(dry run — no changes applied)");
        }
        return Ok(());
    }

    require_root()?;
    let start = Instant::now();
    let progress = crate::progress::Progress::new(ctx);
    let op = progress.op("apply", lab_name);
    let applied = nlink_lab::apply_with_ctx(&mut running, &desired, &apply_opts, &op).await;
    progress.finish();
    let report = applied?;
    tracing::info!("apply: {} op(s), {} removal(s)", report.ops, report.removed);
    // what the next `apply` without a file converges on
    if let Err(e) = nlink_lab::state::set_source(lab_name, &source) {
        tracing::warn!("could not record the source of '{lab_name}': {e}");
    }
    let elapsed = start.elapsed();
    let assertions_failed = running.assertions_failed();

    if ctx.json {
        let mut out = serde_json::json!({
            "schema_version": 1,
            "lab": lab_name,
            "ops": report.ops,
            "removed": report.removed,
            "applied": report.applied,
            "conntrack_deleted": report.conntrack_deleted,
            "duration_ms": elapsed.as_millis() as u64,
        });
        if !running.assertion_results().is_empty() {
            out["assertions"] = serde_json::to_value(running.assertion_results())?;
            out["assertions_failed"] = serde_json::Value::Bool(assertions_failed);
        }
        println!("{out}");
    } else if !ctx.quiet {
        // A change only the plan saw (a container re-created) counts as
        // its ops, not as the empty diff's zero.
        let changes = match drift_view
            .as_ref()
            .map_or(diff.change_count(), |d| d.change_count())
        {
            0 => report.applied.len() - report.removed,
            n => n,
        };
        println!("\nApplied {changes} change(s) in {elapsed:.0?}");
        if report.conntrack_deleted > 0 {
            println!(
                "Deleted {} tracked flow(s) of the changed NAT rules: they take the new rules",
                report.conntrack_deleted
            );
        }
        common::print_assertions(running.assertion_results());
    }

    common::assertions_outcome(running.assertion_results(), strict, lab_name, "as applied")
}
