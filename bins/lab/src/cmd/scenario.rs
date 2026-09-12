//! `nlink-lab scenario`.

use crate::ctx::{Ctx, green, red, require_root};
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (must be deployed).
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Scenario name as declared in the topology (`scenario "name" { … }`).
    /// Omit to list the scenarios the lab defines.
    pub name: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, name } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
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
            nlink_lab::Error::invalid_topology(format!(
                "lab '{lab}' has no scenario '{name}' (run without a name to list them)"
            ))
        })?;
    require_root()?;
    let result = nlink_lab::scenario::run_scenario(&running, &scenario).await?;
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
        for (index, step) in result.steps.iter().enumerate() {
            println!("  t={:>6}ms  step {index}", step.time_ms);
            for action in &step.actions {
                let mark = if action.ok { green("ok") } else { red("FAIL") };
                let detail = action
                    .detail
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                println!("    {mark}  {}{detail}", action.description);
            }
        }
    }
    if !result.passed {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
