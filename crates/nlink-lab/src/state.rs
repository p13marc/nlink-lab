//! State persistence for running labs.
//!
//! Tracks deployed labs in `$XDG_STATE_HOME/nlink-lab/labs/` (or `~/.local/state/nlink-lab/labs/`).
//! Each lab gets a directory with `state.json` and `topology.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::Topology;

/// Current on-disk schema of [`LabState`]. Bumped when a field's
/// meaning changes; additive fields are `#[serde(default)]` and do not
/// bump it. Files without the field are schema 1.
pub const SCHEMA_VERSION: u32 = 2;

/// Persisted state for a deployed lab.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LabState {
    /// On-disk schema version ([`SCHEMA_VERSION`]); absent in files
    /// written before 0.9 (schema 1).
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    /// Lab name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Map of node_name -> namespace_name.
    pub namespaces: std::collections::BTreeMap<String, String>,
    /// Background process PIDs: (node_name, pid).
    pub pids: Vec<(String, u32)>,
    /// `/proc/<pid>/stat` start time (clock ticks since boot) of every
    /// PID in `pids`, captured when it was spawned. A PID is only ever
    /// signalled when its current start time still matches — after the
    /// spawning CLI exits the child is reparented and reaped, and the
    /// number can be reused by an unrelated process (issue #30). PIDs
    /// recorded by schema-1 files have no entry and are never signalled.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub starttimes: std::collections::BTreeMap<u32, u64>,
    /// Root-namespace veth peer created for each node's `mgmt0` when the
    /// lab has a host-reachable management bridge: node name → peer
    /// interface name. Persisted so `destroy` deletes exactly what
    /// deploy created instead of recomputing names from a node index
    /// that shifts when nodes are added or removed (issue #32).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub mgmt_peers: std::collections::BTreeMap<String, String>,
    /// WireGuard public keys: node_name -> (wg_iface -> base64-encoded public key).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub wg_public_keys:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Container state: node_name -> container info.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub containers: std::collections::BTreeMap<String, ContainerState>,
    /// Container runtime binary used ("docker" or "podman").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// Whether DNS hosts entries were injected into /etc/hosts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dns_injected: bool,

    /// Whether mac80211_hwsim was loaded for this lab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wifi_loaded: bool,

    /// Saved impairments before partition (endpoint → Impairment).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub saved_impairments: std::collections::BTreeMap<String, crate::types::Impairment>,

    /// Impairments set at runtime with `nlink-lab impair` (endpoint →
    /// Impairment). They override the topology's `impair` for `apply`,
    /// `verify` and snapshots until the topology changes that endpoint
    /// or `apply --reset-impairments` drops them (#59).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub live_impairments: std::collections::BTreeMap<String, crate::types::Impairment>,

    /// Log file paths for spawned processes: pid → (stdout_path, stderr_path).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub process_logs: std::collections::BTreeMap<u32, (String, String)>,
    /// Background `exec` blocks → pid, keyed `"<node>:<index>"`, so
    /// `apply` can stop exactly the process an edited or removed `exec`
    /// line started (#84). Absent in files written before 0.9: those
    /// processes are never signalled by index (only by node removal).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub exec_pids: std::collections::BTreeMap<String, u32>,
}

fn schema_v1() -> u32 {
    1
}

impl LabState {
    /// Fresh state at the current schema version. Every field other than
    /// `name` starts empty/false; the deployer fills them in.
    pub fn new(name: impl Into<String>, created_at: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: name.into(),
            created_at: created_at.into(),
            namespaces: Default::default(),
            pids: Vec::new(),
            starttimes: Default::default(),
            mgmt_peers: Default::default(),
            wg_public_keys: Default::default(),
            containers: Default::default(),
            runtime: None,
            dns_injected: false,
            wifi_loaded: false,
            saved_impairments: Default::default(),
            live_impairments: Default::default(),
            process_logs: Default::default(),
            exec_pids: Default::default(),
        }
    }
}

/// Process-wide lock serializing every test that mutates `XDG_STATE_HOME`
/// (this module's tests and `events::tests`). Poisoning is fine — the
/// panic that poisoned the lock is already reported by the test runner.
#[cfg(test)]
pub(crate) fn xdg_state_lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let m = LOCK.get_or_init(|| Mutex::new(()));
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ─── Snapshots (#59) ────────────────────────────────────────

/// Directory holding a lab's snapshots: `<state_dir>/snapshots/<name>/`.
pub fn snapshots_dir(lab: &str) -> PathBuf {
    state_dir(lab).join("snapshots")
}

/// A checkpoint of everything nlink-lab manages for a lab: the topology,
/// the runtime bookkeeping (`state.json`, including live impairments and
/// partitions) and a little metadata.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub meta: SnapshotInfo,
    pub topology: Topology,
    pub state: LabState,
}

/// Listing entry for `nlink-lab snapshot <lab> --list`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnapshotInfo {
    /// Snapshot name (unique per lab).
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Free-text description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Nodes in the snapshot's topology.
    #[serde(default)]
    pub node_count: usize,
    /// Links in the snapshot's topology.
    #[serde(default)]
    pub link_count: usize,
    /// Runtime impairments (`nlink-lab impair`) captured.
    #[serde(default)]
    pub live_impairments: usize,
    /// Partitioned endpoints captured.
    #[serde(default)]
    pub partitions: usize,
}

/// Snapshot names are path components: letters, digits, `-`, `_`, `.`
/// (not leading), at most 64 bytes.
pub fn validate_snapshot_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(Error::invalid_topology(format!(
            "invalid snapshot name '{name}': use letters, digits, '-', '_' or '.' (max 64, not starting with '.')"
        )));
    }
    Ok(())
}

/// Write a snapshot; an existing one with the same name is replaced.
pub fn snapshot_save(
    lab: &str,
    name: &str,
    description: Option<&str>,
    state: &LabState,
    topology: &Topology,
) -> Result<SnapshotInfo> {
    validate_snapshot_name(name)?;
    let dir = snapshots_dir(lab).join(name);
    std::fs::create_dir_all(&dir)?;
    let meta = SnapshotInfo {
        name: name.to_string(),
        created_at: crate::deploy::now_iso8601(),
        description: description.map(str::to_string),
        node_count: topology.nodes.len(),
        link_count: topology.links.len(),
        live_impairments: state.live_impairments.len(),
        partitions: state.saved_impairments.len(),
    };
    atomic_write(
        &dir.join("state.json"),
        &serde_json::to_string_pretty(state)?,
    )?;
    let topo_toml = toml::to_string_pretty(topology).map_err(|e| Error::State {
        op: "write",
        detail: format!("failed to serialize topology: {e}"),
        path: dir.join("topology.toml"),
    })?;
    atomic_write(&dir.join("topology.toml"), &topo_toml)?;
    atomic_write(
        &dir.join("meta.json"),
        &serde_json::to_string_pretty(&meta)?,
    )?;
    crate::events::record(
        lab,
        crate::events::LifecycleKind::SnapshotTaken {
            name: name.to_string(),
        },
    );
    Ok(meta)
}

/// Load one snapshot.
pub fn snapshot_load(lab: &str, name: &str) -> Result<Snapshot> {
    validate_snapshot_name(name)?;
    let dir = snapshots_dir(lab).join(name);
    if !dir.join("meta.json").exists() {
        return Err(Error::NotFound {
            name: format!("snapshot '{name}' of lab '{lab}'"),
        });
    }
    let read = |file: &str| -> Result<String> {
        std::fs::read_to_string(dir.join(file)).map_err(|e| Error::State {
            op: "read",
            detail: e.to_string(),
            path: dir.join(file),
        })
    };
    let meta: SnapshotInfo =
        serde_json::from_str(&read("meta.json")?).map_err(|e| Error::State {
            op: "parse",
            detail: format!("failed to parse snapshot metadata: {e}"),
            path: dir.join("meta.json"),
        })?;
    let state: LabState = serde_json::from_str(&read("state.json")?).map_err(|e| Error::State {
        op: "parse",
        detail: format!("failed to parse snapshot state: {e}"),
        path: dir.join("state.json"),
    })?;
    let topology: Topology = toml::from_str(&read("topology.toml")?).map_err(|e| Error::State {
        op: "parse",
        detail: format!("failed to parse snapshot topology: {e}"),
        path: dir.join("topology.toml"),
    })?;
    Ok(Snapshot {
        meta,
        topology,
        state,
    })
}

/// All snapshots of a lab, newest first.
pub fn snapshot_list(lab: &str) -> Result<Vec<SnapshotInfo>> {
    let dir = snapshots_dir(lab);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let meta_path = entry.path().join("meta.json");
        let Ok(text) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        if let Ok(meta) = serde_json::from_str::<SnapshotInfo>(&text) {
            out.push(meta);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Delete one snapshot.
pub fn snapshot_remove(lab: &str, name: &str) -> Result<()> {
    validate_snapshot_name(name)?;
    let dir = snapshots_dir(lab).join(name);
    if !dir.exists() {
        return Err(Error::NotFound {
            name: format!("snapshot '{name}' of lab '{lab}'"),
        });
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Get the logs directory for a specific lab.
pub fn logs_dir(name: &str) -> PathBuf {
    state_dir(name).join("logs")
}

/// Persisted state for a container node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    /// Container ID.
    pub id: String,
    /// Container name.
    pub name: String,
    /// Container image.
    pub image: String,
    /// Current init PID (captured at deploy, refreshed by `restart`).
    pub pid: u32,
}

/// Summary info about a running lab (for status listing).
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct LabInfo {
    /// Lab name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Get the base state directory.
///
/// `$XDG_STATE_HOME/nlink-lab/labs`, else `$HOME/.local/state/nlink-lab/labs`,
/// else — no `HOME` at all, e.g. a systemd unit — `/var/lib/nlink-lab/labs`
/// for root and a per-uid directory under the system temp dir otherwise.
/// It is never the world-writable, predictable `/tmp/nlink-lab` a root
/// process used to fall back to (issue #38).
fn base_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(state_home).join("nlink-lab").join("labs")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("nlink-lab")
            .join("labs")
    } else if unsafe { libc::geteuid() } == 0 {
        PathBuf::from("/var/lib/nlink-lab/labs")
    } else {
        std::env::temp_dir()
            .join(format!("nlink-lab-{}", unsafe { libc::getuid() }))
            .join("labs")
    }
}

/// Directory holding per-lab lock files. Kept *outside* the lab's own
/// state directory so `remove()` can never unlink a lock another process
/// is holding (which used to let a concurrent `lock()` succeed against a
/// fresh inode mid-destroy).
///
/// A lock file can still be deleted deliberately — `destroy` does, and
/// [`sweep_locks`] cleans up after labs that are already gone (issue
/// #103) — but only by whoever holds it, and `acquire` revalidates the
/// inode it locked so a waiter can never inherit a detached one. That is
/// the same hazard this directory placement avoids, handled explicitly
/// rather than by never unlinking.
fn locks_dir() -> PathBuf {
    base_dir().join(".locks")
}

/// Get the state directory for a specific lab.
pub fn state_dir(name: &str) -> PathBuf {
    base_dir().join(name)
}

/// Check if state exists for a lab.
pub fn exists(name: &str) -> bool {
    state_dir(name).join("state.json").exists()
}

/// Acquire an exclusive lock on a lab's state directory.
///
/// Returns a [`LabLock`] guard that holds the lock until dropped.
/// Fails immediately if another process holds the lock.
pub fn lock(name: &str) -> Result<LabLock> {
    acquire(name, false)
}

/// Like [`lock`] but waits for the lock instead of failing. Used by
/// short read-modify-write updates of the state file (`save_state`)
/// that merely need to take turns with a concurrent deploy/apply/destroy.
pub fn lock_blocking(name: &str) -> Result<LabLock> {
    acquire(name, true)
}

/// How many times [`acquire`] re-opens before giving up. A retry only
/// happens when the lock file was removed or replaced between our `open`
/// and our `flock`, so more than one is already unusual.
const ACQUIRE_ATTEMPTS: u32 = 8;

/// `flock` a lab's lock file, then **prove it is still the file at that
/// path**.
///
/// The revalidation is what makes removing a lock file safe (issue #103).
/// `flock` is a property of an inode, not a path: if a lock file is
/// unlinked while a second process sits between its `open` and its
/// `flock`, that process would lock the now-detached inode and a third
/// process would create and lock a *fresh* one — two holders, no mutual
/// exclusion. Comparing the locked file's identity against the path
/// closes that window: a stale inode is detected and the attempt is
/// retried against whatever is at the path now.
fn acquire(name: &str, blocking: bool) -> Result<LabLock> {
    let path = lock_path(name);
    let op = if blocking {
        libc::LOCK_EX
    } else {
        libc::LOCK_EX | libc::LOCK_NB
    };
    for _ in 0..ACQUIRE_ATTEMPTS {
        let file = open_lock_file(name)?;
        let ret = unsafe { libc::flock(file.as_raw_fd(), op) };
        if ret != 0 {
            if blocking {
                return Err(Error::deploy_failed(format!(
                    "failed to lock lab '{name}': {}",
                    std::io::Error::last_os_error()
                )));
            }
            return Err(Error::deploy_failed(format!(
                "lab '{name}' is locked by another process"
            )));
        }
        if same_file(&file, &path) {
            return Ok(LabLock { _file: file, path });
        }
        // The file we locked is no longer the one at this path — it was
        // removed (a `destroy`) or replaced while we were acquiring. Drop
        // it and lock whatever is there now.
        drop(file);
    }
    Err(Error::deploy_failed(format!(
        "lab '{name}': lock file kept changing under us ({ACQUIRE_ATTEMPTS} attempts)"
    )))
}

/// Is `file` the same inode as whatever `path` names right now?
fn same_file(file: &std::fs::File, path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(held), Ok(current)) = (file.metadata(), std::fs::metadata(path)) else {
        // The path is gone: our inode is certainly detached.
        return false;
    };
    held.dev() == current.dev() && held.ino() == current.ino()
}

fn lock_path(name: &str) -> PathBuf {
    locks_dir().join(format!("{name}.lock"))
}

fn open_lock_file(name: &str) -> Result<std::fs::File> {
    let dir = locks_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(name))?)
}

/// Guard that holds a file lock on a lab's state directory.
/// The lock is released when this guard is dropped.
pub struct LabLock {
    /// Held, never read: dropping it is what releases the `flock`.
    _file: std::fs::File,
    path: PathBuf,
}

impl LabLock {
    /// Delete the lock file, then release the lock.
    ///
    /// Only correct because the holder is the one deleting it and
    /// `acquire` revalidates: a process already blocked on this inode
    /// wakes up, sees that the path no longer resolves to it, and retries
    /// against the new file. Called by `destroy`, so a lab's lock file
    /// does not outlive the lab (issue #103).
    pub fn remove_file(self) {
        if let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!("could not remove lock file {}: {e}", self.path.display());
        }
        // `self` (and with it the flock) is released here, after the
        // unlink — never before, or a waiter could take the dead inode
        // and believe it holds the lock.
        drop(self);
    }
}

use std::os::unix::io::AsRawFd;

/// Acquire a *blocking* exclusive lock on a global sentinel that
/// serialises any operation touching globally-shared host state —
/// today, just `/etc/hosts` mutations from `dns::inject_hosts` and
/// friends. Unlike [`lock`] (per-lab, non-blocking, fails fast), this
/// blocks until the lock is available, since concurrent deploys
/// merely need to take turns rather than fail.
///
/// Held across the full read-modify-write of `/etc/hosts` so two
/// parallel deploys can't lose each other's managed sections.
/// Released when the returned guard is dropped. (Plan 157 PR D —
/// round-5 §1.2 prime suspect.)
pub fn hosts_lock() -> Result<LabLock> {
    let lock_path = base_dir().join(".hosts.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&lock_path)?;
    // Blocking lock — concurrent deploys serialise here.
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(Error::deploy_failed(format!(
            "failed to acquire /etc/hosts lock: {}",
            std::io::Error::last_os_error()
        )));
    }
    // This sentinel is never removed (there is exactly one of it, not one
    // per lab), so it needs no revalidation.
    Ok(LabLock {
        _file: file,
        path: lock_path,
    })
}

/// Save lab state and topology.
pub fn save(state: &LabState, topology: &Topology) -> Result<()> {
    let dir = state_dir(&state.name);
    std::fs::create_dir_all(&dir)?;

    // Atomic write: write to temp file then rename to prevent corruption on crash
    let state_json = serde_json::to_string_pretty(state)?;
    atomic_write(&dir.join("state.json"), &state_json)?;

    let topo_toml = toml::to_string_pretty(topology).map_err(|e| Error::State {
        op: "write",
        detail: format!("failed to serialize topology: {e}"),
        path: dir.join("topology.toml"),
    })?;
    atomic_write(&dir.join("topology.toml"), &topo_toml)?;

    Ok(())
}

/// Write content to a file atomically: temp file, fsync, rename, then
/// directory fsync. A crash leaves either the old or the new file, never
/// a torn one, and the rename is durable rather than merely visible.
fn atomic_write(path: &std::path::Path, content: &str) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        // Best effort: some filesystems refuse fsync on directories.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Load lab state and topology.
pub fn load(name: &str) -> Result<(LabState, Topology)> {
    let dir = state_dir(name);

    let state_path = dir.join("state.json");
    if !state_path.exists() {
        return Err(Error::NotFound {
            name: name.to_string(),
        });
    }

    let state_json = std::fs::read_to_string(&state_path)?;
    let state: LabState = serde_json::from_str(&state_json).map_err(|e| Error::State {
        op: "parse",
        detail: format!("failed to parse state: {e}"),
        path: state_path,
    })?;

    let topo_path = dir.join("topology.toml");
    let topo_toml = std::fs::read_to_string(&topo_path)?;
    let topology: Topology = toml::from_str(&topo_toml).map_err(|e| crate::Error::State {
        op: "parse",
        detail: format!("failed to parse topology state: {e}"),
        path: topo_path.clone(),
    })?;

    Ok((state, topology))
}

/// List all saved labs.
pub fn list() -> Result<Vec<LabInfo>> {
    let base = base_dir();
    if !base.exists() {
        return Ok(Vec::new());
    }

    let mut labs = Vec::new();
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let state_path = entry.path().join("state.json");
            if state_path.exists()
                && let Ok(json) = std::fs::read_to_string(&state_path)
                && let Ok(state) = serde_json::from_str::<LabState>(&json)
            {
                labs.push(LabInfo {
                    name: name.clone(),
                    node_count: state.namespaces.len(),
                    created_at: state.created_at.clone(),
                });
            }
        }
    }

    labs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(labs)
}

/// Labs whose state directory holds a `journal.json` — an undo journal
/// left by a deploy/apply that was interrupted before it could finish
/// or roll back. `deploy` unwinds its own lab's journal; `destroy
/// --orphans` unwinds all of them.
pub fn labs_with_pending_journal() -> Vec<String> {
    let base = base_dir();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut labs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("journal.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    labs.sort();
    labs
}

/// Remove a lab's state directory.
pub fn remove(name: &str) -> Result<()> {
    let dir = state_dir(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Lock files in `.locks/` whose lab has no state directory.
///
/// Every lab name that has ever been locked leaves a file behind, and
/// before issue #103 nothing removed them: a machine that runs the test
/// suite accumulates thousands (each test lab is uniquely named). `destroy`
/// now removes its own, and this finds the ones left by everything that
/// came before — or by a lab whose state directory was deleted by hand.
pub fn stale_locks() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(locks_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.strip_suffix(".lock")?;
            // "No state.json" and not merely "no directory": a crashed
            // deploy can leave an empty directory behind, and its lock is
            // stale too. This is safe to be liberal about because nothing
            // is ever removed without acquiring it first — an in-flight
            // deploy, which takes the lock *before* writing state.json,
            // holds it and is skipped.
            (!exists(name)).then(|| path.clone())
        })
        .collect()
}

/// Remove one lab's lock file, but only if nobody holds it. Returns
/// whether it went away.
///
/// Acquiring it first is the proof that no other process is mid-deploy on
/// that name, and the unlink happens while *we* hold it, so a waiter
/// revalidates instead of taking a detached inode (see `acquire`).
pub fn remove_lock_if_unheld(name: &str) -> bool {
    match lock(name) {
        Ok(held) => {
            held.remove_file();
            true
        }
        // Held by someone else: leave it, it is doing its job.
        Err(_) => false,
    }
}

/// Remove the lock files [`stale_locks`] found, skipping any that are
/// currently held. Returns how many went away.
pub fn sweep_locks() -> usize {
    stale_locks()
        .iter()
        .filter_map(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".lock"))
        })
        .filter(|name| remove_lock_if_unheld(name))
        .count()
}

/// Load only the `namespaces` map from a lab's state.json.
///
/// Cheaper than [`load`] (no topology parse) — used by `status --scan` to
/// cross-check that resources the state claims exist are still present on
/// the host.
pub fn load_namespace_names(name: &str) -> Result<Vec<String>> {
    let state_path = state_dir(name).join("state.json");
    if !state_path.exists() {
        return Err(Error::NotFound {
            name: name.to_string(),
        });
    }
    let state_json = std::fs::read_to_string(&state_path)?;
    let state: LabState = serde_json::from_str(&state_json).map_err(|e| Error::State {
        op: "parse",
        detail: format!("failed to parse state: {e}"),
        path: state_path,
    })?;
    Ok(state.namespaces.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Process-wide lock serializing the tests that mutate
    /// `XDG_STATE_HOME`. Without this, `cargo test`'s default
    /// multi-thread runner would let two parallel tests overwrite
    /// the env var mid-run — tests A and B both set their own
    /// tempdir; B's setter wins; A's `save("test-lab")` then
    /// writes to B's tempdir; A's `assert!(exists("test-lab"))`
    /// reads back through B's env value, but B may have already
    /// dropped its `TempDir` and torn down the directory. Net
    /// result: random `assertion failed: exists("test-lab")`
    /// failures on busy runners. The mutex also doubles as a
    /// safety guard around the `unsafe std::env::set_var` —
    /// modern Rust requires no other thread to be reading the
    /// env while we mutate it, and serializing the
    /// state-test set is the cleanest way to honor that.
    fn xdg_state_lock() -> std::sync::MutexGuard<'static, ()> {
        super::xdg_state_lock_for_tests()
    }

    /// `_dir` keeps the tempdir alive; the returned guard keeps
    /// the process-wide env-var-mutating critical section closed
    /// for the test's lifetime. Both have to outlive every
    /// `save`/`load`/`exists`/`remove` call the test makes.
    struct StateTestEnv {
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    fn temp_state_env() -> StateTestEnv {
        let guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: `xdg_state_lock` above serializes every test in
        // this module that touches `XDG_STATE_HOME`. Only one
        // such test can be inside this critical section at a
        // time, so no other thread is concurrently reading the
        // env var.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        StateTestEnv {
            _dir: dir,
            _guard: guard,
        }
    }

    #[test]
    fn test_save_load_roundtrip() {
        let _dir = temp_state_env();

        let mut namespaces = BTreeMap::new();
        namespaces.insert("r1".to_string(), "lab-r1".to_string());
        namespaces.insert("h1".to_string(), "lab-h1".to_string());

        let mut state = LabState::new("test-lab", "2026-03-22T14:00:00Z");
        state.namespaces = namespaces;
        state.pids = vec![("r1".to_string(), 1234)];

        let topology = crate::parser::parse(
            r#"lab "test-lab"
node r1
node h1
link r1:eth0 -- h1:eth0
"#,
        )
        .unwrap();

        save(&state, &topology).unwrap();
        assert!(exists("test-lab"));

        let (loaded_state, loaded_topo) = load("test-lab").unwrap();
        assert_eq!(loaded_state.name, "test-lab");
        assert_eq!(loaded_state.namespaces.len(), 2);
        assert_eq!(loaded_state.pids.len(), 1);
        assert_eq!(loaded_topo.lab.name, "test-lab");
        assert_eq!(loaded_topo.nodes.len(), 2);

        remove("test-lab").unwrap();
        assert!(!exists("test-lab"));
    }

    #[test]
    fn test_load_not_found() {
        let _dir = temp_state_env();
        assert!(load("nonexistent").is_err());
    }

    #[test]
    fn test_list_empty() {
        let _dir = temp_state_env();
        let labs = list().unwrap();
        assert!(labs.is_empty());
    }

    #[test]
    fn lock_lives_outside_the_lab_dir_and_survives_remove() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        let held = lock("locked-lab").expect("first lock");
        assert!(
            lock("locked-lab").is_err(),
            "second lock must fail while held"
        );
        // `remove` must not free the lock by deleting its file.
        remove("locked-lab").unwrap();
        assert!(
            lock("locked-lab").is_err(),
            "lock still held after remove()"
        );
        assert!(!state_dir("locked-lab").join(".lock").exists());
        drop(held);
        assert!(lock("locked-lab").is_ok());
        // A blocking lock acquires once the other is released.
        assert!(lock_blocking("locked-lab").is_ok());
    }

    /// The whole point of `.locks/` living outside the lab directory: a
    /// lab's lock must outlive `remove()`. Extended for #103 — the lock
    /// file *is* removable now, but only by the holder.
    #[test]
    fn destroy_style_removal_frees_the_lock_and_deletes_its_file() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        let held = lock("doomed").expect("first lock");
        let path = lock_path("doomed");
        assert!(path.exists());
        assert!(lock("doomed").is_err(), "still held");

        // What `RunningLab::destroy` does: unlink while holding, then release.
        held.remove_file();
        assert!(!path.exists(), "the lock file goes with the lab");
        // …and the name is immediately lockable again, against a fresh file.
        let again = lock("doomed").expect("lockable after removal");
        assert!(path.exists(), "a fresh lock file was created");
        assert!(lock("doomed").is_err(), "and it excludes properly");
        drop(again);
    }

    #[test]
    fn stale_locks_finds_only_locks_without_a_lab() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        // A live lab: state.json present.
        drop(lock("live"));
        let live_dir = state_dir("live");
        std::fs::create_dir_all(&live_dir).unwrap();
        std::fs::write(live_dir.join("state.json"), "{}").unwrap();
        // A lab whose directory exists but never got a state file — a
        // crashed deploy. Its lock is stale too.
        drop(lock("crashed"));
        std::fs::create_dir_all(state_dir("crashed")).unwrap();
        // And one with nothing left at all.
        drop(lock("gone"));

        let stale: Vec<String> = stale_locks()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(stale.contains(&"gone.lock".to_string()), "{stale:?}");
        assert!(stale.contains(&"crashed.lock".to_string()), "{stale:?}");
        assert!(!stale.contains(&"live.lock".to_string()), "{stale:?}");
    }

    #[test]
    fn sweep_removes_stale_locks_but_never_a_held_one() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        drop(lock("stale-a"));
        drop(lock("stale-b"));
        // Held by "another process": the sweep must leave it alone, since
        // acquiring it is the proof that nobody is mid-deploy.
        let held = lock("busy").expect("hold one");

        assert_eq!(sweep_locks(), 2, "both unheld ones");
        assert!(!lock_path("stale-a").exists());
        assert!(!lock_path("stale-b").exists());
        assert!(lock_path("busy").exists(), "a held lock is never swept");
        drop(held);
        assert_eq!(sweep_locks(), 1, "and is swept once released");
    }

    /// The protocol that makes removal safe. Without the inode
    /// revalidation in `acquire`, the waiter below would wake up owning a
    /// lock on a deleted file while a *third* caller locks a fresh one at
    /// the same path — two holders, no mutual exclusion.
    #[test]
    fn a_waiter_revalidates_instead_of_inheriting_a_deleted_lock() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        let held = lock("raced").expect("first lock");
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            // Blocks on the inode that is about to be unlinked.
            let got = lock_blocking("raced").expect("acquires eventually");
            tx.send(()).unwrap();
            // Hold it until the test says so.
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(got);
        });
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Destroy: unlink while holding, then release.
        held.remove_file();
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the waiter must acquire after the lock file is removed");

        // The waiter holds a lock on whatever is at the path *now*, so a
        // fresh attempt must be excluded. If it revalidated wrongly, the
        // waiter would be holding a detached inode and this would succeed.
        assert!(
            lock("raced").is_err(),
            "the waiter's lock must still exclude a new caller"
        );
        waiter.join().unwrap();
        assert!(lock("raced").is_ok(), "lockable again once released");
    }

    #[test]
    fn schema_v1_files_load_with_defaults() {
        let json = r#"{
            "name": "old", "created_at": "2026-01-01T00:00:00Z",
            "namespaces": {"r1": "old-r1"}, "pids": [["r1", 42]]
        }"#;
        let st: LabState = serde_json::from_str(json).unwrap();
        assert_eq!(st.schema_version, 1);
        assert!(st.starttimes.is_empty());
        assert!(st.mgmt_peers.is_empty());
        assert!(st.exec_pids.is_empty());
        let fresh = LabState::new("n", "t");
        assert_eq!(fresh.schema_version, SCHEMA_VERSION);
        let back: LabState = serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
    }
    #[test]
    fn snapshots_save_list_load_remove() {
        let _env = temp_state_env();
        let topo = crate::Lab::new("snap-lab")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
            .build();
        let mut st = LabState::new("snap-lab".to_string(), "2026-09-13T00:00:00Z".to_string());
        st.live_impairments.insert(
            "a:eth0".into(),
            crate::types::Impairment {
                delay: Some("5ms".into()),
                ..Default::default()
            },
        );
        st.saved_impairments
            .insert("b:eth0".into(), crate::types::Impairment::default());
        save(&st, &topo).unwrap();

        let meta = snapshot_save("snap-lab", "before", Some("baseline"), &st, &topo).unwrap();
        assert_eq!(meta.node_count, 2);
        assert_eq!(meta.link_count, 1);
        assert_eq!(meta.live_impairments, 1);
        assert_eq!(meta.partitions, 1);
        snapshot_save("snap-lab", "after", None, &st, &topo).unwrap();

        let list = snapshot_list("snap-lab").unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|s| s.name == "before" && s.description.as_deref() == Some("baseline"))
        );

        let snap = snapshot_load("snap-lab", "before").unwrap();
        assert_eq!(snap.topology.nodes.len(), 2);
        assert_eq!(
            snap.state.live_impairments["a:eth0"].delay.as_deref(),
            Some("5ms")
        );
        assert!(snap.state.saved_impairments.contains_key("b:eth0"));

        snapshot_remove("snap-lab", "before").unwrap();
        assert_eq!(snapshot_list("snap-lab").unwrap().len(), 1);
        assert!(matches!(
            snapshot_load("snap-lab", "before"),
            Err(Error::NotFound { .. })
        ));
        assert!(snapshot_remove("snap-lab", "before").is_err());
        assert!(snapshot_list("no-such-lab").unwrap().is_empty());
    }

    #[test]
    fn snapshot_names_are_validated() {
        for bad in ["", ".hidden", "a/b", "x y", "é", &"n".repeat(65)] {
            assert!(
                validate_snapshot_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        for ok in ["before", "v1.2", "run_3-final", "A"] {
            validate_snapshot_name(ok).unwrap();
        }
    }

    #[test]
    fn live_impairments_survive_state_roundtrip() {
        let _env = temp_state_env();
        let topo = crate::Lab::new("live-lab").node("a", |n| n).build();
        let mut st = LabState::new("live-lab".to_string(), "t".to_string());
        st.live_impairments.insert(
            "a:eth0".into(),
            crate::types::Impairment {
                loss: Some("1%".into()),
                ..Default::default()
            },
        );
        save(&st, &topo).unwrap();
        let (back, _) = load("live-lab").unwrap();
        assert_eq!(back.live_impairments["a:eth0"].loss.as_deref(), Some("1%"));
        // Old files without the field still load.
        let json = std::fs::read_to_string(state_dir("live-lab").join("state.json")).unwrap();
        assert!(json.contains("live_impairments"));
        let stripped: LabState = serde_json::from_str(
            &json.replace("\"live_impairments\"", "\"unknown_field_ignored\""),
        )
        .unwrap_or_else(|_| LabState::new("live-lab".to_string(), "t".to_string()));
        assert!(stripped.live_impairments.is_empty());
    }
}
