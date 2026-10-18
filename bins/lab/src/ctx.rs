//! Per-invocation context shared by the subcommand handlers: global
//! flags, colour helpers, topology parsing with `--set`, and the
//! root / capability check.

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

/// `-L/--lab`: the lab a lab-scoped command acts on.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LabArg {
    /// The lab. Default: `$NLINK_LAB`, then the one `nlink-lab use`
    /// selected, then the only running lab.
    #[arg(short = 'L', long = "lab", value_name = "LAB", add = lab_completer())]
    pub lab: Option<String>,
}

/// Where the lab of a command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabFrom {
    /// `-L LAB`.
    Flag,
    /// `NLINK_LAB`.
    Env,
    /// `nlink-lab use LAB`.
    Context,
    /// The only lab running.
    Only,
}

impl LabArg {
    /// The lab: the flag, `NLINK_LAB`, the current lab, or the only one.
    pub fn resolve(&self) -> nlink_lab::Result<String> {
        resolve_lab(self.lab.as_deref()).map(|(lab, _)| lab)
    }
}

/// Who runs this: the user behind `sudo` (`SUDO_UID`), else the caller.
/// The current lab is theirs, not root's.
pub fn caller_uid() -> u32 {
    // SAFETY: getuid/geteuid cannot fail.
    if unsafe { libc::geteuid() } == 0
        && let Some(uid) = std::env::var("SUDO_UID").ok().and_then(|u| u.parse().ok())
    {
        return uid;
    }
    unsafe { libc::getuid() }
}

/// The lab `flag` names, else `NLINK_LAB`, else the caller's current lab
/// (`nlink-lab use`), else the only running lab; and where it came from.
pub fn resolve_lab(flag: Option<&str>) -> nlink_lab::Result<(String, LabFrom)> {
    let checked = |lab: String, from| {
        nlink_lab::state::validate_lab_name(&lab)?;
        Ok((lab, from))
    };
    if let Some(lab) = flag {
        return checked(lab.to_string(), LabFrom::Flag);
    }
    if let Some(lab) = std::env::var("NLINK_LAB").ok().filter(|l| !l.is_empty()) {
        return checked(lab, LabFrom::Env);
    }
    if let Some(lab) = nlink_lab::state::context_get(caller_uid()) {
        if !nlink_lab::state::exists(&lab) {
            return Err(nlink_lab::Error::invalid_argument(format!(
                "the current lab '{lab}' (`nlink-lab use`) is not running: \
                 `nlink-lab use LAB` picks another, `nlink-lab use --clear` forgets it"
            )));
        }
        return checked(lab, LabFrom::Context);
    }
    let labs = nlink_lab::state::list()?;
    match labs.as_slice() {
        [only] => checked(only.name.clone(), LabFrom::Only),
        [] => Err(nlink_lab::Error::invalid_argument(
            "no lab is running: deploy one, or name one with -L LAB",
        )),
        many => Err(nlink_lab::Error::invalid_argument(format!(
            "{} labs are running ({}): pick one with -L LAB, NLINK_LAB=LAB or `nlink-lab use LAB`",
            many.len(),
            many.iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Parse a topology file, optionally with CLI `--set` parameters.
pub fn parse_topology(
    path: &std::path::Path,
    params: &[(String, String)],
) -> nlink_lab::Result<nlink_lab::Topology> {
    if params.is_empty() {
        nlink_lab::parser::parse_file(path)
    } else {
        nlink_lab::parser::parse_file_with_params(path, params)
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

/// The lab a completion is for: a `-L LAB` already on the line being
/// completed (the shell passes its words to the binary), else the current
/// lab, else none (every lab's names are offered).
fn completing_lab() -> Option<String> {
    let words: Vec<String> = std::env::args().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(lab) = w.strip_prefix("--lab=") {
            return Some(lab.to_string());
        }
        if (w == "-L" || w == "--lab")
            && let Some(lab) = words.get(i + 1)
        {
            return Some(lab.clone());
        }
    }
    resolve_lab(None).ok().map(|(lab, _)| lab)
}

/// The labs completions draw on: the one being worked on, or all of them.
fn completion_labs() -> Vec<String> {
    match completing_lab() {
        Some(lab) => vec![lab],
        None => nlink_lab::state::list()
            .unwrap_or_default()
            .into_iter()
            .map(|l| l.name)
            .collect(),
    }
}

/// Node names of the lab being worked on (container nodes too: `logs`
/// takes only those).
pub fn node_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let nodes = completion_labs().into_iter().flat_map(|lab| {
            nlink_lab::state::load(&lab)
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

/// `node:iface` endpoints of the lab being worked on.
pub fn endpoint_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let endpoints = completion_labs().into_iter().flat_map(|lab| {
            nlink_lab::state::load(&lab)
                .map(|(_, topo)| nlink_lab::impair_parse::topology_endpoints(&topo))
                .unwrap_or_default()
        });
        candidates(current, endpoints)
    })
}

/// Snapshots of the lab being worked on.
pub fn snapshot_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let names = completion_labs().into_iter().flat_map(|lab| {
            nlink_lab::state::snapshot_list(&lab)
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.name)
        });
        candidates(current, names)
    })
}

/// Tracked processes of the lab being worked on (`kill`, `stats --pid`,
/// `logs --pid`), alive ones.
pub fn pid_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let pids = completion_labs().into_iter().flat_map(|lab| {
            nlink_lab::RunningLab::load(&lab)
                .map(|l| {
                    l.process_status_alive_only()
                        .into_iter()
                        .map(|p| p.pid.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        candidates(current, pids)
    })
}

/// `KEY=` for every `param` the topology file on the line declares (with
/// its default after the `=`).
pub fn param_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let file = std::env::args()
            .skip(1)
            .map(std::path::PathBuf::from)
            .find(|p| p.extension().is_some_and(|e| e == "nll") && p.is_file());
        let params = file
            .and_then(|f| nlink_lab::parser::declared_param_defs(f).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(k, default)| format!("{k}={}", default.unwrap_or_default()));
        candidates(current, params)
    })
}

/// Built-in templates (`init`).
pub fn template_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        candidates(
            current,
            nlink_lab::templates::list()
                .iter()
                .map(|t| t.name.to_string()),
        )
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
