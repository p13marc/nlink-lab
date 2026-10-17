//! `nlink-lab destroy`.

use crate::ctx::{Ctx, require_root};
use crate::host_scan::{force_cleanup, reap_orphans};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (omit with --all or --orphans).
    pub name: Option<String>,

    /// Continue cleanup even if some resources are already gone.
    #[arg(long)]
    pub force: bool,

    /// Destroy all running labs.
    #[arg(long)]
    pub all: bool,

    /// Also reap mgmt bridges / veths / namespaces with no state file
    /// (left behind by a crashed deploy), and remove lock files of labs
    /// that no longer exist. A lab whose lock is held (a deploy still
    /// running) or whose state file does not parse is left alone.
    /// Implies best-effort cleanup; can be combined with a lab name or
    /// --all, or used on its own.
    #[arg(long)]
    pub orphans: bool,
}

pub async fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        name,
        force,
        all,
        orphans,
    } = args;
    require_root()?;
    if all {
        let labs = nlink_lab::RunningLab::list()?;
        if labs.is_empty() && !orphans {
            println!("No running labs.");
            return Ok(());
        }
        // One lab failing does not stop the others; the count says what
        // was destroyed, and the exit code that something was not.
        let mut destroyed = 0;
        let mut failed = Vec::new();
        for info in &labs {
            let result = match nlink_lab::RunningLab::load(&info.name) {
                Ok(lab) => lab.destroy().await.map(|()| "Destroyed"),
                Err(_) if force => {
                    force_cleanup(&info.name, &info.name).await;
                    Ok("Force-cleaned")
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(what) => {
                    println!("{what} '{}'", info.name);
                    destroyed += 1;
                }
                Err(e) => {
                    eprintln!("Failed to destroy '{}': {e}", info.name);
                    failed.push(info.name.clone());
                }
            }
        }
        if !labs.is_empty() {
            println!("{destroyed} lab(s) destroyed");
        }
        if orphans {
            reap_orphans(&nlink_lab::RunningLab::list()?).await;
        }
        if !failed.is_empty() {
            return Err(nlink_lab::Error::deploy_failed(format!(
                "{} lab(s) not destroyed: {}",
                failed.len(),
                failed.join(", ")
            )));
        }
        return Ok(());
    }
    if orphans && name.is_none() {
        // `destroy --orphans` alone: reap without touching state-backed labs.
        let labs = nlink_lab::RunningLab::list()?;
        reap_orphans(&labs).await;
        return Ok(());
    }
    let name = name.ok_or_else(|| {
        nlink_lab::Error::deploy_failed("lab name required (or use --all/--orphans)")
    })?;
    // before `--force` can sweep with it: `""` or `..` named the state
    // directory itself (#221)
    nlink_lab::state::validate_lab_name(&name)?;
    match nlink_lab::RunningLab::load(&name) {
        Ok(lab) => {
            let node_count = lab.namespace_count();
            let topo = lab.topology();
            let container_count = topo.nodes.values().filter(|n| n.image.is_some()).count();
            let link_count = topo.links.len();
            let process_count = lab.process_status().iter().filter(|p| p.alive).count();
            lab.destroy().await?;
            println!("Lab {name:?} destroyed:");
            println!("  Nodes:       {node_count}");
            if container_count > 0 {
                println!("  Containers:  {container_count} stopped and removed");
            }
            println!("  Links:       {link_count}");
            if process_count > 0 {
                println!("  Processes:   {process_count} killed");
            }
        }
        Err(e) if force => {
            eprintln!("warning: state not found, attempting force cleanup: {e}");
            force_cleanup(&name, &name).await;
            println!("Lab {name:?} force-cleaned");
        }
        Err(nlink_lab::Error::NotFound { .. }) => {
            // Idempotent: destroying a non-existent lab is a no-op
        }
        Err(e) => return Err(e),
    }
    // `destroy NAME --orphans`: the named lab, then the reaping the flag
    // asks for. It used to be ignored with a name.
    if orphans {
        reap_orphans(&nlink_lab::RunningLab::list()?).await;
    }
    Ok(())
}
