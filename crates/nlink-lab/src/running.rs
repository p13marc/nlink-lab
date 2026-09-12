//! Running lab interaction.
//!
//! [`RunningLab`] provides methods to interact with a deployed lab:
//! executing commands, spawning processes, modifying impairments, and destroying.

use std::collections::BTreeMap;

use nlink::netlink::diagnostics::{Diagnostics, InterfaceDiag, Issue};
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
    /// Whether mac80211_hwsim was loaded.
    wifi_loaded: bool,
    /// Saved impairments before partition (endpoint → Impairment).
    saved_impairments: BTreeMap<String, crate::types::Impairment>,
    /// Log file paths for spawned processes: pid → (stdout_path, stderr_path).
    process_logs: BTreeMap<u32, (String, String)>,
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
}

/// Output from executing a command in a lab node.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Process exit code.
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

/// Status of a tracked background process.
///
/// **Retention**: tracked processes are *not* removed from the lab's PID
/// list when they exit. They remain with `alive == false` until the lab
/// is destroyed (or the state file is cleaned manually). Consumers
/// polling for "is X still running?" must check [`alive`](Self::alive),
/// not just look up the PID.
#[derive(Debug, Clone, serde::Serialize)]
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
    /// Path to stderr log file (if captured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
}

/// Diagnostic results for a single node.
#[derive(Debug)]
pub struct NodeDiagnostic {
    /// Node name.
    pub node: String,
    /// Per-interface diagnostics.
    pub interfaces: Vec<InterfaceDiag>,
    /// Issues detected.
    pub issues: Vec<Issue>,
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
            process_logs: BTreeMap::new(),
            assertion_results: Vec::new(),
        }
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
    pub fn ns_resolver_of(&self, node: &str) -> Option<crate::deploy::NsRef> {
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

    /// Whether mac80211_hwsim was loaded.
    pub fn wifi_loaded(&self) -> bool {
        self.wifi_loaded
    }

    /// Get the number of nodes (namespaces + containers).
    pub fn namespace_count(&self) -> usize {
        self.namespace_names.len() + self.containers.len()
    }

    /// Get node names.
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.namespace_names
            .keys()
            .chain(self.containers.keys())
            .map(|s| s.as_str())
    }

    /// Look up the namespace name for a node.
    /// Get the namespace name for a bare namespace node.
    pub fn namespace_for(&self, node: &str) -> Result<&str> {
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

    /// Access background PIDs (crate-internal).
    pub(crate) fn pids(&self) -> &[(String, u32)] {
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
        if let Some(container) = self.containers.get(node) {
            return namespace::connection_for_pid(container.pid).map_err(|e| {
                Error::deploy_failed(format!(
                    "connection for container node '{node}' (pid {}): {e}",
                    container.pid
                ))
            });
        }
        let ns_name = self.namespace_for(node)?;
        namespace::connection_for(ns_name)
            .map_err(|e| Error::deploy_failed(format!("connection for '{ns_name}': {e}")))
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
        self.mgmt_peers = mgmt_peers;
        self.dns_injected = dns_injected;
        self.wifi_loaded = wifi_loaded;
    }

    pub(crate) fn set_process_logs(&mut self, logs: BTreeMap<u32, (String, String)>) {
        self.process_logs = logs;
    }

    pub(crate) fn process_logs_map(&self) -> &BTreeMap<u32, (String, String)> {
        &self.process_logs
    }

    pub(crate) fn saved_impairments_map(&self) -> &BTreeMap<String, crate::types::Impairment> {
        &self.saved_impairments
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
        if let Some(container) = self.containers.get(node) {
            // Use docker/podman exec for container nodes
            let rt_binary = self
                .runtime_binary
                .as_deref()
                .ok_or_else(|| Error::deploy_failed("no container runtime binary in state"))?;
            let wd_str = opts.workdir.map(|p| p.to_string_lossy().into_owned());
            let env_pairs: Vec<String> = opts.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let mut all_args: Vec<&str> = vec!["exec"];
            if let Some(ref wd) = wd_str {
                all_args.push("-w");
                all_args.push(wd.as_str());
            }
            for pair in &env_pairs {
                all_args.push("-e");
                all_args.push(pair.as_str());
            }
            all_args.push(&container.id);
            all_args.push(cmd);
            all_args.extend(args);
            let mut command = std::process::Command::new(rt_binary);
            command.args(&all_args);
            if let Some(timeout) = opts.timeout {
                let mut child = command
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        Error::deploy_failed(format!("exec in container '{node}' failed: {e}"))
                    })?;
                wait_with_timeout(&mut child, timeout)?;
                let output = child.wait_with_output().map_err(Error::Io)?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                })
            } else {
                let output = command.output().map_err(|e| {
                    Error::deploy_failed(format!("exec in container '{node}' failed: {e}"))
                })?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                })
            }
        } else {
            let ns_name = self.namespace_for(node)?;
            let mut command = std::process::Command::new(cmd);
            command.args(args);
            if let Some(wd) = opts.workdir {
                command.current_dir(wd);
            }
            for (k, v) in opts.env {
                command.env(k, v);
            }
            if let Some(timeout) = opts.timeout {
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::piped());
                let mut child = crate::ns_exec::spawn(ns_name, command)
                    .map_err(|e| Error::deploy_failed(format!("exec in '{node}' failed: {e}")))?;
                wait_with_timeout(&mut child, timeout)?;
                let output = child.wait_with_output().map_err(Error::Io)?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                })
            } else {
                let output = crate::ns_exec::spawn_output(ns_name, command)
                    .map_err(|e| Error::deploy_failed(format!("exec in '{node}' failed: {e}")))?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                })
            }
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
        if let Some(container) = self.containers.get(node) {
            let rt_binary = self
                .runtime_binary
                .as_deref()
                .ok_or_else(|| Error::deploy_failed("no container runtime binary in state"))?;
            let wd_str = opts.workdir.map(|p| p.to_string_lossy().into_owned());
            let env_pairs: Vec<String> = opts.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let mut all_args: Vec<&str> = vec!["exec", "-i"];
            if let Some(ref wd) = wd_str {
                all_args.push("-w");
                all_args.push(wd.as_str());
            }
            for pair in &env_pairs {
                all_args.push("-e");
                all_args.push(pair.as_str());
            }
            all_args.push(&container.id);
            all_args.push(cmd);
            all_args.extend(args);
            let mut command = std::process::Command::new(rt_binary);
            command
                .args(&all_args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
            run_with_optional_timeout(&mut command, opts.timeout).map_err(|e| match e {
                Error::Timeout(_) => e,
                other => Error::deploy_failed(format!(
                    "attached exec in container '{node}' failed: {other}"
                )),
            })
        } else {
            let ns_name = self.namespace_for(node)?;
            // Enter the namespace via nsenter so stdio inherits naturally.
            // Uses the `--net=<path>` single-argv form (see the same pattern
            // used by the `shell` subcommand).
            let ns_path = format!("/var/run/netns/{ns_name}");
            let mut command = std::process::Command::new("nsenter");
            command
                .arg(format!("--net={ns_path}"))
                .arg("--")
                .arg(cmd)
                .args(args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
            if let Some(wd) = opts.workdir {
                command.current_dir(wd);
            }
            for (k, v) in opts.env {
                command.env(k, v);
            }
            run_with_optional_timeout(&mut command, opts.timeout).map_err(|e| match e {
                Error::Timeout(_) => e,
                other => Error::deploy_failed(format!("attached exec in '{node}' failed: {other}")),
            })
        }
    }

    /// Spawn a background process in a lab node.
    pub fn spawn(&mut self, node: &str, cmd: &[&str]) -> Result<u32> {
        if cmd.is_empty() {
            return Err(Error::invalid_topology("empty command"));
        }
        let ns_name = self.namespace_for(node)?;

        let mut command = std::process::Command::new(cmd[0]);
        command.args(&cmd[1..]);

        let pid = crate::ns_exec::spawn_detached(ns_name, command)
            .map_err(|e| Error::deploy_failed(format!("spawn in '{node}' failed: {e}")))?;
        self.track_pid(node, pid);
        Ok(pid)
    }

    /// Re-save the current state to disk (e.g., after spawning a new process).
    pub fn save_state(&self) -> Result<()> {
        // Take turns with a concurrent deploy/apply/destroy: this is a
        // read-modify-write of state.json (issue #38).
        let _lock = crate::state::lock_blocking(&self.topology.lab.name)?;
        let (mut lab_state, _) = state::load(&self.topology.lab.name)?;
        lab_state.namespaces = self.namespace_names.clone();
        lab_state.containers = self.containers.clone();
        lab_state.runtime = self.runtime_binary.clone();
        lab_state.dns_injected = self.dns_injected;
        lab_state.wifi_loaded = self.wifi_loaded;
        lab_state.pids = self.pids.clone();
        lab_state.starttimes = self.starttimes.clone();
        lab_state.exec_pids = self.exec_pids.clone();
        lab_state.mgmt_peers = self.mgmt_peers.clone();
        lab_state.saved_impairments = self.saved_impairments.clone();
        lab_state.process_logs = self.process_logs.clone();
        state::save(&lab_state, &self.topology)
    }

    /// Record a container node's new init PID (after `restart`) so every
    /// later `/proc/<pid>/ns/net` reference targets the live container.
    pub fn set_container_pid(&mut self, node: &str, pid: u32) -> Result<()> {
        match self.containers.get_mut(node) {
            Some(c) => {
                c.pid = pid;
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
        let ns_name = self.namespace_for(node)?.to_string();

        let log_dir = opts
            .log_dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| state::logs_dir(self.name()));
        std::fs::create_dir_all(&log_dir)?;

        let cmd_basename = std::path::Path::new(cmd[0])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("cmd");

        let stdout_path = log_dir.join(format!("{node}-{cmd_basename}.stdout"));
        let stderr_path = log_dir.join(format!("{node}-{cmd_basename}.stderr"));

        let stdout_file = std::fs::File::create(&stdout_path)?;
        let stderr_file = std::fs::File::create(&stderr_path)?;

        let mut command = std::process::Command::new(cmd[0]);
        command.args(&cmd[1..]);
        command.stdout(stdout_file);
        command.stderr(stderr_file);
        if let Some(wd) = opts.workdir {
            command.current_dir(wd);
        }
        for (k, v) in opts.env {
            command.env(k, v);
        }

        let pid = crate::ns_exec::spawn_detached(&ns_name, command)
            .map_err(|e| Error::deploy_failed(format!("spawn in '{node}' failed: {e}")))?;
        self.track_pid(node, pid);
        self.process_logs.insert(
            pid,
            (
                stdout_path.to_string_lossy().to_string(),
                stderr_path.to_string_lossy().to_string(),
            ),
        );

        // Rename files to include PID
        let final_stdout = log_dir.join(format!("{node}-{cmd_basename}-{pid}.stdout"));
        let final_stderr = log_dir.join(format!("{node}-{cmd_basename}-{pid}.stderr"));
        let _ = std::fs::rename(&stdout_path, &final_stdout);
        let _ = std::fs::rename(&stderr_path, &final_stderr);
        self.process_logs.insert(
            pid,
            (
                final_stdout.to_string_lossy().to_string(),
                final_stderr.to_string_lossy().to_string(),
            ),
        );

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
        // Verify node exists
        self.namespace_for(node)?;

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

        // From host-reachable management network (mgmt0)
        if self.topology.lab.mgmt_host_reachable
            && let Some(ref mgmt_subnet) = self.topology.lab.mgmt_subnet
            && let Ok((base_ip, prefix)) = crate::helpers::parse_cidr(mgmt_subnet)
            && let std::net::IpAddr::V4(base_v4) = base_ip
        {
            let base_u32 = u32::from(base_v4);
            // Nodes get .2, .3, ... in sorted order (same as deploy)
            let mut sorted_nodes: Vec<&str> =
                self.namespace_names.keys().map(|s| s.as_str()).collect();
            sorted_nodes.sort();
            if let Some(idx) = sorted_nodes.iter().position(|&n| n == node) {
                let node_ip = std::net::Ipv4Addr::from(base_u32 + 2 + idx as u32);
                addrs
                    .entry("mgmt0".to_string())
                    .or_default()
                    .push(format!("{node_ip}/{prefix}"));
            }
        }

        Ok(addrs)
    }

    /// Wait for a TCP port to accept connections inside a node's namespace.
    pub async fn wait_for_tcp(
        &self,
        node: &str,
        ip: &str,
        port: u16,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let probe = self.exec(
                node,
                "bash",
                &["-c", &format!("echo > /dev/tcp/{ip}/{port}")],
            );
            if probe.is_ok_and(|o| o.exit_code == 0) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::deploy_failed(format!(
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
            let probe = self.exec(node, "sh", &["-c", cmd]);
            if probe.is_ok_and(|o| o.exit_code == 0) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::deploy_failed(format!(
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
                if let Ok(contents) = std::fs::read_to_string(p)
                    && contents.lines().any(|line| pattern.is_match(line))
                {
                    return Ok(());
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::deploy_failed(format!(
                    "timeout waiting for log line matching '{}' on PID {pid}",
                    pattern.as_str()
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait until process `pid` has a TCP listener bound to `port`
    /// inside its namespace. Reads `/proc/<pid>/net/tcp` and
    /// `/proc/<pid>/net/tcp6` (which reflect the namespace's socket
    /// table when the reading process is in the same netns) and looks
    /// for a row with `st = 0A` (TCP_LISTEN) and the matching local
    /// port.
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
        pid: u32,
        port: u16,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let interval = std::cmp::min(interval, std::time::Duration::from_millis(250));
        let port_hex = format!("{port:04X}");
        loop {
            for proto in &["tcp", "tcp6"] {
                let path = format!("/proc/{pid}/net/{proto}");
                if let Ok(out) = self.exec(node, "cat", &[&path])
                    && out.exit_code == 0
                    && proc_net_tcp_has_listener(&out.stdout, &port_hex)
                {
                    return Ok(());
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::deploy_failed(format!(
                    "timeout waiting for TCP listener on port {port} for PID {pid}"
                )));
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
    pub async fn wait_for_fd_stable(
        &self,
        node: &str,
        pid: u32,
        stable_for: std::time::Duration,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let interval = std::cmp::min(interval, std::time::Duration::from_millis(250));
        let mut last_count: Option<u32> = None;
        let mut last_change = std::time::Instant::now();
        loop {
            let count = count_fd_dir(self, node, pid)?;
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
                return Err(Error::deploy_failed(format!(
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
            let probe = self.exec(node, "test", &["-e", path]);
            if probe.is_ok_and(|o| o.exit_code == 0) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::deploy_failed(format!(
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
        Err(Error::deploy_failed(format!(
            "no link found for endpoint '{endpoint}'"
        )))
    }

    /// Modify the netem impairment on an interface at runtime.
    pub async fn set_impairment(
        &self,
        endpoint: &str,
        impairment: &crate::types::Impairment,
    ) -> Result<()> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        let conn = self.route_conn_for(&ep.node)?;

        let netem = crate::deploy::build_netem(impairment)?;

        // `replace_qdisc` is add-or-update in one idempotent call, so no
        // change-then-add fallback that used to swallow every error from
        // the first attempt (issue #40).
        conn.replace_qdisc(&ep.iface, netem)
            .await
            .map_err(|e| Error::deploy_failed(format!("set impairment on '{endpoint}': {e}")))
    }

    /// Whether the given endpoint is currently in
    /// [`partition`](Self::partition)ed state — i.e. its pre-partition
    /// impairment is saved off and a 100% loss qdisc is installed.
    /// Used by `nlink-lab impair --show --json` to surface the
    /// partition flag distinct from "user installed `--loss 100%`".
    pub fn is_partitioned(&self, endpoint: &str) -> bool {
        self.saved_impairments.contains_key(endpoint)
    }

    /// Remove all impairments from an interface.
    ///
    /// Idempotent: a `QdiscNotFound` from the kernel is treated as
    /// success — we're converging on the same end state. Other errors
    /// propagate. Also prunes any `saved_impairments` bookkeeping for
    /// this endpoint so a subsequent [`partition`](Self::partition) on
    /// the same endpoint goes through the real install path instead of
    /// short-circuiting on a stale "is partitioned" flag.
    pub async fn clear_impairment(&mut self, endpoint: &str) -> Result<()> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        let conn = self.route_conn_for(&ep.node)?;

        // Delete the root qdisc (removes all netem config). Idempotent:
        // a missing qdisc is the same as "already cleared" —
        // `del_qdisc_if_exists` (nlink 0.24) returns Ok(false) rather
        // than a `QdiscNotFound` error we'd have to match.
        conn.del_qdisc_if_exists(&ep.iface, nlink::TcHandle::ROOT)
            .await
            .map_err(|e| Error::deploy_failed(format!("clear impairment on '{endpoint}': {e}")))?;

        // Drop any "is partitioned" bookkeeping for this endpoint and
        // persist. Without this, a follow-up `partition()` would see
        // the stale entry and return early without installing the
        // qdisc — the silent no-op reported as round-4 §1.
        if self.saved_impairments.remove(endpoint).is_some() {
            self.save_state()?;
        }
        Ok(())
    }

    /// Partition an endpoint: save current impairment, apply 100% loss.
    pub async fn partition(&mut self, endpoint: &str) -> Result<()> {
        // Don't double-partition (preserve original saved config)
        if self.saved_impairments.contains_key(endpoint) {
            return Ok(());
        }

        // Read current impairment from topology (or default if none)
        let current = self
            .topology
            .impairments
            .get(endpoint)
            .cloned()
            .unwrap_or_default();

        self.saved_impairments.insert(endpoint.to_string(), current);

        // Apply 100% loss
        let partition_imp = crate::types::Impairment {
            loss: Some("100%".to_string()),
            ..Default::default()
        };
        self.set_impairment(endpoint, &partition_imp).await?;
        self.save_state()?;
        Ok(())
    }

    /// Heal an endpoint: restore saved impairment from before partition.
    pub async fn heal(&mut self, endpoint: &str) -> Result<()> {
        let saved = self.saved_impairments.remove(endpoint).ok_or_else(|| {
            Error::deploy_failed(format!("endpoint '{endpoint}' is not partitioned"))
        })?;

        if saved == crate::types::Impairment::default() {
            self.clear_impairment(endpoint).await?;
        } else {
            self.set_impairment(endpoint, &saved).await?;
        }
        self.save_state()?;
        Ok(())
    }

    /// Run diagnostics on the lab, optionally filtered to a single node.
    pub async fn diagnose(&self, node: Option<&str>) -> Result<Vec<NodeDiagnostic>> {
        let mut results = Vec::new();

        // Diagnose bare namespace nodes
        for (node_name, ns_name) in &self.namespace_names {
            if let Some(filter) = node
                && node_name != filter
            {
                continue;
            }
            let conn: Connection<Route> = namespace::connection_for(ns_name)
                .map_err(|e| Error::deploy_failed(format!("connection for '{ns_name}': {e}")))?;
            let diag = Diagnostics::new(conn);
            let report = diag.scan().await.map_err(|e| {
                Error::deploy_failed(format!("diagnostics scan for '{node_name}': {e}"))
            })?;
            results.push(NodeDiagnostic {
                node: node_name.clone(),
                interfaces: report.interfaces,
                issues: report.issues,
            });
        }

        // Diagnose container nodes
        for (node_name, container) in &self.containers {
            if let Some(filter) = node
                && node_name != filter
            {
                continue;
            }
            let conn: Connection<Route> =
                namespace::connection_for_pid(container.pid).map_err(|e| {
                    Error::deploy_failed(format!("connection for container '{node_name}': {e}"))
                })?;
            let diag = Diagnostics::new(conn);
            let report = diag.scan().await.map_err(|e| {
                Error::deploy_failed(format!("diagnostics scan for container '{node_name}': {e}"))
            })?;
            results.push(NodeDiagnostic {
                node: node_name.clone(),
                interfaces: report.interfaces,
                issues: report.issues,
            });
        }

        Ok(results)
    }

    /// Kill a tracked background process by PID.
    pub fn kill_process(&self, pid: u32) -> Result<()> {
        if !self.pids.iter().any(|(_, p)| *p == pid) {
            return Err(Error::deploy_failed(format!(
                "pid {pid} is not tracked by lab '{}'",
                self.topology.lab.name
            )));
        }
        match kill_tracked(pid, self.starttimes.get(&pid).copied()) {
            KillOutcome::Signalled | KillOutcome::Gone => Ok(()),
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
        // Acquire exclusive lock
        let _lock = crate::state::lock(&self.topology.lab.name)?;

        // Release any subnet-pool entries this lab claimed at deploy
        // time (round-5 §2.5). Best-effort — pool errors are not
        // fatal for destroy. The pool flock keeps this safe against
        // concurrent free_for_lab calls.
        let _ = crate::subnet_pool::free_for_lab(&self.topology.lab.name);

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

        // 4. Delete namespaces (and their ownership tags)
        for ns_name in self.namespace_names.values() {
            if namespace::exists(ns_name)
                && let Err(e) = namespace::delete(ns_name)
            {
                tracing::warn!("failed to delete namespace '{ns_name}': {e}");
            }
            crate::netns_tag::untag(ns_name);
        }

        // 4b. Delete management namespace (bridges) if it exists
        if !self.topology.networks.is_empty() {
            let mgmt_ns = format!("{}-mgmt", self.topology.lab.prefix());
            if namespace::exists(&mgmt_ns)
                && let Err(e) = namespace::delete(&mgmt_ns)
            {
                tracing::warn!("failed to delete management namespace '{mgmt_ns}': {e}");
            }
            crate::netns_tag::untag(&mgmt_ns);
        }

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

        // 5c. Unload mac80211_hwsim and clean up WiFi configs
        if self.wifi_loaded {
            crate::wifi::release_hwsim(&self.topology.lab.name);
            crate::wifi::cleanup_configs(&self.topology.lab.name);
        }

        // 6. Remove state file
        state::remove(&self.topology.lab.name)?;

        Ok(())
    }

    /// Load a running lab from saved state.
    pub fn load(name: &str) -> Result<Self> {
        let (lab_state, topology) = state::load(name)?;
        Ok(Self {
            topology,
            namespace_names: lab_state.namespaces,
            containers: lab_state.containers,
            runtime_binary: lab_state.runtime,
            pids: lab_state.pids,
            dns_injected: lab_state.dns_injected,
            wifi_loaded: lab_state.wifi_loaded,
            saved_impairments: lab_state.saved_impairments,
            process_logs: lab_state.process_logs,
            starttimes: lab_state.starttimes,
            exec_pids: lab_state.exec_pids,
            mgmt_peers: lab_state.mgmt_peers,
            assertion_results: Vec::new(),
        })
    }

    /// List all saved labs.
    pub fn list() -> Result<Vec<LabInfo>> {
        state::list()
    }

    /// Check status of tracked background processes.
    ///
    /// `alive` is `true` only if the PID still exists **and** is not a
    /// zombie. Spawned processes are detached (double-forked and
    /// reparented to init, see [`crate::ns_exec::spawn_detached`]), so
    /// nlink-lab itself never leaves zombies; the check still matters
    /// for processes a spawned program forks and abandons (hostapd's
    /// `-B` parent, shell wrappers), which `kill(pid, 0)` keeps
    /// reporting as deliverable.
    pub fn process_status(&self) -> Vec<ProcessInfo> {
        self.pids
            .iter()
            .map(|(node, pid)| {
                let alive = pid_is_alive(*pid);
                let logs = self.process_logs.get(pid);
                ProcessInfo {
                    node: node.clone(),
                    pid: *pid,
                    host_pid: *pid,
                    alive,
                    stdout_log: logs.map(|(s, _)| s.clone()),
                    stderr_log: logs.map(|(_, s)| s.clone()),
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

    /// Sample resource usage for a single process inside a node's
    /// namespace. Reads `/proc/<pid>/{stat,status}` plus the entry
    /// count of `/proc/<pid>/fd/`, then assembles into a structured
    /// [`crate::ProcStat`].
    ///
    /// The reads happen via [`exec`](Self::exec) inside the target
    /// namespace, so:
    ///
    /// - The mount-namespaced `/proc` view (when `dns hosts` etc. has
    ///   set up `/etc/netns/`) is what the parser sees.
    /// - The exec runs as the same UID as `nlink-lab` itself —
    ///   typically root via `check_root` — so `/proc/<pid>/fd/` is
    ///   readable even though it's mode 0700 owned by the spawned
    ///   process's UID.
    ///
    /// `pid` is the host PID (same as ns PID — `CLONE_NEWPID` isn't
    /// used). See `docs/ARCHITECTURE.md` "Process & namespace model".
    /// (Round-5 §2.2.)
    pub fn proc_stat(&self, node: &str, pid: u32) -> Result<crate::proc_stat::ProcStat> {
        let stat_path = format!("/proc/{pid}/stat");
        let status_path = format!("/proc/{pid}/status");

        let stat_out = self.exec(node, "cat", &[&stat_path])?;
        if stat_out.exit_code != 0 {
            return Err(Error::deploy_failed(format!(
                "read {stat_path}: exit {} stderr={}",
                stat_out.exit_code,
                stat_out.stderr.trim()
            )));
        }
        let stat_fields = crate::proc_stat::parse_stat(&stat_out.stdout).ok_or_else(|| {
            Error::deploy_failed(format!("parse /proc/{pid}/stat (unexpected format)"))
        })?;

        let status_out = self.exec(node, "cat", &[&status_path])?;
        if status_out.exit_code != 0 {
            return Err(Error::deploy_failed(format!(
                "read {status_path}: exit {} stderr={}",
                status_out.exit_code,
                status_out.stderr.trim()
            )));
        }
        let status_fields = crate::proc_stat::parse_status(&status_out.stdout);

        // Count entries in /proc/<pid>/fd. Direct `ls` exec — see
        // `count_fd_dir` for why we don't go through `sh -c`.
        let fd_count = count_fd_dir(self, node, pid)?;

        // /proc/stat for btime — needed to convert starttime_ticks
        // (jiffies since boot) into a Unix timestamp.
        let proc_stat_out = self.exec(node, "cat", &["/proc/stat"])?;
        let btime = crate::proc_stat::parse_btime(&proc_stat_out.stdout).unwrap_or(0);

        // Tick rate. `getconf CLK_TCK` is portable across ns + host.
        let tck_out = self.exec(node, "getconf", &["CLK_TCK"])?;
        let tick_hz: u64 = tck_out.stdout.trim().parse().unwrap_or(100);

        Ok(crate::proc_stat::assemble(
            pid,
            &stat_fields,
            &status_fields,
            fd_count,
            btime,
            tick_hz,
        ))
    }
}

/// Count entries in `/proc/<pid>/fd` inside a target node's
/// namespace. Exec's `ls` directly — *not* via `sh -c "... | wc -l"`,
/// because:
///
/// 1. `2>/dev/null` in the shell version swallowed any error, so a
///    failed `ls` (any cause) produced an empty stdin to `wc -l`,
///    which silently emitted `0`. Round-5 follow-up bug:
///    `proc-stat`'s `fd_count` always reported 0.
/// 2. SUID-installed `nlink-lab` invocations have ruid != euid;
///    `bash`/`dash` demote euid to ruid when invoked that way (per
///    `man bash`'s "If the shell is started with the effective user
///    id not equal to the real user id, [...] the effective user id
///    is set to the real user id"). The shell-wrapped `ls
///    /proc/<pid>/fd` (mode 0700, root-owned) then hits EACCES, but
///    `2>/dev/null` hides it. Direct `ls` exec doesn't go through a
///    shell so the demotion doesn't apply.
///
/// Returns the count as `u32`. Errors propagate (non-zero exit,
/// stderr is included in the message). `None`-out shouldn't happen
/// in practice.
pub(crate) fn count_fd_dir(lab: &RunningLab, node: &str, pid: u32) -> Result<u32> {
    let path = format!("/proc/{pid}/fd");
    let out = lab.exec(node, "ls", &[&path])?;
    if out.exit_code != 0 {
        return Err(Error::deploy_failed(format!(
            "list {path}: exit {} stderr={}",
            out.exit_code,
            out.stderr.trim()
        )));
    }
    Ok(out.stdout.lines().count() as u32)
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
fn run_with_optional_timeout(
    command: &mut std::process::Command,
    timeout: Option<std::time::Duration>,
) -> Result<i32> {
    if let Some(t) = timeout {
        let mut child = command.spawn().map_err(Error::Io)?;
        wait_with_timeout(&mut child, t)?;
        let status = child.wait().map_err(Error::Io)?;
        Ok(status.code().unwrap_or(-1))
    } else {
        let status = command.status().map_err(Error::Io)?;
        Ok(status.code().unwrap_or(-1))
    }
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

/// Check whether a process is alive **and not a zombie**.
///
/// `kill(pid, 0)` alone is insufficient: a zombie (a process that has
/// exited but hasn't been waited-on by its parent) still has an entry
/// in the kernel process table and `kill(pid, 0)` returns 0. nlink-lab's
/// own spawns are detached and reaped by init
/// ([`crate::ns_exec::spawn_detached`]), but a tracked pid may still be
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
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false; // ESRCH: PID is gone entirely.
    }
    // PID exists. Check if it's a zombie.
    let stat_path = format!("/proc/{pid}/stat");
    if let Ok(content) = std::fs::read_to_string(&stat_path)
        && let Some(after_comm) = content.rsplit_once(')')
    {
        let mut fields = after_comm.1.split_whitespace();
        if let Some(state) = fields.next() {
            return state != "Z";
        }
    }
    // /proc unreadable or unparseable — fall back to "alive" since
    // kill(pid, 0) said the PID is at least present.
    true
}

#[cfg(test)]
mod pid_alive_tests {
    use super::*;

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
