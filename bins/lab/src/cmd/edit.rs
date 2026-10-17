//! Live topology edits on top of `apply` (issue #69), for `node` and
//! `link`: the library's [`Edit`] list, checked and applied as one.

use crate::ctx::{Ctx, require_root};
use crate::output::{DryRunReport, print_layered};
use anstream::println;
use nlink_lab::edit::Edit;

/// Make `edits` to `lab`: checked together, the result validated, then
/// applied (MTUs live, the rest through `apply`). `node` and `link` run
/// it.
pub async fn run_edits(
    ctx: &Ctx,
    lab: &str,
    edits: &[Edit],
    dry_run: bool,
) -> nlink_lab::Result<()> {
    require_root()?;
    let lab = lab.to_string();
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    // Every edit is checked, and the result validated, before anything
    // changes: a bad edit next to an MTU change used to fail after the MTU
    // changed (#209).
    let ed = nlink_lab::edit::edited(running.topology(), edits)?;
    crate::cmd::common::print_warnings(&ed.warnings);
    if !ed.structural {
        // MTU changes are applied directly (both ends, live) rather than
        // through the plan: the plan only knows an MTU at veth creation.
        if !dry_run {
            nlink_lab::edit::apply_edited(&mut running, &ed).await?;
        }
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "lab": lab, "edits": ed.log, "ops": 0, "removed": 0, "applied": [] })
            );
        } else if !ctx.quiet {
            for l in &ed.log {
                println!("  {l}");
            }
        }
        return Ok(());
    }
    if dry_run {
        let plan = nlink_lab::apply_plan(&running, &ed.topology)?;
        let layered = nlink_lab::compute_layered_diff(&running, &ed.topology).await?;
        if ctx.json {
            let removals: Vec<String> = plan
                .ops
                .iter()
                .filter(|o| o.is_removal())
                .map(|o| o.describe())
                .collect();
            let plan_ops: Vec<String> = plan.work().map(|o| o.describe()).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &DryRunReport::new(&lab, &layered, &removals).with_ops(&plan_ops)
                )?
            );
            return Ok(());
        }
        for l in &ed.log {
            println!("  {l}");
        }
        crate::cmd::common::print_plan(&plan);
        print_layered(&layered, &[]);
        return Ok(());
    }
    let progress = crate::progress::Progress::new(ctx);
    let op = progress.op("apply", &lab);
    for (ep, _, mtu) in &ed.mtus {
        running.set_link_mtu(ep, *mtu).await?;
    }
    let applied = nlink_lab::apply_with_ctx(
        &mut running,
        &ed.topology,
        &nlink_lab::ApplyOptions::default(),
        &op,
    )
    .await;
    progress.finish();
    let report = applied?;
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "lab": lab,
                "edits": ed.log,
                "ops": report.ops,
                "removed": report.removed,
                "applied": report.applied,
            })
        );
    } else if !ctx.quiet {
        for l in &ed.log {
            println!("  {l}");
        }
        println!(
            "Applied {} op(s) ({} removal(s)) to lab {:?}",
            report.ops, report.removed, lab
        );
    }
    Ok(())
}
