//! `nlink-lab apply`.

use std::path::PathBuf;
use std::time::Instant;

use crate::ctx::{Ctx, parse_topology, red, require_root, validation_failed, yellow};

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
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        dry_run,
        check,
    } = args;
    // --check implies --dry-run.
    let dry_run = dry_run || check;

    let desired = parse_topology(&topology, &params)?;
    let result = desired.validate();
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }
    if result.has_errors() {
        return Err(validation_failed(&desired.lab.name, &result));
        #[allow(unreachable_code)]
        for e in result.errors() {
            eprintln!("  {} {e}", red("ERROR"));
        }
        return Err(nlink_lab::Error::Validation("see errors above".into()));
    }

    // Load current topology from running lab state
    let lab_name = &desired.lab.name;
    if !nlink_lab::state::exists(lab_name) {
        return Err(nlink_lab::Error::NotFound {
            name: format!("{lab_name} (deploy first, then apply changes)"),
        });
    }
    let mut running = nlink_lab::RunningLab::load(lab_name)?;
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
        Some(nlink_lab::compute_layered_diff(&running, &desired).await?)
    } else {
        None
    };

    // JSON dry-run output for CI consumption.
    if ctx.json && dry_run {
        let layered = layered_view
            .as_ref()
            .expect("layered_view is Some when dry_run");
        #[derive(serde::Serialize)]
        struct DryRunReport<'a> {
            /// Plan 159d — typed-shape schema marker.
            /// `3` = v3 (this format). Downstream `jq`
            /// consumers should branch on this. v3 dropped
            /// the v1 `diff` / `layered_summary` /
            /// `layered_summary_deprecated` fields (Plan
            /// 160 / 0.7.0) — use `network`/`nftables`.
            schema_version: u32,
            lab: &'a str,
            no_op: bool,
            change_count: usize,
            /// Plan 159d — typed per-namespace
            /// `NetworkConfig` diff under
            /// `nlink/serde`. Empty map elided.
            #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
            network: &'a std::collections::BTreeMap<String, nlink_lab::diff::ConfigDiff>,
            /// Plan 159d — typed per-namespace
            /// `NftablesDiff`. Empty map elided.
            #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
            nftables: &'a std::collections::BTreeMap<String, nlink_lab::diff::NftablesDiff>,
        }
        let report = DryRunReport {
            schema_version: 3,
            lab: lab_name,
            no_op: layered.is_empty(),
            change_count: layered.change_count(),
            network: &layered.network,
            nftables: &layered.nftables,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        if check && !layered.is_empty() {
            return Err(nlink_lab::Error::Validation(format!(
                "drift detected: {} change(s) needed to converge",
                layered.change_count(),
            )));
        }
        return Ok(());
    }

    // Non-JSON path. For --check / --dry-run, render the
    // layered diff (richer than the lab-graph-only
    // TopologyDiff). For ordinary apply, the existing
    // TopologyDiff render is what gets printed.
    let layered_is_empty = layered_view.as_ref().map(|l| l.is_empty()).unwrap_or(true);

    if (check || dry_run) && layered_is_empty {
        if !ctx.quiet {
            println!("No changes to apply.");
        }
        return Ok(());
    }
    if !(check || dry_run) && diff.is_empty() {
        if !ctx.quiet {
            println!("No changes to apply.");
        }
        return Ok(());
    }

    // --check: exit non-zero if any layered drift.
    if check {
        let layered = layered_view
            .as_ref()
            .expect("layered_view is Some when check");
        if !ctx.quiet {
            println!("Drift detected for lab '{lab_name}':");
            print!("{layered}");
            println!("{} change(s) needed to converge", layered.change_count());
        }
        return Err(nlink_lab::Error::Validation(format!(
            "drift detected: {} change(s) needed to converge",
            layered.change_count(),
        )));
    }

    if !ctx.quiet {
        println!("Changes for lab '{lab_name}':");
        if let Some(layered) = &layered_view {
            print!("{layered}");
            println!("{} change(s)", layered.change_count());
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
    let report = nlink_lab::apply(&mut running, &desired).await?;
    tracing::info!("apply: {} op(s), {} removal(s)", report.ops, report.removed);
    let elapsed = start.elapsed();

    if !ctx.quiet {
        println!(
            "\nApplied {} change(s) in {:.0?}",
            diff.change_count(),
            elapsed
        );
    }
    Ok(())
}
