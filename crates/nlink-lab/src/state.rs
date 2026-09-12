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

    /// Log file paths for spawned processes: pid → (stdout_path, stderr_path).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub process_logs: std::collections::BTreeMap<u32, (String, String)>,
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
            process_logs: Default::default(),
        }
    }
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
#[derive(Debug, Clone, serde::Serialize)]
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
    let file = open_lock_file(name)?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        return Err(Error::deploy_failed(format!(
            "lab '{name}' is locked by another process"
        )));
    }
    Ok(LabLock { _file: file })
}

/// Like [`lock`] but waits for the lock instead of failing. Used by
/// short read-modify-write updates of the state file (`save_state`)
/// that merely need to take turns with a concurrent deploy/apply/destroy.
pub fn lock_blocking(name: &str) -> Result<LabLock> {
    let file = open_lock_file(name)?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(Error::deploy_failed(format!(
            "failed to lock lab '{name}': {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(LabLock { _file: file })
}

fn open_lock_file(name: &str) -> Result<std::fs::File> {
    let dir = locks_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{name}.lock")))?)
}

/// Guard that holds a file lock on a lab's state directory.
/// The lock is released when this guard is dropped.
pub struct LabLock {
    _file: std::fs::File,
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
    Ok(LabLock { _file: file })
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
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        // Poisoning is fine — the panic that poisoned the lock
        // is already reported by the test runner; we just keep
        // going so the remaining tests run cleanly.
        m.lock().unwrap_or_else(|e| e.into_inner())
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
        let fresh = LabState::new("n", "t");
        assert_eq!(fresh.schema_version, SCHEMA_VERSION);
        let back: LabState = serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
    }
}
