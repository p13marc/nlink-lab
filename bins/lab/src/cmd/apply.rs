//! `nlink-lab apply`.

use anstream::{eprintln, print, println};
use std::path::PathBuf;
use std::time::Instant;

use crate::ctx::{Ctx, parse_topology, require_root, validation_failed, yellow};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the updated topology file (.nll).
    pub topology: PathBuf,

    /// Set a `param` value (repeatable): --set wan_delay=50ms. Use the
    /// same values the lab was deployed with.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Show what would change without applying.
    #[arg(long)]
    pub dry_run: bool,

    /// Drift check — exit non-zero if the live lab differs from
    /// the NLL. Useful as a CI gate. Implies --dry-run.
    #[arg(long)]
    pub check: bool,

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
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        dry_run,
        check,
        strict,
        skip_validate,
        reset_impairments,
    } = args;
    let apply_opts = nlink_lab::ApplyOptions { reset_impairments };
    // --check implies --dry-run.
    let dry_run = dry_run || check;

    let mut desired = parse_topology(&topology, &params)?;
    if skip_validate {
        desired.assertions.clear();
    }
    let result = desired.validate();
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }
    if result.has_errors() {
        return Err(validation_failed(&desired.lab.name, &result));
    }

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

    // Plan 158f Phase 2 — when --check or --dry-run is set,
    // compute the layered diff (topology + per-namespace
    // NetworkConfig + NftablesConfig) so the user sees the
    // full set of kernel changes apply would commit, not
    // just the lab-graph subset.
    //
    // `compute_layered_diff` walks every node and runs a
    // dump round-trip per (node, protocol family). For a
    // 50-node lab that's ~100 dumps; in practice ms-scale
    // on a quiet host. The cost only applies on
    // --check/--dry-run paths; normal apply doesn't pay it.
    let layered_view = if check || dry_run {
        Some(nlink_lab::compute_layered_diff_with(&running, &desired, &apply_opts).await?)
    } else {
        None
    };
    // Removal ops (deleted nodes/links/routes/…) come from the plan
    // diff: the layered diff only sees what the declarative layers
    // still declare, never what disappeared (#83).
    // …and so does the plan's other work: a container re-created because
    // its options changed is in neither the lab graph nor the layers.
    let (removals, plan_ops): (Vec<String>, Vec<String>) = if check || dry_run {
        let plan = nlink_lab::apply_plan_with(&running, &desired, &apply_opts)?;
        (
            plan.ops
                .iter()
                .filter(|o| o.is_removal())
                .map(|o| o.describe())
                .collect(),
            plan.work().map(|o| o.describe()).collect(),
        )
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
        let no_op = report.no_op;
        let change_count = report.change_count;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if check && !no_op {
            return Err(nlink_lab::Error::Validation(format!(
                "drift detected: {change_count} change(s) needed to converge"
            )));
        }
        return Ok(());
    }

    // Non-JSON path. For --check / --dry-run, render the
    // layered diff (richer than the lab-graph-only
    // TopologyDiff). For ordinary apply, the existing
    // TopologyDiff render is what gets printed.
    let layered_is_empty = layered_view.as_ref().map(|l| l.is_empty()).unwrap_or(true)
        && removals.is_empty()
        && plan_ops.is_empty();

    if (check || dry_run) && layered_is_empty {
        if !ctx.quiet {
            println!("No changes to apply.");
        }
        return Ok(());
    }
    // An unchanged topology can still leave the kernel to fix: a hand edit
    // `verify` reports, or what a newer nlink-lab plans differently for the
    // same file (a forward chain that now accepts). Look before saying so.
    let mut drift_view = None;
    if !(check || dry_run) && diff.is_empty() {
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
                        "name": lab_name,
                        "ops": 0,
                        "removed": 0,
                        "applied": [],
                        "apply_time_ms": 0,
                    })
                );
            } else if !ctx.quiet {
                println!("No changes to apply.");
            }
            return Ok(());
        }
        drift_view = Some(layered);
    }

    // --check: exit non-zero if any layered drift.
    if check {
        let layered = layered_view
            .as_ref()
            .expect("layered_view is Some when check");
        let change_count = layered.change_count() + removals.len() + plan_ops.len();
        if !ctx.quiet {
            println!("Drift detected for lab '{lab_name}':");
            crate::output::print_layered(layered, &removals);
            crate::output::print_plan_ops(&plan_ops);
            println!("{change_count} change(s) needed to converge");
        }
        return Err(nlink_lab::Error::Validation(format!(
            "drift detected: {change_count} change(s) needed to converge"
        )));
    }

    // Under `--json`, stdout carries the one JSON document printed after
    // the apply; the change summary is for humans (#177).
    if !ctx.quiet && !ctx.json {
        println!("Changes for lab '{lab_name}':");
        if let Some(layered) = &drift_view {
            println!("  (topology unchanged; the kernel has drifted from it)");
            crate::output::print_layered(layered, &removals);
            crate::output::print_plan_ops(&plan_ops);
            println!("{} change(s)", layered.change_count());
        } else if let Some(layered) = &layered_view {
            crate::output::print_layered(layered, &removals);
            crate::output::print_plan_ops(&plan_ops);
            println!(
                "{} change(s)",
                layered.change_count() + removals.len() + plan_ops.len()
            );
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
    let report = nlink_lab::apply_with(&mut running, &desired, &apply_opts).await?;
    tracing::info!("apply: {} op(s), {} removal(s)", report.ops, report.removed);
    let elapsed = start.elapsed();
    let assertions_failed = running.assertions_failed();

    if ctx.json {
        let mut out = serde_json::json!({
            "name": lab_name,
            "ops": report.ops,
            "removed": report.removed,
            "applied": report.applied,
            "apply_time_ms": elapsed.as_millis() as u64,
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
        for a in running.assertion_results() {
            if a.passed {
                println!("  PASS {}", a.description);
            } else {
                println!(
                    "  FAIL {}: {}",
                    a.description,
                    a.detail.as_deref().unwrap_or_default()
                );
            }
        }
    }

    if assertions_failed {
        if strict {
            return Err(nlink_lab::Error::Validation(format!(
                "{} of {} assertion(s) failed (lab {:?} left as applied for inspection)",
                running
                    .assertion_results()
                    .iter()
                    .filter(|a| !a.passed)
                    .count(),
                running.assertion_results().len(),
                lab_name
            )));
        }
        eprintln!(
            "  {} assertions failed; pass --strict to make this fatal",
            yellow("WARN")
        );
    }
    Ok(())
}
