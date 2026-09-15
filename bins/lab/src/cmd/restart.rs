//! `nlink-lab restart`.

use crate::ctx::{Ctx, require_root};
use crate::host_scan::node_link_count;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,
    /// Node name (must be a container node).
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node } = args;
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).cloned().ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Restart is only available for container nodes."
        ))
    })?;
    // `docker restart` recreates the container's network namespace, so
    // every veth the deployer moved into it is gone afterwards (issue
    // #31). Point-to-point links are re-created below; bridge-network
    // ports are not handled yet, so refuse those up front.
    if let Some(net) = running
        .topology()
        .networks
        .iter()
        .find(|(_, n)| n.members.iter().any(|m| m == &node))
        .map(|(name, _)| name.clone())
    {
        return Err(nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is a member of network '{net}'; restarting would drop its bridge \
             port and re-attaching network ports is not supported yet (issue #31)"
        )));
    }
    let links = node_link_count(running.topology(), &node);
    let rt =
        nlink_lab::container::Runtime::with_binary(running.runtime_binary().unwrap_or("docker"));
    if !ctx.quiet {
        eprint!("Restarting '{node}'...");
    }
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
    let old_pid = container.pid;
    running.set_container_pid(&node, pid)?;
    running.save_state()?;
    let mut ops = 0;
    if links > 0 {
        // The new namespace has no veths: put the node's links back.
        let report = nlink_lab::reattach_node(&mut running, &node).await?;
        ops = report.ops;
    }
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "lab": lab, "node": node, "old_pid": old_pid, "pid": pid,
                "links": links, "ops": ops,
            })
        );
    } else if !ctx.quiet {
        if links > 0 {
            eprintln!(" done (pid {old_pid} -> {pid}; {links} link(s) re-attached, {ops} op(s))");
        } else {
            eprintln!(" done (pid {old_pid} -> {pid})");
        }
    }
    Ok(())
}
