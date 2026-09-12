//! Undo journal for deploy / apply.
//!
//! Every kernel or host mutation `apply.rs` performs records its
//! inverse here *after* it succeeded. On any error the journal is
//! unwound newest-first; on success it is discarded. The journal is
//! also persisted next to the lab's state (`journal.json`) as it
//! grows, so a deploy killed by SIGKILL leaves a record the next
//! `deploy`/`destroy --orphans` can unwind instead of orphans.

use std::path::PathBuf;

use nlink::netlink::namespace;
use nlink::{Connection, Route};
use serde::{Deserialize, Serialize};

use super::NsRef;

/// One reversible mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Undo {
    FreeSubnets {
        lab: String,
    },
    DeleteNamespace {
        ns: String,
    },
    RemoveContainer {
        binary: String,
        id: String,
    },
    ReleaseHwsim {
        lab: String,
    },
    /// A link in the root namespace (mgmt bridge/peers, a macvlan not
    /// yet moved) — deleted through netlink.
    DeleteHostLink {
        name: String,
    },
    /// A link inside a node namespace (a veth end takes its peer along).
    DeleteLink {
        ns: NsRef,
        iface: String,
    },
    ClearQdisc {
        ns: NsRef,
        iface: String,
    },
    RemoveHosts {
        lab: String,
    },
    RemoveNetnsEtc {
        ns: String,
    },
    /// Identity-checked kill (see `running::kill_tracked`).
    KillProcess {
        pid: u32,
        starttime: Option<u64>,
    },
    RemoveDir {
        path: PathBuf,
    },
    CleanupWifiConfigs {
        lab: String,
    },
}

/// The journal: `Undo` entries in the order their ops succeeded.
#[derive(Debug)]
pub struct Journal {
    lab: String,
    path: Option<PathBuf>,
    entries: Vec<Undo>,
    armed: bool,
}

impl Journal {
    /// Journal for `lab`, persisted under its state directory.
    pub fn new(lab: &str) -> Self {
        Self {
            lab: lab.to_string(),
            path: Some(crate::state::state_dir(lab).join("journal.json")),
            entries: Vec::new(),
            armed: true,
        }
    }

    /// In-memory journal (tests, dry runs).
    pub fn ephemeral(lab: &str) -> Self {
        Self {
            lab: lab.to_string(),
            path: None,
            entries: Vec::new(),
            armed: true,
        }
    }

    /// Path of a persisted journal left by an interrupted run, if any.
    pub fn pending_path(lab: &str) -> Option<PathBuf> {
        let p = crate::state::state_dir(lab).join("journal.json");
        p.is_file().then_some(p)
    }

    /// Load a journal left behind by an interrupted deploy.
    pub fn load_pending(lab: &str) -> Option<Journal> {
        let path = Self::pending_path(lab)?;
        let text = std::fs::read_to_string(&path).ok()?;
        let entries: Vec<Undo> = serde_json::from_str(&text).ok()?;
        Some(Journal {
            lab: lab.to_string(),
            path: Some(path),
            entries,
            armed: true,
        })
    }

    pub fn lab(&self) -> &str {
        &self.lab
    }

    pub fn entries(&self) -> &[Undo] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record the inverse of a mutation that just succeeded.
    pub fn record(&mut self, undo: Undo) {
        self.entries.push(undo);
        self.persist();
    }

    fn persist(&self) {
        if let Some(path) = &self.path
            && let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
            && let Ok(json) = serde_json::to_string(&self.entries)
        {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    /// The run succeeded: forget the journal (and its file).
    pub fn discard(&mut self) {
        self.armed = false;
        self.entries.clear();
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Unwind every entry, newest first. Failures are logged, never
    /// fatal — the point is to remove as much as possible.
    pub async fn unwind(&mut self) {
        if !self.armed {
            return;
        }
        let root: Option<Connection<Route>> = Connection::<Route>::new().ok();
        for undo in self.entries.iter().rev() {
            tracing::debug!(?undo, "rollback");
            match undo {
                Undo::FreeSubnets { lab } => {
                    let _ = crate::subnet_pool::free_for_lab(lab);
                }
                Undo::DeleteNamespace { ns } => {
                    crate::dns::remove_netns_etc(ns);
                    if namespace::exists(ns)
                        && let Err(e) = namespace::delete(ns)
                    {
                        tracing::warn!("rollback: delete namespace '{ns}': {e}");
                    }
                    crate::netns_tag::untag(ns);
                }
                Undo::RemoveContainer { binary, id } => {
                    let _ = std::process::Command::new(binary)
                        .args(["rm", "-f", id])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                Undo::ReleaseHwsim { lab } => {
                    crate::wifi::release_hwsim(lab);
                }
                Undo::DeleteHostLink { name } => {
                    if let Some(conn) = &root
                        && let Err(e) = conn.del_link_if_exists(name.as_str()).await
                    {
                        tracing::warn!("rollback: delete host link '{name}': {e}");
                    }
                }
                Undo::DeleteLink { ns, iface } => {
                    if let Ok(conn) = ns.connection::<Route>()
                        && let Err(e) = conn.del_link_if_exists(iface.as_str()).await
                    {
                        tracing::warn!("rollback: delete link '{iface}' in {ns:?}: {e}");
                    }
                }
                Undo::ClearQdisc { ns, iface } => {
                    if let Ok(conn) = ns.connection::<Route>() {
                        let _ = conn
                            .del_qdisc_if_exists(iface.as_str(), nlink::TcHandle::ROOT)
                            .await;
                    }
                }
                Undo::RemoveHosts { lab } => {
                    let _ = crate::dns::remove_hosts(lab);
                }
                Undo::RemoveNetnsEtc { ns } => crate::dns::remove_netns_etc(ns),
                Undo::KillProcess { pid, starttime } => {
                    let _ = crate::running::kill_tracked(*pid, *starttime);
                }
                Undo::RemoveDir { path } => {
                    let _ = std::fs::remove_dir_all(path);
                }
                Undo::CleanupWifiConfigs { lab } => crate::wifi::cleanup_configs(lab),
            }
        }
        self.discard();
    }
}

impl Drop for Journal {
    /// Last resort for panics: the synchronous subset (netlink deletes
    /// need the async connection and are left for `destroy --orphans`,
    /// which finds the persisted journal).
    fn drop(&mut self) {
        if !self.armed || self.entries.is_empty() {
            return;
        }
        tracing::warn!(
            "deploy of '{}' interrupted with {} journal entries; unwinding what can be done synchronously",
            self.lab,
            self.entries.len()
        );
        for undo in self.entries.iter().rev() {
            match undo {
                Undo::FreeSubnets { lab } => {
                    let _ = crate::subnet_pool::free_for_lab(lab);
                }
                Undo::DeleteNamespace { ns } => {
                    crate::dns::remove_netns_etc(ns);
                    if namespace::exists(ns) {
                        let _ = namespace::delete(ns);
                    }
                    crate::netns_tag::untag(ns);
                }
                Undo::RemoveContainer { binary, id } => {
                    let _ = std::process::Command::new(binary)
                        .args(["rm", "-f", id])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                Undo::ReleaseHwsim { lab } => {
                    let _ = crate::wifi::release_hwsim(lab);
                }
                Undo::RemoveHosts { lab } => {
                    let _ = crate::dns::remove_hosts(lab);
                }
                Undo::RemoveNetnsEtc { ns } => crate::dns::remove_netns_etc(ns),
                Undo::KillProcess { pid, starttime } => {
                    let _ = crate::running::kill_tracked(*pid, *starttime);
                }
                Undo::RemoveDir { path } => {
                    let _ = std::fs::remove_dir_all(path);
                }
                Undo::CleanupWifiConfigs { lab } => crate::wifi::cleanup_configs(lab),
                Undo::DeleteHostLink { .. } | Undo::DeleteLink { .. } | Undo::ClearQdisc { .. } => {
                }
            }
        }
        // The file stays so `destroy --orphans` can finish the netlink half.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_serde_roundtrip() {
        let entries = vec![
            Undo::DeleteNamespace { ns: "lab-a".into() },
            Undo::DeleteLink {
                ns: NsRef::Container {
                    id: "abc".into(),
                    pid: 42,
                },
                iface: "eth0".into(),
            },
            Undo::KillProcess {
                pid: 7,
                starttime: Some(99),
            },
        ];
        let json = serde_json::to_string(&entries).unwrap();
        let back: Vec<Undo> = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{entries:?}"), format!("{back:?}"));
    }

    #[test]
    fn ephemeral_journal_records_and_discards() {
        let mut j = Journal::ephemeral("t");
        j.record(Undo::FreeSubnets { lab: "t".into() });
        assert_eq!(j.entries().len(), 1);
        j.discard();
        assert!(j.is_empty());
    }
}
