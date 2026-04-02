use crate::color::{green, red};
use crate::util::check_root;
use std::path::PathBuf;

pub(crate) async fn run(
    path: PathBuf,
    junit: Option<PathBuf>,
    tap: bool,
    fail_fast: bool,
) -> nlink_lab::Result<()> {
    check_root();

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
        match nlink_lab::test_runner::run_test(file).await {
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
        std::process::exit(1);
    }
    Ok(())
}
