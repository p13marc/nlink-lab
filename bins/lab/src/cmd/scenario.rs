//! `nlink-lab scenario`.

use crate::ctx::{Ctx, green, red, require_root};
use crate::output::{EXIT_VALIDATION, set_exit_code};
use anstream::println;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Scenario name as declared in the topology (`scenario "name" { … }`).
    /// Omit to list the scenarios the lab defines.
    pub name: Option<String>,

    /// When it fails, keep the lab's failure bundle in `DIR/<lab>/`
    /// (see `nlink-lab bundle`).
    #[arg(long, value_name = "DIR")]
    pub bundle: Option<std::path::PathBuf>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, name, bundle } = args;
    let lab = lab.resolve()?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    let scenarios = &running.topology().scenarios;
    let Some(name) = name else {
        if ctx.json {
            let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
            println!("{}", serde_json::to_string_pretty(&names)?);
        } else if scenarios.is_empty() {
            println!("Lab '{lab}' defines no scenarios.");
        } else {
            for sc in scenarios {
                println!("{}  ({} steps)", sc.name, sc.steps.len());
            }
        }
        return Ok(());
    };
    let scenario = scenarios
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| {
            nlink_lab::Error::invalid_argument(format!(
                "lab '{lab}' has no scenario '{name}' (run without a name to list them)"
            ))
        })?;
    require_root()?;
    let progress = crate::progress::Progress::new(ctx);
    let op = progress.op("scenario", running.name());
    let ran = nlink_lab::internal::scenario::run_scenario_with(&mut running, &scenario, &op).await;
    progress.finish();
    let result = ran?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Scenario {:?}: {}",
            scenario.name,
            if result.passed {
                green("PASS")
            } else {
                red("FAIL")
            }
        );
        let line = |action: &nlink_lab::internal::scenario::ActionResult| {
            let mark = if action.ok { green("ok") } else { red("FAIL") };
            let detail = action
                .detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            println!("    {mark}  {}{detail}", action.description);
        };
        for (index, step) in result.steps.iter().enumerate() {
            println!("  t={:>6}ms  step {index}", step.time_ms);
            step.actions.iter().for_each(line);
        }
        if !result.cleanup.is_empty() {
            println!("  put back");
            result.cleanup.iter().for_each(line);
        }
    }
    if !result.passed {
        if let Some(root) = bundle {
            crate::cmd::common::failure_bundle(&running, &root, &format!("scenario {name} failed"))
                .await;
        }
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
