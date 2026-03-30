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

/// Status of a tracked background process.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessInfo {
    /// Node the process runs in.
    pub node: String,
    /// Process ID.
    pub pid: u32,
    /// Whether the process is still alive.
    pub alive: bool,
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
    ) -> Self {
        Self {
            topology,
            namespace_names,
            containers,
            runtime_binary,
            pids,
            dns_injected,
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
        if let Some(container) = self.containers.get(node) {
            // Use docker/podman exec for container nodes
            let rt_binary = self
                .runtime_binary
                .as_deref()
                .ok_or_else(|| Error::deploy_failed("no container runtime binary in state"))?;
            let mut all_args = vec!["exec", &container.id, cmd];
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
            let output = namespace::spawn_output(ns_name, command)
                .map_err(|e| Error::deploy_failed(format!("exec in '{node}' failed: {e}")))?;
            Ok(ExecOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
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

        let child = namespace::spawn(ns_name, command)
            .map_err(|e| Error::deploy_failed(format!("spawn in '{node}' failed: {e}")))?;
        let pid = child.id();
        self.pids.push((node.to_string(), pid));
        Ok(pid)
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
        match conn.change_qdisc(&ep.iface, "root", netem.clone()).await {
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
        conn.del_qdisc(&ep.iface, "root")
            .await
            .map_err(|e| Error::deploy_failed(format!("clear impairment on '{endpoint}': {e}")))?;
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

        // 3. Delete namespaces
        for ns_name in self.namespace_names.values() {
            if namespace::exists(ns_name)
                && let Err(e) = namespace::delete(ns_name)
            {
                tracing::warn!("failed to delete namespace '{ns_name}': {e}");
            }
        }

        // 4. Delete management namespace (bridges) if it exists
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
        })
    }

    /// List all saved labs.
    pub fn list() -> Result<Vec<LabInfo>> {
        state::list()
    }

    /// Check status of tracked background processes.
    pub fn process_status(&self) -> Vec<ProcessInfo> {
        self.pids
            .iter()
            .map(|(node, pid)| {
                let alive = unsafe { libc::kill(*pid as i32, 0) } == 0;
                ProcessInfo {
                    node: node.clone(),
                    pid: *pid,
                    alive,
                }
            })
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
