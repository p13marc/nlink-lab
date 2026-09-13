//! `nlink-lab snapshot`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Snapshot name to create (letters, digits, `-`, `_`, `.`).
    #[arg(required_unless_present_any = ["list", "delete"], conflicts_with_all = ["list", "delete"])]
    pub name: Option<String>,

    /// Free-text description stored with the snapshot.
    #[arg(long, short = 'd')]
    pub description: Option<String>,

    /// List the lab's snapshots (newest first).
    #[arg(long, short = 'l')]
    pub list: bool,

    /// Delete a snapshot.
    #[arg(long, value_name = "NAME", conflicts_with = "list")]
    pub delete: Option<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        name,
        description,
        list,
        delete,
    } = args;
    if !nlink_lab::state::exists(&lab) {
        return Err(nlink_lab::Error::NotFound { name: lab });
    }
    if list {
        let snaps = nlink_lab::state::snapshot_list(&lab)?;
        if ctx.json {
            println!("{}", serde_json::to_string_pretty(&snaps)?);
        } else if snaps.is_empty() {
            println!("No snapshots for lab {lab:?}");
        } else {
            println!(
                "{:<24} {:<26} {:>5} {:>5} {:>6} {:>5}  DESCRIPTION",
                "NAME", "CREATED", "NODES", "LINKS", "IMPAIR", "PART"
            );
            for s in &snaps {
                println!(
                    "{:<24} {:<26} {:>5} {:>5} {:>6} {:>5}  {}",
                    s.name,
                    s.created_at,
                    s.node_count,
                    s.link_count,
                    s.live_impairments,
                    s.partitions,
                    s.description.as_deref().unwrap_or("")
                );
            }
        }
        return Ok(());
    }
    if let Some(name) = delete {
        nlink_lab::state::snapshot_remove(&lab, &name)?;
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "lab": lab, "snapshot": name, "action": "deleted" })
            );
        } else if !ctx.quiet {
            println!("Deleted snapshot {name:?} of lab {lab:?}");
        }
        return Ok(());
    }
    let name = name.expect("clap requires NAME without --list/--delete");
    // Read-modify-nothing: the snapshot is a copy of the persisted state.
    let _lock = nlink_lab::state::lock_blocking(&lab)?;
    let (state, topology) = nlink_lab::state::load(&lab)?;
    let meta =
        nlink_lab::state::snapshot_save(&lab, &name, description.as_deref(), &state, &topology)?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&meta)?);
    } else if !ctx.quiet {
        println!(
            "Snapshot {:?} of lab {lab:?} saved ({} nodes, {} links, {} runtime impairment(s), {} partition(s))",
            meta.name, meta.node_count, meta.link_count, meta.live_impairments, meta.partitions
        );
    }
    Ok(())
}
