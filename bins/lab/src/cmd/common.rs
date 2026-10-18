//! What several subcommands print or look up the same way: validation
//! issues, assertion results, plans and drift, the `.nll` files under a
//! path, a lab's node.

use std::path::{Path, PathBuf};

use anstream::{eprintln, println};

use crate::ctx::{green, red, yellow};

/// One `WARN` line each, on stderr.
pub fn print_warnings<T: std::fmt::Display>(warnings: impl IntoIterator<Item = T>) {
    for w in warnings {
        eprintln!("  {} {w}", yellow("WARN"));
    }
}

/// A failed validation as the structured error (exit 2, the issues in
/// the JSON document), after printing them.
pub fn validation_failed(lab: &str, result: &nlink_lab::ValidationResult) -> nlink_lab::Error {
    eprintln!("Validation failed for {lab:?}:");
    for e in result.errors() {
        eprintln!("  {} {e}", red("ERROR"));
    }
    nlink_lab::Error::ValidationErrors(result.errors().cloned().collect())
}

/// Validate `topology`: print its warnings, fail on its errors.
pub fn check_topology(topology: &nlink_lab::Topology) -> nlink_lab::Result<()> {
    let result = topology.validate();
    print_warnings(result.warnings());
    if result.has_errors() {
        return Err(validation_failed(&topology.lab.name, &result));
    }
    Ok(())
}

/// One line per `validate` assertion: `PASS` or `FAIL` and why.
pub fn print_assertions(results: &[nlink_lab::test_runner::AssertionResult]) {
    for a in results {
        if a.passed {
            println!("  {} {}", green("PASS"), a.description);
        } else {
            println!(
                "  {} {}{}",
                red("FAIL"),
                a.description,
                a.detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            );
        }
    }
}

/// How a run with failed assertions ends: an error (exit 2) with
/// `--strict`, a warning otherwise. `left` says how the lab stays
/// (`deployed`, `as applied`).
pub fn assertions_outcome(
    results: &[nlink_lab::test_runner::AssertionResult],
    strict: bool,
    lab: &str,
    left: &str,
) -> nlink_lab::Result<()> {
    let failed = results.iter().filter(|a| !a.passed).count();
    if failed == 0 {
        return Ok(());
    }
    if strict {
        return Err(nlink_lab::Error::Validation(format!(
            "{failed} of {} assertion(s) failed (lab {lab:?} left {left} for inspection)",
            results.len()
        )));
    }
    eprintln!(
        "  {} assertions failed; pass --strict to make this fatal",
        yellow("WARN")
    );
    Ok(())
}

/// A plan's ops under their stage headings.
pub fn print_plan(plan: &nlink_lab::Plan) {
    println!("Plan ({} op(s)):", plan.ops.len());
    let mut last = None;
    for op in &plan.ops {
        if last != Some(op.stage()) {
            println!("  [{}]", op.stage().name());
            last = Some(op.stage());
        }
        let mark = if op.is_removal() { "-" } else { "+" };
        println!("    {mark} {}", op.describe());
    }
}

/// A plan's removals and its other work, described: what `apply` and
/// `verify` count as changes besides the layered diff.
pub fn plan_lists(plan: &nlink_lab::Plan) -> (Vec<String>, Vec<String>) {
    let removals = plan
        .ops
        .iter()
        .filter(|o| o.is_removal())
        .map(|o| o.describe())
        .collect();
    let work = plan.work().map(|o| o.describe()).collect();
    (removals, work)
}

/// The changes between a lab and its topology: the layered diff, the
/// removals and the plan's other work. Returns how many there are.
pub fn print_drift(
    layered: &nlink_lab::diff::LayeredDiff,
    removals: &[String],
    work: &[String],
) -> usize {
    crate::output::print_layered(layered, removals);
    if !work.is_empty() {
        println!("Plan:");
        for op in work {
            println!("  + {op}");
        }
    }
    layered.change_count() + removals.len() + work.len()
}

/// The `.nll` files `path` names: the file itself, or those in the
/// directory (and below it with `recursive`), sorted.
pub fn collect_nll(path: &Path, recursive: bool) -> std::io::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for e in entries {
            if e.is_dir() {
                if recursive {
                    walk(&e, recursive, out)?;
                }
            } else if e.extension().is_some_and(|x| x == "nll") {
                out.push(e);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if path.is_dir() {
        walk(path, recursive, &mut out)?;
    } else {
        out.push(path.to_path_buf());
    }
    Ok(out)
}

/// `node` of `lab`, or `NodeNotFound` after listing the lab's nodes.
pub fn require_node(lab: &nlink_lab::RunningLab, node: &str) -> nlink_lab::Result<()> {
    let names: Vec<&str> = lab.node_names().collect();
    if names.contains(&node) {
        return Ok(());
    }
    eprintln!("Available nodes: {}", names.join(", "));
    Err(nlink_lab::Error::NodeNotFound {
        name: node.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nll_files_are_collected_sorted_and_optionally_deep() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        for f in ["b.nll", "a.nll", "notes.md"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        std::fs::write(sub.join("c.nll"), "").unwrap();
        let names = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter()
                .map(|p| p.strip_prefix(dir.path()).unwrap().display().to_string())
                .collect()
        };
        assert_eq!(
            names(collect_nll(dir.path(), false).unwrap()),
            ["a.nll", "b.nll"]
        );
        assert_eq!(
            names(collect_nll(dir.path(), true).unwrap()),
            ["a.nll", "b.nll", "sub/c.nll"]
        );
    }

    #[test]
    fn failed_assertions_fail_only_when_strict() {
        let r = |passed| nlink_lab::test_runner::AssertionResult {
            description: "reach a b".into(),
            passed,
            detail: None,
            duration_ms: 0,
        };
        assert!(assertions_outcome(&[r(true)], true, "l", "deployed").is_ok());
        assert!(assertions_outcome(&[r(false)], false, "l", "deployed").is_ok());
        let e = assertions_outcome(&[r(true), r(false)], true, "l", "deployed").unwrap_err();
        assert!(e.to_string().contains("1 of 2 assertion(s) failed"), "{e}");
    }
}
