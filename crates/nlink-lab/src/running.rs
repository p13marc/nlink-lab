//! Running lab interaction.
//!
//! [`RunningLab`] provides methods to interact with a deployed lab:
//! executing commands, spawning processes, modifying impairments, and destroying.

use std::collections::BTreeMap;
use std::time::Instant;

use nlink::netlink::diagnostics::{DiagnosticReport, Diagnostics, InterfaceDiag, Issue, RouteDiag};
use nlink::netlink::namespace;
use nlink::{Connection, Route};

use crate::error::{Error, Result};
use crate::state::{self, ContainerState, LabInfo};
use crate::types::EndpointRef;
use crate::types::Topology;

/// A deployed, running lab.
pub struct RunningLab {
    /// The topology used to deploy.
    topology: Topology,
    /// Map of node_name -> namespace_name (bare namespace nodes only).
    namespace_names: BTreeMap<String, String>,
    /// Map of node_name -> container state (container nodes only).
    containers: BTreeMap<String, ContainerState>,
    /// Container runtime binary ("docker" or "podman"), if any containers.
    runtime_binary: Option<String>,
    /// Background process PIDs: (node_name, pid).
    pids: Vec<(String, u32)>,
    /// Whether DNS hosts entries were injected into /etc/hosts.
    dns_injected: bool,
    /// Whether the lab made Wi-Fi radios.
    wifi_loaded: bool,
    /// Partitioned endpoints (`impair --partition`), each with the
    /// impairment it had then (what an older nlink-lab restores on heal).
    saved_impairments: BTreeMap<String, crate::types::Impairment>,
    /// Impairments set at runtime (`impair`), persisted so `apply`,
    /// `verify` and snapshots see them (#59).
    live_impairments: BTreeMap<String, crate::types::Impairment>,
    /// Log file paths for spawned processes: pid → (stdout_path, stderr_path).
    process_logs: BTreeMap<u32, (String, String)>,
    /// Tag of each tracked process on a container node (see
    /// `LabState::process_tags`).
    process_tags: BTreeMap<u32, String>,
    /// `/proc/<pid>/stat` start time per tracked PID (see
    /// `LabState::starttimes`). A PID without an entry is never signalled.
    starttimes: BTreeMap<u32, u64>,
    /// Background `exec` block → pid (`"<node>:<index>"`, see
    /// `LabState::exec_pids`).
    exec_pids: BTreeMap<String, u32>,
    /// node → root-namespace mgmt veth peer name (see `LabState::mgmt_peers`).
    mgmt_peers: std::collections::BTreeMap<String, String>,
    /// Outcome of the `validate { … }` assertions run at deploy step 19.
    /// Empty when the topology has no assertions or the lab was
    /// [`load`](Self::load)ed from state (results are not persisted).
    assertion_results: Vec<crate::test_runner::AssertionResult>,
    /// The state this handle last read or wrote. A save writes only what
    /// changed since, merged into the file as it is then, so two
    /// processes saving the same lab keep each other's changes.
    baseline: std::sync::Mutex<Baseline>,
}

/// A copy of the handle, baseline included: the copy saves only what it
/// changes itself, as two loads of the lab would (#234: the daemon hands
/// one to a blocking task).
impl Clone for RunningLab {
    fn clone(&self) -> Self {
        Self {
            topology: self.topology.clone(),
            namespace_names: self.namespace_names.clone(),
            containers: self.containers.clone(),
            runtime_binary: self.runtime_binary.clone(),
            pids: self.pids.clone(),
            dns_injected: self.dns_injected,
            wifi_loaded: self.wifi_loaded,
            saved_impairments: self.saved_impairments.clone(),
            live_impairments: self.live_impairments.clone(),
            process_logs: self.process_logs.clone(),
            process_tags: self.process_tags.clone(),
            starttimes: self.starttimes.clone(),
            exec_pids: self.exec_pids.clone(),
            mgmt_peers: self.mgmt_peers.clone(),
            assertion_results: self.assertion_results.clone(),
            baseline: std::sync::Mutex::new(
                self.baseline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            ),
        }
    }
}

/// The persisted part of a [`RunningLab`], as of its last load or save.
#[derive(Clone, Default)]
struct Baseline {
    namespaces: BTreeMap<String, String>,
    containers: BTreeMap<String, ContainerState>,
    runtime: Option<String>,
    dns_injected: bool,
    wifi_loaded: bool,
    pids: Vec<(String, u32)>,
    starttimes: BTreeMap<u32, u64>,
    exec_pids: BTreeMap<String, u32>,
    mgmt_peers: BTreeMap<String, String>,
    saved_impairments: BTreeMap<String, crate::types::Impairment>,
    live_impairments: BTreeMap<String, crate::types::Impairment>,
    process_logs: BTreeMap<u32, (String, String)>,
    process_tags: BTreeMap<u32, String>,
    /// The topology, serialized: `Topology` has no `PartialEq`.
    topology: String,
}

/// `fresh` becomes `mine` where this handle changed it since `base`.
fn take_changed<T: PartialEq + Clone>(fresh: &mut T, base: &T, mine: &T) {
    if mine != base {
        *fresh = mine.clone();
    }
}

/// This handle's own insertions, changes and removals since `base`,
/// applied to the file's `fresh` map.
fn merge_map<K: Ord + Clone, V: PartialEq + Clone>(
    fresh: &mut BTreeMap<K, V>,
    base: &BTreeMap<K, V>,
    mine: &BTreeMap<K, V>,
) {
    for (k, v) in mine {
        if base.get(k) != Some(v) {
            fresh.insert(k.clone(), v.clone());
        }
    }
    for k in base.keys() {
        if !mine.contains_key(k) {
            fresh.remove(k);
        }
    }
}

/// [`merge_map`] for the tracked `(node, pid)` list.
fn merge_pids(fresh: &mut Vec<(String, u32)>, base: &[(String, u32)], mine: &[(String, u32)]) {
    fresh.retain(|p| mine.contains(p) || !base.contains(p));
    for p in mine {
        if !base.contains(p) && !fresh.contains(p) {
            fresh.push(p.clone());
        }
    }
}

/// `map[key]` as `from` has it (present or absent).
fn restore_entry<V: Clone>(map: &mut BTreeMap<String, V>, from: &BTreeMap<String, V>, key: &str) {
    match from.get(key) {
        Some(v) => {
            map.insert(key.to_string(), v.clone());
        }
        None => {
            map.remove(key);
        }
    }
}

/// `fresh` (the state file) with this handle's changes since `base`
/// applied: what [`RunningLab::save_merged`] writes and
/// [`RunningLab::refresh`] takes in.
fn merge_into(fresh: &mut state::LabState, base: &Baseline, mine: &Baseline) {
    take_changed(&mut fresh.namespaces, &base.namespaces, &mine.namespaces);
    take_changed(&mut fresh.containers, &base.containers, &mine.containers);
    take_changed(&mut fresh.runtime, &base.runtime, &mine.runtime);
    take_changed(
        &mut fresh.dns_injected,
        &base.dns_injected,
        &mine.dns_injected,
    );
    take_changed(&mut fresh.wifi_loaded, &base.wifi_loaded, &mine.wifi_loaded);
    take_changed(&mut fresh.mgmt_peers, &base.mgmt_peers, &mine.mgmt_peers);
    merge_pids(&mut fresh.pids, &base.pids, &mine.pids);
    merge_map(&mut fresh.starttimes, &base.starttimes, &mine.starttimes);
    merge_map(&mut fresh.exec_pids, &base.exec_pids, &mine.exec_pids);
    merge_map(
        &mut fresh.saved_impairments,
        &base.saved_impairments,
        &mine.saved_impairments,
    );
    merge_map(
        &mut fresh.live_impairments,
        &base.live_impairments,
        &mine.live_impairments,
    );
    merge_map(
        &mut fresh.process_logs,
        &base.process_logs,
        &mine.process_logs,
    );
    merge_map(
        &mut fresh.process_tags,
        &base.process_tags,
        &mine.process_tags,
    );
}

impl Baseline {
    /// The baseline of a handle that has just read `st` and `topology`.
    fn of_state(st: &state::LabState, topology: &Topology) -> Self {
        Self {
            namespaces: st.namespaces.clone(),
            containers: st.containers.clone(),
            runtime: st.runtime.clone(),
            dns_injected: st.dns_injected,
            wifi_loaded: st.wifi_loaded,
            pids: st.pids.clone(),
            starttimes: st.starttimes.clone(),
            exec_pids: st.exec_pids.clone(),
            mgmt_peers: st.mgmt_peers.clone(),
            saved_impairments: st.saved_impairments.clone(),
            live_impairments: st.live_impairments.clone(),
            process_logs: st.process_logs.clone(),
            process_tags: st.process_tags.clone(),
            topology: serde_json::to_string(topology).unwrap_or_default(),
        }
    }
}

/// A change to a running lab, from [`RunningLab::begin`] to
/// [`RunningLab::commit`]: it holds the lab's lock (#332).
pub(crate) struct Mutation {
    lock: state::LabLock,
}

impl Mutation {
    /// The lock, for a call that needs it as a token.
    #[allow(dead_code)]
    pub(crate) fn lock(&self) -> &state::LabLock {
        &self.lock
    }
}

/// A container whose init PID now names a process that started at
/// another time is not running: the PID was reused, possibly by a host
/// process, and every `/proc/<pid>/ns/net` reached through it would be
/// that process's namespace. PID 0 names none, so whatever the node is
/// asked fails instead.
fn zero_stale_container_pids(containers: &mut BTreeMap<String, ContainerState>) {
    for (node, c) in containers.iter_mut() {
        if c.pid != 0
            && let Some(t) = c.starttime
            && host_starttime(c.pid) != Some(t)
        {
            tracing::warn!(
                "container node '{node}': its init (pid {}) is gone; `nlink-lab restart` \
                 reattaches a restarted container",
                c.pid
            );
            c.pid = 0;
        }
    }
}

/// Output from executing a command in a lab node.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Process exit code; `128 + signo` when a signal ended it, as a
    /// shell reports it (it was `-1`, which `exec` turned into success,
    /// #226).
    pub exit_code: i32,
}

/// Which log stream(s) [`RunningLab::wait_for_log_line`] watches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogStream {
    /// Match lines from the captured stdout file only.
    Stdout,
    /// Match lines from the captured stderr file only.
    Stderr,
    /// Match lines from either stream. Default — most services emit the
    /// "ready" signal to whichever they default to.
    #[default]
    Both,
}

/// Optional knobs for [`RunningLab::exec_with_opts`] and
/// [`RunningLab::exec_attached_with_opts`].
///
/// Construct with [`ExecOpts::default`] (or `..Default::default()`) and
/// override fields as needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecOpts<'a> {
    /// Working directory for the child. Namespace nodes: `chdir()` on the
    /// host filesystem. Container nodes: `-w <path>` to docker/podman.
    pub workdir: Option<&'a std::path::Path>,
    /// Additional environment variables, applied on top of the inherited
    /// environment. Namespace nodes: set on the `Command` directly.
    /// Container nodes: passed as repeated `-e KEY=VALUE` to the runtime.
    pub env: &'a [(&'a str, &'a str)],
    /// Maximum wall-clock time the child is allowed to run. On expiry
    /// the child is sent SIGTERM, then SIGKILL after a 1-second grace
    /// period. Returns [`Error::Timeout`] when triggered. `None` =
    /// no timeout (matches the historical behaviour). Mirrors
    /// `coreutils timeout(1)`; the CLI translates `Error::Timeout`
    /// into exit code 124.
    pub timeout: Option<std::time::Duration>,
}

/// How [`RunningLab::prepare_exec`] wires a command.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecMode {
    /// It reads stdin (`exec -i` on a container node).
    pub stdin: bool,
    /// The caller gives it a terminal (`exec -t` on a container node).
    pub tty: bool,
    /// It may have to be stopped before it ends (a timeout, a cancel, a
    /// client that went away): on a container node it is tagged, so the
    /// command can be found and stopped in the container, where killing
    /// the runtime's client does not reach it (#226).
    pub stoppable: bool,
}

/// A command [`RunningLab::prepare_exec`] made, not started yet.
#[derive(Debug)]
pub struct PreparedExec {
    command: std::process::Command,
    /// The network namespace a namespace node's (or a foreign netns
    /// node's) command enters.
    netns: Option<crate::deploy::NsRef>,
    /// A container's init pid and the command's tag.
    stop: Option<(u32, String)>,
    /// `exec in 'node'`, for errors.
    label: String,
}

impl PreparedExec {
    /// The command, to set its stdio (and, before spawning, anything
    /// else `Command` allows, like a `pre_exec` hook for a terminal).
    pub fn command(&mut self) -> &mut std::process::Command {
        &mut self.command
    }

    pub fn spawn(self) -> Result<ExecChild> {
        let label = self.label;
        let child = match &self.netns {
            Some(ns) => ns
                .spawn(self.command)
                .map_err(|e| Error::nlink(format!("{label} failed"), e))?,
            None => {
                let mut command = self.command;
                command
                    .spawn()
                    .map_err(|e| Error::deploy_failed(format!("{label} failed: {e}")))?
            }
        };
        Ok(ExecChild {
            child,
            stop: self.stop,
        })
    }
}

/// A started exec: the process, and how to stop the command it runs.
#[derive(Debug)]
pub struct ExecChild {
    /// The process: the command on a namespace node, the runtime's client
    /// on a container node.
    pub child: std::process::Child,
    stop: Option<(u32, String)>,
}

impl ExecChild {
    /// What stops the command in a container, kept apart from the child
    /// that is waited for.
    pub fn stopper(&self) -> ExecStopper {
        ExecStopper(self.stop.clone())
    }

    /// Stop the command and its process.
    pub fn kill(&mut self) {
        self.stopper().stop();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Stops a container command an exec started, by its tag (see
/// [`ExecMode::stoppable`]); nothing on a namespace node, where killing
/// the process is enough.
#[derive(Debug, Clone)]
pub struct ExecStopper(Option<(u32, String)>);

impl ExecStopper {
    pub fn stop(&self) {
        if let Some((pid, tag)) = &self.0 {
            stop_tagged(*pid, tag);
        }
    }
}

/// Optional knobs for [`RunningLab::spawn_with_logs_with_opts`]. Same env
/// and workdir semantics as [`ExecOpts`], plus a custom log directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpawnOpts<'a> {
    /// Log directory override. `None` uses the lab's default state-dir
    /// `logs/` subfolder.
    pub log_dir: Option<&'a std::path::Path>,
    /// Working directory for the child (chdir before exec).
    pub workdir: Option<&'a std::path::Path>,
    /// Additional environment variables (set via `Command::env`).
    pub env: &'a [(&'a str, &'a str)],
}

/// A lab's runtime traffic control (live impairments, partitions), as a
/// scenario found it (#326).
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeTc {
    live: BTreeMap<String, crate::types::Impairment>,
    partitions: BTreeMap<String, crate::types::Impairment>,
}

/// Status of a tracked background process.
///
/// **Retention**: tracked processes are *not* removed from the lab's PID
/// list when they exit. They remain with `alive == false` until the lab
/// is destroyed (or the state file is cleaned manually). Consumers
/// polling for "is X still running?" must check [`alive`](Self::alive),
/// not just look up the PID.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ProcessInfo {
    /// Node the process runs in.
    pub node: String,
    /// Process ID.
    pub pid: u32,
    /// Explicit alias for `pid`. Equal today because nlink-lab does not
    /// use `CLONE_NEWPID` (host_pid == ns_pid for every spawned
    /// process). The field exists so consumers that explicitly want
    /// "the host PID" can name it; if a future nlink-lab adds
    /// `CLONE_NEWPID`, an `ns_pid` field will join `host_pid` and
    /// `pid` will follow `host_pid`. See `docs/ARCHITECTURE.md`
    /// "Process & namespace model".
    pub host_pid: u32,
    /// Whether the process is still alive (`kill(pid, 0)` returns 0).
    ///
    /// Stays `false` after the process exits — the entry is kept for
    /// post-mortem inspection (log paths, exit ordering). See the
    /// type-level retention note above.
    pub alive: bool,
    /// Path to stdout log file (if captured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    /// Exit code once the process has exited, read from the `.rc` file the
    /// spawn reaper writes next to the logs (`128 + signo` for a signal
    /// death). `None` while alive, or for processes spawned by a release
    /// without the reaper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Path to stderr log file (if captured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
}

/// Diagnostic results for a single node.
#[derive(Debug, Clone)]
pub struct NodeDiagnostic {
    /// Node name.
    pub node: String,
    /// When the link counters were read: the end of the link dump.
    /// [`RunningLab::diagnose_since`] takes rates over the time since.
    pub taken_at: Instant,
    /// Per-interface diagnostics.
    pub interfaces: Vec<InterfaceDiag>,
    /// Issues detected.
    pub issues: Vec<Issue>,
}

impl NodeDiagnostic {
    /// As `diagnose --json` and the daemon print it: `{node, interfaces:
    /// [{name, state, mtu, rx_bytes, tx_bytes, issues}], issues}`, each
    /// issue `{severity, message}`. An interface's issues are listed under
    /// it only; the node's own list has the others (#226).
    pub fn to_json(&self) -> serde_json::Value {
        let issue = |i: &nlink::netlink::diagnostics::Issue| serde_json::json!({ "severity": i.severity.to_string(), "message": i.to_string() });
        serde_json::json!({
            "node": self.node,
            "interfaces": self.interfaces.iter().map(|iface| serde_json::json!({
                "name": iface.name,
                "state": format!("{:?}", iface.state),
                "mtu": iface.mtu,
                "rx_bytes": iface.stats.rx_bytes(),
                "tx_bytes": iface.stats.tx_bytes(),
                "issues": iface.issues.iter().map(issue).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "issues": self.issues.iter().filter(|i| i.interface.is_none()).map(issue).collect::<Vec<_>>(),
        })
    }

    fn from_report(node: &str, report: DiagnosticReport) -> Self {
        Self {
            node: node.to_string(),
            taken_at: report.timestamp,
            interfaces: report.interfaces,
            issues: report.issues,
        }
    }
}

/// The fallback devices the tunnel modules create in each namespace.
const FALLBACK_TUNNELS: &[&str] = &[
    "gre0", "gretap0", "erspan0", "tunl0", "sit0", "ip_vti0", "ip6tnl0", "ip6gre0", "ip6_vti0",
];

/// The node's entry in an earlier [`RunningLab::diagnose`] result.
fn previous_of<'a>(previous: &'a [NodeDiagnostic], node: &str) -> Option<&'a NodeDiagnostic> {
    previous.iter().find(|d| d.node == node)
}

/// One node's scan, with rates since `previous` when there is one.
async fn scan_node(
    conn: Connection<Route>,
    previous: Option<&NodeDiagnostic>,
) -> nlink::Result<DiagnosticReport> {
    // A lab node often has no default route on purpose (a router, the
    // hosts of `examples/simple.nll`): every one was a warning (#226).
    let config = nlink::netlink::diagnostics::DiagnosticsConfig {
        warn_no_default_route: false,
        ..Default::default()
    };
    let diag = Diagnostics::with_config(conn, config);
    match previous {
        // Only the timestamp and the interfaces' names, ifindexes and
        // counters are read back.
        Some(prev) => {
            diag.scan_since(&DiagnosticReport {
                timestamp: prev.taken_at,
                interfaces: prev.interfaces.clone(),
                routes: RouteDiag::default(),
                issues: Vec::new(),
            })
            .await
        }
        None => diag.scan().await,
    }
}

impl RunningLab {
    /// Create a new RunningLab (called by the deployer).
    pub(crate) fn new(
        topology: Topology,
        namespace_names: BTreeMap<String, String>,
        containers: BTreeMap<String, ContainerState>,
        runtime_binary: Option<String>,
        pids: Vec<(String, u32)>,
        dns_injected: bool,
        wifi_loaded: bool,
    ) -> Self {
        Self {
            topology,
            namespace_names,
            containers,
            runtime_binary,
            pids,
            dns_injected,
            wifi_loaded,
            starttimes: BTreeMap::new(),
            exec_pids: BTreeMap::new(),
            mgmt_peers: std::collections::BTreeMap::new(),
            saved_impairments: BTreeMap::new(),
            live_impairments: BTreeMap::new(),
            process_logs: BTreeMap::new(),
            process_tags: BTreeMap::new(),
            assertion_results: Vec::new(),
            baseline: Default::default(),
        }
    }

    /// The persisted fields as they are now.
    fn snapshot(&self) -> Baseline {
        Baseline {
            namespaces: self.namespace_names.clone(),
            containers: self.containers.clone(),
            runtime: self.runtime_binary.clone(),
            dns_injected: self.dns_injected,
            wifi_loaded: self.wifi_loaded,
            pids: self.pids.clone(),
            starttimes: self.starttimes.clone(),
            exec_pids: self.exec_pids.clone(),
            mgmt_peers: self.mgmt_peers.clone(),
            saved_impairments: self.saved_impairments.clone(),
            live_impairments: self.live_impairments.clone(),
            process_logs: self.process_logs.clone(),
            process_tags: self.process_tags.clone(),
            topology: serde_json::to_string(&self.topology).unwrap_or_default(),
        }
    }

    /// The lab as its state file has it now, with what only memory holds
    /// (the deploy's assertion results) carried over: what a handle (#328)
    /// keeps after a change that failed.
    pub(crate) fn reloaded(&self) -> Result<RunningLab> {
        let mut fresh = RunningLab::load(self.name())?;
        fresh.assertion_results = self.assertion_results.clone();
        Ok(fresh)
    }

    /// This handle's state is what is on disk now: deploy just wrote it.
    pub(crate) fn mark_saved(&self) {
        *self.baseline.lock().unwrap_or_else(|p| p.into_inner()) = self.snapshot();
    }

    /// Get the topology used to deploy this lab.
    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Structured results of the topology's `validate { … }` assertions
    /// as evaluated at the end of `deploy()` (step 19), in declaration
    /// order. Empty if the topology has no assertions or this handle
    /// was loaded from saved state.
    pub fn assertion_results(&self) -> &[crate::test_runner::AssertionResult] {
        &self.assertion_results
    }

    /// `true` if at least one deploy-time assertion did not pass —
    /// including assertions that could not be evaluated (no target
    /// address, exec failure). This is the check `deploy --strict`
    /// should make before deciding to tear the lab down / exit non-zero.
    pub fn assertions_failed(&self) -> bool {
        self.assertion_results.iter().any(|r| !r.passed)
    }

    /// Record deploy-time assertion results (crate-internal, set by
    /// `deploy()` step 19).
    pub(crate) fn set_assertion_results(
        &mut self,
        results: Vec<crate::test_runner::AssertionResult>,
    ) {
        self.assertion_results = results;
    }

    /// Get the lab name.
    pub fn name(&self) -> &str {
        &self.topology.lab.name
    }

    /// Plan 159b — return the bare-namespace name for a node,
    /// or `None` when the node runs as a container. Used by the
    /// `watch` command to open name-based netlink subscriptions.
    pub fn namespace_name_of(&self, node: &str) -> Option<&str> {
        self.namespace_names.get(node).map(String::as_str)
    }

    /// Plan 159b Phase 4 — return the resolver shape needed to
    /// open a netlink connection inside a node's namespace.
    /// Bare namespaces use a name-based path
    /// (`/var/run/netns/<name>`); container namespaces use the
    /// init PID's `/proc/<pid>/ns/net`. Returns `None` if the
    /// node isn't running.
    /// The named firewall counters of `node` (`counter name …` in its
    /// firewall), read from its `nlink-lab` nftables table. Empty when the
    /// node has none (#166).
    pub async fn firewall_counters(&self, node: &str) -> Result<Vec<FirewallCounter>> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?;
        read_firewall_counters(&ns).await
    }

    /// [`firewall_counters`](Self::firewall_counters) for synchronous code
    /// (the assertion engine): on a thread of its own with its own runtime,
    /// so it works inside or outside one.
    pub fn firewall_counters_blocking(&self, node: &str) -> Result<Vec<FirewallCounter>> {
        self.read_node_blocking(node, async |ns| read_firewall_counters(&ns).await)
    }

    /// Run `read` against `node`'s namespace on a thread and runtime of its
    /// own, for the sync callers (assertions, `impair --show`).
    fn read_node_blocking<T: Send + 'static>(
        &self,
        node: &str,
        read: impl AsyncFnOnce(crate::deploy::NsRef) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?;
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::deploy_failed(format!("reader runtime: {e}")))?
                .block_on(read(ns))
        })
        .join()
        .map_err(|_| Error::deploy_failed("the reader thread panicked".to_string()))?
    }

    /// What `impair --show` reports for `endpoint`: its root qdisc, read
    /// over netlink (#179). `None` when it has the kernel's default.
    pub fn impair_show_blocking(
        &self,
        endpoint: &str,
    ) -> Result<Option<crate::impair_parse::ImpairShow>> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        let iface = ep.iface.clone();
        self.read_node_blocking(&ep.node, async move |ns| {
            let conn: Connection<Route> = ns
                .connection()
                .map_err(|e| Error::nlink("qdisc reader", e))?;
            let root = conn
                .get_root_qdisc_by_name(iface.as_str())
                .await
                .map_err(|e| Error::nlink("qdisc reader", e))?;
            Ok(root
                .as_ref()
                .and_then(crate::impair_parse::impair_show_from))
        })
    }

    /// Open a TCP connection to `addr` from inside `node` and close it
    /// (`validate { tcp-connect … }`, `wait-for --tcp`). A socket made in
    /// the node's namespace, so the node needs no shell (#179).
    pub fn tcp_connect(
        &self,
        node: &str,
        addr: std::net::SocketAddr,
        timeout: std::time::Duration,
    ) -> Result<()> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?;
        let netns = ns
            .netns()
            .map_err(|e| Error::nlink(format!("namespace of '{node}'"), e))?;
        netns
            .run(|| std::net::TcpStream::connect_timeout(&addr, timeout).map(drop))
            .map_err(|e| Error::nlink(format!("namespace of '{node}'"), e))?
            .map_err(Error::Io)
    }

    /// The routes in `node`'s main table to exactly `dst`/`prefix_len` (what
    /// `ip route show PREFIX` lists), read over netlink (#179). Each is its
    /// hops: its gateway and device, one per nexthop for a multipath route.
    pub fn main_routes_to_blocking(
        &self,
        node: &str,
        dst: std::net::IpAddr,
        prefix_len: u8,
    ) -> Result<Vec<Vec<RouteHop>>> {
        self.read_node_blocking(node, async move |ns| {
            read_main_routes_to(&ns, dst, prefix_len).await
        })
    }

    /// Whether the lab changes `node`'s traffic control: always for a node
    /// of its own, for a foreign one with `manage tc` (#344).
    fn tc_is_ours(&self, node: &str) -> Result<()> {
        match self
            .topology
            .nodes
            .get(node)
            .and_then(|n| n.foreign.as_ref())
        {
            Some(f) if !f.manage_tc => Err(crate::foreign::refuse(
                node,
                "change traffic control (without `manage tc`)",
            )),
            _ => Ok(()),
        }
    }

    /// A foreign node's namespace, found again (#344): `None` for a node
    /// of the lab's own.
    pub(crate) fn foreign_ns(&self, node: &str) -> Option<Result<crate::deploy::NsRef>> {
        let foreign = self.topology.nodes.get(node)?.foreign.as_ref()?;
        Some(crate::foreign::resolve(
            node,
            foreign,
            self.runtime_binary.as_deref(),
        ))
    }

    pub fn ns_resolver_of(&self, node: &str) -> Option<crate::deploy::NsRef> {
        if let Some(ns) = self.foreign_ns(node) {
            return ns.inspect_err(|e| tracing::warn!("{e}")).ok();
        }
        if let Some(name) = self.namespace_names.get(node) {
            return Some(crate::deploy::NsRef::Named { name: name.clone() });
        }
        if let Some(state) = self.containers.get(node) {
            return Some(crate::deploy::NsRef::Container {
                id: state.id.clone(),
                pid: state.pid,
            });
        }
        None
    }

    /// Whether DNS hosts entries were injected into /etc/hosts.
    pub fn dns_injected(&self) -> bool {
        self.dns_injected
    }

    /// Whether the lab made Wi-Fi radios (the name predates them: it
    /// once meant that the lab loaded `mac80211_hwsim`).
    pub fn wifi_loaded(&self) -> bool {
        self.wifi_loaded
    }

    /// Get the number of nodes (namespaces + containers).
    pub fn namespace_count(&self) -> usize {
        self.namespace_names.len() + self.containers.len()
    }

    /// Get node names: the lab's namespaces and containers, then its
    /// foreign nodes (#344).
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.namespace_names
            .keys()
            .chain(self.containers.keys())
            .chain(
                self.topology
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.is_foreign())
                    .map(|(name, _)| name),
            )
            .map(|s| s.as_str())
    }

    /// Look up the namespace name for a node.
    /// Get the namespace name for a bare namespace node.
    pub fn namespace_for(&self, node: &str) -> Result<&str> {
        if self
            .topology
            .nodes
            .get(node)
            .is_some_and(|n| n.is_foreign())
        {
            return Err(crate::foreign::refuse(node, "do that"));
        }
        self.namespace_names
            .get(node)
            .map(|s| s.as_str())
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })
    }

    /// Access namespace names map (crate-internal, used by apply_diff).
    pub(crate) fn namespace_names(&self) -> &BTreeMap<String, String> {
        &self.namespace_names
    }

    /// All `(namespace_name, interface_name)` pairs suitable for
    /// per-interface capture / diagnostics. Used by
    /// [`crate::test_helpers::LabCapture`] to spin up parallel
    /// pcaps. Skips container nodes — they need a different
    /// capture path that handles the runtime's network model.
    pub fn capture_targets(&self) -> Vec<(String, String)> {
        use crate::types::EndpointRef;
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut out: Vec<(String, String)> = Vec::new();

        // Walk every link's endpoints.
        for link in &self.topology.links {
            for ep_str in &link.endpoints {
                let Some(ep) = EndpointRef::parse(ep_str) else {
                    continue;
                };
                let Some(ns) = self.namespace_names.get(&ep.node) else {
                    continue;
                };
                let key = (ns.clone(), ep.iface.clone());
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        // Walk every shared-network member.
        for network in self.topology.networks.values() {
            for member in &network.members {
                let Some(ep) = EndpointRef::parse(member) else {
                    continue;
                };
                let Some(ns) = self.namespace_names.get(&ep.node) else {
                    continue;
                };
                let key = (ns.clone(), ep.iface.clone());
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        out
    }

    /// Get the container state for a container node, if it is one.
    pub fn container_for(&self, node: &str) -> Option<&ContainerState> {
        self.containers.get(node)
    }

    /// Access container states map.
    pub fn containers(&self) -> &BTreeMap<String, ContainerState> {
        &self.containers
    }

    /// Tracked background PIDs as `(node, pid)` pairs: `spawn`ed
    /// processes, background `run` blocks, Wi-Fi and FRR daemons.
    pub fn pids(&self) -> &[(String, u32)] {
        &self.pids
    }

    /// Recorded start times of tracked PIDs (see [`LabState::starttimes`]).
    pub(crate) fn starttimes(&self) -> &BTreeMap<u32, u64> {
        &self.starttimes
    }

    /// Record PID start times captured at deploy time.
    /// Background `exec` block (`"<node>:<index>"`) → pid.
    pub fn exec_pids(&self) -> &BTreeMap<String, u32> {
        &self.exec_pids
    }

    pub(crate) fn set_exec_pids(&mut self, exec_pids: BTreeMap<String, u32>) {
        self.exec_pids = exec_pids;
    }

    /// Move a spawned pid into the node's cgroup when the node declares
    /// `cpu`/`memory` (issue #66). Best effort.
    fn attach_cgroup(&self, node: &str, pid: u32) {
        let Some(n) = self.topology.nodes.get(node) else {
            return;
        };
        match crate::cgroup::ensure_node(self.name(), node, n.cpu.as_deref(), n.memory.as_deref()) {
            Ok(Some(dir)) => {
                if let Err(e) = crate::cgroup::attach(&dir, pid) {
                    tracing::warn!(
                        "node '{node}': cannot move pid {pid} into {}: {e}",
                        dir.display()
                    );
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("node '{node}': cgroup limits: {e}"),
        }
    }

    pub(crate) fn set_starttimes(&mut self, starttimes: BTreeMap<u32, u64>) {
        self.starttimes = starttimes;
    }

    /// node → mgmt veth peer name created by deploy (see [`LabState::mgmt_peers`]).
    pub(crate) fn mgmt_peers(&self) -> &std::collections::BTreeMap<String, String> {
        &self.mgmt_peers
    }

    /// Record the mgmt veth peers created by deploy.
    pub(crate) fn set_mgmt_peers(&mut self, peers: std::collections::BTreeMap<String, String>) {
        self.mgmt_peers = peers;
    }

    /// RTNETLINK connection into a node's network namespace — bare
    /// namespace or container (via its init PID). Impairment, partition
    /// and heal used to go through `namespace_for`, which only knows
    /// namespace nodes, so container nodes got `NodeNotFound` (issue #39).
    pub(crate) fn route_conn_for(&self, node: &str) -> Result<Connection<Route>> {
        self.ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?
            .connection()
            .map_err(|e| Error::nlink(format!("connection for '{node}'"), e))
    }

    /// Replace the process/log/mgmt bookkeeping after an `apply`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn absorb_apply(
        &mut self,
        namespace_names: BTreeMap<String, String>,
        containers: BTreeMap<String, ContainerState>,
        pids: Vec<(String, u32)>,
        starttimes: BTreeMap<u32, u64>,
        exec_pids: BTreeMap<String, u32>,
        process_logs: BTreeMap<u32, (String, String)>,
        process_tags: BTreeMap<u32, String>,
        mgmt_peers: BTreeMap<String, String>,
        dns_injected: bool,
        wifi_loaded: bool,
    ) {
        self.namespace_names = namespace_names;
        self.containers = containers;
        self.pids = pids;
        self.starttimes = starttimes;
        self.exec_pids = exec_pids;
        self.process_logs = process_logs;
        self.process_tags = process_tags;
        self.mgmt_peers = mgmt_peers;
        self.dns_injected = dns_injected;
        self.wifi_loaded = wifi_loaded;
    }

    pub(crate) fn set_process_logs(&mut self, logs: BTreeMap<u32, (String, String)>) {
        self.process_logs = logs;
    }

    pub(crate) fn set_process_tags(&mut self, tags: BTreeMap<u32, String>) {
        self.process_tags = tags;
    }

    pub(crate) fn process_tags_map(&self) -> &BTreeMap<u32, String> {
        &self.process_tags
    }

    pub(crate) fn process_logs_map(&self) -> &BTreeMap<u32, (String, String)> {
        &self.process_logs
    }

    /// Impairments set at runtime with [`set_impairment`](Self::set_impairment),
    /// keyed by endpoint. They override the topology's `impair` until the
    /// topology changes that endpoint (see [`crate::apply`]).
    pub fn live_impairments(&self) -> &BTreeMap<String, crate::types::Impairment> {
        &self.live_impairments
    }

    /// Replace the runtime-impairment record (used by `apply` to drop
    /// entries the new topology overrides, or to reset them all).
    pub(crate) fn set_live_impairments(
        &mut self,
        live: BTreeMap<String, crate::types::Impairment>,
    ) {
        self.live_impairments = live;
    }

    /// Partitioned endpoints (pre-partition impairment per endpoint).
    pub fn partitions(&self) -> &BTreeMap<String, crate::types::Impairment> {
        &self.saved_impairments
    }

    #[cfg(test)]
    pub(crate) fn set_partitions(&mut self, map: BTreeMap<String, crate::types::Impairment>) {
        self.saved_impairments = map;
    }

    /// Forget the partitions `apply` just converged over (their 100%
    /// loss qdisc was replaced by the plan), keeping the rest.
    pub(crate) fn retain_partitions(&mut self, keep: impl Fn(&str) -> bool) {
        self.saved_impairments.retain(|ep, _| keep(ep));
    }

    /// Track a freshly spawned background process: its PID and the start
    /// time that proves the PID still belongs to it later.
    fn track_pid(&mut self, node: &str, pid: u32) {
        self.pids.push((node.to_string(), pid));
        if let Some(st) = host_starttime(pid) {
            self.starttimes.insert(pid, st);
        }
    }

    /// Runtime binary (docker or podman).
    pub fn runtime_binary(&self) -> Option<&str> {
        self.runtime_binary.as_deref()
    }

    /// The lab's container runtime: the one it was deployed with, else
    /// docker. It was `runtime_binary().unwrap_or("docker")` in four
    /// places (#234).
    pub fn runtime(&self) -> crate::container::Runtime {
        crate::container::Runtime::with_binary(self.runtime_binary().unwrap_or("docker"))
    }

    /// Replace the topology (crate-internal, used after apply).
    pub(crate) fn set_topology(&mut self, topology: Topology) {
        self.topology = topology;
    }

    /// Execute a command in a lab node and collect output.
    pub fn exec(&self, node: &str, cmd: &str, args: &[&str]) -> Result<ExecOutput> {
        self.exec_with_opts(node, cmd, args, ExecOpts::default())
    }

    /// Execute with a working directory only. Thin wrapper over
    /// [`exec_with_opts`](Self::exec_with_opts).
    pub fn exec_in(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
        workdir: Option<&std::path::Path>,
    ) -> Result<ExecOutput> {
        self.exec_with_opts(
            node,
            cmd,
            args,
            ExecOpts {
                workdir,
                ..Default::default()
            },
        )
    }

    /// Execute a command in a lab node with full control over workdir + env.
    ///
    /// See [`ExecOpts`] for semantics. For namespace nodes, env vars are
    /// applied via `Command::env` (additive — inherited environment is
    /// preserved). For container nodes, env vars are passed as repeated
    /// `-e KEY=VALUE` to the runtime.
    pub fn exec_with_opts(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
        opts: ExecOpts<'_>,
    ) -> Result<ExecOutput> {
        let mode = ExecMode {
            // tagged when it may have to be stopped: killing the runtime's
            // client at the timeout left the command running (#226)
            stoppable: opts.timeout.is_some(),
            ..Default::default()
        };
        let mut prepared = self.prepare_exec(node, cmd, args, &opts, mode)?;
        // stdin closed, with a timeout or without: it read the caller's
        // stdin (#226)
        prepared
            .command()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let running = prepared.spawn()?;
        match opts.timeout {
            Some(timeout) => {
                let stop = running.stopper();
                output_with_timeout(running.child, timeout).inspect_err(|e| {
                    if matches!(e, Error::Timeout(_)) {
                        stop.stop();
                    }
                })
            }
            None => {
                let output = running.child.wait_with_output().map_err(Error::Io)?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: exit_code_of(output.status),
                })
            }
        }
    }

    /// The command that runs `cmd args` in `node` with `opts`, not started
    /// yet: through the runtime's `exec` on a container node, entering the
    /// node's network namespace (with its `/etc/netns` overlay where the
    /// host allows one, as `ip netns exec` does) on a namespace node. Set
    /// its stdio with [`PreparedExec::command`], then spawn it.
    ///
    /// `exec`, the attached exec and the daemon's streaming exec and shell
    /// all start their commands through it.
    pub fn prepare_exec(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
        opts: &ExecOpts<'_>,
        mode: ExecMode,
    ) -> Result<PreparedExec> {
        let what = if mode.stdin { "attached exec" } else { "exec" };
        // a foreign node's namespace, found again; a command run in a
        // foreign container is still ours to stop (#344)
        let foreign = self.foreign_ns(node).transpose()?;
        let foreign_container = match &foreign {
            Some(crate::deploy::NsRef::Container { id, pid }) => Some(ContainerState {
                id: id.clone(),
                name: id.clone(),
                image: String::new(),
                pid: *pid,
                starttime: None,
            }),
            _ => None,
        };
        if let Some(container) = foreign_container.as_ref().or(self.containers.get(node)) {
            let detected;
            let rt_binary = match self.runtime_binary.as_deref() {
                Some(bin) => bin,
                None if foreign_container.is_some() => {
                    detected = crate::container::Runtime::detect()?;
                    detected.binary()
                }
                None => {
                    return Err(Error::deploy_failed("no container runtime binary in state"));
                }
            };
            let mut command = std::process::Command::new(rt_binary);
            command.arg("exec");
            if mode.stdin {
                command.arg("-i");
            }
            if mode.tty {
                command.arg("-t");
            }
            if let Some(wd) = opts.workdir {
                command.arg("-w").arg(wd);
            }
            for (k, v) in opts.env {
                command.arg("-e").arg(format!("{k}={v}"));
            }
            let stop = mode.stoppable.then(|| {
                let tag = new_process_tag(self.name(), node);
                command.arg("-e").arg(format!("{TAG_VAR}={tag}"));
                (container.pid, tag)
            });
            command.arg(&container.id).arg(cmd).args(args);
            Ok(PreparedExec {
                command,
                netns: None,
                stop,
                label: format!("{what} in container '{node}'"),
            })
        } else {
            let ns = match foreign {
                Some(ns) => ns,
                None => crate::deploy::NsRef::Named {
                    name: self.namespace_for(node)?.to_string(),
                },
            };
            let mut command = std::process::Command::new(cmd);
            command.args(args);
            if let Some(wd) = opts.workdir {
                command.current_dir(wd);
            }
            for (k, v) in opts.env {
                command.env(k, v);
            }
            Ok(PreparedExec {
                command,
                netns: Some(ns),
                stop: None,
                label: format!("{what} in '{node}'"),
            })
        }
    }

    /// Execute a command in a lab node and inherit stdio so stdout/stderr
    /// stream live to the caller's terminal.
    ///
    /// Use this for commands that produce output over time (a service, a
    /// `tail -f`, a `ping`) — the buffered [`exec`] path only prints after
    /// the child exits. Returns the child's exit code; output is not
    /// captured.
    ///
    /// [`exec`]: Self::exec
    pub fn exec_attached(&self, node: &str, cmd: &str, args: &[&str]) -> Result<i32> {
        self.exec_attached_with_opts(node, cmd, args, ExecOpts::default())
    }

    /// Streaming exec with a working directory only. Thin wrapper over
    /// [`exec_attached_with_opts`](Self::exec_attached_with_opts).
    pub fn exec_attached_in(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
        workdir: Option<&std::path::Path>,
    ) -> Result<i32> {
        self.exec_attached_with_opts(
            node,
            cmd,
            args,
            ExecOpts {
                workdir,
                ..Default::default()
            },
        )
    }

    /// Streaming exec with full options (workdir + env). See [`ExecOpts`].
    pub fn exec_attached_with_opts(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
        opts: ExecOpts<'_>,
    ) -> Result<i32> {
        let mode = ExecMode {
            stdin: true,
            stoppable: opts.timeout.is_some(),
            ..Default::default()
        };
        let mut prepared = self.prepare_exec(node, cmd, args, &opts, mode)?;
        prepared
            .command()
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let label = prepared.label.clone();
        let running = prepared.spawn()?;
        let stop = running.stopper();
        wait_with_optional_timeout(running.child, opts.timeout).map_err(|e| match e {
            Error::Timeout(_) => {
                stop.stop();
                e
            }
            other => Error::deploy_failed(format!("{label} failed: {other}")),
        })
    }

    /// Spawn a background process in a lab node: [`spawn_with_logs`]
    /// into the lab's log directory, so `ps` reports its exit code and
    /// `logs` its output (#320).
    ///
    /// [`spawn_with_logs`]: Self::spawn_with_logs
    pub fn spawn(&mut self, node: &str, cmd: &[&str]) -> Result<u32> {
        self.spawn_with_logs_with_opts(node, cmd, SpawnOpts::default())
    }

    /// Re-save the current state to disk (e.g., after spawning a new process).
    ///
    /// Only what this handle changed since it was loaded (or last saved)
    /// is written, merged into the state file as it is now: a process it
    /// spawned, one it killed, an impairment it set. A concurrent command
    /// that saved meanwhile keeps its own changes: they used to be
    /// overwritten with this handle's stale copy, nodes an `apply` added
    /// included.
    pub fn save_state(&self) -> Result<()> {
        // Take turns with a concurrent deploy/apply/destroy: this is a
        // read-modify-write of state.json (issue #38).
        let lock = crate::state::lock_blocking(&self.topology.lab.name)?;
        self.save_merged(&lock, None)
    }

    /// Start a change to this lab (#332): wait for its lock (up to
    /// [`state::lock_timeout`], then [`Error::Busy`] naming the holder),
    /// and take in what other processes saved since this handle last
    /// read or wrote the state, keeping what this handle changed. One
    /// mutation per lab runs at a time, whichever process makes it: two
    /// CLIs, a CLI and the daemon, a scenario and `top`.
    pub(crate) async fn begin(&mut self, op: &str) -> Result<Mutation> {
        let lock =
            crate::state::lock_wait(&self.topology.lab.name, op, crate::state::lock_timeout())
                .await?;
        self.locked(lock)
    }

    /// [`begin`](Self::begin) for a synchronous caller.
    pub(crate) fn begin_blocking(&mut self, op: &str) -> Result<Mutation> {
        let lock = crate::state::lock_wait_blocking(
            &self.topology.lab.name,
            op,
            crate::state::lock_timeout(),
        )?;
        self.locked(lock)
    }

    fn locked(&mut self, lock: crate::state::LabLock) -> Result<Mutation> {
        let name = self.topology.lab.name.clone();
        if !state::exists(&name) {
            // destroyed while this waited: its lock file went with it, and
            // this attempt made a new one
            lock.remove_file();
            return Err(Error::NotFound { name });
        }
        self.refresh(&lock)?;
        Ok(Mutation { lock })
    }

    /// Save the mutation [`begin`](Self::begin) started and let go of the
    /// lab's lock.
    pub(crate) fn commit(&self, m: Mutation) -> Result<()> {
        self.save_merged(&m.lock, None)
    }

    /// Take in the state file as it is now, under the lab's lock: the
    /// reverse of [`save_merged`](Self::save_merged)'s merge. What this
    /// handle changed since its baseline stays (a spawned pid a test body
    /// never saved, a topology an edit changed); everything else comes
    /// from the file. The baseline becomes the file, so the next save
    /// writes this handle's changes.
    fn refresh(&mut self, _lock: &state::LabLock) -> Result<()> {
        let (mut fresh, fresh_topology) = state::load(&self.topology.lab.name)?;
        let disk = Baseline::of_state(&fresh, &fresh_topology);
        let mine = self.snapshot();
        let mut base = self.baseline.lock().unwrap_or_else(|p| p.into_inner());
        merge_into(&mut fresh, &base, &mine);
        if mine.topology == base.topology {
            self.topology = fresh_topology;
        }
        zero_stale_container_pids(&mut fresh.containers);
        self.namespace_names = fresh.namespaces;
        self.containers = fresh.containers;
        self.runtime_binary = fresh.runtime;
        self.dns_injected = fresh.dns_injected;
        self.wifi_loaded = fresh.wifi_loaded;
        self.mgmt_peers = fresh.mgmt_peers;
        self.pids = fresh.pids;
        self.starttimes = fresh.starttimes;
        self.exec_pids = fresh.exec_pids;
        self.saved_impairments = fresh.saved_impairments;
        self.live_impairments = fresh.live_impairments;
        self.process_logs = fresh.process_logs;
        self.process_tags = fresh.process_tags;
        *base = disk;
        Ok(())
    }

    /// The merge [`save_state`](Self::save_state) does, for a caller that
    /// holds the lab's lock (the token). `topology` is written as given;
    /// `None` writes this handle's when it changed one, else keeps the
    /// file's.
    pub(crate) fn save_merged(
        &self,
        _lock: &state::LabLock,
        topology: Option<&Topology>,
    ) -> Result<()> {
        let (mut fresh, fresh_topology) = state::load(&self.topology.lab.name)?;
        let mine = self.snapshot();
        // Written by a newer nlink-lab: rewriting it at this schema drops
        // the fields this one does not know (#221).
        if fresh.schema_version > state::SCHEMA_VERSION {
            return Err(Error::invalid_argument(format!(
                "lab '{}' was deployed by a newer nlink-lab (state schema {}, this one writes {}): \
                 use that version to change it",
                self.topology.lab.name,
                fresh.schema_version,
                state::SCHEMA_VERSION
            )));
        }
        let mut base = self.baseline.lock().unwrap_or_else(|p| p.into_inner());
        fresh.schema_version = state::SCHEMA_VERSION;
        merge_into(&mut fresh, &base, &mine);
        let topology = match topology {
            Some(t) => t,
            None if mine.topology != base.topology => &self.topology,
            None => &fresh_topology,
        };
        state::save(&fresh, topology)?;
        *base = mine;
        Ok(())
    }

    /// Record a container node's new init PID (after `restart`) so every
    /// later `/proc/<pid>/ns/net` reference targets the live container.
    pub fn set_container_pid(&mut self, node: &str, pid: u32) -> Result<()> {
        match self.containers.get_mut(node) {
            Some(c) => {
                c.pid = pid;
                c.starttime = host_starttime(pid);
                Ok(())
            }
            None => Err(Error::NodeNotFound {
                name: node.to_string(),
            }),
        }
    }

    /// Spawn a background process with stdout/stderr captured to log files.
    pub fn spawn_with_logs(
        &mut self,
        node: &str,
        cmd: &[&str],
        log_dir: Option<&std::path::Path>,
    ) -> Result<u32> {
        self.spawn_with_logs_with_opts(
            node,
            cmd,
            SpawnOpts {
                log_dir,
                ..Default::default()
            },
        )
    }

    /// Spawn with a working directory in addition to the log directory.
    /// Thin wrapper over
    /// [`spawn_with_logs_with_opts`](Self::spawn_with_logs_with_opts).
    pub fn spawn_with_logs_in(
        &mut self,
        node: &str,
        cmd: &[&str],
        log_dir: Option<&std::path::Path>,
        workdir: Option<&std::path::Path>,
    ) -> Result<u32> {
        self.spawn_with_logs_with_opts(
            node,
            cmd,
            SpawnOpts {
                log_dir,
                workdir,
                ..Default::default()
            },
        )
    }

    /// Spawn a background process with full control over log dir, working
    /// directory, and environment. See [`SpawnOpts`]. The log file basename
    /// is derived from `cmd[0]` after `Path::file_name()` — env vars are
    /// applied via `Command::env` and do **not** affect the basename.
    pub fn spawn_with_logs_with_opts(
        &mut self,
        node: &str,
        cmd: &[&str],
        opts: SpawnOpts<'_>,
    ) -> Result<u32> {
        if cmd.is_empty() {
            return Err(Error::invalid_topology("empty command"));
        }
        if self
            .topology
            .nodes
            .get(node)
            .is_some_and(|n| n.is_foreign())
        {
            return Err(crate::foreign::refuse(node, "spawn or track processes"));
        }
        // a new process of the lab, saved with it: one change at a time,
        // whichever process makes it (#332)
        let m = self.begin_blocking("spawn")?;
        // A container node: the process is `<runtime> exec <id> <cmd>` run on
        // the host, exactly as a deploy-time `run ... background` does (#112),
        // so its output lands in the lab's log dir like any other process.
        // The tracked pid is the runtime client's; its exit code is the
        // command's.
        let container = self.containers.get(node).map(|c| c.id.clone());
        let ns_name = if container.is_some() {
            String::new()
        } else {
            self.namespace_for(node)?.to_string()
        };

        // Absolute: the paths are stored in the state file, and the
        // exit-code reaper runs after `--workdir`'s chdir. A relative
        // `--log-dir` broke `logs` and `ps` from any other directory (#209).
        let log_dir = match opts.log_dir {
            Some(dir) => std::path::absolute(dir)?,
            None => state::logs_dir(self.name()),
        };
        if opts.log_dir.is_some() {
            std::fs::create_dir_all(&log_dir)?;
        } else {
            state::ensure_dir(&log_dir)?;
        }

        let cmd_basename = std::path::Path::new(cmd[0])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("cmd");

        // A name of its own until the pid is known: two spawns of the same
        // command on one node used to open (and truncate) the same file,
        // and the second one's rename then failed silently.
        let temp_log = |ext: &str| -> Result<(std::fs::File, std::path::PathBuf)> {
            tempfile::Builder::new()
                .prefix(&format!("{node}-{cmd_basename}."))
                .suffix(&format!(".{ext}"))
                .tempfile_in(&log_dir)?
                .keep()
                .map_err(|e| Error::Io(e.error))
        };
        let (stdout_file, stdout_path) = temp_log("stdout")?;
        let (stderr_file, stderr_path) = temp_log("stderr")?;
        // what finds the command inside the container (#215)
        let tag = container
            .as_ref()
            .map(|_| new_process_tag(&self.topology.lab.name, node));

        let mut command = if let Some(id) = &container {
            let rt = self
                .runtime_binary
                .clone()
                .unwrap_or_else(|| "docker".to_string());
            let mut c = std::process::Command::new(rt);
            c.arg("exec");
            if let Some(wd) = opts.workdir {
                c.arg("-w").arg(wd);
            }
            for (k, v) in opts.env {
                c.arg("-e").arg(format!("{k}={v}"));
            }
            c.arg("-e")
                .arg(format!("{TAG_VAR}={}", tag.as_deref().unwrap_or_default()));
            c.arg(id);
            c.args(cmd);
            c
        } else {
            let mut c = std::process::Command::new(cmd[0]);
            c.args(&cmd[1..]);
            if let Some(wd) = opts.workdir {
                c.current_dir(wd);
            }
            for (k, v) in opts.env {
                c.env(k, v);
            }
            c
        };
        command.stdout(stdout_file);
        command.stderr(stderr_file);

        // The reaper writes `<log_dir>/<node>-<basename>-<pid>.rc` when the
        // process exits: the only source of a detached process's exit code.
        let rc_prefix = log_dir.join(format!("{node}-{cmd_basename}-"));
        let pid = if container.is_some() {
            crate::ns_exec::spawn_detached_path_reaped(
                std::path::Path::new("/proc/self/ns/net"),
                command,
                &rc_prefix,
            )
        } else {
            crate::ns_exec::spawn_detached_reaped(&ns_name, command, &rc_prefix)
        }
        .map_err(|e| Error::nlink(format!("spawn in '{node}' failed"), e))?;
        self.track_pid(node, pid);
        self.attach_cgroup(node, pid);
        self.process_logs.insert(
            pid,
            (
                stdout_path.to_string_lossy().to_string(),
                stderr_path.to_string_lossy().to_string(),
            ),
        );

        // Rename files to include PID; where a rename fails, the log is
        // recorded where it is.
        let rename =
            |from: &std::path::Path, to: std::path::PathBuf| match std::fs::rename(from, &to) {
                Ok(()) => to,
                Err(e) => {
                    tracing::warn!(
                        "keeping log {}: rename to {}: {e} (`ps` will not show its exit code)",
                        from.display(),
                        to.display()
                    );
                    from.to_path_buf()
                }
            };
        let final_stdout = rename(
            &stdout_path,
            log_dir.join(format!("{node}-{cmd_basename}-{pid}.stdout")),
        );
        let final_stderr = rename(
            &stderr_path,
            log_dir.join(format!("{node}-{cmd_basename}-{pid}.stderr")),
        );
        self.process_logs.insert(
            pid,
            (
                final_stdout.to_string_lossy().to_string(),
                final_stderr.to_string_lossy().to_string(),
            ),
        );
        if let Some(tag) = tag {
            self.process_tags.insert(pid, tag);
        }
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::Spawned {
                node: node.to_string(),
                pid,
                cmd: cmd.join(" "),
            },
        );
        self.commit(m)?;
        Ok(pid)
    }

    /// Get log file paths for a tracked process.
    pub fn log_paths(&self, pid: u32) -> Option<(&str, &str)> {
        self.process_logs
            .get(&pid)
            .map(|(stdout, stderr)| (stdout.as_str(), stderr.as_str()))
    }

    /// Collect all IP addresses for a node, grouped by interface name.
    pub fn node_addresses(
        &self,
        node: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
        // Verify node exists: a namespace node or a container node.
        if !self.topology.nodes.contains_key(node) {
            return Err(Error::NodeNotFound {
                name: node.to_string(),
            });
        }

        let mut addrs: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();

        // From links
        for link in &self.topology.links {
            for (i, ep_str) in link.endpoints.iter().enumerate() {
                if let Some(ep) = EndpointRef::parse(ep_str)
                    && ep.node == node
                    && let Some(ref link_addrs) = link.addresses
                {
                    addrs
                        .entry(ep.iface.to_string())
                        .or_default()
                        .push(link_addrs[i].clone());
                }
            }
        }

        // From node interfaces (loopback, vxlan, bond, etc.)
        if let Some(n) = self.topology.nodes.get(node) {
            for (iface_name, iface_cfg) in &n.interfaces {
                for addr in &iface_cfg.addresses {
                    addrs
                        .entry(iface_name.clone())
                        .or_default()
                        .push(addr.clone());
                }
            }
        }

        // From network bridge port addresses (subnet auto-allocation)
        for network in self.topology.networks.values() {
            for member in &network.members {
                if let Some(ep) = EndpointRef::parse(member)
                    && ep.node == node
                {
                    // Port keys can be either "node:iface" or "node"
                    let port = network
                        .ports
                        .get(member)
                        .or_else(|| network.ports.get(&ep.node));
                    if let Some(port) = port {
                        for addr in &port.addresses {
                            addrs
                                .entry(ep.iface.to_string())
                                .or_default()
                                .push(addr.clone());
                        }
                    }
                }
            }
        }

        // From the management network (mgmt0), host-reachable or not
        if let Some(ref mgmt_subnet) = self.topology.lab.mgmt_subnet {
            // Nodes get the 2nd, 3rd, ... host address in name order, over
            // every node as deploy numbers them, containers included: over
            // the namespace nodes alone, a lab with a container reported
            // another node's address.
            let sorted_nodes: Vec<&str> = self.topology.nodes.keys().map(String::as_str).collect();
            if let Some(idx) = sorted_nodes.iter().position(|&n| n == node)
                && let Ok(mgmt) = crate::helpers::mgmt_addresses(mgmt_subnet, sorted_nodes.len())
            {
                addrs
                    .entry("mgmt0".to_string())
                    .or_default()
                    .push(format!("{}/{}", mgmt.nodes[idx], mgmt.prefix));
            }
        }

        Ok(addrs)
    }

    /// A sock_diag connection into a node's namespace (#75: listener
    /// probes ask the kernel instead of shelling `bash`/`cat`).
    fn sockdiag_conn_for(&self, node: &str) -> Result<Connection<nlink::netlink::SockDiag>> {
        if let Some(container) = self.containers.get(node) {
            return Connection::new_in(nlink::netlink::namespace::NetnsRef::Pid(container.pid))
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "sock_diag connection for container node '{node}' (pid {}): {e}",
                        container.pid
                    ))
                });
        }
        let ns_name = self.namespace_for(node)?;
        Connection::new_in(ns_name)
            .map_err(|e| Error::nlink(format!("sock_diag connection for '{ns_name}'"), e))
    }

    /// Is there a TCP listener on `port` in `node`'s namespace bound to
    /// `ip` (or to the wildcard address)? `Ok(None)` when sock_diag is
    /// unavailable (a hardened container runtime), so callers can fall
    /// back to procfs/exec probes.
    async fn tcp_listener_present(
        &self,
        node: &str,
        ip: Option<std::net::IpAddr>,
        port: u16,
    ) -> Option<bool> {
        use nlink::sockdiag::SocketFilter;
        let conn = self.sockdiag_conn_for(node).ok()?;
        let filter = SocketFilter::tcp().listening().local_port(port).build();
        let sockets = conn.query(&filter).await.ok()?;
        Some(sockets.iter().any(|s| {
            let Some(inet) = s.as_inet() else {
                return false;
            };
            if inet.local.port() != port {
                return false;
            }
            match ip {
                None => true,
                Some(want) => {
                    let bound = inet.local.ip();
                    bound.is_unspecified() || bound == want
                }
            }
        }))
    }

    /// Wait for a TCP listener on `ip:port` inside a node's namespace.
    ///
    /// Probes the kernel's socket table through sock_diag (no connection
    /// attempts that leave `connection refused` noise in the service's
    /// logs); falls back to connecting, from a socket in the node's
    /// namespace, where sock_diag is not permitted.
    pub async fn wait_for_tcp(
        &self,
        node: &str,
        ip: &str,
        port: u16,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        // An unknown node is an error, not "not listening yet": it used to
        // wait out the whole timeout (#209).
        self.ns_resolver_of(node)
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })?;
        let deadline = std::time::Instant::now() + timeout;
        let want: Option<std::net::IpAddr> = ip.parse().ok();
        loop {
            let present = match self.tcp_listener_present(node, want, port).await {
                Some(present) => present,
                None => want.is_some_and(|ip| {
                    let addr = std::net::SocketAddr::new(ip, port);
                    self.tcp_connect(
                        node,
                        addr,
                        interval.max(std::time::Duration::from_millis(500)),
                    )
                    .is_ok()
                }),
            };
            if present {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(format!(
                    "timeout waiting for {ip}:{port} on node '{node}'"
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait for a command to succeed (exit 0) inside a node's namespace.
    pub async fn wait_for_exec(
        &self,
        node: &str,
        cmd: &str,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // A probe that cannot run (unknown node, no namespace) is an
            // error, not "not ready yet" (#209). It runs in what is left of
            // the timeout: one that hung held the wait past it (#226).
            let left = deadline
                .saturating_duration_since(std::time::Instant::now())
                .max(std::time::Duration::from_millis(100));
            let opts = ExecOpts {
                timeout: Some(left),
                ..Default::default()
            };
            match self.exec_with_opts(node, "sh", &["-c", cmd], opts) {
                Ok(out) if out.exit_code == 0 => return Ok(()),
                Ok(_) | Err(Error::Timeout(_)) => {}
                Err(e) => return Err(e),
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(format!(
                    "timeout waiting for command to succeed on node '{node}': {cmd}"
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait until a tracked spawned process emits a log line matching
    /// `pattern` on the chosen stream(s). Reads from offset 0 each poll
    /// so a line emitted *before* the watcher started is matched too.
    ///
    /// Returns immediately on the first match. On timeout, returns an
    /// error naming the regex source. The poll interval is the minimum
    /// of `interval` and 250ms — enough granularity for spawn-readiness
    /// latency budgets without hammering the filesystem.
    pub async fn wait_for_log_line(
        &self,
        pid: u32,
        pattern: &regex::Regex,
        stream: LogStream,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let (stdout_path, stderr_path) = self
            .log_paths(pid)
            .ok_or_else(|| Error::deploy_failed(format!("no log files tracked for PID {pid}")))?;
        let paths: Vec<&str> = match stream {
            LogStream::Stdout => vec![stdout_path],
            LogStream::Stderr => vec![stderr_path],
            LogStream::Both => vec![stdout_path, stderr_path],
        };

        let deadline = std::time::Instant::now() + timeout;
        let interval = std::cmp::min(interval, std::time::Duration::from_millis(250));
        loop {
            for p in &paths {
                // lossy: one byte that is not UTF-8 made the whole log
                // unreadable, and the wait timed out (#209)
                if let Ok(bytes) = std::fs::read(p)
                    && String::from_utf8_lossy(&bytes)
                        .lines()
                        .any(|line| pattern.is_match(line))
                {
                    return Ok(());
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(format!(
                    "timeout waiting for log line matching '{}' on PID {pid}",
                    pattern.as_str()
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait until a process of `node` (`pid`, when it is given) has a TCP
    /// listener bound to `port` inside its namespace. Asks sock_diag, or
    /// reads `/proc/<pid>/net/tcp{,6}` (`/proc/net/tcp{,6}` without a
    /// `pid`), which reflect the namespace's socket table, for a row with
    /// `st = 0A` (TCP_LISTEN) and the matching local port.
    ///
    /// Cheaper than [`wait_for_tcp`](Self::wait_for_tcp) — no actual
    /// `connect(2)` is attempted, so there's no logged
    /// connection-refused noise on the target side, and binds to
    /// non-routable addresses are observable too. Useful when the
    /// service is up and listening but isn't ready to accept the
    /// test's specific connection yet (e.g. cold-start TLS handshake).
    /// Round-5 §2.4.
    pub async fn wait_for_port(
        &self,
        node: &str,
        pid: Option<u32>,
        port: u16,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let interval = std::cmp::min(interval, std::time::Duration::from_millis(250));
        let port_hex = format!("{port:04X}");
        loop {
            // sock_diag first (#75); the procfs scan stays as the
            // fallback for runtimes that deny NETLINK_SOCK_DIAG.
            match self.tcp_listener_present(node, None, port).await {
                Some(true) => return Ok(()),
                Some(false) => {}
                None => {
                    for proto in &["tcp", "tcp6"] {
                        let path = match pid {
                            Some(pid) => format!("/proc/{pid}/net/{proto}"),
                            None => format!("/proc/net/{proto}"),
                        };
                        if let Ok(out) = self.exec(node, "cat", &[&path])
                            && out.exit_code == 0
                            && proc_net_tcp_has_listener(&out.stdout, &port_hex)
                        {
                            return Ok(());
                        }
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(match pid {
                    Some(pid) => {
                        format!("timeout waiting for TCP listener on port {port} for PID {pid}")
                    }
                    None => {
                        format!("timeout waiting for TCP listener on port {port} on node '{node}'")
                    }
                }));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait until process `pid`'s open-fd count has been stable for
    /// `stable_for`. Heuristic — a process *can* open more files
    /// later, so this isn't a guarantee of readiness. Prefer
    /// [`wait_for_log_line`](Self::wait_for_log_line) or
    /// [`wait_for_port`](Self::wait_for_port) when a deterministic
    /// signal is available. (Round-5 §2.4.)
    ///
    /// `pid` is a PID [`spawn`](Self::spawn) returned: the host's. On a
    /// container node that is the runtime's `exec` client, so its fds
    /// are the client's, not the command's.
    pub async fn wait_for_fd_stable(
        &self,
        _node: &str,
        pid: u32,
        stable_for: std::time::Duration,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let interval = std::cmp::min(interval, std::time::Duration::from_millis(250));
        let mut last_count: Option<u32> = None;
        let mut last_change = std::time::Instant::now();
        // `pid` is what `spawn` returned, a host PID, so it is counted on
        // the host as it is: no exec into the node per poll (#157).
        loop {
            let count = count_fds_on_host(pid)
                .map_err(|e| Error::deploy_failed(format!("list /proc/{pid}/fd: {e}")))?;
            match last_count {
                Some(prev) if prev == count => {
                    if last_change.elapsed() >= stable_for {
                        return Ok(());
                    }
                }
                _ => {
                    last_count = Some(count);
                    last_change = std::time::Instant::now();
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(format!(
                    "timeout waiting for fd-count to stabilise on PID {pid}"
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait for a file to exist inside a node's namespace.
    pub async fn wait_for_file(
        &self,
        node: &str,
        path: &str,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.exec(node, "test", &["-e", path])?.exit_code == 0 {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::TimedOut(format!(
                    "timeout waiting for file '{path}' on node '{node}'"
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Given "nodeA:eth0", find the other end of the link → "nodeB:eth0".
    pub fn peer_endpoint(&self, endpoint: &str) -> Result<String> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        let needle = format!("{}:{}", ep.node, ep.iface);
        for link in &self.topology.links {
            if link.endpoints[0] == needle {
                return Ok(link.endpoints[1].clone());
            }
            if link.endpoints[1] == needle {
                return Ok(link.endpoints[0].clone());
            }
        }
        Err(Error::invalid_argument(format!(
            "no link found for endpoint '{endpoint}'"
        )))
    }

    /// Set the MTU of both ends of the link `endpoint` belongs to, live,
    /// and record it in the topology so `verify` and a later `apply` agree.
    ///
    /// nlink-lab otherwise only sets an MTU when it creates a veth; this is
    /// the runtime path (`edit --set-mtu`) a path-MTU fault needs.
    pub async fn set_link_mtu(&mut self, endpoint: &str, mtu: u32) -> Result<()> {
        // both ends change: a foreign one only when the lab manages it
        // (checked before the lock: a refusal waits for nothing)
        let peer = self.peer_endpoint(endpoint)?;
        for ep in [endpoint, peer.as_str()] {
            if let Some(r) = EndpointRef::parse(ep) {
                self.tc_is_ours(&r.node)?;
            }
        }
        let m = self.begin("mtu").await?;
        let peer = self.peer_endpoint(endpoint)?;
        for ep in [endpoint.to_string(), peer] {
            let r = EndpointRef::parse(&ep).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: ep.clone(),
            })?;
            let ns = self
                .ns_resolver_of(&r.node)
                .ok_or_else(|| Error::NodeNotFound {
                    name: r.node.clone(),
                })?;
            let conn: nlink::Connection<nlink::Route> = ns
                .connection()
                .map_err(|e| Error::deploy_failed(format!("netlink in '{}': {e}", r.node)))?;
            conn.set_link_mtu(r.iface.as_str(), mtu)
                .await
                .map_err(|e| Error::nlink(format!("set mtu {mtu} on {ep}"), e))?;
        }
        let needle = endpoint.to_string();
        for link in &mut self.topology.links {
            if link.endpoints.contains(&needle) {
                link.mtu = Some(mtu);
            }
        }
        self.commit(m)
    }

    /// Impair what `endpoint` sends (`egress`) and what it receives
    /// (`ingress`, installed on the egress of its link's other end), and
    /// return that other end when `ingress` is given. The peer is looked
    /// up first: an endpoint without one (a network member) changes
    /// nothing (#209).
    pub async fn set_directed_impairment(
        &mut self,
        endpoint: &str,
        egress: Option<&crate::types::Impairment>,
        ingress: Option<&crate::types::Impairment>,
    ) -> Result<Option<String>> {
        let peer = match ingress {
            Some(_) => Some(self.peer_endpoint(endpoint)?),
            None => None,
        };
        // one mutation for both ends (#332)
        let m = self.begin("impair").await?;
        if let Some(egress) = egress {
            self.set_impairment_locked(endpoint, egress).await?;
        }
        if let (Some(peer), Some(ingress)) = (&peer, ingress) {
            self.set_impairment_locked(peer, ingress).await?;
        }
        self.commit(m)?;
        Ok(peer)
    }

    /// [`set_impairment`](Self::set_impairment) in `dir`: on what
    /// `endpoint` sends, on what it receives, or both. Returns the peer
    /// endpoint an `in` or `both` impairment went on.
    pub async fn impair(
        &mut self,
        endpoint: &str,
        impairment: &crate::types::Impairment,
        dir: crate::api::Direction,
    ) -> Result<Option<String>> {
        use crate::api::Direction;
        let (egress, ingress) = match dir {
            Direction::Out => (Some(impairment), None),
            Direction::In => (None, Some(impairment)),
            Direction::Both => (Some(impairment), Some(impairment)),
        };
        self.set_directed_impairment(endpoint, egress, ingress)
            .await
    }

    /// Modify the netem impairment on an interface at runtime.
    ///
    /// The value is recorded in the lab state (`live_impairments`) so a
    /// later `apply`, `verify` or `snapshot` sees what is really installed
    /// instead of the topology's declaration (#59). It becomes the
    /// endpoint's root netem, taking over from a `qdisc` block or rate
    /// limit there until cleared; on the source interface of a network's
    /// per-pair matrix it is the matrix's default impairment (#169). A
    /// partitioned endpoint stays partitioned.
    pub async fn set_impairment(
        &mut self,
        endpoint: &str,
        impairment: &crate::types::Impairment,
    ) -> Result<()> {
        let m = self.begin("impair").await?;
        self.set_impairment_locked(endpoint, impairment).await?;
        self.commit(m)
    }

    /// [`set_impairment`](Self::set_impairment) inside a mutation that
    /// has the lab's lock: changes the kernel and this handle, saves
    /// nothing.
    async fn set_impairment_locked(
        &mut self,
        endpoint: &str,
        impairment: &crate::types::Impairment,
    ) -> Result<()> {
        let value = impairment.clone();
        self.change_tc(endpoint, |lab| {
            if lab.saved_impairments.contains_key(endpoint) {
                lab.saved_impairments
                    .insert(endpoint.to_string(), value.clone());
            }
            lab.live_impairments.insert(endpoint.to_string(), value);
        })
        .await?;
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::Impaired {
                endpoint: endpoint.to_string(),
                impairment: Box::new(impairment.clone()),
            },
        );
        Ok(())
    }

    /// Set (or, with `None`, remove) a network's per-pair impairment from
    /// `src` to `dst`, and converge `src`'s node only (#340): what a
    /// replay's ticks need, without planning the whole lab as an `apply`
    /// does. `rate_cap` is the rule's HTB cap; `None` keeps none.
    pub async fn set_pair_impairment(
        &mut self,
        network: &str,
        src: &str,
        dst: &str,
        impairment: Option<&crate::types::Impairment>,
        rate_cap: Option<&str>,
    ) -> Result<()> {
        let m = self.begin("impair-pair").await?;
        self.set_pair_locked(network, src, dst, impairment, rate_cap)
            .await?;
        self.commit(m)
    }

    async fn set_pair_locked(
        &mut self,
        network: &str,
        src: &str,
        dst: &str,
        impairment: Option<&crate::types::Impairment>,
        rate_cap: Option<&str>,
    ) -> Result<()> {
        let net = self
            .topology
            .networks
            .get(network)
            .ok_or_else(|| Error::invalid_argument(format!("no network '{network}'")))?;
        let member = |node: &str| {
            net.members
                .iter()
                .any(|m| EndpointRef::parse(m).is_some_and(|e| e.node == node))
        };
        for node in [src, dst] {
            if !member(node) {
                return Err(Error::invalid_argument(format!(
                    "'{node}' is not a member of network '{network}'"
                )));
            }
        }
        let handle = self
            .ns_resolver_of(src)
            .ok_or_else(|| Error::NodeNotFound {
                name: src.to_string(),
            })?;
        let saved = self.topology.networks[network].impairments.clone();
        let before = crate::deploy::live_view(self);
        let rules = &mut self
            .topology
            .networks
            .get_mut(network)
            .expect("looked up above")
            .impairments;
        rules.retain(|r| !(r.src == src && r.dst == dst));
        if let Some(imp) = impairment {
            rules.push(crate::types::NetworkImpairment {
                src: src.to_string(),
                dst: dst.to_string(),
                impairment: imp.clone(),
                rate_cap: rate_cap.map(str::to_string),
            });
        }
        let after = crate::deploy::live_view(self);
        if let Err(e) = crate::deploy::reconcile_node_tc(
            &handle,
            src,
            (&before.0, &before.1),
            (&after.0, &after.1),
        )
        .await
        {
            self.topology
                .networks
                .get_mut(network)
                .expect("looked up above")
                .impairments = saved;
            return Err(e);
        }
        Ok(())
    }

    /// The endpoints an impairment `dir` of `endpoint` goes on: itself,
    /// its link's other end, or both.
    fn directed_endpoints(
        &self,
        endpoint: &str,
        dir: crate::api::Direction,
    ) -> Result<Vec<String>> {
        use crate::api::Direction;
        Ok(match dir {
            Direction::Out => vec![endpoint.to_string()],
            Direction::In => vec![self.peer_endpoint(endpoint)?],
            Direction::Both => vec![endpoint.to_string(), self.peer_endpoint(endpoint)?],
        })
    }

    /// What `target` has now: where a replay starts from (#340).
    pub(crate) fn replay_base(
        &self,
        target: &crate::replay::ReplayTarget,
    ) -> crate::types::Impairment {
        use crate::replay::ReplayTarget;
        match target {
            ReplayTarget::Endpoint { endpoint, .. } => self
                .live_impairments
                .get(endpoint)
                .or_else(|| self.topology.impairments.get(endpoint))
                .cloned()
                .unwrap_or_default(),
            ReplayTarget::Pair { network, src, dst } => self
                .topology
                .networks
                .get(network)
                .and_then(|n| {
                    n.impairments
                        .iter()
                        .find(|r| r.src == *src && r.dst == *dst)
                })
                .map(|r| r.impairment.clone())
                .unwrap_or_default(),
        }
    }

    /// One tick of a replay (#340): `target` gets `imp`, in the kernel and
    /// in this handle, under the lab's lock; nothing is saved, and no
    /// event is logged. A pair rule with a `rate-cap` takes the trace's
    /// rate as its cap.
    pub(crate) async fn replay_tick(
        &mut self,
        target: &crate::replay::ReplayTarget,
        imp: &crate::types::Impairment,
    ) -> Result<()> {
        use crate::replay::ReplayTarget;
        let m = self.begin("replay").await?;
        match target {
            ReplayTarget::Endpoint { endpoint, dir } => {
                for ep in self.directed_endpoints(endpoint, *dir)? {
                    let value = imp.clone();
                    self.change_tc(&ep, |lab| {
                        if lab.saved_impairments.contains_key(&ep) {
                            lab.saved_impairments.insert(ep.clone(), value.clone());
                        }
                        lab.live_impairments.insert(ep.clone(), value);
                    })
                    .await?;
                }
            }
            ReplayTarget::Pair { network, src, dst } => {
                let cap = self
                    .topology
                    .networks
                    .get(network)
                    .and_then(|n| {
                        n.impairments
                            .iter()
                            .find(|r| r.src == *src && r.dst == *dst)
                    })
                    .and_then(|r| r.rate_cap.clone());
                let mut imp = imp.clone();
                let cap = match (cap, imp.rate.take()) {
                    (Some(_), Some(rate)) => Some(rate),
                    (cap, rate) => {
                        imp.rate = rate;
                        cap
                    }
                };
                self.set_pair_locked(network, src, dst, Some(&imp), cap.as_deref())
                    .await?;
            }
        }
        drop(m);
        Ok(())
    }

    /// End a replay (#340): with `keep` what it last set is saved (a
    /// `keep` scenario); otherwise `target` goes back to what the state
    /// file has, which the ticks never wrote.
    pub(crate) async fn replay_end(
        &mut self,
        target: &crate::replay::ReplayTarget,
        keep: bool,
    ) -> Result<()> {
        use crate::replay::ReplayTarget;
        let m = self.begin("replay").await?;
        if keep {
            return self.commit(m);
        }
        let base = self
            .baseline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let before = crate::deploy::live_view(self);
        let nodes: Vec<String> = match target {
            ReplayTarget::Endpoint { endpoint, dir } => {
                let eps = self.directed_endpoints(endpoint, *dir)?;
                for ep in &eps {
                    restore_entry(&mut self.live_impairments, &base.live_impairments, ep);
                    restore_entry(&mut self.saved_impairments, &base.saved_impairments, ep);
                }
                eps.iter()
                    .filter_map(|ep| EndpointRef::parse(ep).map(|e| e.node))
                    .collect()
            }
            ReplayTarget::Pair { network, src, dst } => {
                // only the replayed rule goes back, in its place: another
                // rule of the network is not the replay's
                let file: Topology = serde_json::from_str(&base.topology)
                    .map_err(|e| Error::deploy_failed(format!("the state's topology: {e}")))?;
                let is_it = |i: &crate::types::NetworkImpairment| i.src == *src && i.dst == *dst;
                let was = file
                    .networks
                    .get(network)
                    .and_then(|n| n.impairments.iter().find(|i| is_it(i)).cloned());
                if let Some(now) = self.topology.networks.get_mut(network) {
                    match (now.impairments.iter().position(is_it), was) {
                        (Some(at), Some(was)) => now.impairments[at] = was,
                        (Some(at), None) => {
                            now.impairments.remove(at);
                        }
                        (None, Some(was)) => now.impairments.push(was),
                        (None, None) => {}
                    }
                }
                vec![src.clone()]
            }
        };
        let after = crate::deploy::live_view(self);
        for node in &nodes {
            let handle = self
                .ns_resolver_of(node)
                .ok_or_else(|| Error::NodeNotFound { name: node.clone() })?;
            crate::deploy::reconcile_node_tc(
                &handle,
                node,
                (&before.0, &before.1),
                (&after.0, &after.1),
            )
            .await?;
        }
        // the file already says this
        drop(m);
        Ok(())
    }

    /// Apply `change` to the runtime state (`live_impairments`,
    /// partitions), then converge `endpoint`'s node on its traffic
    /// control: declared before and after the change, and reconciled
    /// (#108). If that fails, the runtime state is put back.
    async fn change_tc(&mut self, endpoint: &str, change: impl FnOnce(&mut Self)) -> Result<()> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        self.tc_is_ours(&ep.node)?;
        let handle = self
            .ns_resolver_of(&ep.node)
            .ok_or_else(|| Error::NodeNotFound {
                name: ep.node.clone(),
            })?;
        let (live, saved) = (
            self.live_impairments.clone(),
            self.saved_impairments.clone(),
        );
        let before = crate::deploy::live_view(self);
        change(self);
        let after = crate::deploy::live_view(self);
        if let Err(e) = crate::deploy::reconcile_node_tc(
            &handle,
            &ep.node,
            (&before.0, &before.1),
            (&after.0, &after.1),
        )
        .await
        {
            self.live_impairments = live;
            self.saved_impairments = saved;
            return Err(e);
        }
        Ok(())
    }

    /// The runtime traffic control: live impairments and partitions, for a
    /// scenario to put back when it ends (#326).
    pub(crate) fn runtime_tc(&self) -> RuntimeTc {
        RuntimeTc {
            live: self.live_impairments.clone(),
            partitions: self.saved_impairments.clone(),
        }
    }

    /// Put the runtime traffic control back to `state`: every node with an
    /// endpoint whose impairment or partition differs is reconciled from
    /// what it has to what `state` says, as one `impair` or `heal` would
    /// (#326). Returns the endpoints that changed. If a node fails, the
    /// runtime state is put back as it was.
    pub(crate) async fn restore_runtime_tc(
        &mut self,
        state: RuntimeTc,
        only: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Vec<String>> {
        let m = self.begin("restore-impairments").await?;
        // With `only`, the endpoints outside it keep what they have now: a
        // scenario puts back what it touched, not what an operator changed
        // elsewhere meanwhile (#332).
        let target = |now: &BTreeMap<String, crate::types::Impairment>,
                      was: &BTreeMap<String, crate::types::Impairment>| {
            match only {
                None => was.clone(),
                Some(eps) => {
                    let mut out = now.clone();
                    for ep in eps {
                        match was.get(ep) {
                            Some(v) => out.insert(ep.clone(), v.clone()),
                            None => out.remove(ep),
                        };
                    }
                    out
                }
            }
        };
        let live_target = target(&self.live_impairments, &state.live);
        let partitions_target = target(&self.saved_impairments, &state.partitions);
        let differs = |a: &BTreeMap<String, crate::types::Impairment>,
                       b: &BTreeMap<String, crate::types::Impairment>| {
            a.keys()
                .chain(b.keys())
                .filter(|k| a.get(*k) != b.get(*k))
                .cloned()
                .collect::<std::collections::BTreeSet<String>>()
        };
        let mut changed = differs(&self.live_impairments, &live_target);
        // a partition is its presence; what it saved is its own business
        changed.extend(
            self.saved_impairments
                .keys()
                .chain(partitions_target.keys())
                .filter(|k| {
                    self.saved_impairments.contains_key(*k) != partitions_target.contains_key(*k)
                })
                .cloned(),
        );
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let nodes: std::collections::BTreeSet<String> = changed
            .iter()
            .filter_map(|ep| EndpointRef::parse(ep).map(|e| e.node))
            .collect();
        let (live, saved) = (
            self.live_impairments.clone(),
            self.saved_impairments.clone(),
        );
        let before = crate::deploy::live_view(self);
        self.live_impairments = live_target;
        self.saved_impairments = partitions_target;
        let after = crate::deploy::live_view(self);
        for node in &nodes {
            let Some(handle) = self.ns_resolver_of(node) else {
                self.live_impairments = live;
                self.saved_impairments = saved;
                return Err(Error::NodeNotFound { name: node.clone() });
            };
            if let Err(e) = crate::deploy::reconcile_node_tc(
                &handle,
                node,
                (&before.0, &before.1),
                (&after.0, &after.1),
            )
            .await
            {
                self.live_impairments = live;
                self.saved_impairments = saved;
                return Err(e);
            }
        }
        self.commit(m)?;
        Ok(changed.into_iter().collect())
    }

    /// Whether the given endpoint is currently
    /// [`partition`](Self::partition)ed. Used by `nlink-lab impair --show
    /// --json` to surface the partition flag distinct from "user
    /// installed `--loss 100%`".
    pub fn is_partitioned(&self, endpoint: &str) -> bool {
        self.saved_impairments.contains_key(endpoint)
    }

    /// Remove all impairments from an interface, and its partition.
    ///
    /// The root qdisc is deleted, then the endpoint converges on what the
    /// topology declares other than an impairment: a `qdisc` block, a rate
    /// limit, a network's per-pair matrix (without a default impairment).
    /// Deleting it first also removes a 100%-loss netem that an older
    /// nlink-lab installed for a partition. Idempotent.
    pub async fn clear_impairment(&mut self, endpoint: &str) -> Result<()> {
        let m = self.begin("clear-impairment").await?;
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        self.tc_is_ours(&ep.node)?;
        self.route_conn_for(&ep.node)?
            .del_qdisc_if_exists(&ep.iface, nlink::TcHandle::ROOT)
            .await
            .map_err(|e| Error::nlink(format!("clear impairment on '{endpoint}'"), e))?;
        // A cleared endpoint is "no impairment" until the topology says
        // otherwise again: record the empty value so `apply` does not
        // reinstall the declared one behind the operator's back.
        let declared = self.topology.impairments.contains_key(endpoint);
        self.change_tc(endpoint, |lab| {
            lab.saved_impairments.remove(endpoint);
            if declared {
                lab.live_impairments
                    .insert(endpoint.to_string(), crate::types::Impairment::default());
            } else {
                lab.live_impairments.remove(endpoint);
            }
        })
        .await?;
        self.commit(m)?;
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::ImpairCleared {
                endpoint: endpoint.to_string(),
            },
        );
        Ok(())
    }

    /// Partition an endpoint: drop everything it sends and receives, as a
    /// black hole its own stack cannot see (#347). A matchall on both
    /// hooks of a clsact qdisc, ahead of any other filter: a drop on
    /// ingress, a steal on egress. Its root qdisc and the tree under it
    /// (netem, `qdisc` block, rate limit, per-pair matrix) stay as they
    /// are (#168).
    pub async fn partition(&mut self, endpoint: &str) -> Result<()> {
        let m = self.begin("partition").await?;
        if self.saved_impairments.contains_key(endpoint) {
            return Ok(());
        }
        // Recorded with the impairment in effect, which older nlink-lab
        // versions restore on `heal`.
        let current = self
            .live_impairments
            .get(endpoint)
            .or_else(|| self.topology.impairments.get(endpoint))
            .cloned()
            .unwrap_or_default();
        self.change_tc(endpoint, |lab| {
            lab.saved_impairments.insert(endpoint.to_string(), current);
        })
        .await?;
        self.commit(m)?;
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::Partitioned {
                endpoint: endpoint.to_string(),
            },
        );
        Ok(())
    }

    /// Heal an endpoint: take its partition's filters away. What it
    /// carried under them is still there.
    pub async fn heal(&mut self, endpoint: &str) -> Result<()> {
        let m = self.begin("heal").await?;
        if !self.saved_impairments.contains_key(endpoint) {
            return Err(Error::deploy_failed(format!(
                "endpoint '{endpoint}' is not partitioned"
            )));
        }
        self.change_tc(endpoint, |lab| {
            lab.saved_impairments.remove(endpoint);
        })
        .await?;
        self.commit(m)?;
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::Healed {
                endpoint: endpoint.to_string(),
            },
        );
        Ok(())
    }

    /// Run diagnostics on the lab, optionally filtered to a single node.
    ///
    /// The `rates` on each returned `InterfaceDiag` are zero: there is no
    /// previous sample to take them against. A caller sampling on a
    /// cadence keeps the result and passes it to
    /// [`diagnose_since`](Self::diagnose_since).
    pub async fn diagnose(&self, node: Option<&str>) -> Result<Vec<NodeDiagnostic>> {
        self.diagnose_since(node, &[]).await
    }

    /// [`diagnose`](Self::diagnose), with each interface's `rates` taken
    /// since `previous` — what an earlier call returned — over the time
    /// between the two reads of the counters (nlink's
    /// `Diagnostics::scan_since`, #191).
    ///
    /// A node or interface missing from `previous`, a link recreated since
    /// (its counters went backwards), or another link at the same ifindex
    /// has zero rates.
    /// A node's report without the tunnel devices the kernel puts in
    /// every new namespace when their modules are loaded (`gre0`, `sit0`,
    /// …): always down, they were nine warnings on every node, in
    /// `diagnose`, the backend and `top` (#226). One the topology declares
    /// stays.
    fn node_diagnostic(&self, node: &str, report: DiagnosticReport) -> NodeDiagnostic {
        let mut diag = NodeDiagnostic::from_report(node, report);
        let declared = |iface: &str| {
            let ep = format!("{node}:{iface}");
            self.topology
                .links
                .iter()
                .any(|l| l.endpoints.contains(&ep))
                || self
                    .topology
                    .networks
                    .values()
                    .any(|n| n.members.contains(&ep))
                || self.topology.nodes.get(node).is_some_and(|n| {
                    n.interfaces.contains_key(iface) || n.wireguard.contains_key(iface)
                })
        };
        let fallback = |iface: &str| FALLBACK_TUNNELS.contains(&iface) && !declared(iface);
        diag.interfaces.retain(|i| !fallback(&i.name));
        diag.issues
            .retain(|i| !i.interface.as_deref().is_some_and(fallback));
        diag
    }

    pub async fn diagnose_since(
        &self,
        node: Option<&str>,
        previous: &[NodeDiagnostic],
    ) -> Result<Vec<NodeDiagnostic>> {
        // an unknown node printed nothing and exited 0 (#209)
        if let Some(name) = node
            && !self.node_names().any(|n| n == name)
        {
            return Err(Error::NodeNotFound {
                name: name.to_string(),
            });
        }
        let wanted = |name: &str| node.is_none_or(|filter| filter == name);
        let mut results = Vec::new();

        // Over every node, one that cannot be read (a container that
        // exited or was restarted) is left out: it failed the whole scan,
        // and the backend and `top` showed nothing for any node (#218).
        // Asked for by name, its error is the answer.
        let skip_or_fail = |name: &str, e: Error| -> Result<()> {
            if node.is_some() {
                return Err(e);
            }
            tracing::warn!("diagnostics: node '{name}' left out: {e}");
            Ok(())
        };
        for (node_name, ns_name) in self.namespace_names.iter().filter(|(n, _)| wanted(n)) {
            let scanned = match Connection::<Route>::new_in(ns_name) {
                Ok(conn) => scan_node(conn, previous_of(previous, node_name))
                    .await
                    .map_err(|e| Error::nlink(format!("diagnostics scan for '{node_name}'"), e)),
                Err(e) => Err(Error::nlink(format!("connection for '{ns_name}'"), e)),
            };
            match scanned {
                Ok(report) => results.push(self.node_diagnostic(node_name, report)),
                Err(e) => skip_or_fail(node_name, e)?,
            }
        }

        for (node_name, container) in self.containers.iter().filter(|(n, _)| wanted(n)) {
            let scanned = match Connection::<Route>::new_in(
                nlink::netlink::namespace::NetnsRef::Pid(container.pid),
            ) {
                Ok(conn) => scan_node(conn, previous_of(previous, node_name))
                    .await
                    .map_err(|e| {
                        Error::nlink(format!("diagnostics scan for container '{node_name}'"), e)
                    }),
                Err(e) => Err(Error::nlink(
                    format!("connection for container '{node_name}'"),
                    e,
                )),
            };
            match scanned {
                Ok(report) => results.push(self.node_diagnostic(node_name, report)),
                Err(e) => skip_or_fail(node_name, e)?,
            }
        }

        Ok(results)
    }

    /// `act(init_pid, tag)` for the processes a tracked PID started in a
    /// container node, if it has a tag; what `act` returns, else 0.
    fn tagged_in_container(&self, pid: u32, act: impl FnOnce(u32, &str) -> usize) -> usize {
        let Some(tag) = self.process_tags.get(&pid) else {
            return 0;
        };
        let Some((node, _)) = self.pids.iter().find(|(_, p)| *p == pid) else {
            return 0;
        };
        self.containers.get(node).map_or(0, |c| act(c.pid, tag))
    }

    /// Kill a tracked background process by PID. `Ok(false)` when it had
    /// already exited (`kill` said "Killed" for those, #209).
    pub fn kill_process(&self, pid: u32) -> Result<bool> {
        if !self.pids.iter().any(|(_, p)| *p == pid) {
            return Err(Error::deploy_failed(format!(
                "pid {pid} is not tracked by lab '{}'",
                self.topology.lab.name
            )));
        }
        // On a container node, what the `exec` client started too: the
        // client forwards no signal, so it kept running (#215).
        let inside = self.tagged_in_container(pid, stop_tagged);
        match kill_tracked(pid, self.starttimes.get(&pid).copied()) {
            outcome @ (KillOutcome::Signalled | KillOutcome::Gone) => {
                crate::events::record(
                    &self.topology.lab.name,
                    crate::events::LifecycleKind::Killed {
                        node: self
                            .pids
                            .iter()
                            .find(|(_, p)| *p == pid)
                            .map(|(n, _)| n.clone()),
                        pid,
                    },
                );
                Ok(matches!(outcome, KillOutcome::Signalled) || inside > 0)
            }
            KillOutcome::Unverified => Err(Error::deploy_failed(format!(
                "refusing to signal pid {pid}: its start time was not recorded (state file \
                 written by an older release) so it cannot be proven to still be the lab's process"
            ))),
            KillOutcome::Reused => Err(Error::deploy_failed(format!(
                "refusing to signal pid {pid}: it now belongs to a different process"
            ))),
        }
    }

    /// Send one signal to a tracked process, with the same PID-reuse guard
    /// as [`RunningLab::kill_process`] and no TERM/KILL escalation.
    ///
    /// `STOP`/`CONT` are the point: a frozen process keeps its sockets, the
    /// kernel keeps ACKing, and only the application protocol can tell it
    /// is dead -- the "zombie peer" every resilience lab needs.
    pub fn signal_process(&self, pid: u32, signal: Signal) -> Result<()> {
        let node = self
            .pids
            .iter()
            .find(|(_, p)| *p == pid)
            .map(|(n, _)| n.clone());
        if node.is_none() {
            return Err(Error::deploy_failed(format!(
                "pid {pid} is not tracked by lab '{}'",
                self.topology.lab.name
            )));
        }
        self.tagged_in_container(pid, |init, tag| signal_tagged(init, tag, signal.number()));
        match signal_tracked(pid, self.starttimes.get(&pid).copied(), signal) {
            KillOutcome::Signalled | KillOutcome::Gone => {
                crate::events::record(
                    &self.topology.lab.name,
                    crate::events::LifecycleKind::Signalled {
                        node,
                        pid,
                        signal: signal.name().to_string(),
                    },
                );
                Ok(())
            }
            KillOutcome::Unverified => Err(Error::deploy_failed(format!(
                "refusing to signal pid {pid}: its start time was not recorded (state file \
                 written by an older release) so it cannot be proven to still be the lab's process"
            ))),
            KillOutcome::Reused => Err(Error::deploy_failed(format!(
                "refusing to signal pid {pid}: it now belongs to a different process"
            ))),
        }
    }

    /// Destroy the lab: kill processes, remove containers, delete namespaces, remove state.
    pub async fn destroy(self) -> Result<()> {
        self.destroy_with(&crate::ops::OpCtx::default()).await
    }

    /// [`destroy`](Self::destroy), reporting its phases to `ctx` (#236).
    /// It is not cancelled: a half-destroyed lab is worse than either.
    pub async fn destroy_with(self, ctx: &crate::ops::OpCtx) -> Result<()> {
        ctx.set_lab(self.name());
        ctx.started();
        let result = self.destroy_ctx(ctx).await;
        ctx.finished(&result, false);
        result
    }

    async fn destroy_ctx(self, ctx: &crate::ops::OpCtx) -> Result<()> {
        const PHASES: [&str; 6] = [
            "stopped processes",
            "removed radios and containers",
            "removed the management bridge",
            "deleted namespaces",
            "removed DNS entries and configs",
            "removed the state",
        ];
        let step = |n: usize| ctx.step(n, PHASES.len(), || PHASES[n - 1].to_string());
        // Acquire exclusive lock
        let lock = crate::state::lock_for(&self.topology.lab.name, "destroy")?;

        // Release any subnet-pool entries this lab claimed at deploy
        // time (round-5 §2.5). Best-effort — pool errors are not
        // fatal for destroy. The pool flock keeps this safe against
        // concurrent free_for_lab calls.
        let _ = crate::subnet_pool::free_for_lab(&self.topology.lab.name);
        let _ = crate::host_registry::release(&self.topology.lab.name);

        // 1. Kill background processes — only those whose recorded start
        // time still matches (issue #30).
        for (_node, pid) in &self.pids {
            match kill_tracked(*pid, self.starttimes.get(pid).copied()) {
                KillOutcome::Signalled | KillOutcome::Gone => {}
                KillOutcome::Unverified => tracing::warn!(
                    "pid {pid}: start time not recorded (older state file); not signalled"
                ),
                KillOutcome::Reused => {
                    tracing::warn!("pid {pid}: now belongs to another process; not signalled")
                }
            }
        }

        // 1a. Then what the lab's commands left in its namespaces, which
        // nothing tracks (`exec … iperf3 -D`, a shell's `&`): it would
        // keep each deleted namespace alive. Containers stop with their
        // runtime.
        let namespaces: Vec<String> = self.namespace_names.values().cloned().collect();
        let strays = crate::host::stop_namespace_processes(&namespaces).await;
        if strays > 0 {
            tracing::info!("stopped {strays} untracked process(es) in the lab's namespaces");
        }

        // 1b. Drop the lab's cgroups now that its processes are gone
        crate::cgroup::remove_lab(&self.topology.lab.name);
        step(1);

        // 1c. Delete the Wi-Fi radios, each from the namespace it is in:
        // hwsim takes a request for a radio only from there (#176).
        for (node_name, node) in &self.topology.nodes {
            let Some(ns) = self.ns_resolver_of(node_name) else {
                continue;
            };
            for wifi in &node.wifi {
                let radio =
                    crate::wifi::radio_name(&self.topology.namespace_name(node_name), &wifi.name);
                if let Err(e) = crate::wifi::delete_radio(&ns, &radio).await {
                    tracing::warn!("{e}");
                }
            }
        }

        // 2. Remove containers
        if let Some(binary) = &self.runtime_binary {
            for container in self.containers.values() {
                let _ = std::process::Command::new(binary)
                    .args(["rm", "-f", &container.id])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        step(2);

        // 3. Delete root-namespace mgmt bridge + veth peers explicitly.
        // We must delete each veth peer individually — bridge cascade doesn't
        // reliably remove veths whose peers are in active namespaces.
        if self.topology.lab.mgmt_host_reachable
            && let Ok(root_conn) = Connection::<Route>::new()
        {
            // Prefer the peer names deploy persisted; fall back to the
            // index scheme over *every* node (namespaces and containers,
            // matching deploy's enumeration) for state files that predate
            // `mgmt_peers` (issue #32).
            let peers: Vec<String> = if !self.mgmt_peers.is_empty() {
                self.mgmt_peers.values().cloned().collect()
            } else {
                let mut all: Vec<&str> = self
                    .namespace_names
                    .keys()
                    .chain(self.containers.keys())
                    .map(|s| s.as_str())
                    .collect();
                all.sort();
                all.dedup();
                (0..all.len())
                    .map(|idx| self.topology.lab.mgmt_peer_name(idx))
                    .collect()
            };
            // `del_link_if_exists` (nlink 0.24) treats an
            // already-absent link as Ok(false); a real failure is
            // logged but never aborts best-effort teardown.
            for peer in &peers {
                if let Err(e) = root_conn.del_link_if_exists(peer.as_str()).await {
                    tracing::warn!("failed to delete mgmt veth peer '{peer}': {e}");
                }
            }
            let bridge_name = self.topology.lab.mgmt_bridge_name();
            if let Err(e) = root_conn.del_link_if_exists(bridge_name.as_str()).await {
                tracing::warn!("failed to delete mgmt bridge '{bridge_name}': {e}");
            }
        }

        // 3b. A foreign tap's ports and bridge (#344): the tap itself
        // stays, released from the bridge.
        let tap_links = crate::foreign::tap_host_links(&self.topology);
        if !tap_links.is_empty()
            && let Ok(root_conn) = Connection::<Route>::new()
        {
            for name in &tap_links {
                if let Err(e) = root_conn.del_link_if_exists(name.as_str()).await {
                    tracing::warn!("failed to delete '{name}': {e}");
                }
            }
        }

        step(3);

        // 4. Delete namespaces (and their ownership tags)
        for ns_name in self.namespace_names.values() {
            if namespace::exists(ns_name)
                && let Err(e) = namespace::delete(ns_name)
            {
                tracing::warn!("failed to delete namespace '{ns_name}': {e}");
            }
            crate::netns_tag::untag(ns_name);
        }

        // 4b. Delete management namespace (bridges) if it exists: a lab
        // with networks or a `mgmt` subnet has one
        if !self.topology.networks.is_empty() || self.topology.lab.mgmt_subnet.is_some() {
            let mgmt_ns = format!("{}-mgmt", self.topology.lab.prefix());
            if namespace::exists(&mgmt_ns)
                && let Err(e) = namespace::delete(&mgmt_ns)
            {
                tracing::warn!("failed to delete management namespace '{mgmt_ns}': {e}");
            }
            crate::netns_tag::untag(&mgmt_ns);
        }

        step(4);

        // 5. Remove DNS hosts entries from /etc/hosts
        if self.dns_injected
            && let Err(e) = crate::dns::remove_hosts(&self.topology.lab.name)
        {
            tracing::warn!("failed to remove /etc/hosts entries: {e}");
        }

        // 5b. Remove per-namespace /etc/netns/ directories
        if self.dns_injected {
            for ns_name in self.namespace_names.values() {
                crate::dns::remove_netns_etc(ns_name);
            }
        }

        // 5c. hostapd / wpa_supplicant configs
        if self.wifi_loaded {
            crate::wifi::cleanup_configs(&self.topology.lab.name);
        }

        // 5d. FRR runtime directories (daemons were killed in step 1)
        crate::frr::cleanup(&self.topology);

        step(5);

        // 6. Remove state file
        // Recorded before the state directory (and the log with it) goes
        // away: an `events --follow` reader sees the line first.
        crate::events::record(
            &self.topology.lab.name,
            crate::events::LifecycleKind::Destroyed,
        );
        state::remove(&self.topology.lab.name)?;

        // The lab is gone, so its lock file should be too (issue #103) —
        // unlinked while we still hold it, which is what makes it safe.
        lock.remove_file();
        step(6);

        Ok(())
    }

    /// Load a running lab from saved state.
    pub fn load(name: &str) -> Result<Self> {
        let (mut lab_state, topology) = state::load(name)?;
        zero_stale_container_pids(&mut lab_state.containers);
        let lab = Self {
            topology,
            namespace_names: lab_state.namespaces,
            containers: lab_state.containers,
            runtime_binary: lab_state.runtime,
            pids: lab_state.pids,
            dns_injected: lab_state.dns_injected,
            wifi_loaded: lab_state.wifi_loaded,
            saved_impairments: lab_state.saved_impairments,
            live_impairments: lab_state.live_impairments,
            process_logs: lab_state.process_logs,
            process_tags: lab_state.process_tags,
            starttimes: lab_state.starttimes,
            exec_pids: lab_state.exec_pids,
            mgmt_peers: lab_state.mgmt_peers,
            assertion_results: Vec::new(),
            baseline: Default::default(),
        };
        lab.mark_saved();
        Ok(lab)
    }

    /// List all saved labs.
    pub fn list() -> Result<Vec<LabInfo>> {
        state::list()
    }

    /// Check status of tracked background processes.
    ///
    /// `alive` is `true` only if the PID still exists **and** is not a
    /// zombie. Spawned processes are detached (double-forked and
    /// reparented to init, see `ns_exec::spawn_detached`), so
    /// nlink-lab itself never leaves zombies; the check still matters
    /// for processes a spawned program forks and abandons (hostapd's
    /// `-B` parent, shell wrappers), which `kill(pid, 0)` keeps
    /// reporting as deliverable.
    pub fn process_status(&self) -> Vec<ProcessInfo> {
        self.pids
            .iter()
            .map(|(node, pid)| {
                // A PID whose start time is not the one recorded at spawn is
                // another process now: ours exited and the number was reused.
                let reused = self
                    .starttimes
                    .get(pid)
                    .is_some_and(|&t| host_starttime(*pid).is_some_and(|now| now != t));
                let alive = pid_is_alive(*pid) && !reused;
                let logs = self.process_logs.get(pid);
                ProcessInfo {
                    node: node.clone(),
                    pid: *pid,
                    host_pid: *pid,
                    alive,
                    stdout_log: logs.map(|(s, _)| s.clone()),
                    stderr_log: logs.map(|(_, s)| s.clone()),
                    exit_code: if alive {
                        None
                    } else {
                        logs.and_then(|(s, _)| read_exit_code(s))
                    },
                }
            })
            .collect()
    }

    /// Like [`process_status`](Self::process_status), but filters out any
    /// entry whose tracked PID has exited. Useful for "is X still
    /// running?" polling loops that would otherwise have to filter
    /// `alive == false` themselves and risk forgetting to.
    pub fn process_status_alive_only(&self) -> Vec<ProcessInfo> {
        self.process_status()
            .into_iter()
            .filter(|p| p.alive)
            .collect()
    }

    /// Sample resource usage for one process of a node.
    ///
    /// Thin wrapper over [`proc_stat_many`](Self::proc_stat_many): `pid`
    /// is the PID as the node sees it — the host PID on a namespace node
    /// (no `CLONE_NEWPID`), the container's own PID on a container node.
    /// See `docs/ARCHITECTURE.md` "Process & namespace model".
    pub fn proc_stat(&self, node: &str, pid: u32) -> Result<crate::proc_stat::ProcStat> {
        self.proc_stat_many(node, &[pid])?
            .pop()
            .ok_or_else(|| Error::deploy_failed(format!("no such process: {pid}")))
    }

    /// Host PIDs of every process whose **network namespace** is this
    /// node's.
    ///
    /// Walks `/proc` and compares the `(dev, ino)` of each process's
    /// `/proc/<pid>/ns/net` with the node's namespace file — the same
    /// identity `ip netns pids` uses. Needs root, like everything else
    /// that reads another process's `/proc/<pid>/ns/`.
    ///
    /// Works for container nodes too (the host sees every PID), and
    /// reports host PIDs in that case — a container's own PID namespace
    /// numbers them differently.
    ///
    /// Skips processes that vanish mid-walk rather than failing: a
    /// sampled lab is by definition churning.
    pub fn node_pids(&self, node: &str) -> Result<Vec<u32>> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::deploy_failed(format!("node '{node}' is not running")))?;
        let want = std::fs::metadata(ns.ns_path()).map_err(|e| {
            Error::deploy_failed(format!(
                "stat namespace of '{node}' ({}): {e}",
                ns.ns_path().display()
            ))
        })?;
        let (want_dev, want_ino) = {
            use std::os::unix::fs::MetadataExt;
            (want.dev(), want.ino())
        };
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc").map_err(Error::Io)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let pid: u32 = match name.to_str().and_then(|s| s.parse().ok()) {
                Some(p) => p,
                None => continue,
            };
            // A process that exits between the readdir and the stat is
            // not an error, it is just not in the sample.
            let Ok(md) = std::fs::metadata(format!("/proc/{pid}/ns/net")) else {
                continue;
            };
            use std::os::unix::fs::MetadataExt;
            if md.dev() == want_dev && md.ino() == want_ino {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        Ok(pids)
    }

    /// Sample resource usage for several processes in one call, each
    /// named by its PID as the node sees it (see
    /// [`host_pids`](Self::host_pids)).
    ///
    /// Every read comes from the host's `/proc`, for container nodes too:
    /// root on the host sees every process, so nothing enters the node
    /// (#157). Per process that is `/proc/<pid>/{stat,status,smaps_rollup}`
    /// and the entry count of `/proc/<pid>/fd/`; per call, `btime` from
    /// `/proc/stat` and the clock-tick rate, which are the host's for
    /// every node.
    ///
    /// A PID the node has no process for is an error; a PID whose
    /// `smaps_rollup` cannot be read simply yields `pss_kb: None`.
    pub fn proc_stat_many(
        &self,
        node: &str,
        pids: &[u32],
    ) -> Result<Vec<crate::proc_stat::ProcStat>> {
        let consts = HostConstants::read();
        self.host_pids(node, pids)?
            .into_iter()
            .map(|host_pid| {
                sample_on_host(node, host_pid, None, consts).map_err(|e| {
                    Error::deploy_failed(format!("sample process {host_pid} of '{node}': {e}"))
                })
            })
            .collect()
    }

    /// Sample every process in the node's network namespace
    /// ([`node_pids`](Self::node_pids)). A process that exits between the
    /// walk and its reads is left out: a sampled lab churns.
    pub fn proc_stat_all(&self, node: &str) -> Result<Vec<crate::proc_stat::ProcStat>> {
        let consts = HostConstants::read();
        let mut out = Vec::new();
        for host_pid in self.node_pids(node)? {
            match sample_on_host(node, host_pid, None, consts) {
                Ok(stat) => out.push(stat),
                Err(e) if vanished(&e) => {}
                Err(e) => {
                    return Err(Error::deploy_failed(format!(
                        "sample process {host_pid} of '{node}': {e}"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// Sample the processes the node's pidfiles name (#157): every file
    /// matching `pattern`, an absolute path whose last component may hold
    /// `*` and `?` (`/run/*.pid`). Each record's `name` is the pidfile's
    /// stem, which tells apart processes that share a command.
    ///
    /// The files are read from the host: through the container's root
    /// (`/proc/<init>/root`) on a container node, where their PIDs are
    /// the container's, and straight from the filesystem on a namespace
    /// node, which shares the host's. A symlink in the directory part is
    /// followed from the host. A pidfile that does not hold a PID, or
    /// names no process of the node (a stale one), is skipped.
    pub fn proc_stat_pidfiles(
        &self,
        node: &str,
        pattern: &str,
    ) -> Result<Vec<crate::proc_stat::ProcStat>> {
        let path = std::path::Path::new(pattern);
        let (Some(dir), Some(file)) = (path.parent(), path.file_name()) else {
            return Err(Error::deploy_failed(format!(
                "pidfile glob '{pattern}' names no file"
            )));
        };
        if !path.is_absolute() {
            return Err(Error::deploy_failed(format!(
                "pidfile glob '{pattern}' must be an absolute path"
            )));
        }
        if dir.to_string_lossy().contains(['*', '?']) {
            return Err(Error::deploy_failed(format!(
                "pidfile glob '{pattern}': only the file name may hold wildcards"
            )));
        }
        let file = file.to_string_lossy();
        let root = match self.ns_resolver_of(node) {
            Some(crate::deploy::NsRef::Container { pid, .. }) => {
                std::path::PathBuf::from(format!("/proc/{pid}/root"))
            }
            Some(_) => std::path::PathBuf::from("/"),
            None => {
                return Err(Error::deploy_failed(format!(
                    "node '{node}' is not running"
                )));
            }
        };
        let host_dir = root.join(dir.strip_prefix("/").unwrap_or(dir));
        let entries = match std::fs::read_dir(&host_dir) {
            Ok(entries) => entries,
            // No such directory: no pidfiles, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error::deploy_failed(format!(
                    "list {} of '{node}': {e}",
                    dir.display()
                )));
            }
        };
        let mut pidfiles: Vec<(String, u32)> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                crate::proc_stat::wildcard_match(&file, &entry.file_name().to_string_lossy())
                    && entry.file_type().is_ok_and(|t| t.is_file())
            })
            .filter_map(|entry| {
                let pid = std::fs::read_to_string(entry.path())
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
                Some((crate::proc_stat::pidfile_name(&entry.path())?, pid))
            })
            .collect();
        pidfiles.sort();

        let consts = HostConstants::read();
        let ns_pids: Vec<u32> = pidfiles.iter().map(|(_, pid)| *pid).collect();
        let mut out = Vec::new();
        for ((name, _), host_pid) in pidfiles.iter().zip(self.resolve_pids(node, &ns_pids)?) {
            let Some(host_pid) = host_pid else {
                continue;
            };
            match sample_on_host(node, host_pid, Some(name), consts) {
                Ok(stat) => out.push(stat),
                Err(e) if vanished(&e) => {}
                Err(e) => {
                    return Err(Error::deploy_failed(format!(
                        "sample process {host_pid} ({name}) of '{node}': {e}"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// The host PID of each process named by its PID as `node` sees it
    /// (#157):
    ///
    /// - on a **namespace node** it is the host PID already, and must be
    ///   in the node's network namespace;
    /// - on a **container node** it is a PID in the container's own PID
    ///   namespace, matched against the last `NSpid:` field of the
    ///   node's processes ([`node_pids`](Self::node_pids)) that share the
    ///   container init's PID namespace.
    ///
    /// A PID the node has no process for is an error.
    pub fn host_pids(&self, node: &str, pids: &[u32]) -> Result<Vec<u32>> {
        self.resolve_pids(node, pids)?
            .into_iter()
            .zip(pids)
            .map(|(host, pid)| {
                host.ok_or_else(|| {
                    Error::deploy_failed(format!("node '{node}' has no process {pid}"))
                })
            })
            .collect()
    }

    /// [`host_pids`](Self::host_pids), with `None` for a PID the node has
    /// no process for.
    fn resolve_pids(&self, node: &str, pids: &[u32]) -> Result<Vec<Option<u32>>> {
        let ns = self
            .ns_resolver_of(node)
            .ok_or_else(|| Error::deploy_failed(format!("node '{node}' is not running")))?;
        if let crate::deploy::NsRef::Container { pid: init, .. } = ns {
            let pidns = ns_identity(format!("/proc/{init}/ns/pid")).map_err(|e| {
                Error::deploy_failed(format!("PID namespace of container node '{node}': {e}"))
            })?;
            let mut by_ns_pid = std::collections::HashMap::new();
            for host in self.node_pids(node)? {
                if ns_identity(format!("/proc/{host}/ns/pid")).ok() != Some(pidns) {
                    continue;
                }
                let ns_pid = std::fs::read_to_string(format!("/proc/{host}/status"))
                    .ok()
                    .and_then(|t| crate::proc_stat::parse_status(&t).ns_pid);
                if let Some(ns_pid) = ns_pid {
                    by_ns_pid.insert(ns_pid, host);
                }
            }
            return Ok(pids.iter().map(|p| by_ns_pid.get(p).copied()).collect());
        }
        let netns = ns_identity(ns.ns_path()).map_err(|e| {
            Error::deploy_failed(format!(
                "stat namespace of '{node}' ({}): {e}",
                ns.ns_path().display()
            ))
        })?;
        Ok(pids
            .iter()
            .map(|&pid| {
                (ns_identity(format!("/proc/{pid}/ns/net")).ok() == Some(netns)).then_some(pid)
            })
            .collect())
    }
}

/// `(dev, ino)` of a namespace file: equal for two processes in one
/// namespace.
fn ns_identity(path: impl AsRef<std::path::Path>) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path)?;
    Ok((md.dev(), md.ino()))
}

/// The per-call constants of a sample: the boot time and the clock-tick
/// rate, both the host's.
#[derive(Clone, Copy)]
struct HostConstants {
    btime: u64,
    tick_hz: u64,
}

impl HostConstants {
    fn read() -> Self {
        let btime = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|t| crate::proc_stat::parse_btime(&t))
            .unwrap_or(0);
        // SAFETY: `sysconf` is thread-safe and takes no pointers.
        let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            btime,
            tick_hz: if tck > 0 { tck as u64 } else { 100 },
        }
    }
}

/// One process's record from the host's `/proc`.
///
/// `smaps_rollup` is best-effort: it does not exist before kernel 4.14
/// and a kernel thread has no rollup, both of which read back as "no
/// PSS" rather than an error.
fn sample_on_host(
    node: &str,
    host_pid: u32,
    name: Option<&str>,
    consts: HostConstants,
) -> std::io::Result<crate::proc_stat::ProcStat> {
    let stat_text = std::fs::read_to_string(format!("/proc/{host_pid}/stat"))?;
    let status_text = std::fs::read_to_string(format!("/proc/{host_pid}/status"))?;
    let smaps =
        std::fs::read_to_string(format!("/proc/{host_pid}/smaps_rollup")).unwrap_or_default();
    let fd_count = count_fds_on_host(host_pid)?;
    let stat = crate::proc_stat::parse_stat(&stat_text).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("/proc/{host_pid}/stat: unexpected format"),
        )
    })?;
    let status = crate::proc_stat::parse_status(&status_text);
    Ok(crate::proc_stat::assemble(
        node,
        crate::proc_stat::Sampled {
            host_pid,
            stat: &stat,
            status: &status,
            fd_count,
            pss_kb: crate::proc_stat::parse_pss_kb(&smaps),
            name,
        },
        consts.btime,
        consts.tick_hz,
    ))
}

/// The entries of `/proc/<pid>/fd` (mode 0700, the process owner's or
/// root's to list), from the host.
fn count_fds_on_host(host_pid: u32) -> std::io::Result<u32> {
    Ok(std::fs::read_dir(format!("/proc/{host_pid}/fd"))?.count() as u32)
}

/// A `/proc` read that failed because the process exited meanwhile:
/// `ENOENT` for the directory, `ESRCH` for a file of a dying one.
fn vanished(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH)
}

/// True iff `text` (the body of `/proc/<pid>/net/tcp` or `tcp6`)
/// contains a row with state `0A` (TCP_LISTEN) and a local port
/// matching `port_hex` (uppercase 4-hex-digit form). Pure function,
/// unit-testable.
///
/// Format reminder (one row per socket):
///
/// ```text
///   sl  local_address      rem_address      st ...
///   0:  0100007F:1F90      00000000:0000    0A ...
/// ```
///
/// `local_address` is `IP:PORT` in hex; we split on `:` and match the
/// port half (case-sensitive uppercase, which is what the kernel emits).
pub(crate) fn proc_net_tcp_has_listener(text: &str, port_hex: &str) -> bool {
    for line in text.lines().skip(1) {
        // Skip header line.
        let mut cols = line.split_whitespace();
        // sl, local, rem, st
        let _sl = cols.next();
        let local = match cols.next() {
            Some(s) => s,
            None => continue,
        };
        let _rem = cols.next();
        let st = match cols.next() {
            Some(s) => s,
            None => continue,
        };
        if st != "0A" {
            continue;
        }
        if let Some((_, p)) = local.rsplit_once(':')
            && p == port_hex
        {
            return true;
        }
    }
    false
}

/// Wait for a child to exit, escalating SIGTERM → SIGKILL on deadline.
///
/// On timeout: sends SIGTERM, gives the child up to 1 second to exit
/// cleanly, then `child.kill()` (SIGKILL). Either way the child has
/// been reaped on return so `wait_with_output()` (or further `try_wait`)
/// won't block. Returns [`Error::Timeout`] when the deadline fires;
/// `Ok(())` if the child exits in time. Used by `exec_with_opts` and
/// `exec_attached_with_opts` when [`ExecOpts::timeout`] is set.
fn wait_with_timeout(child: &mut std::process::Child, timeout: std::time::Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let poll = std::time::Duration::from_millis(50);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => return Err(Error::Io(e)),
        }
        if std::time::Instant::now() >= deadline {
            // SIGTERM, 1s grace, then SIGKILL — matches `coreutils
            // timeout(1)`'s default escalation.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let grace_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if std::time::Instant::now() >= grace_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(poll);
            }
            return Err(Error::Timeout(timeout));
        }
        std::thread::sleep(poll);
    }
}

/// Run a `Command` to completion with an optional deadline. Returns the
/// child's exit code on normal exit, [`Error::Timeout`] when the deadline
/// fires. Used by the *attached* exec path where stdio is inherited and
/// no output capture is needed.
/// `child`'s exit code and output, killing it after `timeout`.
///
/// Both pipes are drained while it runs. A child that wrote more than a
/// pipe holds (64 KiB) used to block on its write until the timeout killed
/// it, losing everything it printed. After it exits, end-of-file is waited
/// for only until the deadline: a background grandchild that keeps the
/// pipe open used to make the read ignore the timeout.
pub(crate) fn output_with_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> Result<ExecOutput> {
    use std::io::Read;
    use std::sync::{Arc, Mutex, mpsc};
    let deadline = std::time::Instant::now() + timeout;
    fn drain(
        pipe: Option<impl Read + Send + 'static>,
    ) -> (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let (done, eof) = mpsc::channel();
        match pipe {
            Some(mut pipe) => {
                let sink = Arc::clone(&buf);
                std::thread::spawn(move || {
                    let mut chunk = [0u8; 8192];
                    while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                        sink.lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .extend_from_slice(&chunk[..n]);
                    }
                    let _ = done.send(());
                });
            }
            None => {
                let _ = done.send(());
            }
        }
        (buf, eof)
    }
    let (stdout, stdout_eof) = drain(child.stdout.take());
    let (stderr, stderr_eof) = drain(child.stderr.take());
    wait_with_timeout(&mut child, timeout)?;
    for eof in [&stdout_eof, &stderr_eof] {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let _ = eof.recv_timeout(left.max(std::time::Duration::from_millis(100)));
    }
    let status = child.wait().map_err(Error::Io)?;
    let text = |buf: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8_lossy(&buf.lock().unwrap_or_else(|p| p.into_inner())).into_owned()
    };
    Ok(ExecOutput {
        stdout: text(&stdout),
        stderr: text(&stderr),
        exit_code: exit_code_of(status),
    })
}

/// Wait for `child`, killing it after `timeout`; its exit code
/// ([`exit_code_of`]).
fn wait_with_optional_timeout(
    mut child: std::process::Child,
    timeout: Option<std::time::Duration>,
) -> Result<i32> {
    if let Some(t) = timeout {
        wait_with_timeout(&mut child, t)?;
    }
    let status = child.wait().map_err(Error::Io)?;
    Ok(exit_code_of(status))
}

/// A process's exit code as a shell reports it: `128 + signo` when a
/// signal ended it. It was `-1`, which `exec` clamped to 0 (#226).
pub(crate) fn exit_code_of(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

/// Start time (clock ticks since boot, field 22 of `/proc/<pid>/stat`)
/// of a process on the host, or `None` when it no longer exists.
///
/// PIDs are recycled; the pair `(pid, starttime)` is not. Everything
/// that signals a tracked PID checks this first (issue #30).
pub(crate) fn host_starttime(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` may contain spaces/parens: parse from the last ')'.
    let rest = &text[text.rfind(')')? + 1..];
    // fields after ')' start at field 3 (state); starttime is field 22.
    rest.split_whitespace().nth(22 - 3)?.parse().ok()
}

/// Result of an identity-checked signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    /// The process was ours and has been signalled.
    Signalled,
    /// The PID no longer exists — nothing to do.
    Gone,
    /// No start time was recorded for this PID, so it cannot be proven
    /// to be ours; not signalled.
    Unverified,
    /// The PID exists but its start time differs — reused by another
    /// process; not signalled.
    Reused,
}

/// The environment variable that tags a process nlink-lab starts inside a
/// container (#215): `NLINK_LAB_TAG=<tag>`.
pub(crate) const TAG_VAR: &str = "NLINK_LAB_TAG";

/// A tag no other process has: lab, node, this process and a counter.
pub(crate) fn new_process_tag(lab: &str, node: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{lab}.{node}.{}.{nanos}.{n}", std::process::id())
}

/// The host PIDs of the processes in `init_pid`'s PID namespace whose
/// environment carries `tag`: a tagged command and everything it started.
fn tagged_pids(init_pid: u32, tag: &str) -> Vec<i32> {
    if init_pid == 0 {
        return Vec::new();
    }
    let Ok(ns) = std::fs::read_link(format!("/proc/{init_pid}/ns/pid")) else {
        return Vec::new();
    };
    let want = format!("{TAG_VAR}={tag}");
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|p| {
            std::fs::read_link(format!("/proc/{p}/ns/pid")).is_ok_and(|l| l == ns)
                && std::fs::read(format!("/proc/{p}/environ"))
                    .is_ok_and(|env| env.split(|b| *b == 0).any(|v| v == want.as_bytes()))
        })
        .collect()
}

/// Send `signal` to the processes tagged `tag` in a container (see
/// [`TAG_VAR`]); how many there were.
pub(crate) fn signal_tagged(init_pid: u32, tag: &str, signal: i32) -> usize {
    let pids = tagged_pids(init_pid, tag);
    for p in &pids {
        // SAFETY: plain kill(2) on PIDs just read from /proc.
        unsafe { libc::kill(*p, signal) };
    }
    pids.len()
}

/// Stop the processes tagged `tag` in a container: SIGTERM, a grace
/// period, SIGKILL for what is left. Stopping the `exec` client alone left
/// them running: no runtime forwards its signals (#215). How many there
/// were.
pub(crate) fn stop_tagged(init_pid: u32, tag: &str) -> usize {
    let found = signal_tagged(init_pid, tag, libc::SIGTERM);
    if found > 0 {
        for _ in 0..20 {
            if tagged_pids(init_pid, tag).is_empty() {
                return found;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        signal_tagged(init_pid, tag, libc::SIGKILL);
    }
    found
}

/// Best-effort, identity-checked kill of a tracked process: SIGTERM,
/// a short grace period, then SIGKILL — but only when the PID's current
/// start time matches the one recorded at spawn.
pub(crate) fn kill_tracked(pid: u32, expected_starttime: Option<u64>) -> KillOutcome {
    let Some(current) = host_starttime(pid) else {
        return KillOutcome::Gone;
    };
    match expected_starttime {
        None => return KillOutcome::Unverified,
        Some(expected) if expected != current => return KillOutcome::Reused,
        Some(_) => {}
    }
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    // Re-check before the SIGKILL: the grace period is a window too.
    if host_starttime(pid) == Some(current) {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    KillOutcome::Signalled
}

/// The exit code the spawn reaper recorded for a process whose stdout log
/// is `stdout_log` (`<...>.stdout` -> `<...>.rc`).
pub(crate) fn read_exit_code(stdout_log: &str) -> Option<i32> {
    let rc = stdout_log.strip_suffix(".stdout")?.to_string() + ".rc";
    std::fs::read_to_string(rc).ok()?.trim().parse().ok()
}

/// A signal `nlink-lab kill --signal` may send.  Deliberately a closed set:
/// the lab tracks processes it started, and these are the ones a fault
/// scenario needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
    Stop,
    Cont,
    Hup,
    Int,
    Usr1,
    Usr2,
}

impl Signal {
    pub fn name(self) -> &'static str {
        match self {
            Signal::Term => "TERM",
            Signal::Kill => "KILL",
            Signal::Stop => "STOP",
            Signal::Cont => "CONT",
            Signal::Hup => "HUP",
            Signal::Int => "INT",
            Signal::Usr1 => "USR1",
            Signal::Usr2 => "USR2",
        }
    }

    /// The signal's number.
    pub fn number(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
            Signal::Stop => libc::SIGSTOP,
            Signal::Cont => libc::SIGCONT,
            Signal::Hup => libc::SIGHUP,
            Signal::Int => libc::SIGINT,
            Signal::Usr1 => libc::SIGUSR1,
            Signal::Usr2 => libc::SIGUSR2,
        }
    }
}

/// Parse `TERM`, `sigterm`, `SIGSTOP`, ... into a [`Signal`].
pub fn parse_signal(name: &str) -> Result<Signal> {
    let upper = name.trim().to_ascii_uppercase();
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    Ok(match bare {
        "TERM" => Signal::Term,
        "KILL" => Signal::Kill,
        "STOP" => Signal::Stop,
        "CONT" => Signal::Cont,
        "HUP" => Signal::Hup,
        "INT" => Signal::Int,
        "USR1" => Signal::Usr1,
        "USR2" => Signal::Usr2,
        _ => {
            return Err(Error::invalid_argument(format!(
                "unknown signal {name:?} (TERM, KILL, STOP, CONT, HUP, INT, USR1, USR2)"
            )));
        }
    })
}

/// [`kill_tracked`] without the escalation: one signal, same guards.
pub(crate) fn signal_tracked(
    pid: u32,
    expected_starttime: Option<u64>,
    signal: Signal,
) -> KillOutcome {
    let Some(current) = host_starttime(pid) else {
        return KillOutcome::Gone;
    };
    match expected_starttime {
        None => return KillOutcome::Unverified,
        Some(expected) if expected != current => return KillOutcome::Reused,
        Some(_) => {}
    }
    unsafe {
        libc::kill(pid as i32, signal.number());
    }
    KillOutcome::Signalled
}

/// Check whether a process is alive **and not a zombie**.
///
/// `kill(pid, 0)` alone is insufficient: a zombie (a process that has
/// exited but hasn't been waited-on by its parent) still has an entry
/// in the kernel process table and `kill(pid, 0)` returns 0. nlink-lab's
/// own spawns are detached and reaped by init
/// (`ns_exec::spawn_detached`), but a tracked pid may still be
/// a zombie of *its* parent (a daemon's `-B` wrapper, a shell).
///
/// To match the user-facing meaning of "alive" (the process is
/// actually running), we also read `/proc/<pid>/stat` and treat the
/// `Z` (zombie) state as not alive. The `stat` format is:
///
/// ```text
/// PID (comm) STATE PPID …
/// ```
///
/// where `comm` may contain spaces and parentheses, so we parse from
/// the last `)` rightward.
pub(crate) fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid as i32, 0) } != 0
        // EPERM: it exists, and is someone else's — a root-spawned
        // process asked about by a member of the state root's group
        && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
    {
        return false; // ESRCH: PID is gone entirely.
    }
    // PID exists. Check if it's a zombie.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(content) => content
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_none_or(|state| state != "Z"),
        // reaped between the two looks: a zombie that was, so not alive
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // unreadable — kill(pid, 0) said the PID is at least present
        Err(_) => true,
    }
}

#[cfg(test)]
mod pid_alive_tests {
    use super::*;

    /// Someone else's live process is alive: as a user, `kill(1, 0)` is
    /// EPERM, which read as "gone" made every root-spawned process look
    /// dead to a non-root `ps`.
    #[test]
    fn another_users_process_is_alive() {
        assert!(pid_is_alive(1));
    }

    /// A live, busy process is reported alive.
    #[test]
    fn alive_for_running_process() {
        // sleep(60) gives us 60 seconds to check. Spawn it, take the
        // PID, kill at the end of the test.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(
            pid_is_alive(pid),
            "expected sleep(60) to be alive immediately after spawn"
        );
        // Cleanup: kill + reap to avoid leaving a zombie behind.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A zombie process (exited, not yet reaped) must read as **dead**.
    /// This is the regression test for the integration-suite failure:
    /// before the /proc/<pid>/stat check was added, kill(pid, 0)
    /// returned 0 for zombies and pid_is_alive() falsely returned true.
    #[test]
    fn dead_for_zombie() {
        // Spawn `true`, capture its pid, drop the Child without
        // wait()-ing. The process exits ~immediately; std::process::
        // Child's Drop does NOT reap, so it sticks as a zombie.
        let child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        std::mem::drop(child); // intentionally don't wait()
        // Give the kernel a moment to actually run + exit `true`.
        // 50ms is generous on any modern host.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Confirm it's actually a zombie by reading /proc directly.
        // If for some reason it isn't (e.g. running on a system with
        // SIGCHLD-IGN'd by some test framework), skip the assertion
        // rather than fail spuriously.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .unwrap_or_default();
        let is_zombie = stat
            .rsplit_once(')')
            .and_then(|(_, after)| after.split_whitespace().next())
            .is_some_and(|state| state == "Z");

        if is_zombie {
            assert!(
                !pid_is_alive(pid),
                "zombie pid {pid} must read as dead; /proc says state=Z"
            );
        } else {
            // Process was already reaped (e.g. test runner has a
            // SIGCHLD handler) — there's no zombie to test against.
            // The completely-gone case is covered separately below.
            eprintln!(
                "skipping zombie assertion: pid {pid} not in zombie state \
                 (test runner may have reaped it)"
            );
        }

        // Reap it so the test process leaves no zombie behind.
        unsafe {
            let mut status = 0;
            libc::waitpid(pid as i32, &mut status, libc::WNOHANG);
        }
    }

    /// A reaped (gone) PID must read as dead.
    #[test]
    fn dead_for_reaped_pid() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        // Reap it ourselves.
        let _ = child.wait();
        // Give the scheduler a moment to actually free the slot.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!pid_is_alive(pid), "reaped pid {pid} must read as dead");
    }
}

#[cfg(test)]
mod proc_net_tcp_tests {
    use super::*;

    /// A real-world `/proc/<pid>/net/tcp` snippet: one listener on
    /// 0.0.0.0:8080 (port 0x1F90) plus one ESTABLISHED conn that
    /// must NOT be matched as a listener.
    const SAMPLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:0050 0100007F:E1F0 01 00000000:00000000 00:00000000 00000000     0        0 67890 1 0000000000000000 0 0 0 10 0\n";

    #[test]
    fn finds_listener_on_matching_port() {
        assert!(proc_net_tcp_has_listener(SAMPLE, "1F90"));
    }

    #[test]
    fn rejects_non_matching_port() {
        assert!(!proc_net_tcp_has_listener(SAMPLE, "0050"));
    }

    /// State 01 = TCP_ESTABLISHED. Even if the port matches, an
    /// established connection is not a listener.
    #[test]
    fn rejects_established_state() {
        // Port 0x0050 (80) appears with state 01 in SAMPLE — matches
        // port-wise but not state-wise.
        assert!(!proc_net_tcp_has_listener(SAMPLE, "0050"));
    }

    #[test]
    fn empty_input_returns_false() {
        assert!(!proc_net_tcp_has_listener("", "1F90"));
        // Header-only also returns false.
        assert!(!proc_net_tcp_has_listener(
            "  sl  local_address ...\n",
            "1F90"
        ));
    }
}

#[cfg(test)]
mod pid_identity_tests {
    use super::*;

    #[test]
    fn a_signal_death_is_128_plus_the_signal() {
        // #226: it was -1, which `exec` exited 0 for
        let status = |script: &str| {
            std::process::Command::new("sh")
                .args(["-c", script])
                .status()
                .unwrap()
        };
        assert_eq!(exit_code_of(status("kill -9 $$")), 137);
        assert_eq!(exit_code_of(status("kill -TERM $$")), 143);
        assert_eq!(exit_code_of(status("exit 3")), 3);
    }

    #[test]
    fn host_starttime_of_self_is_stable_and_nonzero() {
        let me = std::process::id();
        let a = host_starttime(me).expect("own /proc entry");
        let b = host_starttime(me).expect("own /proc entry");
        assert_eq!(a, b);
        assert!(a > 0);
    }

    #[test]
    fn host_starttime_of_missing_pid_is_none() {
        // PID_MAX is 4194304 on 64-bit; nothing lives above it.
        assert_eq!(host_starttime(4_194_305), None);
    }

    #[test]
    fn parse_signal_accepts_bare_and_prefixed_names() {
        assert_eq!(parse_signal("STOP").unwrap(), Signal::Stop);
        assert_eq!(parse_signal("sigcont").unwrap(), Signal::Cont);
        assert_eq!(parse_signal(" SIGKILL ").unwrap(), Signal::Kill);
        assert!(parse_signal("SEGV").is_err(), "not in the closed set");
        assert!(parse_signal("9").is_err(), "numbers are not accepted");
    }

    #[test]
    fn signal_tracked_never_signals_unverified_or_reused() {
        let me = std::process::id();
        let real = host_starttime(me).unwrap();
        assert_eq!(
            signal_tracked(me, None, Signal::Stop),
            KillOutcome::Unverified
        );
        assert_eq!(
            signal_tracked(me, Some(real + 1), Signal::Stop),
            KillOutcome::Reused
        );
        assert_eq!(
            signal_tracked(4_194_305, Some(1), Signal::Stop),
            KillOutcome::Gone
        );
    }

    #[test]
    fn kill_tracked_never_signals_unverified_or_reused() {
        let me = std::process::id();
        let real = host_starttime(me).unwrap();
        // No recorded start time → refuse (would otherwise SIGTERM the test runner).
        assert_eq!(kill_tracked(me, None), KillOutcome::Unverified);
        // Wrong start time → the PID was reused → refuse.
        assert_eq!(kill_tracked(me, Some(real + 1)), KillOutcome::Reused);
        // Gone PID → nothing to do, regardless of the recorded value.
        assert_eq!(kill_tracked(4_194_305, Some(1)), KillOutcome::Gone);
    }
}

/// One named firewall counter (`counter name …`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FirewallCounter {
    pub name: String,
    pub packets: u64,
    pub bytes: u64,
}

/// One way a route goes: its gateway and output device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHop {
    pub via: Option<std::net::IpAddr>,
    pub dev: Option<String>,
}

impl std::fmt::Display for RouteHop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut sep = "";
        if let Some(via) = &self.via {
            write!(f, "via {via}")?;
            sep = " ";
        }
        if let Some(dev) = &self.dev {
            write!(f, "{sep}dev {dev}")?;
        }
        Ok(())
    }
}

async fn read_main_routes_to(
    ns: &crate::deploy::NsRef,
    dst: std::net::IpAddr,
    prefix_len: u8,
) -> Result<Vec<Vec<RouteHop>>> {
    let conn: Connection<Route> = ns
        .connection()
        .map_err(|e| Error::nlink("route reader", e))?;
    let names: BTreeMap<u32, String> = conn
        .get_links()
        .await
        .map_err(|e| Error::nlink("route reader", e))?
        .iter()
        .filter_map(|l| Some((l.ifindex(), l.name()?.to_string())))
        .collect();
    let dev = |ifindex: u32| names.get(&ifindex).cloned();
    let family = if dst.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    } as u8;
    let want = crate::helpers::network_address(dst, prefix_len);
    let routes = conn
        .get_routes_for_table(254)
        .await
        .map_err(|e| Error::nlink("route reader", e))?;
    Ok(routes
        .iter()
        .filter(|r| r.family() == family && r.dst_len() == prefix_len)
        .filter(|r| {
            r.destination()
                .map(|d| crate::helpers::network_address(*d, prefix_len))
                .unwrap_or(crate::helpers::network_address(dst, 0))
                == want
        })
        .map(|r| match r.multipath() {
            Some(hops) if !hops.is_empty() => hops
                .iter()
                .map(|h| RouteHop {
                    via: h.gateway,
                    dev: dev(h.ifindex),
                })
                .collect(),
            _ => vec![RouteHop {
                via: r.gateway().or(r.via()).copied(),
                dev: r.oif().and_then(dev),
            }],
        })
        .collect())
}

async fn read_firewall_counters(ns: &crate::deploy::NsRef) -> Result<Vec<FirewallCounter>> {
    use nlink::netlink::Nftables;
    use nlink::netlink::nftables::{Family, ObjectState};
    let conn: Connection<Nftables> = ns.connection()?;
    let objects = match conn
        .list_objects_in(crate::deploy::plan::nftables::NLINK_LAB_TABLE, Family::Inet)
        .await
    {
        Ok(o) => o,
        // no firewall on the node: no table, no counters
        Err(e) if e.is_not_found() => return Ok(Vec::new()),
        Err(e) => return Err(Error::nlink("read firewall counters", e)),
    };
    Ok(objects
        .into_iter()
        .filter_map(|o| match o.state {
            ObjectState::Counter { packets, bytes, .. } => Some(FirewallCounter {
                name: o.name,
                packets,
                bytes,
            }),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod state_merge_tests {
    use super::*;

    /// A tracked PID that is alive but started at another time is another
    /// process: `ps` reports ours as dead rather than the stranger as it.
    /// The processes carrying a tag in a PID namespace are found and
    /// stopped; an untagged one is left alone (#215). The test's own
    /// namespace stands in for a container's.
    #[test]
    fn tagged_processes_are_found_and_stopped() {
        let tag = new_process_tag("t", "n");
        let spawn = |env: Option<&str>| {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            if let Some(tag) = env {
                c.env(TAG_VAR, tag);
            }
            c.spawn().unwrap()
        };
        let mut tagged = spawn(Some(&tag));
        let mut other = spawn(None);
        let me = std::process::id();
        // `spawn` returns while the child is still in execve (posix_spawn
        // wakes the parent at the mm switch), when its environ may not be
        // readable yet: it failed about one run in six under load
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while tagged_pids(me, &tag).is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(tagged_pids(me, &tag), vec![tagged.id() as i32]);
        assert_eq!(stop_tagged(me, &tag), 1);
        assert!(
            tagged.wait().unwrap().code().is_none(),
            "killed by a signal"
        );
        assert!(
            other.try_wait().unwrap().is_none(),
            "the untagged one runs on"
        );
        other.kill().unwrap();
        other.wait().unwrap();
    }

    /// A wait on a node the lab does not have is an error at once; it
    /// waited out its whole timeout and exited 124 (#209).
    #[tokio::test]
    async fn waits_on_an_unknown_node_fail_at_once() {
        let lab = RunningLab::new(
            Topology::default(),
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            Vec::new(),
            false,
            false,
        );
        let long = std::time::Duration::from_secs(30);
        let tick = std::time::Duration::from_millis(10);
        let start = std::time::Instant::now();
        for result in [
            lab.wait_for_tcp("nosuch", "127.0.0.1", 80, long, tick)
                .await,
            lab.wait_for_exec("nosuch", "true", long, tick).await,
            lab.wait_for_file("nosuch", "/x", long, tick).await,
        ] {
            assert!(
                matches!(result, Err(Error::NodeNotFound { .. })),
                "{result:?}"
            );
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_reused_pid_is_not_alive() {
        let me = std::process::id();
        let started = host_starttime(me).unwrap();
        let lab = |start: u64| {
            let mut lab = RunningLab::new(
                Topology::default(),
                BTreeMap::new(),
                BTreeMap::new(),
                None,
                vec![("n".into(), me)],
                false,
                false,
            );
            lab.set_starttimes([(me, start)].into());
            lab
        };
        assert!(lab(started).process_status()[0].alive);
        assert!(!lab(started + 1).process_status()[0].alive);
    }

    /// `ip … --iface mgmt0` reports the address deploy gives the node's
    /// mgmt veth, for a container node and for a namespace node beside
    /// it: it numbered over namespace nodes alone, and refused containers.
    #[test]
    fn node_addresses_match_the_planned_mgmt_addresses() {
        let topo = crate::parser::parse(
            r#"lab "m" { mgmt 172.31.0.0/24 host-reachable }
node app image "alpine:latest"
node db
link app:eth0 -- db:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let plan = crate::deploy::plan_for(&topo).unwrap();
        let planned: BTreeMap<String, String> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                crate::deploy::Op::CreateMgmtVeth {
                    node,
                    node_ip,
                    prefix,
                    ..
                } => Some((node.clone(), format!("{node_ip}/{prefix}"))),
                _ => None,
            })
            .collect();
        let container = ContainerState {
            id: "c".into(),
            name: "m-app".into(),
            image: "alpine:latest".into(),
            pid: 1,
            starttime: None,
        };
        let lab = RunningLab::new(
            topo,
            [("db".to_string(), "m-db".to_string())].into(),
            [("app".to_string(), container)].into(),
            Some("podman".into()),
            Vec::new(),
            false,
            false,
        );
        for node in ["app", "db"] {
            let addrs = lab.node_addresses(node).unwrap();
            assert_eq!(addrs["mgmt0"], [planned[node].clone()], "{node}: {addrs:?}");
        }
        assert!(lab.node_addresses("nope").is_err());
    }

    /// A mutation re-reads the state under the lock (#332): what another
    /// handle saved meanwhile comes in, and what this handle changed and
    /// never saved (a pid a `#[lab_test]` body spawned) stays, and is
    /// written by the commit.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // the state-dir test lock, one test at a time
    async fn a_mutation_takes_in_the_file_and_keeps_its_own_changes() {
        let _guard = crate::state::xdg_state_lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(crate::state::STATE_DIR_ENV, dir.path()) };
        let topology = crate::parser::parse("lab \"mut\"\nnode a\n").unwrap();
        crate::state::save(&crate::state::LabState::new("mut", "now"), &topology).unwrap();

        let mut mine = RunningLab::load("mut").unwrap();
        let theirs = {
            let mut t = RunningLab::load("mut").unwrap();
            t.live_impairments
                .insert("a:eth0".into(), crate::types::Impairment::default());
            t.save_state().unwrap();
            t
        };
        // never saved here
        mine.pids.push(("a".into(), 4242));

        let m = mine.begin("test").await.unwrap();
        assert!(
            mine.live_impairments.contains_key("a:eth0"),
            "theirs came in"
        );
        assert_eq!(mine.pids, [("a".to_string(), 4242)], "mine stayed");
        mine.commit(m).unwrap();

        let (st, _) = crate::state::load("mut").unwrap();
        assert_eq!(st.pids, [("a".to_string(), 4242)]);
        assert!(st.live_impairments.contains_key("a:eth0"));
        drop(theirs);
    }

    /// A mutation waits for the lab's lock, then fails with `Busy`
    /// naming the holder (#332); a lab destroyed while it waited is gone.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // the state-dir test lock, one test at a time
    async fn a_mutation_waits_then_names_who_holds_the_lab() {
        let _guard = crate::state::xdg_state_lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(crate::state::STATE_DIR_ENV, dir.path()) };
        unsafe { std::env::set_var(crate::state::LOCK_TIMEOUT_ENV, "300ms") };
        let topology = crate::parser::parse("lab \"busy\"\nnode a\n").unwrap();
        crate::state::save(&crate::state::LabState::new("busy", "now"), &topology).unwrap();
        let mut lab = RunningLab::load("busy").unwrap();

        let held = crate::state::lock_for("busy", "deploy").unwrap();
        let err = lab.begin("impair").await.err().expect("busy");
        assert!(matches!(err, Error::Busy(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("deploy by pid"), "{msg}");
        assert!(msg.contains("this process"), "{msg}");
        drop(held);
        drop(lab.begin("impair").await.unwrap());

        crate::state::remove("busy").unwrap();
        let err = lab.begin("impair").await.err().expect("gone");
        assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
        unsafe { std::env::remove_var(crate::state::LOCK_TIMEOUT_ENV) };
    }

    /// Two handles of one lab, each saving its own change: the merge keeps
    /// both, and a removal by one does not resurrect or drop the other's.
    #[test]
    fn saves_merge_each_handles_own_changes() {
        let base: Vec<(String, u32)> = vec![("a".into(), 1)];
        // B saved first: added 3. A, loaded before that, added 2.
        let mut fresh = vec![("a".into(), 1), ("b".into(), 3)];
        merge_pids(&mut fresh, &base, &[("a".into(), 1), ("a".into(), 2)]);
        assert_eq!(
            fresh,
            [
                ("a".to_string(), 1),
                ("b".to_string(), 3),
                ("a".to_string(), 2)
            ]
        );
        // A killed 1 and added nothing; B's 3 stays.
        let mut fresh = vec![("a".into(), 1), ("b".into(), 3)];
        merge_pids(&mut fresh, &base, &[]);
        assert_eq!(fresh, [("b".to_string(), 3)]);

        let m = |kv: &[(&str, u32)]| -> BTreeMap<String, u32> {
            kv.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        let mut fresh = m(&[("x", 1), ("y", 9), ("z", 5)]);
        // Mine changed x, removed z, added w; y is someone else's.
        merge_map(
            &mut fresh,
            &m(&[("x", 1), ("z", 5)]),
            &m(&[("x", 2), ("w", 7)]),
        );
        assert_eq!(fresh, m(&[("w", 7), ("x", 2), ("y", 9)]));

        let mut ns = m(&[("h3", 1)]);
        take_changed(&mut ns, &m(&[]), &m(&[]));
        assert_eq!(
            ns,
            m(&[("h3", 1)]),
            "unchanged here: the file's value stays"
        );
        take_changed(&mut ns, &m(&[]), &m(&[("h1", 1)]));
        assert_eq!(ns, m(&[("h1", 1)]));
    }
}
