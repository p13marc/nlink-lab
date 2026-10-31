//! `nlink-lab destroy`.

use crate::ctx::{Ctx, LabFrom, require_root};
use crate::host_scan::{force_cleanup, reap_orphans};

#[derive(clap::Args)]
pub struct Args {
    /// The labs to destroy. Without one: `-L`, `$NLINK_LAB` or the
    /// current lab (`nlink-lab use`) — never a guess at the only lab.
    #[arg(add = crate::ctx::lab_completer(), conflicts_with_all = ["all", "lab"])]
    pub names: Vec<String>,

    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Clean up a lab whose state is gone or unreadable: its namespaces
    /// (unless another lab owns them) and its mgmt bridge.
    #[arg(long)]
    pub force: bool,

    /// Destroy every running lab.
    #[arg(long)]
    pub all: bool,

    /// Also reap mgmt bridges / veths / namespaces with no state file
    /// (left behind by a crashed deploy), and remove lock files of labs
    /// that no longer exist. A lab whose lock is held (a deploy still
    /// running) or whose state file does not parse is left alone. Alone,
    /// it destroys no lab.
    #[arg(long)]
    pub orphans: bool,
}

/// Destroy one lab, or clean up after it with `force`. A lab that does
/// not exist is already destroyed.
async fn destroy_one(ctx: &Ctx, name: &str, force: bool) -> nlink_lab::Result<&'static str> {
    let quiet = ctx.quiet || ctx.json;
    // before `--force` can sweep with it: `""` or `..` named the state
    // directory itself (#221)
    nlink_lab::internal::state::validate_lab_name(name)?;
    match nlink_lab::RunningLab::load(name) {
        Ok(lab) => {
            let summary = (
                lab.namespace_count(),
                lab.topology()
                    .nodes
                    .values()
                    .filter(|n| n.image.is_some())
                    .count(),
                lab.process_status().iter().filter(|p| p.alive).count(),
            );
            let progress = crate::progress::Progress::new(ctx);
            let destroyed = lab
                .destroy_with(&progress.op_uncancellable("destroy", name))
                .await;
            progress.finish();
            destroyed?;
            if !quiet {
                let (nodes, containers, processes) = summary;
                let mut parts = vec![format!("{nodes} node(s)")];
                if containers > 0 {
                    parts.push(format!("{containers} container(s)"));
                }
                if processes > 0 {
                    parts.push(format!("{processes} process(es) killed"));
                }
                println!("Destroyed '{name}' ({})", parts.join(", "));
            }
            Ok("destroyed")
        }
        Err(e) if force => {
            eprintln!("warning: state of '{name}' not usable, cleaning up: {e}");
            force_cleanup(name, name).await;
            if !quiet {
                println!("Force-cleaned '{name}'");
            }
            Ok("force_cleaned")
        }
        // Idempotent: destroying a lab that does not exist is a no-op
        Err(nlink_lab::Error::NotFound { .. }) => Ok("absent"),
        Err(e) => Err(e),
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        names,
        lab,
        force,
        all,
        orphans,
    } = args;
    let names: Vec<String> = if all {
        nlink_lab::RunningLab::list()?
            .into_iter()
            .map(|l| l.name)
            .collect()
    } else if !names.is_empty() {
        names
    } else if lab.lab.is_some() || !orphans {
        let (name, from) = crate::ctx::resolve_lab(lab.lab.as_deref())?;
        // the only running lab is a guess, and destroying it is final
        if from == LabFrom::Only {
            return Err(nlink_lab::Error::invalid_argument(format!(
                "name the lab to destroy: `nlink-lab destroy {name}` (or -L, NLINK_LAB, `nlink-lab use`)"
            )));
        }
        vec![name]
    } else {
        Vec::new()
    };
    require_root()?;
    if all && names.is_empty() && !orphans && !ctx.quiet {
        println!("No running labs.");
    }
    // One lab failing does not stop the others; the exit code says that
    // something was not destroyed.
    let mut results = Vec::new();
    let mut failed = Vec::new();
    for name in &names {
        match destroy_one(ctx, name, force).await {
            Ok(what) => {
                // the caller's current lab is gone: forget it, not leave
                // every next command failing on it
                let uid = crate::ctx::caller_uid();
                if nlink_lab::internal::state::context_get(uid).as_deref() == Some(name.as_str()) {
                    let _ = nlink_lab::internal::state::context_set(uid, None);
                }
                results.push(serde_json::json!({ "lab": name, "result": what }))
            }
            Err(e) => {
                eprintln!("Failed to destroy '{name}': {e}");
                failed.push(name.clone());
                results.push(
                    serde_json::json!({ "lab": name, "result": "failed", "error": e.to_string() }),
                );
            }
        }
    }
    if orphans {
        reap_orphans(&nlink_lab::RunningLab::list()?).await;
    }
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({ "schema_version": 1, "labs": results })
        );
    }
    if !failed.is_empty() {
        return Err(nlink_lab::Error::deploy_failed(format!(
            "{} lab(s) not destroyed: {}",
            failed.len(),
            failed.join(", ")
        )));
    }
    Ok(())
}
