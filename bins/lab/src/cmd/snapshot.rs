//! `nlink-lab snapshot`: checkpoints of a running lab — the topology,
//! the runtime impairments and the partitions — under
//! `<state>/labs/<lab>/snapshots/<name>/`.

use crate::ctx::{Ctx, LabArg};
use crate::output::{Report, Table, emit, table::terminal_width};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Save a checkpoint of the lab.
    Save {
        #[command(flatten)]
        lab: LabArg,
        /// Snapshot name (letters, digits, `-`, `_`, `.`).
        name: String,
        /// Free-text description stored with it.
        #[arg(long)]
        description: Option<String>,
    },
    /// List the lab's snapshots, newest first.
    Ls {
        #[command(flatten)]
        lab: LabArg,
    },
    /// Delete snapshots.
    Rm {
        #[command(flatten)]
        lab: LabArg,
        #[arg(required = true, add = crate::ctx::snapshot_completer())]
        names: Vec<String>,
    },
    /// Bring the lab back to a snapshot: `apply` its topology, then put
    /// back its runtime impairments and partitions.
    ///
    /// Restores what nlink-lab manages, not hand-made `ip`/`tc` edits
    /// inside the namespaces. `--dry-run` prints the plan.
    Restore(crate::cmd::restore::Args),
}

/// `snapshot ls`'s result.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct SnapshotList {
    pub lab: String,
    pub snapshots: Vec<nlink_lab::internal::state::SnapshotInfo>,
}

impl Report for SnapshotList {
    fn text(&self) -> String {
        if self.snapshots.is_empty() {
            return format!("No snapshots of lab '{}'.\n", self.lab);
        }
        let mut t = Table::new(&[
            "NAME",
            "CREATED",
            "NODES",
            "LINKS",
            "IMPAIR",
            "PART",
            "DESCRIPTION",
        ])
        .right(2)
        .right(3)
        .right(4)
        .right(5);
        for s in &self.snapshots {
            t.row([
                s.name.clone(),
                s.created_at.clone(),
                s.node_count.to_string(),
                s.link_count.to_string(),
                s.live_impairments.to_string(),
                s.partitions.to_string(),
                s.description.clone().unwrap_or_default(),
            ]);
        }
        t.render(terminal_width())
    }

    fn quiet(&self) -> Option<String> {
        Some(
            self.snapshots
                .iter()
                .map(|s| format!("{}\n", s.name))
                .collect(),
        )
    }
}

fn existing(lab: &LabArg) -> nlink_lab::Result<String> {
    let lab = lab.resolve()?;
    if !nlink_lab::internal::state::exists(&lab) {
        return Err(nlink_lab::Error::NotFound { name: lab });
    }
    Ok(lab)
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    match args.action {
        Action::Ls { lab } => {
            let lab = existing(&lab)?;
            let snapshots = nlink_lab::internal::state::snapshot_list(&lab)?;
            emit(ctx, &SnapshotList { lab, snapshots })
        }
        Action::Rm { lab, names } => {
            let lab = existing(&lab)?;
            for name in &names {
                nlink_lab::internal::state::snapshot_remove(&lab, name)?;
                if !ctx.quiet && !ctx.json {
                    println!("Deleted snapshot {name:?} of lab {lab:?}");
                }
            }
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({ "schema_version": 1, "lab": lab, "deleted": names })
                );
            }
            Ok(())
        }
        Action::Save {
            lab,
            name,
            description,
        } => {
            let lab = existing(&lab)?;
            // Read-modify-nothing: the snapshot is a copy of the persisted state.
            let _lock = nlink_lab::internal::state::lock_wait_blocking(
                &lab,
                "snapshot",
                nlink_lab::internal::state::lock_timeout(),
            )?;
            let (state, topology) = nlink_lab::internal::state::load(&lab)?;
            let meta = nlink_lab::internal::state::snapshot_save(
                &lab,
                &name,
                description.as_deref(),
                &state,
                &topology,
            )?;
            if ctx.json {
                println!("{}", serde_json::to_string_pretty(&meta)?);
            } else if !ctx.quiet {
                println!(
                    "Snapshot {:?} of lab {lab:?} saved ({} nodes, {} links, {} runtime impairment(s), {} partition(s))",
                    meta.name,
                    meta.node_count,
                    meta.link_count,
                    meta.live_impairments,
                    meta.partitions
                );
            }
            Ok(())
        }
        Action::Restore(args) => crate::cmd::restore::run(ctx, args).await,
    }
}
