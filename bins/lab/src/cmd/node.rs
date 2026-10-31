//! `nlink-lab node`: add, remove or restart a node of a running lab.

use crate::ctx::{Ctx, LabArg, require_root};
use nlink_lab::internal::edit::Edit;

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
    if running.container_for(node).is_none() {
        return Err(nlink_lab::Error::invalid_argument(format!(
            "node '{node}' is not a container: only container nodes restart"
        )));
    }
    if !ctx.quiet && !ctx.json {
        eprint!("Restarting '{node}'...");
    }
    // `docker restart` recreates the container's network namespace: what
    // the deployer put in it is put back (#31)
    let restarted = nlink_lab::internal::ops::mutate::restart_node(
        &mut running,
        node,
        &nlink_lab::OpCtx::default(),
    )
    .await;
    if restarted.is_err() && !ctx.quiet && !ctx.json {
        eprintln!(" failed");
    }
    let nlink_lab::internal::ops::mutate::Restarted {
        old_pid,
        pid,
        links,
        ops,
        ..
    } = restarted?;
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
