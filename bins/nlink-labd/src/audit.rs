//! The audit log: one NDJSON line per call an operator made — who, what,
//! on which lab, how it ended. Environment values and topologies are left
//! out: they may hold secrets.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use nlink_lab_shared::api::{ApiError, Request, Response};

use crate::authz::Peer;

pub(crate) struct Audit {
    path: PathBuf,
}

/// A call being made, recorded when it ends.
pub(crate) struct Entry {
    started: std::time::Instant,
    line: serde_json::Map<String, serde_json::Value>,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The record of `request` by `peer`, to finish with [`Audit::record`].
    pub fn entry(&self, peer: &Peer, request: &Request) -> Entry {
        let mut line = serde_json::Map::new();
        line.insert("ts".into(), nlink_lab::events::now_iso8601().into());
        line.insert("uid".into(), peer.uid.into());
        line.insert("user".into(), peer.user.clone().into());
        if let Some(pid) = peer.pid {
            line.insert("pid".into(), pid.into());
        }
        line.insert("method".into(), request.method().into());
        if let Some(lab) = request.lab() {
            line.insert("lab".into(), lab.into());
        }
        if let Some(detail) = detail(request) {
            line.insert("detail".into(), detail);
        }
        Entry {
            started: std::time::Instant::now(),
            line,
        }
    }

    /// Append the entry with how the call ended. Best effort: a failure
    /// is logged, the call is not failed for it.
    pub fn record(&self, mut entry: Entry, result: &Result<Response, ApiError>) {
        entry.line.insert(
            "duration_ms".into(),
            (entry.started.elapsed().as_millis() as u64).into(),
        );
        entry.line.insert("ok".into(), result.is_ok().into());
        match result {
            Ok(Response::Exited { code, .. }) => {
                entry.line.insert("exit_code".into(), (*code).into());
            }
            Err(e) => {
                entry.line.insert("error".into(), e.message.clone().into());
            }
            Ok(_) => {}
        }
        let mut text = serde_json::Value::Object(entry.line).to_string();
        text.push('\n');
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o640)
            .open(&self.path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = appended {
            tracing::warn!("audit log {}: {e}", self.path.display());
        }
    }
}

/// What the call was about, beyond its method and lab: never a secret.
fn detail(request: &Request) -> Option<serde_json::Value> {
    match request {
        Request::Exec {
            node,
            argv,
            env,
            stdin,
            tty,
            ..
        } => Some(serde_json::json!({
            "node": node,
            "argv": argv,
            "env": env.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            "stdin": stdin,
            "tty": tty,
        })),
        Request::GetTopology { redact, .. } => Some(serde_json::json!({ "redact": redact })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_records_the_call_but_not_its_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Audit::new(dir.path().join("audit.ndjson"));
        let peer = Peer {
            uid: 1000,
            gid: 1000,
            pid: Some(42),
            user: "alice".into(),
            groups: vec![],
        };
        let request = Request::Exec {
            lab: "wan".into(),
            node: "r1".into(),
            argv: vec!["ip".into(), "addr".into()],
            env: vec![("TOKEN".into(), "s3cret".into())],
            workdir: None,
            stdin: false,
            tty: false,
            timeout_ms: None,
        };
        let entry = audit.entry(&peer, &request);
        audit.record(
            entry,
            &Ok(Response::Exited {
                code: Some(0),
                signal: None,
                duration_ms: 3,
                timed_out: false,
            }),
        );
        let text = std::fs::read_to_string(dir.path().join("audit.ndjson")).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["user"], "alice");
        assert_eq!(line["method"], "exec");
        assert_eq!(line["lab"], "wan");
        assert_eq!(line["detail"]["env"][0], "TOKEN");
        assert_eq!(line["exit_code"], 0);
        assert!(!text.contains("s3cret"));
    }
}
