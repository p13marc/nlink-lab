//! `nlink-lab test`.

use std::path::PathBuf;

use crate::ctx::{Ctx, green, parse_set_params, red, require_root};
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Topology file or directory of .nll files.
    pub path: PathBuf,

    /// Set a `param` value (repeatable) for every file: --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Write JUnit XML results to file.
    #[arg(long)]
    pub junit: Option<PathBuf>,

    /// Write TAP output to stdout.
    #[arg(long)]
    pub tap: bool,

    /// Stop on first failure.
    #[arg(long)]
    pub fail_fast: bool,
}

pub async fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        path,
        params,
        junit,
        tap,
        fail_fast,
    } = args;
    require_root()?;
    let cli_params = parse_set_params(&params)?;

    // Collect .nll files
    let files: Vec<PathBuf> = if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "nll"))
            .collect();
        entries.sort();
        entries
    } else {
        vec![path.clone()]
    };

    if files.is_empty() {
        eprintln!("No .nll files found in {}", path.display());
        return Ok(());
    }

    let mut all_results = Vec::new();
    let mut any_failed = false;

    for file in &files {
        eprint!("Testing {} ... ", file.display());
        match nlink_lab::test_runner::run_test_with_params(file, &cli_params).await {
            Ok(result) => {
                let pass_count = result.assertions.iter().filter(|a| a.passed).count();
                let total = result.assertions.len();
                if result.passed {
                    eprintln!(
                        "{} ({pass_count}/{total} assertions, {}ms)",
                        green("PASS"),
                        result.total_ms
                    );
                } else {
                    eprintln!(
                        "{} ({pass_count}/{total} assertions, {}ms)",
                        red("FAIL"),
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
                all_results.push(result);
            }
            Err(e) => {
                eprintln!("{}: {e}", red("ERROR"));
                any_failed = true;
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
