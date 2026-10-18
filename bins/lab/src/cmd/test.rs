//! `nlink-lab test`.

use anstream::{eprintln, print};
use std::path::PathBuf;

use crate::ctx::{Ctx, green, red, require_root};
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Topology file or directory of .nll files.
    pub path: PathBuf,

    /// Set a `param` value (repeatable) for every file: --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value)]
    pub params: Vec<(String, String)>,

    /// Write JUnit XML results to file.
    #[arg(long)]
    pub junit: Option<PathBuf>,

    /// Write TAP output to stdout.
    #[arg(long)]
    pub tap: bool,

    /// Stop on first failure.
    #[arg(long)]
    pub fail_fast: bool,

    /// Also run every `scenario` block of each topology after its
    /// `validate` assertions: each action is one more result.
    #[arg(long, alias = "scenario")]
    pub scenarios: bool,

    /// Also run every `benchmark` block: each test is one more result.
    #[arg(long, alias = "benchmark")]
    pub benchmarks: bool,

    /// Leave a lab whose test failed deployed, for inspection (destroy it
    /// with `nlink-lab destroy LAB`).
    #[arg(long)]
    pub keep: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        path,
        params,
        junit,
        tap,
        fail_fast,
        scenarios,
        benchmarks,
        keep,
    } = args;
    let opts = nlink_lab::test_runner::TestOptions {
        scenarios,
        benchmarks,
        keep_failed: keep,
    };
    require_root()?;
    let cli_params = params;

    // Collect .nll files
    let files: Vec<PathBuf> = crate::cmd::common::collect_nll(&path, false)?;

    if files.is_empty() {
        eprintln!("No .nll files found in {}", path.display());
        return Ok(());
    }

    let mut all_results = Vec::new();
    let mut any_failed = false;

    for file in &files {
        let started = std::time::Instant::now();
        // the deploy's stages and waits, then the destroy's, live
        let progress = crate::progress::Progress::new(ctx);
        let op = progress.op("test", "");
        let ran = nlink_lab::test_runner::run_test_opts(file, &cli_params, opts, &op).await;
        progress.finish();
        match ran {
            Ok(result) => {
                let pass_count = result.assertions.iter().filter(|a| a.passed).count();
                let total = result.assertions.len();
                if result.passed {
                    eprintln!(
                        "{} {} ({pass_count}/{total} assertions, {}ms)",
                        green("PASS"),
                        file.display(),
                        result.total_ms
                    );
                } else {
                    eprintln!(
                        "{} {} ({pass_count}/{total} assertions, {}ms)",
                        red("FAIL"),
                        file.display(),
                        result.total_ms
                    );
                    for a in &result.assertions {
                        if !a.passed {
                            eprintln!(
                                "  {} {}{}",
                                red("FAIL"),
                                a.description,
                                a.detail
                                    .as_ref()
                                    .map(|d| format!(": {d}"))
                                    .unwrap_or_default()
                            );
                        }
                    }
                    any_failed = true;
                }
                if let Some(err) = &result.error {
                    eprintln!("  {} {err}", red("ERROR"));
                }
                if let Some(lab) = &result.kept {
                    eprintln!(
                        "  kept lab '{lab}' for inspection: `nlink-lab destroy {lab}` when done"
                    );
                }
                all_results.push(result);
            }
            Err(e) => {
                eprintln!("{} {}: {e}", red("ERROR"), file.display());
                any_failed = true;
                // in the reports too: a file that did not deploy was
                // missing from them, which then read as green (#208)
                all_results.push(nlink_lab::test_runner::TestResult::errored(
                    file.display().to_string(),
                    &e,
                    started.elapsed().as_millis() as u64,
                ));
                if fail_fast {
                    break;
                }
            }
        }
        if any_failed && fail_fast {
            break;
        }
    }

    // Output formats
    if let Some(junit_path) = &junit {
        let xml = nlink_lab::test_runner::format_junit(&all_results);
        std::fs::write(junit_path, &xml)?;
        eprintln!("JUnit results written to {}", junit_path.display());
    }

    if tap {
        print!("{}", nlink_lab::test_runner::format_tap(&all_results));
    }

    if any_failed {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
