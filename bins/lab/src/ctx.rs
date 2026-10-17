//! Per-invocation context shared by the subcommand handlers: global
//! flags, colour helpers, topology parsing with `--set`, and the
//! root / capability check.

use anstream::eprintln;
/// Global flags shared by every subcommand handler.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    /// `--format`.
    pub format: crate::output::Format,
    /// `--format json|ndjson` (`--json`): machine-readable output.
    pub json: bool,
    /// `--quiet`: suppress informational output.
    pub quiet: bool,
    /// `-v`: extra detail (also raises the tracing level).
    pub verbose: bool,
}

pub use crate::output::style::{bold, green, red, yellow};

/// Turn a failed validation into the structured error (exit 2, issues
/// in the JSON envelope), printing the human-readable list first.
pub fn validation_failed(lab: &str, result: &nlink_lab::ValidationResult) -> nlink_lab::Error {
    eprintln!("Validation failed for {lab:?}:");
    for e in result.errors() {
        eprintln!("  {} {e}", red("ERROR"));
    }
    nlink_lab::Error::ValidationErrors(result.errors().cloned().collect())
}

/// Parse repeated `--set KEY=VALUE` strings into `(key, value)` pairs.
pub fn parse_set_params(params: &[String]) -> nlink_lab::Result<Vec<(String, String)>> {
    params
        .iter()
        .map(|p| {
            let (key, value) = p.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_argument(format!(
                    "invalid --set format: '{p}' (expected KEY=VALUE)"
                ))
            })?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Parse a topology file, optionally with CLI `--set` parameters.
pub fn parse_topology(
    path: &std::path::Path,
    params: &[String],
) -> nlink_lab::Result<nlink_lab::Topology> {
    let cli_params = parse_set_params(params)?;

    if cli_params.is_empty() {
        nlink_lab::parser::parse_file(path)
    } else {
        nlink_lab::parser::parse_file_with_params(path, &cli_params)
    }
}

/// Root, or CAP_NET_ADMIN + CAP_SYS_ADMIN in the effective set.
pub fn has_privileges() -> bool {
    nlink_lab::privileges::has_privileges()
}

// ── dynamic shell completion (issue #62) ──────────────────────────
// `COMPLETE=bash nlink-lab` (see `completions --help`) makes the shell
// call back into the binary; these completers answer with live data.

fn candidates(
    current: &std::ffi::OsStr,
    items: impl IntoIterator<Item = String>,
) -> Vec<clap_complete::CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let mut v: Vec<String> = items
        .into_iter()
        .filter(|s| s.starts_with(&*prefix))
        .collect();
    v.sort();
    v.dedup();
    v.into_iter()
        .map(clap_complete::CompletionCandidate::new)
        .collect()
}

/// Deployed lab names.
pub fn lab_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let labs = nlink_lab::state::list().unwrap_or_default();
        candidates(current, labs.into_iter().map(|l| l.name))
    })
}

/// Node names of every deployed lab (the shell cannot tell us which lab
/// the previous argument named, so the union is offered).
pub fn node_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let labs = nlink_lab::state::list().unwrap_or_default();
        let nodes = labs.into_iter().flat_map(|l| {
            nlink_lab::state::load(&l.name)
                // container nodes too: `logs <lab> <node>` takes only those
                .map(|(st, _)| {
                    st.namespaces
                        .keys()
                        .chain(st.containers.keys())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        candidates(current, nodes)
    })
}

/// Lint rule ids (for `lint --allow`).
pub fn lint_rule_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        candidates(
            current,
            nlink_lab::LINT_RULE_IDS.iter().map(|s| s.to_string()),
        )
    })
}

/// Validation rule ids (for `validate --deny/--allow`).
pub fn rule_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        candidates(current, nlink_lab::rule_ids().iter().map(|s| s.to_string()))
    })
}

/// Fail fast, before any namespace/netlink work, unless this process is
/// root or holds CAP_NET_ADMIN *and* CAP_SYS_ADMIN. The old check only
/// warned and accepted any capability bit at all (#44).
pub fn require_root() -> nlink_lab::Result<()> {
    if has_privileges() {
        Ok(())
    } else {
        Err(nlink_lab::Error::deploy_failed(
            "this command needs root (sudo), a SUID binary, or CAP_NET_ADMIN+CAP_SYS_ADMIN \
             (see `just install-caps`)",
        ))
    }
}
