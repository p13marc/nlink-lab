//! Running lab interaction.
//!
//! [`RunningLab`] provides methods to interact with a deployed lab:
//! executing commands, spawning processes, modifying impairments, and destroying.

use std::collections::HashMap;

use nlink::netlink::namespace;
use nlink::{Connection, Route};

use crate::error::{Error, Result};
use crate::state::{self, LabInfo};
use crate::types::EndpointRef;
use crate::types::Topology;

/// A deployed, running lab.
pub struct RunningLab {
    /// The topology used to deploy.
    topology: Topology,
    /// Map of node_name -> namespace_name.
    namespace_names: HashMap<String, String>,
    /// Background process PIDs: (node_name, pid).
    pids: Vec<(String, u32)>,
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

impl RunningLab {
    /// Create a new RunningLab (called by the deployer).
    pub(crate) fn new(
        topology: Topology,
        namespace_names: HashMap<String, String>,
        pids: Vec<(String, u32)>,
    ) -> Self {
        Self {
            topology,
            namespace_names,
            pids,
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

    /// Get the number of namespaces.
    pub fn namespace_count(&self) -> usize {
        self.namespace_names.len()
    }

    /// Get node names.
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.namespace_names.keys().map(|s| s.as_str())
    }

    /// Look up the namespace name for a node.
    fn namespace_for(&self, node: &str) -> Result<&str> {
        self.namespace_names
            .get(node)
            .map(|s| s.as_str())
            .ok_or_else(|| Error::NodeNotFound {
                name: node.to_string(),
            })
    }

    /// Execute a command in a lab node and collect output.
    pub fn exec(&self, node: &str, cmd: &str, args: &[&str]) -> Result<ExecOutput> {
        let ns_name = self.namespace_for(node)?;
        let ns_path = format!("/var/run/netns/{ns_name}");

        let mut command = std::process::Command::new(cmd);
        command.args(args);

        let output = spawn_output_in_namespace(&ns_path, command)?;

        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    /// Spawn a background process in a lab node.
    pub fn spawn(&mut self, node: &str, cmd: &[&str]) -> Result<u32> {
        if cmd.is_empty() {
            return Err(Error::invalid_topology("empty command"));
        }
        let ns_name = self.namespace_for(node)?;
        let ns_path = format!("/var/run/netns/{ns_name}");

        let mut command = std::process::Command::new(cmd[0]);
        command.args(&cmd[1..]);

        let child = spawn_in_namespace(&ns_path, command)?;
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
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;

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

    /// Destroy the lab: kill processes, delete namespaces, remove state.
    pub async fn destroy(self) -> Result<()> {
        // 1. Kill background processes
        for (_node, pid) in &self.pids {
            kill_process(*pid);
        }

        // 2. Delete namespaces
        for (_node_name, ns_name) in &self.namespace_names {
            if namespace::exists(ns_name) {
                if let Err(e) = namespace::delete(ns_name) {
                    tracing::warn!("failed to delete namespace '{ns_name}': {e}");
                }
            }
        }

        // 3. Delete management namespace (bridges) if it exists
        if !self.topology.networks.is_empty() {
            let mgmt_ns = format!("{}-mgmt", self.topology.lab.prefix());
            if namespace::exists(&mgmt_ns) {
                if let Err(e) = namespace::delete(&mgmt_ns) {
                    tracing::warn!("failed to delete management namespace '{mgmt_ns}': {e}");
                }
            }
        }

        // 4. Remove state file
        state::remove(&self.topology.lab.name)?;

        Ok(())
    }

    /// Load a running lab from saved state.
    pub fn load(name: &str) -> Result<Self> {
        let (lab_state, topology) = state::load(name)?;
        Ok(Self {
            topology,
            namespace_names: lab_state.namespaces,
            pids: lab_state.pids,
        })
    }

    /// List all saved labs.
    pub fn list() -> Result<Vec<LabInfo>> {
        state::list()
    }
}

/// Spawn a process in a namespace using pre_exec + setns.
fn spawn_in_namespace(
    ns_path: &str,
    mut cmd: std::process::Command,
) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;

    let ns_path = ns_path.to_string();
    unsafe {
        cmd.pre_exec(move || {
            let file = std::fs::File::open(&ns_path)?;
            let ret = libc::setns(std::os::fd::AsRawFd::as_raw_fd(&file), libc::CLONE_NEWNET);
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
        .map_err(|e| Error::deploy_failed(format!("spawn failed: {e}")))
}

/// Spawn a process in a namespace and wait for output.
fn spawn_output_in_namespace(
    ns_path: &str,
    mut cmd: std::process::Command,
) -> Result<std::process::Output> {
    use std::os::unix::process::CommandExt;

    let ns_path = ns_path.to_string();
    unsafe {
        cmd.pre_exec(move || {
            let file = std::fs::File::open(&ns_path)?;
            let ret = libc::setns(std::os::fd::AsRawFd::as_raw_fd(&file), libc::CLONE_NEWNET);
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.output()
        .map_err(|e| Error::deploy_failed(format!("spawn failed: {e}")))
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
