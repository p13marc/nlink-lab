//! `nlink-lab benchmark`.

use crate::ctx::{Ctx, green, red, require_root};
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (must be deployed).
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Benchmark name as declared in the topology
    /// (`benchmark "name" { … }`). Omit to list the benchmarks the lab
    /// defines.
    pub name: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, name } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let benchmarks = &running.topology().benchmarks;
    let Some(name) = name else {
        if ctx.json {
            let names: Vec<&str> = benchmarks.iter().map(|b| b.name.as_str()).collect();
            println!("{}", serde_json::to_string_pretty(&names)?);
        } else if benchmarks.is_empty() {
            println!("Lab '{lab}' defines no benchmarks.");
        } else {
            for b in benchmarks {
                println!("{}  ({} test(s))", b.name, b.tests.len());
            }
        }
        return Ok(());
    };
    let benchmark = benchmarks
        .iter()
        .find(|b| b.name == name)
        .cloned()
        .ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!(
                "lab '{lab}' has no benchmark '{name}' (run without a name to list them)"
            ))
        })?;
    require_root()?;
    let result = nlink_lab::benchmark::run_benchmark(&running, &benchmark)?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Benchmark {:?}: {}",
            result.name,
            if result.passed {
                green("PASS")
            } else {
                red("FAIL")
            }
        );
        for test in &result.tests {
            let mark = if test.passed {
                green("ok")
            } else {
                red("FAIL")
            };
            println!("  {mark}  {}", test.description);
            // Metrics first: they are the point of the run, and a
            // benchmark with no assertions still has something to say.
            for (metric, value) in &test.metrics {
                println!("      {metric:<14} {value}");
            }
            for a in &test.assertions {
                let mark = if a.passed { green("ok") } else { red("FAIL") };
                let actual = a
                    .actual
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".into());
                println!(
                    "      {mark}  assert {} {} {} (actual {actual})",
                    a.metric, a.op, a.threshold
                );
            }
        }
    }
    if !result.passed {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
