//! `nlink-lab restart`.

use crate::ctx::{Ctx, require_root};
use crate::host_scan::node_link_count;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,
    /// Node name (must be a container node).
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).cloned().ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Restart is only available for container nodes."
        ))
    })?;
    // `docker restart` recreates the container's network namespace, so
    // every veth the deployer moved into it is gone afterwards (issue
    // #31); `reattach_node` re-creates them, its bridge-network ports and
    // its mgmt veth included, with its sysctls and `run` commands.
    let links = node_link_count(running.topology(), &node);
    let rt = running.runtime();
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
    // The new namespace has none of what the old one had: its links, and
    // its sysctls and `run` commands too, even on a node with no links.
    let ops = nlink_lab::reattach_node(&mut running, &node).await?.ops;
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
