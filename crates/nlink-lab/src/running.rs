//! Running lab interaction.
//!
//! [`RunningLab`] provides methods to interact with a deployed lab:
//! executing commands, spawning processes, modifying impairments, and destroying.

use std::collections::HashMap;

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
    namespace_names: HashMap<String, String>,
    /// Map of node_name -> container state (container nodes only).
    containers: HashMap<String, ContainerState>,
    /// Container runtime binary ("docker" or "podman"), if any containers.
    runtime_binary: Option<String>,
    /// Background process PIDs: (node_name, pid).
    pids: Vec<(String, u32)>,
    /// Whether DNS hosts entries were injected into /etc/hosts.
    dns_injected: bool,
    /// Whether mac80211_hwsim was loaded.
    wifi_loaded: bool,
    /// Saved impairments before partition (endpoint → Impairment).
    saved_impairments: HashMap<String, crate::types::Impairment>,
    /// Log file paths for spawned processes: pid → (stdout_path, stderr_path).
    process_logs: HashMap<u32, (String, String)>,
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
        namespace_names: HashMap<String, String>,
        containers: HashMap<String, ContainerState>,
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
            saved_impairments: HashMap::new(),
            process_logs: HashMap::new(),
        }
    }

    /// Get the topology used to deploy this lab.
    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Get the lab name.
    pub fn name(&self) -> &str {
        &self.topology.lab.name
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
    pub(crate) fn namespace_names(&self) -> &HashMap<String, String> {
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

    /// Mutable access to namespace names map (crate-internal, used by apply_diff).
    pub(crate) fn namespace_names_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.namespace_names
    }

    /// Get the container state for a container node, if it is one.
    pub fn container_for(&self, node: &str) -> Option<&ContainerState> {
        self.containers.get(node)
    }

    /// Access container states map.
    pub fn containers(&self) -> &HashMap<String, ContainerState> {
        &self.containers
    }

    /// Mutable access to container states map (crate-internal, used by apply_diff).
    pub(crate) fn containers_mut(&mut self) -> &mut HashMap<String, ContainerState> {
        &mut self.containers
    }

    /// Access background PIDs (crate-internal).
    pub(crate) fn pids(&self) -> &[(String, u32)] {
        &self.pids
    }

    /// Runtime binary (docker or podman).
    pub fn runtime_binary(&self) -> Option<&str> {
        self.runtime_binary.as_deref()
    }

    /// Set the runtime binary (crate-internal, used by apply_diff).
    pub(crate) fn set_runtime_binary(&mut self, binary: String) {
        self.runtime_binary = Some(binary);
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
            let output = std::process::Command::new(rt_binary)
                .args(&all_args)
                .output()
                .map_err(|e| {
                    Error::deploy_failed(format!("exec in container '{node}' failed: {e}"))
                })?;
            Ok(ExecOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            })
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
            let output = namespace::spawn_output_with_etc(ns_name, command)
                .map_err(|e| Error::deploy_failed(format!("exec in '{node}' failed: {e}")))?;
            Ok(ExecOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
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
            let status = std::process::Command::new(rt_binary)
                .args(&all_args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .map_err(|e| {
                    Error::deploy_failed(format!("attached exec in container '{node}' failed: {e}"))
                })?;
            Ok(status.code().unwrap_or(-1))
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
            let status = command.status().map_err(|e| {
                Error::deploy_failed(format!("attached exec in '{node}' failed: {e}"))
            })?;
            Ok(status.code().unwrap_or(-1))
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

        let child = namespace::spawn_with_etc(ns_name, command)
            .map_err(|e| Error::deploy_failed(format!("spawn in '{node}' failed: {e}")))?;
        let pid = child.id();
        self.pids.push((node.to_string(), pid));
        Ok(pid)
    }

    /// Re-save the current state to disk (e.g., after spawning a new process).
    pub fn save_state(&self) -> Result<()> {
        let (mut lab_state, _) = state::load(self.name())?;
        lab_state.pids = self.pids.clone();
        lab_state.saved_impairments = self.saved_impairments.clone();
        lab_state.process_logs = self.process_logs.clone();
        state::save(&lab_state, &self.topology)
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

        let child = nlink::netlink::namespace::spawn_with_etc(&ns_name, command)
            .map_err(|e| Error::deploy_failed(format!("spawn in '{node}' failed: {e}")))?;
        let pid = child.id();
        self.pids.push((node.to_string(), pid));
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
        let ns_name = self.namespace_for(&ep.node)?;
        let conn: Connection<Route> = namespace::connection_for(ns_name)
            .map_err(|e| Error::deploy_failed(format!("connection for '{ns_name}': {e}")))?;

        let netem = crate::deploy::build_netem(impairment)?;

        // Try change first (update existing qdisc), fall back to add
        match conn
            .change_qdisc(&ep.iface, nlink::TcHandle::ROOT, netem.clone())
            .await
        {
            Ok(()) => Ok(()),
            Err(_) => conn
                .add_qdisc(&ep.iface, netem)
                .await
                .map_err(|e| Error::deploy_failed(format!("set impairment on '{endpoint}': {e}"))),
        }
    }

    /// Remove all impairments from an interface.
    pub async fn clear_impairment(&self, endpoint: &str) -> Result<()> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        let ns_name = self.namespace_for(&ep.node)?;
        let conn: Connection<Route> = namespace::connection_for(ns_name)
            .map_err(|e| Error::deploy_failed(format!("connection for '{ns_name}': {e}")))?;

        // Delete the root qdisc (removes all netem config)
        conn.del_qdisc(&ep.iface, nlink::TcHandle::ROOT)
            .await
            .map_err(|e| Error::deploy_failed(format!("clear impairment on '{endpoint}': {e}")))?;
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
                "PID {pid} not tracked by this lab"
            )));
        }
        kill_process(pid);
        Ok(())
    }

    /// Destroy the lab: kill processes, remove containers, delete namespaces, remove state.
    pub async fn destroy(self) -> Result<()> {
        // Acquire exclusive lock
        let _lock = crate::state::lock(&self.topology.lab.name)?;

        // 1. Kill background processes
        for (_node, pid) in &self.pids {
            kill_process(*pid);
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
            let mut sorted_nodes: Vec<&str> =
                self.namespace_names.keys().map(|s| s.as_str()).collect();
            sorted_nodes.sort();
            for (idx, _) in sorted_nodes.iter().enumerate() {
                let peer = self.topology.lab.mgmt_peer_name(idx);
                let _ = root_conn.del_link(&peer).await;
            }
            let bridge_name = self.topology.lab.mgmt_bridge_name();
            let _ = root_conn.del_link(&bridge_name).await;
        }

        // 4. Delete namespaces
        for ns_name in self.namespace_names.values() {
            if namespace::exists(ns_name)
                && let Err(e) = namespace::delete(ns_name)
            {
                tracing::warn!("failed to delete namespace '{ns_name}': {e}");
            }
        }

        // 4b. Delete management namespace (bridges) if it exists
        if !self.topology.networks.is_empty() {
            let mgmt_ns = format!("{}-mgmt", self.topology.lab.prefix());
            if namespace::exists(&mgmt_ns)
                && let Err(e) = namespace::delete(&mgmt_ns)
            {
                tracing::warn!("failed to delete management namespace '{mgmt_ns}': {e}");
            }
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
            crate::wifi::unload_hwsim();
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
        })
    }

    /// List all saved labs.
    pub fn list() -> Result<Vec<LabInfo>> {
        state::list()
    }

    /// Check status of tracked background processes.
    ///
    /// `alive` is `true` only if the PID still exists **and** is not a
    /// zombie. This matters because `spawn_with_logs` returns a
    /// `std::process::Child` that the caller drops without
    /// `wait()`-ing, so an exited child becomes a zombie that
    /// `kill(pid, 0)` will continue to report as deliverable
    /// (returning 0). Without the zombie check, "is this process
    /// still running?" polling would never see a quick-exiting child
    /// transition to dead.
    pub fn process_status(&self) -> Vec<ProcessInfo> {
        self.pids
            .iter()
            .map(|(node, pid)| {
                let alive = pid_is_alive(*pid);
                let logs = self.process_logs.get(pid);
                ProcessInfo {
                    node: node.clone(),
                    pid: *pid,
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
}

/// Best-effort kill of a process.
fn kill_process(pid: u32) {
    unsafe {
        // Try SIGTERM first
        libc::kill(pid as i32, libc::SIGTERM);
    }
    // Give it a moment, then SIGKILL
    std::thread::sleep(std::time::Duration::from_millis(100));
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Check whether a process is alive **and not a zombie**.
///
/// `kill(pid, 0)` alone is insufficient: a zombie (a process that has
/// exited but hasn't been waited-on by its parent) still has an entry
/// in the kernel process table and `kill(pid, 0)` returns 0. Since
/// [`spawn_with_logs`](RunningLab::spawn_with_logs) drops its
/// `std::process::Child` without `wait()`-ing, every quick-exiting
/// child stays a zombie indefinitely from this process's POV.
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
    if let Ok(content) = std::fs::read_to_string(&stat_path) {
        if let Some(after_comm) = content.rsplit_once(')') {
            let mut fields = after_comm.1.trim().split_whitespace();
            if let Some(state) = fields.next() {
                return state != "Z";
            }
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
            .and_then(|(_, after)| after.trim().split_whitespace().next())
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
        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        // Reap it ourselves.
        let _ = child.wait();
        // Give the scheduler a moment to actually free the slot.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            !pid_is_alive(pid),
            "reaped pid {pid} must read as dead"
        );
    }
}
