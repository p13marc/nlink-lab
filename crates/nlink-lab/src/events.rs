//! Lifecycle event log (issue #70).
//!
//! Every deploy/apply/destroy/spawn/kill/impair/partition/snapshot is a
//! separate CLI invocation, so lifecycle events cannot be *observed* by
//! one process the way RTNETLINK drift can; they are *recorded* instead:
//! each operation appends one NDJSON line to
//! `<state_dir>/<lab>/events.ndjson`. `nlink-lab events <lab>` prints
//! and follows the file (merging it with drift and runtime events), and
//! the backend republishes new lines on zenoh / HTTP.
//!
//! Recording is best effort: a failure to append is logged and never
//! fails the operation that produced the event.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Rotate `events.ndjson` to `events.1.ndjson` past this size.
const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// One recorded lifecycle event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LifecycleEvent {
    /// ISO 8601 timestamp (UTC).
    pub ts: String,
    /// Lab name.
    pub lab: String,
    #[serde(flatten)]
    pub kind: LifecycleKind,
}

/// What happened. Serialised with an `event` tag
/// (`{"event":"deployed","nodes":3,…}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LifecycleKind {
    Deployed {
        nodes: usize,
        links: usize,
    },
    Applied {
        ops: usize,
        removed: usize,
    },
    ApplyFailed {
        error: String,
    },
    Destroyed,
    Spawned {
        node: String,
        pid: u32,
        cmd: String,
    },
    Killed {
        node: Option<String>,
        pid: u32,
    },
    Impaired {
        endpoint: String,
        impairment: Box<crate::types::Impairment>,
    },
    ImpairCleared {
        endpoint: String,
    },
    Partitioned {
        endpoint: String,
    },
    Healed {
        endpoint: String,
    },
    SnapshotTaken {
        name: String,
    },
    Restored {
        name: String,
    },
    AssertionsRun {
        passed: usize,
        failed: usize,
    },
    ScenarioStep {
        scenario: String,
        time_ms: u64,
        ok: bool,
        actions: usize,
    },
}

impl LifecycleKind {
    /// Snake-case tag, the same string serde writes in `event`.
    pub fn name(&self) -> &'static str {
        match self {
            LifecycleKind::Deployed { .. } => "deployed",
            LifecycleKind::Applied { .. } => "applied",
            LifecycleKind::ApplyFailed { .. } => "apply_failed",
            LifecycleKind::Destroyed => "destroyed",
            LifecycleKind::Spawned { .. } => "spawned",
            LifecycleKind::Killed { .. } => "killed",
            LifecycleKind::Impaired { .. } => "impaired",
            LifecycleKind::ImpairCleared { .. } => "impair_cleared",
            LifecycleKind::Partitioned { .. } => "partitioned",
            LifecycleKind::Healed { .. } => "healed",
            LifecycleKind::SnapshotTaken { .. } => "snapshot_taken",
            LifecycleKind::Restored { .. } => "restored",
            LifecycleKind::AssertionsRun { .. } => "assertions_run",
            LifecycleKind::ScenarioStep { .. } => "scenario_step",
        }
    }
}

impl LifecycleEvent {
    /// One human-readable line (the `--json`-less `events` output).
    pub fn render_line(&self) -> String {
        use LifecycleKind as K;
        let detail = match &self.kind {
            K::Deployed { nodes, links } => format!("{nodes} node(s), {links} link(s)"),
            K::Applied { ops, removed } => format!("{ops} op(s), {removed} removal(s)"),
            K::ApplyFailed { error } => format!("error: {error}"),
            K::Destroyed => String::new(),
            K::Spawned { node, pid, cmd } => format!("{node} pid {pid}: {cmd}"),
            K::Killed { node, pid } => match node {
                Some(n) => format!("{n} pid {pid}"),
                None => format!("pid {pid}"),
            },
            K::Impaired {
                endpoint,
                impairment,
            } => format!("{endpoint}: {}", describe_impairment(impairment)),
            K::ImpairCleared { endpoint }
            | K::Partitioned { endpoint }
            | K::Healed { endpoint } => endpoint.clone(),
            K::SnapshotTaken { name } | K::Restored { name } => format!("snapshot {name:?}"),
            K::AssertionsRun { passed, failed } => format!("{passed} passed, {failed} failed"),
            K::ScenarioStep {
                scenario,
                time_ms,
                ok,
                actions,
            } => format!(
                "{scenario:?} at {time_ms}ms: {actions} action(s) {}",
                if *ok { "ok" } else { "FAILED" }
            ),
        };
        if detail.is_empty() {
            format!("{} lifecycle {}", self.ts, self.kind.name())
        } else {
            format!("{} lifecycle {} {detail}", self.ts, self.kind.name())
        }
    }
}

fn describe_impairment(imp: &crate::types::Impairment) -> String {
    let mut parts = Vec::new();
    for (k, v) in [
        ("delay", &imp.delay),
        ("jitter", &imp.jitter),
        ("loss", &imp.loss),
        ("rate", &imp.rate),
        ("corrupt", &imp.corrupt),
        ("reorder", &imp.reorder),
        ("duplicate", &imp.duplicate),
        ("limit", &imp.limit),
    ] {
        if let Some(v) = v {
            parts.push(format!("{k} {v}"));
        }
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" ")
    }
}

/// Current time as RFC 3339 UTC (the `ts` every record carries).
pub fn now_iso8601() -> String {
    crate::deploy::now_iso8601()
}

/// `now - d` as RFC 3339 UTC; events are compared lexically against it
/// (`--since`), which is time order for UTC RFC 3339 strings.
pub fn since_iso8601(d: std::time::Duration) -> String {
    (time::OffsetDateTime::now_utc() - d)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// `<state_dir>/<lab>/events.ndjson`.
pub fn events_path(lab: &str) -> PathBuf {
    crate::state::state_dir(lab).join("events.ndjson")
}

/// Append one event now. Best effort: errors are logged, never returned.
pub fn record(lab: &str, kind: LifecycleKind) {
    let ev = LifecycleEvent {
        ts: crate::deploy::now_iso8601(),
        lab: lab.to_string(),
        kind,
    };
    if let Err(e) = append(lab, &ev) {
        tracing::warn!(
            lab,
            event = ev.kind.name(),
            "could not record lifecycle event: {e}"
        );
    }
}

/// Append an event line (rotating a file past [`MAX_BYTES`]).
pub fn append(lab: &str, event: &LifecycleEvent) -> Result<()> {
    let path = events_path(lab);
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    if !dir.exists() {
        // The lab's state directory is gone (destroyed): nothing to
        // record into.
        return Ok(());
    }
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > MAX_BYTES
    {
        let _ = std::fs::rename(&path, dir.join("events.1.ndjson"));
    }
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    // O_APPEND: one write per line, so concurrent CLI invocations never
    // interleave inside a record (lines are far below PIPE_BUF).
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Every recorded event, oldest first (the rotated generation first).
/// Unparseable lines are skipped.
pub fn read(lab: &str) -> Result<Vec<LifecycleEvent>> {
    let path = events_path(lab);
    let mut out = Vec::new();
    for p in [path.with_file_name("events.1.ndjson"), path] {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        out.extend(parse_lines(&text));
    }
    Ok(out)
}

/// Parse NDJSON text into events, skipping bad lines.
pub fn parse_lines(text: &str) -> Vec<LifecycleEvent> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<LifecycleEvent>(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::xdg_state_lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        std::fs::create_dir_all(crate::state::state_dir("ev-lab")).unwrap();
        f()
    }

    #[test]
    fn append_read_roundtrip_and_bad_lines_skipped() {
        with_state_dir(|| {
            record("ev-lab", LifecycleKind::Deployed { nodes: 2, links: 1 });
            record(
                "ev-lab",
                LifecycleKind::Impaired {
                    endpoint: "a:eth0".into(),
                    impairment: Box::new(crate::types::Impairment {
                        delay: Some("5ms".into()),
                        ..Default::default()
                    }),
                },
            );
            // a torn / foreign line
            std::fs::OpenOptions::new()
                .append(true)
                .open(events_path("ev-lab"))
                .unwrap()
                .write_all(b"{not json\n")
                .unwrap();
            record("ev-lab", LifecycleKind::Destroyed);
            let evs = read("ev-lab").unwrap();
            let names: Vec<&str> = evs.iter().map(|e| e.kind.name()).collect();
            assert_eq!(names, vec!["deployed", "impaired", "destroyed"]);
            assert_eq!(evs[0].lab, "ev-lab");
            assert!(
                evs[1].render_line().contains("a:eth0: delay 5ms"),
                "{}",
                evs[1].render_line()
            );
            let json = serde_json::to_string(&evs[0]).unwrap();
            assert!(json.contains("\"event\":\"deployed\""), "{json}");
        });
    }

    #[test]
    fn rotation_keeps_one_generation() {
        with_state_dir(|| {
            let path = events_path("ev-lab");
            // Pretend the file is already over the limit.
            let big = std::fs::File::create(&path).unwrap();
            big.set_len(MAX_BYTES + 1).unwrap();
            record("ev-lab", LifecycleKind::Destroyed);
            assert!(path.with_file_name("events.1.ndjson").exists());
            let fresh = std::fs::metadata(&path).unwrap().len();
            assert!(
                fresh < 1024,
                "new file should hold one line, got {fresh} bytes"
            );
        });
    }

    #[test]
    fn missing_state_dir_is_a_silent_noop() {
        with_state_dir(|| {
            record("never-deployed", LifecycleKind::Destroyed);
            assert!(!events_path("never-deployed").exists());
            assert!(read("never-deployed").unwrap().is_empty());
        });
    }
}
