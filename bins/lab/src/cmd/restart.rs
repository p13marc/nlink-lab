//! `nlink-lab restart`.

use crate::ctx::{Ctx, require_root};
use crate::host_scan::node_link_count;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,
    /// Node name (must be a container node).
    pub node: String,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node } = args;
    require_root()?;
    // Same per-lab flock deploy/destroy take: the PID refresh
    // below rewrites state.json and must not race an `apply`.
    let _lock = nlink_lab::state::lock(&lab)?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).cloned().ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Restart is only available for container nodes."
        ))
    })?;
    // Issue #31: `docker restart` recreates the container's
    // network namespace, so every veth the deployer moved into
    // it is gone afterwards and nothing here can put it back.
    // Refuse up front instead of leaving a half-broken node.
    let links = node_link_count(running.topology(), &node);
    if links > 0 {
        return Err(nlink_lab::Error::deploy_failed(format!(
            "node '{node}' has {links} link(s); restarting would drop its veths \
             — destroy and redeploy, or use apply (issue #31)"
        )));
    }
    let rt =
        nlink_lab::container::Runtime::with_binary(running.runtime_binary().unwrap_or("docker"));
    eprint!("Restarting '{node}'...");
    let status = std::process::Command::new(rt.binary())
        .args(["restart", &container.id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("restart failed: {e}")))?;
    if !status.success() {
        eprintln!(" failed");
        return Err(nlink_lab::Error::deploy_failed(format!(
            "container restart exited with {}",
            status.code().unwrap_or(-1)
        )));
    }
    // The persisted init PID died with the old container process;
    // re-read it (a `.State.Pid` of 0 is rejected by `inspect_pid`)
    // so later `/proc/<pid>/ns/net` references stay valid.
    let pid = rt.inspect_pid(&container.id).map_err(|e| {
        eprintln!(" failed");
        nlink_lab::Error::deploy_failed(format!("'{node}' was restarted but is not running: {e}"))
    })?;
    // `RunningLab::save_state` persists only pids / impairments /
    // process logs and the container map has no public mutator,
    // so the new PID goes through the state module directly.
    let (mut lab_state, topo) = nlink_lab::state::load(&lab)?;
    let entry = lab_state.containers.get_mut(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "state.json for lab '{lab}' no longer lists container '{node}'"
        ))
    })?;
    let old_pid = entry.pid;
    entry.pid = pid;
    nlink_lab::state::save(&lab_state, &topo)?;
    eprintln!(" done (pid {old_pid} -> {pid})");
    Ok(())
}
