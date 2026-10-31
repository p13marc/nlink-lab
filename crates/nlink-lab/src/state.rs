//! State persistence for running labs.
//!
//! Tracks deployed labs under one root for the whole host,
//! `/var/lib/nlink-lab` (or `$NLINK_LAB_STATE_DIR`): `labs/<lab>/` holds
//! `state.json` and `topology.toml`. See [`root_dir`].

use std::path::{Path, PathBuf};

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

    /// Whether the lab made Wi-Fi radios. Before 0.14 (#176): whether it
    /// loaded `mac80211_hwsim`.
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
    /// The tag of each tracked process on a container node, set as
    /// `NLINK_LAB_TAG` in its environment. The tracked PID is the
    /// `docker`/`podman exec` client's, which forwards no signal: the tag
    /// is what finds the process inside the container (#215).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub process_tags: std::collections::BTreeMap<u32, String>,
    /// What the lab was deployed from, so a later `apply` can default to
    /// the same file, parameters and suffix (#234). `None` for a lab
    /// deployed through the library or before 0.14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<LabSource>,
}

/// Where a lab came from: the topology file, its `--set` parameters and
/// the suffix `deploy --suffix`/`--unique` added to its name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LabSource {
    /// Absolute path of the topology file, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `--set KEY=VALUE` parameters, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<(String, String)>,
    /// The suffix added to the topology's lab name (and prefix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// What the containers' relative host paths are relative to, when not
    /// `path`'s directory: a deploy through the daemon stages the file
    /// under the lab's state, and they stay the client's (#285).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
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
            process_tags: Default::default(),
            source: None,
        }
    }
}

/// Process-wide lock serializing every test that mutates `NLINK_LAB_STATE_DIR`
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

/// Listing entry for `nlink-lab snapshot --list`.
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

/// A lab name is a path component of the state directory. Every entry
/// point that turns one into a path checks it: `""` named the `labs/`
/// directory itself, `..` its parent and an absolute name any directory,
/// so `destroy --force ""` deleted every lab's state (#221). The rule is
/// the validator's for a topology's lab name.
pub fn validate_lab_name(name: &str) -> Result<()> {
    if crate::validator::is_valid_name(name) {
        Ok(())
    } else {
        Err(Error::invalid_argument(format!(
            "invalid lab name '{name}': use letters, digits, '_', '.' or '-', starting with a letter, digit or '_'"
        )))
    }
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
    validate_lab_name(lab)?;
    validate_snapshot_name(name)?;
    let dir = snapshots_dir(lab).join(name);
    ensure_dir(&dir)?;
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
    validate_lab_name(lab)?;
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
    validate_lab_name(lab)?;
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
    validate_lab_name(lab)?;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerState {
    /// Container ID.
    pub id: String,
    /// Container name.
    pub name: String,
    /// Container image.
    pub image: String,
    /// Current init PID (captured at deploy, refreshed by `restart`).
    pub pid: u32,
    /// The init's start time (`/proc/<pid>/stat` field 22) when its PID
    /// was recorded. A PID with another start time is another process:
    /// the container's init exited and the number was reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starttime: Option<u64>,
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

/// The environment variable that moves the state root (an absolute path).
pub const STATE_DIR_ENV: &str = "NLINK_LAB_STATE_DIR";

/// The state root when [`STATE_DIR_ENV`] is not set.
pub const DEFAULT_ROOT: &str = "/var/lib/nlink-lab";

/// The group whose members may read the state root (#234).
pub const GROUP: &str = "nlink-lab";

/// nlink-lab's state root: `$NLINK_LAB_STATE_DIR` when it is an absolute
/// path, else `/var/lib/nlink-lab`. One root for the whole host, whoever
/// deploys (#234): it was the deploying user's `$XDG_STATE_HOME` (or
/// `~/.local/state`), so a lab deployed with sudo was invisible without
/// it, the orphan reaper could reap another identity's live lab, and two
/// identities could be handed the same subnet from their own pools.
pub fn root_dir() -> PathBuf {
    root_dir_from(std::env::var_os(STATE_DIR_ENV))
}

fn root_dir_from(env: Option<std::ffi::OsString>) -> PathBuf {
    env.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT))
}

/// The gid of the [`GROUP`] group, when the host has one.
pub fn group_gid() -> Option<u32> {
    let name = std::ffi::CString::new(GROUP).ok()?;
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut out: *mut libc::group = std::ptr::null_mut();
    // SAFETY: getgrnam_r writes into `grp` and `buf`, both owned here and
    // large enough for an ordinary group entry; `out` is null on miss.
    let rc = unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            &mut grp,
            buf.as_mut_ptr(),
            buf.len(),
            &mut out,
        )
    };
    (rc == 0 && !out.is_null()).then_some(grp.gr_gid)
}

/// Create the state root if it is missing: setgid `2750 root:nlink-lab`
/// when that group exists, so group members can read every lab and new
/// directories inherit the group; `0700` otherwise. An existing root is
/// left as the administrator made it.
fn ensure_root(root: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if root.is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|e| privileges(e, parent))?;
    }
    let gid = group_gid();
    let mode = if gid.is_some() { 0o2750 } else { 0o700 };
    match std::fs::DirBuilder::new().mode(mode).create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(privileges(e, root)),
    }
    // the umask may have taken bits off
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(mode))?;
    if let Some(gid) = gid {
        // best effort: a capability-installed binary may lack CAP_CHOWN
        let _ = std::os::unix::fs::chown(root, None, Some(gid));
    }
    Ok(())
}

/// Create the state root if it is missing (see [`root_dir`]), with its
/// modes and group, and return it.
pub fn ensure_state_root() -> Result<PathBuf> {
    let root = root_dir();
    ensure_root(&root)?;
    Ok(root)
}

/// Create a directory under the state root (and the root itself), setgid
/// `2750`, so it inherits the root's group.
pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    ensure_root(&root_dir())?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o2750)
        .create(dir)
        .map_err(|e| privileges(e, dir))
}

/// An I/O error on the state root, said as what to do about it when it
/// is a permission problem.
fn privileges(e: std::io::Error, path: &Path) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::PermissionDenied {
            path: path.to_path_buf(),
            hint: format!(
                "the state root needs root to write; reading it needs membership in the `{GROUP}` group"
            ),
        }
    } else {
        Error::Io(e)
    }
}

// ─── State from before 0.14 (#234) ──────────────────────────

/// The per-user state roots nlink-lab used before 0.14, where any exist on
/// this host: root's and every `/home/*` user's `~/.local/state/nlink-lab`,
/// the caller's `$XDG_STATE_HOME/nlink-lab`, and the per-uid temp-dir
/// fallback. Their labs are read only: the orphan reaper protects them,
/// and [`migrate_lab`] moves them into [`root_dir`].
pub fn legacy_roots() -> Vec<PathBuf> {
    let current = root_dir();
    let mut candidates = vec![PathBuf::from("/root/.local/state/nlink-lab")];
    if let Ok(homes) = std::fs::read_dir("/home") {
        candidates.extend(
            homes
                .flatten()
                .map(|h| h.path().join(".local").join("state").join("nlink-lab")),
        );
    }
    for var in ["XDG_STATE_HOME", "HOME"] {
        if let Some(dir) = std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            candidates.push(if var == "HOME" {
                dir.join(".local").join("state").join("nlink-lab")
            } else {
                dir.join("nlink-lab")
            });
        }
    }
    if let Ok(tmp) = std::fs::read_dir(std::env::temp_dir()) {
        candidates.extend(tmp.flatten().map(|e| e.path()).filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("nlink-lab-"))
                .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()))
        }));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if c != current && c.join("labs").is_dir() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// The labs in [`legacy_roots`]: `(root, lab)` for every `labs/<lab>/state.json`.
pub fn legacy_labs() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for root in legacy_roots() {
        let Ok(entries) = std::fs::read_dir(root.join("labs")) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if validate_lab_name(&name).is_ok() && e.path().join("state.json").is_file() {
                out.push((root.clone(), name));
            }
        }
    }
    out.sort();
    out
}

/// The legacy root holding lab `name`, if one does.
fn legacy_lab_root(name: &str) -> Option<PathBuf> {
    legacy_roots()
        .into_iter()
        .find(|r| r.join("labs").join(name).join("state.json").is_file())
}

/// Move lab `name` from a legacy root into [`root_dir`] (#234). Refused
/// while a process holds the lab's lock in its old root (a deploy, apply
/// or destroy run by an older nlink-lab), or when the state root already
/// has a lab of that name. Log paths in the state are rewritten to the new
/// place; on another filesystem the directory is copied, and a process
/// still writing its log goes on writing the old (deleted) file.
pub fn migrate_lab(from_root: &Path, name: &str) -> Result<PathBuf> {
    use std::os::unix::io::AsRawFd;
    validate_lab_name(name)?;
    let src = from_root.join("labs").join(name);
    if !src.join("state.json").is_file() {
        return Err(Error::NotFound {
            name: name.to_string(),
        });
    }
    let dst = state_dir(name);
    if dst.exists() {
        return Err(Error::AlreadyExists {
            name: name.to_string(),
        });
    }
    let old_lock_path = from_root
        .join("labs")
        .join(".locks")
        .join(format!("{name}.lock"));
    if let Some(dir) = old_lock_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let old_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&old_lock_path)?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(old_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Locked {
            name: name.to_string(),
        });
    }
    let _new_lock = lock(name)?;
    ensure_dir(&base_dir())?;
    match std::fs::rename(&src, &dst) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(&src, &dst)?;
            std::fs::remove_dir_all(&src)?;
        }
        Err(e) => return Err(privileges(e, &src)),
    }
    adopt_tree(&dst)?;
    // log paths point into the old root
    let state_path = dst.join("state.json");
    let mut state: LabState = serde_json::from_str(&std::fs::read_to_string(&state_path)?)?;
    let (old, new) = (
        src.to_string_lossy().into_owned(),
        dst.to_string_lossy().into_owned(),
    );
    for (out, err) in state.process_logs.values_mut() {
        for p in [out, err] {
            if let Some(rest) = p.strip_prefix(&old) {
                *p = format!("{new}{rest}");
            }
        }
    }
    atomic_write(&state_path, &serde_json::to_string_pretty(&state)?)?;
    crate::subnet_pool::adopt_from(from_root, name)?;
    let _ = std::fs::remove_file(&old_lock_path);
    Ok(dst)
}

/// Copy a directory tree (files and directories; symlinks as links).
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_tree(&from, &to)?;
        } else if ty.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Give a moved lab directory the state root's ownership and modes:
/// `root:nlink-lab`, directories `2750`, files `0640`, the secrets and the
/// journal `0600`. As another user than root, only the modes change.
fn adopt_tree(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = unsafe { libc::geteuid() } == 0;
    let gid = group_gid();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if root {
            let _ = std::os::unix::fs::chown(&d, Some(0), gid);
        }
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o2750))?;
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let ty = e.file_type()?;
            if ty.is_dir() {
                stack.push(e.path());
            } else if ty.is_file() {
                if root {
                    let _ = std::os::unix::fs::chown(e.path(), Some(0), gid);
                }
                let secret = matches!(
                    e.file_name().to_str(),
                    Some("secrets.json" | "journal.json")
                );
                let mode = if secret { 0o600 } else { 0o640 };
                std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(mode))?;
            }
        }
    }
    Ok(())
}

/// Give the whole state root its ownership and modes again (`doctor
/// --fix`), as [`ensure_state_root`] would have made it with the group:
/// after the `nlink-lab` group is created, a root made without it is
/// `0700` and unreadable to the group. The `context/` directory stays
/// sticky `1770`, and the files in it stay their users'.
pub fn adopt_root() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = ensure_state_root()?;
    let gid = group_gid();
    let as_root = unsafe { libc::geteuid() } == 0;
    let mode = if gid.is_some() { 0o2750 } else { 0o700 };
    if as_root {
        let _ = std::os::unix::fs::chown(&root, Some(0), gid);
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(mode))?;
    for e in std::fs::read_dir(&root)? {
        let e = e?;
        let ty = e.file_type()?;
        if ty.is_dir() && e.file_name() == "context" {
            if as_root {
                let _ = std::os::unix::fs::chown(e.path(), Some(0), gid);
            }
            std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o1770))?;
        } else if ty.is_dir() {
            adopt_tree(&e.path())?;
        } else if ty.is_file() {
            if as_root {
                let _ = std::os::unix::fs::chown(e.path(), Some(0), gid);
            }
            std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o640))?;
        }
    }
    Ok(())
}

// ─── The current lab (#234) ─────────────────────────────────

/// Directory of the per-user current-lab files: sticky `1770` and owned by
/// the `nlink-lab` group, so a member can set their own and nobody else's.
fn context_dir() -> PathBuf {
    root_dir().join("context")
}

/// The current lab of user `uid`, as `context_set` recorded it. A file
/// owned by another user than `uid` or root is ignored, and so is a name
/// that is not a lab name.
pub fn context_get(uid: u32) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let path = context_dir().join(uid.to_string());
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || (meta.uid() != uid && meta.uid() != 0) {
        return None;
    }
    let name = std::fs::read_to_string(&path).ok()?.trim().to_string();
    validate_lab_name(&name).ok()?;
    Some(name)
}

/// Record (`Some`) or clear (`None`) the current lab of user `uid`.
pub fn context_set(uid: u32, lab: Option<&str>) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = context_dir();
    if !dir.is_dir() {
        ensure_root(&root_dir())?;
        match std::fs::DirBuilder::new().mode(0o1770).create(&dir) {
            Ok(()) => {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1770))?;
                if let Some(gid) = group_gid() {
                    let _ = std::os::unix::fs::chown(&dir, None, Some(gid));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(privileges(e, &dir)),
        }
    }
    let path = dir.join(uid.to_string());
    let Some(name) = lab else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(privileges(e, &path)),
            _ => Ok(()),
        };
    };
    validate_lab_name(name)?;
    // a fresh file, never one another user planted (the directory is
    // shared): created exclusively, no symlink followed
    let tmp = dir.join(format!("{uid}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|e| privileges(e, &tmp))?;
    f.write_all(name.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| privileges(e, &path))?;
    Ok(())
}

/// Record what lab `name` was deployed from (see [`LabSource`]).
pub fn set_source(name: &str, source: &LabSource) -> Result<()> {
    let _lock = lock_blocking(name)?;
    let path = state_dir(name).join("state.json");
    let mut state: LabState =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| privileges(e, &path))?)
            .map_err(|e| Error::State {
                op: "parse",
                detail: format!("failed to parse state: {e}"),
                path: path.clone(),
            })?;
    state.source = Some(source.clone());
    atomic_write(&path, &serde_json::to_string_pretty(&state)?)
}

/// What lab `name` was deployed from, when that was recorded.
pub fn source_of(name: &str) -> Option<LabSource> {
    load(name).ok()?.0.source
}

/// Get the base state directory: `labs/` under [`root_dir`].
fn base_dir() -> PathBuf {
    root_dir().join("labs")
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
    validate_lab_name(name).is_ok() && state_dir(name).join("state.json").exists()
}

/// Acquire an exclusive lock on a lab's state directory.
///
/// Returns a [`LabLock`] guard that holds the lock until dropped.
/// Fails with [`Error::Locked`] if another process holds the lock, after
/// a short grace (`LOCKED_GRACE`, 2 s) for a holder that is only a child
/// about to exec.
pub fn lock(name: &str) -> Result<LabLock> {
    acquire(name, false)
}

/// Like [`lock`] but waits for the lock instead of failing, up to
/// [`lock_timeout`], then fails with [`Error::Busy`] naming the holder.
/// Used by short read-modify-write updates of the state file
/// (`save_state`) that merely need to take turns with a concurrent
/// deploy/apply/destroy. It waited for ever, so a lock that leaked hung
/// its caller (#332).
pub fn lock_blocking(name: &str) -> Result<LabLock> {
    lock_wait_blocking(name, "save", lock_timeout())
}

/// The environment variable that sets how long a mutation waits for a
/// lab's lock: a duration (`30s`, `2m`) or a number of seconds (#332).
pub const LOCK_TIMEOUT_ENV: &str = "NLINK_LAB_LOCK_TIMEOUT";

/// How long a mutation waits for a lab's lock before it fails with
/// [`Error::Busy`]: [`LOCK_TIMEOUT_ENV`], else 30 s.
pub fn lock_timeout() -> std::time::Duration {
    std::env::var(LOCK_TIMEOUT_ENV)
        .ok()
        .and_then(|v| {
            let v = v.trim();
            v.parse::<u64>()
                .ok()
                .map(std::time::Duration::from_secs)
                .or_else(|| crate::helpers::parse_duration(v).ok())
        })
        .unwrap_or(std::time::Duration::from_secs(30))
}

/// Who holds a lab's lock: what the holder writes into the lock file once
/// it has it (#332), so a waiter that gives up can say who it waited for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    pub pid: u32,
    pub uid: u32,
    /// What it is doing: `deploy`, `apply`, `destroy`, `impair`, …
    pub op: String,
    /// RFC 3339.
    pub since: String,
}

impl std::fmt::Display for LockHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let me = if self.pid == std::process::id() {
            " (this process)"
        } else {
            ""
        };
        write!(
            f,
            "{} by pid {}{me} (uid {}), since {}",
            self.op, self.pid, self.uid, self.since
        )
    }
}

/// The record in a lab's lock file, while a holder that wrote one has it:
/// the record goes when its lock is released. A holder that took the
/// plain [`lock`] writes none.
pub fn lock_holder(name: &str) -> Option<LockHolder> {
    let text = std::fs::read_to_string(lock_path(name)).ok()?;
    serde_json::from_str(text.lines().next()?).ok()
}

/// Wait for a lab's lock for `op`, up to `timeout`, without blocking the
/// thread: an async task polls, so a holder on the same runtime (a
/// current-thread `#[lab_test]`, a daemon call) can finish meanwhile.
/// Fails with [`Error::Busy`], naming the holder, at the deadline (#332).
pub async fn lock_wait(name: &str, op: &str, timeout: std::time::Duration) -> Result<LabLock> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match acquire_now(name) {
            Ok(lock) => {
                lock.record(op);
                return Ok(lock);
            }
            Err(Error::Locked { .. }) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(Error::Locked { .. }) => return Err(busy(name, op, timeout)),
            Err(e) => return Err(e),
        }
    }
}

/// [`lock_wait`] for a synchronous caller: it sleeps the thread.
pub fn lock_wait_blocking(name: &str, op: &str, timeout: std::time::Duration) -> Result<LabLock> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match acquire_now(name) {
            Ok(lock) => {
                lock.record(op);
                return Ok(lock);
            }
            Err(Error::Locked { .. }) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(Error::Locked { .. }) => return Err(busy(name, op, timeout)),
            Err(e) => return Err(e),
        }
    }
}

/// [`lock`] for an operation: the holder record names it.
pub fn lock_for(name: &str, op: &str) -> Result<LabLock> {
    let lock = acquire(name, false)?;
    lock.record(op);
    Ok(lock)
}

/// One non-blocking attempt, without [`lock`]'s grace: the waiting loops
/// retry themselves.
fn acquire_now(name: &str) -> Result<LabLock> {
    acquire_with(name, false, std::time::Duration::ZERO)
}

fn busy(name: &str, op: &str, waited: std::time::Duration) -> Error {
    let holder = lock_holder(name)
        .map(|h| format!("held for {h}"))
        .unwrap_or_else(|| "held by another operation".into());
    Error::Busy(format!(
        "lab '{name}' is busy: {holder}; `{op}` waited {}s for it ({LOCK_TIMEOUT_ENV} sets how long)",
        waited.as_secs()
    ))
}

/// How many times [`acquire`] re-opens before giving up. A retry only
/// happens when the lock file was removed or replaced between our `open`
/// and our `flock`, so more than one is already unusual.
const ACQUIRE_ATTEMPTS: u32 = 8;

/// How long a fail-fast [`lock`] keeps trying before it reports
/// [`Error::Locked`]. A lock is also held, for a moment, by every child
/// another thread forks while the holder has it: `flock` belongs to the
/// open file description, and the child's `O_CLOEXEC` copy closes only at
/// its `exec`, after it has entered its namespace. Dropping a lock and
/// taking it again in that window failed with `Locked` although nobody
/// held it — in the daemon, whose jobs fork on several threads, and in
/// the parallel root suite (#339). A real holder (a deploy, an apply, a
/// destroy) holds it for seconds, and is still reported. (The unit tests,
/// which assert held locks often, wait less.)
const LOCKED_GRACE: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(250)
} else {
    std::time::Duration::from_secs(2)
};

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
    acquire_with(name, blocking, LOCKED_GRACE)
}

/// [`acquire`], retrying a non-blocking attempt for up to `grace`.
fn acquire_with(name: &str, blocking: bool, grace: std::time::Duration) -> Result<LabLock> {
    validate_lab_name(name)?;
    let path = lock_path(name);
    let op = if blocking {
        libc::LOCK_EX
    } else {
        libc::LOCK_EX | libc::LOCK_NB
    };
    let deadline = std::time::Instant::now() + grace;
    let mut attempts = 0;
    while attempts < ACQUIRE_ATTEMPTS {
        let file = open_lock_file(name)?;
        let ret = unsafe { libc::flock(file.as_raw_fd(), op) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if blocking {
                return Err(Error::deploy_failed(format!(
                    "failed to lock lab '{name}': {err}"
                )));
            }
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) && std::time::Instant::now() < deadline
            {
                // perhaps only a forked child's copy, about to exec
                drop(file);
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            return Err(Error::Locked {
                name: name.to_string(),
            });
        }
        attempts += 1;
        if same_file(&file, &path) {
            return Ok(LabLock { file, path });
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
    use std::os::unix::fs::OpenOptionsExt;
    let dir = locks_dir();
    ensure_dir(&dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o640)
        .open(lock_path(name))
        .map_err(|e| privileges(e, &lock_path(name)))
}

/// Guard that holds a file lock on a lab's state directory.
/// The lock is released when this guard is dropped.
pub struct LabLock {
    /// Dropping it is what releases the `flock`; it also carries the
    /// holder record.
    file: std::fs::File,
    path: PathBuf,
}

impl LabLock {
    /// Write the holder record into the lock file (#332). Best effort: a
    /// waiter without it just says less.
    fn record(&self, op: &str) {
        use std::os::unix::fs::FileExt as _;
        let holder = LockHolder {
            pid: std::process::id(),
            // SAFETY: getuid(2) cannot fail.
            uid: unsafe { libc::getuid() },
            op: op.to_string(),
            since: crate::events::now_iso8601(),
        };
        if let Ok(mut line) = serde_json::to_vec(&holder) {
            line.push(b'\n');
            // written, then cut to length: a waiter reading the first
            // line meanwhile sees this record or the one before it, never
            // an empty file
            let _ = self.file.write_at(&line, 0);
            let _ = self.file.set_len(line.len() as u64);
        }
    }

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

impl Drop for LabLock {
    /// Clear the holder record while the lock is still held, so a waiter
    /// that gives up later does not name a holder that has let go.
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
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
        ensure_dir(parent)?;
    }
    let file = std::fs::File::create(&lock_path).map_err(|e| privileges(e, &lock_path))?;
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
        file,
        path: lock_path,
    })
}

/// Save lab state and topology.
pub fn save(state: &LabState, topology: &Topology) -> Result<()> {
    validate_lab_name(&state.name)?;
    let dir = state_dir(&state.name);
    ensure_dir(&dir)?;

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
    atomic_write_mode(path, content, 0o640)
}

/// [`atomic_write`] with the file's mode: `0o640` (group-readable) for
/// state, `0o600` for what holds secrets (#234).
pub(crate) fn atomic_write_mode(path: &std::path::Path, content: &str, mode: u32) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(mode)
            .open(&tmp)
            .map_err(|e| privileges(e, &tmp))?;
        // an existing tmp file keeps its old mode
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
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
    validate_lab_name(name)?;
    let dir = state_dir(name);

    let state_path = dir.join("state.json");
    let state_json = match std::fs::read_to_string(&state_path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // still in a per-user root an older nlink-lab used (#234)
            if let Some(old) = legacy_lab_root(name) {
                return Err(Error::MovedState {
                    name: name.to_string(),
                    path: old,
                });
            }
            return Err(Error::NotFound {
                name: name.to_string(),
            });
        }
        Err(e) => return Err(privileges(e, &state_path)),
    };
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
    // "no labs" and "not allowed to look" are different answers (#234)
    let entries = match std::fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(privileges(e, &base)),
    };

    let mut labs = Vec::new();
    for entry in entries {
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
    validate_lab_name(name)?;
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
    validate_lab_name(name)?;
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
    /// `NLINK_LAB_STATE_DIR`. Without this, `cargo test`'s default
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
        // this module that touches `NLINK_LAB_STATE_DIR`. Only one
        // such test can be inside this critical section at a
        // time, so no other thread is concurrently reading the
        // env var.
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };
        StateTestEnv {
            _dir: dir,
            _guard: guard,
        }
    }

    /// The state root in a directory that does not exist yet, so the
    /// test sees what creating it does.
    fn fresh_root_env() -> (StateTestEnv, PathBuf) {
        let env = temp_state_env();
        let root = env._dir.path().join("root");
        // SAFETY: as in `temp_state_env`, under its lock.
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", &root) };
        (env, root)
    }

    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn the_state_root_and_its_files_get_their_modes() {
        // #234: group-readable state when the group exists, private
        // otherwise; secrets never readable by the group
        let (_env, root) = fresh_root_env();
        let state = LabState::new("modes", "2026-10-17T00:00:00Z");
        save(&state, &Topology::default()).unwrap();
        let group = group_gid().is_some();
        assert_eq!(mode(&root), if group { 0o2750 } else { 0o700 });
        // setgid comes from the root: mkdir takes no S_ISGID
        assert_eq!(
            mode(&state_dir("modes")),
            if group { 0o2750 } else { 0o750 }
        );
        assert_eq!(mode(&state_dir("modes").join("state.json")), 0o640);
        assert_eq!(mode(&state_dir("modes").join("topology.toml")), 0o640);
        let _held = lock("modes").unwrap();
        assert_eq!(mode(&lock_path("modes")), 0o640);
    }

    #[test]
    fn an_unreadable_root_is_not_an_empty_one() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads anything
        }
        let (_env, root) = fresh_root_env();
        save(&LabState::new("hidden", "t"), &Topology::default()).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listed = list();
        let loaded = load("hidden");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(listed, Err(Error::PermissionDenied { .. })),
            "{listed:?}"
        );
        assert!(
            matches!(loaded, Err(Error::PermissionDenied { .. })),
            "{:?}",
            loaded.err()
        );
    }

    #[test]
    fn the_current_lab_is_per_user_and_validated() {
        let (_env, root) = fresh_root_env();
        let me = unsafe { libc::getuid() };
        assert_eq!(context_get(me), None);
        context_set(me, Some("lab-a")).unwrap();
        assert_eq!(context_get(me).as_deref(), Some("lab-a"));
        assert_eq!(context_get(me + 1), None, "another user's is separate");
        assert_eq!(mode(&root.join("context")), 0o1770);
        // a file that is not a lab name, or a symlink, is not trusted
        let path = root.join("context").join(me.to_string());
        std::fs::write(&path, "../etc").unwrap();
        assert_eq!(context_get(me), None);
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();
        assert_eq!(context_get(me), None);
        std::fs::remove_file(&path).unwrap();
        context_set(me, Some("lab-b")).unwrap();
        context_set(me, None).unwrap();
        assert_eq!(context_get(me), None);
        context_set(me, None).unwrap(); // clearing twice is fine
        assert!(context_set(me, Some("a/b")).is_err());
    }

    #[test]
    fn the_source_of_a_deploy_is_kept() {
        let _env = temp_state_env();
        save(&LabState::new("src", "t"), &Topology::default()).unwrap();
        assert_eq!(source_of("src"), None);
        let source = LabSource {
            path: Some("/labs/wan.nll".into()),
            params: vec![("delay".into(), "50ms".into())],
            suffix: Some("ci".into()),
            dir: Some("/home/me/labs".into()),
        };
        set_source("src", &source).unwrap();
        assert_eq!(source_of("src"), Some(source));
    }

    /// An old per-user root (the caller's `$XDG_STATE_HOME/nlink-lab`) with
    /// lab `name` in it: a state file with a process log, and its subnet.
    fn legacy_root_with(env: &StateTestEnv, name: &str) -> PathBuf {
        let xdg = env._dir.path().join("xdg");
        let old = xdg.join("nlink-lab");
        let lab = old.join("labs").join(name);
        std::fs::create_dir_all(lab.join("logs")).unwrap();
        let mut st = LabState::new(name, "t");
        let log = lab.join("logs").join("a.stdout");
        std::fs::write(&log, "hello").unwrap();
        st.process_logs.insert(
            7,
            (
                log.to_string_lossy().into_owned(),
                log.to_string_lossy().into_owned(),
            ),
        );
        std::fs::write(lab.join("state.json"), serde_json::to_string(&st).unwrap()).unwrap();
        std::fs::write(
            lab.join("topology.toml"),
            toml::to_string(&Topology::default()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            old.join("subnet-pool.json"),
            format!(
                r#"{{"in_use":{{"10.42.7.0/24":"{name}","10.42.8.0/24":"other"}},"next_offset":0}}"#
            ),
        )
        .unwrap();
        // SAFETY: under the env lock `env` holds.
        unsafe { std::env::set_var("XDG_STATE_HOME", &xdg) };
        old
    }

    #[test]
    fn a_lab_in_an_old_root_is_found_and_moved() {
        // #234: labs deployed by 0.13 under a user's own state directory
        let (env, root) = fresh_root_env();
        let old = legacy_root_with(&env, "old-lab");
        let found = legacy_labs();
        assert!(
            found.contains(&(old.clone(), "old-lab".to_string())),
            "{found:?}"
        );
        // still there: loading it says where it is and what to do
        assert!(matches!(load("old-lab"), Err(Error::MovedState { .. })));

        let moved = migrate_lab(&old, "old-lab").unwrap();
        assert_eq!(moved, root.join("labs").join("old-lab"));
        assert!(!old.join("labs").join("old-lab").exists());
        let (st, _) = load("old-lab").unwrap();
        let (out, _) = &st.process_logs[&7];
        assert!(
            out.starts_with(&moved.to_string_lossy().into_owned()),
            "{out}"
        );
        assert_eq!(std::fs::read_to_string(out).unwrap(), "hello");
        assert_eq!(mode(&moved.join("state.json")), 0o640);
        // its subnet came along, the other lab's did not
        let pool = std::fs::read_to_string(root.join("subnet-pool.json")).unwrap();
        assert!(
            pool.contains("10.42.7.0/24") && !pool.contains("10.42.8.0/24"),
            "{pool}"
        );
        // nothing left to move
        assert!(matches!(
            migrate_lab(&old, "old-lab"),
            Err(Error::NotFound { .. })
        ));
        unsafe { std::env::remove_var("XDG_STATE_HOME") };
    }

    #[test]
    fn a_busy_or_clashing_old_lab_is_not_moved() {
        use std::os::unix::io::AsRawFd;
        let (env, _root) = fresh_root_env();
        let old = legacy_root_with(&env, "busy");
        // an older nlink-lab holds the lab's lock in its old root
        let lock_dir = old.join("labs").join(".locks");
        std::fs::create_dir_all(&lock_dir).unwrap();
        let held = std::fs::File::create(lock_dir.join("busy.lock")).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert!(matches!(
            migrate_lab(&old, "busy"),
            Err(Error::Locked { .. })
        ));
        drop(held);
        // a lab of that name in the state root already
        save(&LabState::new("busy", "t"), &Topology::default()).unwrap();
        assert!(matches!(
            migrate_lab(&old, "busy"),
            Err(Error::AlreadyExists { .. })
        ));
        assert!(old.join("labs").join("busy").join("state.json").exists());
        unsafe { std::env::remove_var("XDG_STATE_HOME") };
    }

    #[test]
    fn a_lab_name_that_is_not_a_path_component_is_refused() {
        // #221: `destroy --force ""` removed every lab's state
        let _dir = temp_state_env();
        let state = LabState::new("keep", "2026-10-17T00:00:00Z");
        save(&state, &Topology::default()).unwrap();
        for bad in ["", ".", "..", "../keep", "/tmp/x", "a/b", "-a"] {
            assert!(
                matches!(remove(bad), Err(Error::InvalidArgument(_))),
                "remove({bad:?})"
            );
            assert!(lock(bad).is_err(), "lock({bad:?})");
            assert!(load(bad).is_err(), "load({bad:?})");
            assert!(snapshot_list(bad).is_err(), "snapshot_list({bad:?})");
            assert!(!exists(bad), "exists({bad:?})");
        }
        assert!(exists("keep"), "the other lab's state survived");
        for good in ["keep", "a.b", "a-1", "_x"] {
            validate_lab_name(good).unwrap();
        }
    }

    #[test]
    fn the_state_root_is_one_place_unless_moved() {
        // #234: it was the deploying user's XDG/HOME directory
        assert_eq!(root_dir_from(None), PathBuf::from("/var/lib/nlink-lab"));
        assert_eq!(
            root_dir_from(Some("/srv/state".into())),
            PathBuf::from("/srv/state")
        );
        // an empty or relative value would follow the current directory
        assert_eq!(root_dir_from(Some("".into())), PathBuf::from(DEFAULT_ROOT));
        assert_eq!(
            root_dir_from(Some("rel".into())),
            PathBuf::from(DEFAULT_ROOT)
        );
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
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

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
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

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

    /// A child another thread forks while a lab's lock is held shares the
    /// lock (flock belongs to the open file description) until it execs:
    /// its `O_CLOEXEC` copy closes only then. Dropping the lock and taking
    /// it again in that window failed with `Locked` — the parallel root
    /// suite's flake (#339). The children here pause 5 ms before exec,
    /// as a namespace exec does while it enters the namespace.
    #[test]
    fn a_forked_childs_copy_of_a_lock_does_not_fail_the_next_lock() {
        use std::os::unix::process::CommandExt as _;
        use std::sync::atomic::{AtomicBool, Ordering};
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let forker = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let mut cmd = std::process::Command::new("true");
                    // SAFETY: usleep is async-signal-safe.
                    unsafe {
                        cmd.pre_exec(|| {
                            libc::usleep(5_000);
                            Ok(())
                        });
                    }
                    let _ = cmd.status();
                }
            })
        };
        let mut failures = 0;
        for _ in 0..200 {
            match lock("relocked") {
                Ok(held) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    drop(held);
                }
                Err(_) => failures += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        forker.join().unwrap();
        assert_eq!(failures, 0, "a dropped lock must be takable again");
    }

    #[test]
    fn stale_locks_finds_only_locks_without_a_lab() {
        let _guard = xdg_state_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

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
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

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
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", dir.path()) };

        let held = lock("raced").expect("first lock");
        let (tx, rx) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let waiter = std::thread::spawn(move || {
            // Blocks on the inode that is about to be unlinked.
            let got = lock_blocking("raced").expect("acquires eventually");
            tx.send(()).unwrap();
            // Hold it until the test says so (past `lock`'s grace).
            let _ = released.recv();
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
        release.send(()).unwrap();
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
