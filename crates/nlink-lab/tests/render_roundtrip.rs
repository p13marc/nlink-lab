//! Render round-trip gate (issue #24).
//!
//! For every `examples/**/*.nll` (except the `examples/imports/` module
//! files, which are not standalone labs) this test asserts that
//!
//! ```text
//! parse_file(src) == parse(render(parse_file(src)))
//! ```
//!
//! structurally (via `serde_json::Value`, since `Topology` does not
//! implement `PartialEq`), that the re-parsed topology validates with the
//! same error set as the original, and that rendering is idempotent
//! (`render(parse(render(t))) == render(t)`).
//!
//! Files that cannot round-trip for reasons outside `render.rs` are
//! listed in [`KNOWN_FAILURES`] with a one-line reason each; the list is
//! the punch list. A listed file that starts passing fails the test so
//! the entry gets removed.

use std::path::{Path, PathBuf};

use nlink_lab::parser;
use nlink_lab::render::try_render;

/// `(path relative to the workspace root, reason)`.
const KNOWN_FAILURES: &[(&str, &str)] = &[
    (
        "examples/pattern-ring.nll",
        "lower.rs pattern lowering sets node.profile without merging the profile's props \
         (sysctls) into the node, unlike `node x : p`; re-parse merges them",
    ),
    (
        "examples/pattern-star.nll",
        "same as pattern-ring: `profile` inside a mesh/ring/star block is not merged into nodes",
    ),
];

/// Find the workspace root (directory containing the workspace
/// `Cargo.toml`).
fn workspace_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let mut dir = PathBuf::from(&manifest_dir);
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let s = std::fs::read_to_string(&cargo_toml).unwrap_or_default();
            if s.contains("[workspace]") {
                return dir;
            }
        }
        if !dir.pop() {
            panic!("could not find workspace root");
        }
    }
}

fn collect_nll_files(dir: &Path, skip: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() || dir == skip {
        return;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_nll_files(&path, skip, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("nll") {
            out.push(path);
        }
    }
}

/// Sorted `"[rule] message"` strings for every error-level issue.
fn validation_errors(topo: &nlink_lab::Topology) -> Vec<String> {
    let mut errs: Vec<String> = topo.validate().errors().map(|e| e.to_string()).collect();
    errs.sort();
    errs
}

/// Walk two JSON values and report the paths at which they differ.
fn json_diff(a: &serde_json::Value, b: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}.{k}");
                match (x.get(k), y.get(k)) {
                    (Some(va), Some(vb)) => json_diff(va, vb, &p, out),
                    (Some(va), None) => out.push(format!("{p}: only in original: {va}")),
                    (None, Some(vb)) => out.push(format!("{p}: only in re-parsed: {vb}")),
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                json_diff(va, vb, &format!("{path}[{i}]"), out);
            }
        }
        _ if a != b => out.push(format!("{path}: original {a} != re-parsed {b}")),
        _ => {}
    }
}

fn roundtrip_one(path: &Path) -> Result<(), String> {
    let topo = parser::parse_file(path).map_err(|e| format!("initial parse failed: {e}"))?;
    let rendered = try_render(&topo).map_err(|e| format!("render failed: {e}"))?;
    let topo2 = parser::parse(&rendered)
        .map_err(|e| format!("re-parse failed: {e}\n--- rendered NLL ---\n{rendered}"))?;

    let a = serde_json::to_value(&topo).unwrap();
    let b = serde_json::to_value(&topo2).unwrap();
    if a != b {
        let mut diffs = Vec::new();
        json_diff(&a, &b, "topology", &mut diffs);
        return Err(format!(
            "topology differs after round-trip:\n  {}\n--- rendered NLL ---\n{rendered}",
            diffs.join("\n  ")
        ));
    }

    let before = validation_errors(&topo);
    let after = validation_errors(&topo2);
    if before != after {
        return Err(format!(
            "validation errors changed after round-trip:\n  before: {before:?}\n  after:  {after:?}\n--- rendered NLL ---\n{rendered}"
        ));
    }

    let rendered2 = try_render(&topo2).map_err(|e| format!("second render failed: {e}"))?;
    if rendered2 != rendered {
        return Err(format!(
            "render is not idempotent\n--- first ---\n{rendered}\n--- second ---\n{rendered2}"
        ));
    }
    Ok(())
}

#[test]
fn examples_render_roundtrip() {
    let root = workspace_root();
    let examples = root.join("examples");
    let mut files = Vec::new();
    collect_nll_files(&examples, &examples.join("imports"), &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no .nll files found under {}",
        examples.display()
    );

    let mut failures = Vec::new();
    let mut unexpected_passes = Vec::new();
    let mut passed = 0usize;

    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let known = KNOWN_FAILURES.iter().find(|(f, _)| *f == rel);
        match roundtrip_one(path) {
            Ok(()) => match known {
                Some((_, reason)) => {
                    unexpected_passes.push(format!("{rel} (listed as: {reason})"));
                }
                None => passed += 1,
            },
            Err(e) => match known {
                Some((_, reason)) => eprintln!("known failure {rel} ({reason}):\n{e}\n"),
                None => failures.push(format!("=== {rel} ===\n{e}")),
            },
        }
    }

    eprintln!(
        "render round-trip: {passed} passed, {} known failures, {} files",
        KNOWN_FAILURES.len(),
        files.len()
    );
    assert!(
        failures.is_empty(),
        "{} example(s) failed the render round-trip:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(
        unexpected_passes.is_empty(),
        "these examples now round-trip — remove them from KNOWN_FAILURES:\n  {}",
        unexpected_passes.join("\n  ")
    );
}

#[test]
fn known_failures_point_at_existing_files() {
    let root = workspace_root();
    for (file, reason) in KNOWN_FAILURES {
        assert!(
            root.join(file).is_file(),
            "KNOWN_FAILURES entry {file:?} ({reason}) does not exist"
        );
        assert!(
            !reason.is_empty(),
            "KNOWN_FAILURES entry {file:?} needs a reason"
        );
    }
}
