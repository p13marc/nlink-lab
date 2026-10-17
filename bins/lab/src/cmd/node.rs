//! `nlink-lab node`: add, remove or restart a node of a running lab.

use crate::ctx::{Ctx, LabArg, require_root};
use crate::host_scan::node_link_count;
use nlink_lab::edit::Edit;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Add a namespace node (link it with `link add`).
    Add {
        #[command(flatten)]
        lab: LabArg,
        /// The node's name.
        name: String,
        /// A profile of the topology for it.
        #[arg(long)]
        profile: Option<String>,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove nodes, and everything attached to them: links (both ends),
    /// impairments, network memberships, dependencies, WireGuard peers.
    Rm {
        #[command(flatten)]
        lab: LabArg,
        #[arg(required = true, add = crate::ctx::node_completer())]
        names: Vec<String>,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Restart a container node, and put back what its new network
    /// namespace lacks: its links, bridge ports and mgmt veth, sysctls and
    /// `run` commands.
    Restart {
        #[command(flatten)]
        lab: LabArg,
        #[arg(add = crate::ctx::node_completer())]
        node: String,
    },
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    match args.action {
        Action::Add {
            lab,
            name,
            profile,
            dry_run,
        } => {
            let lab = lab.resolve()?;
            crate::cmd::edit::run_edits(ctx, &lab, &[Edit::AddNode { name, profile }], dry_run)
                .await
        }
        Action::Rm {
            lab,
            names,
            dry_run,
        } => {
            let lab = lab.resolve()?;
            let edits: Vec<Edit> = names
                .into_iter()
                .map(|name| Edit::RemoveNode { name })
                .collect();
            crate::cmd::edit::run_edits(ctx, &lab, &edits, dry_run).await
        }
        Action::Restart { lab, node } => restart(ctx, &lab.resolve()?, &node).await,
    }
}

async fn restart(ctx: &Ctx, lab: &str, node: &str) -> nlink_lab::Result<()> {
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(lab)?;
    crate::cmd::common::require_node(&running, node)?;
    let container = running.container_for(node).cloned().ok_or_else(|| {
        nlink_lab::Error::invalid_argument(format!(
            "node '{node}' is not a container: only container nodes restart"
        ))
    })?;
    // `docker restart` recreates the container's network namespace, so
    // every veth the deployer moved into it is gone afterwards (issue
    // #31); `reattach_node` re-creates them, its bridge-network ports and
    // its mgmt veth included, with its sysctls and `run` commands.
    let links = node_link_count(running.topology(), node);
    let rt = running.runtime();
    if !ctx.quiet && !ctx.json {
        eprint!("Restarting '{node}'...");
    }
    let status = std::process::Command::new(rt.binary())
        .args(["restart", &container.id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| nlink_lab::Error::Runtime(format!("restart failed: {e}")))?;
    if !status.success() {
        eprintln!(" failed");
        return Err(nlink_lab::Error::Runtime(format!(
            "container restart exited with {}",
            status.code().unwrap_or(-1)
        )));
    }
    // The persisted init PID died with the old container process;
    // re-read it (a `.State.Pid` of 0 is rejected by `inspect_pid`)
    // so later `/proc/<pid>/ns/net` references stay valid.
    let pid = rt.inspect_pid(&container.id).map_err(|e| {
        eprintln!(" failed");
        nlink_lab::Error::Runtime(format!("'{node}' was restarted but is not running: {e}"))
    })?;
    let old_pid = container.pid;
    running.set_container_pid(node, pid)?;
    running.save_state()?;
    // The new namespace has none of what the old one had: its links, and
    // its sysctls and `run` commands too, even on a node with no links.
    let ops = nlink_lab::reattach_node(&mut running, node).await?.ops;
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1, "lab": lab, "node": node, "old_pid": old_pid,
                "pid": pid, "links": links, "ops": ops,
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
