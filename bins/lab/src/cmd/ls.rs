//! `nlink-lab ls`: the labs on this host.

use crate::ctx::{Ctx, bold, caller_uid, red, yellow};
use crate::host_scan::{Orphans, find_orphans};
use crate::output::{Report, Table, emit, table::terminal_width};

#[derive(clap::Args)]
pub struct Args {
    /// Also scan the host for lab resources without a state file
    /// (orphans) and labs whose namespaces are gone (stale).
    #[arg(long)]
    pub scan: bool,
}

/// One running lab.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct LabRow {
    pub lab: String,
    pub nodes: usize,
    /// RFC 3339.
    pub created_at: String,
    /// The topology file it was deployed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The caller's current lab (`nlink-lab use`).
    pub current: bool,
}

/// `ls`'s result.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct LsReport {
    pub labs: Vec<LabRow>,
    /// With `--scan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphans: Option<Orphans>,
    /// Labs in the per-user state directories of older versions
    /// (`doctor --fix` moves them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub legacy: Vec<String>,
}

impl Report for LsReport {
    fn text(&self) -> String {
        let mut out = String::new();
        if self.labs.is_empty() {
            out.push_str("No running labs.\n");
        } else {
            let mut t = Table::new(&["LAB", "NODES", "CREATED", "SOURCE"]).right(1);
            for l in &self.labs {
                let name = if l.current {
                    format!("{} *", bold(&l.lab))
                } else {
                    l.lab.clone()
                };
                t.row([
                    name,
                    l.nodes.to_string(),
                    l.created_at.clone(),
                    l.source.clone().unwrap_or_default(),
                ]);
            }
            out.push_str(&t.render(terminal_width()));
        }
        if !self.legacy.is_empty() {
            out.push_str(&format!(
                "\n{} {} lab(s) in an older state directory: {} (`sudo nlink-lab doctor --fix` moves them)\n",
                yellow("note:"),
                self.legacy.len(),
                self.legacy.join(", ")
            ));
        }
        if let Some(o) = &self.orphans {
            out.push_str(&orphans_text(o));
        }
        out
    }

    fn quiet(&self) -> Option<String> {
        Some(self.labs.iter().map(|l| format!("{}\n", l.lab)).collect())
    }
}

fn orphans_text(o: &Orphans) -> String {
    let mut out = String::new();
    if !o.bridges.is_empty() || !o.veths.is_empty() || !o.netns.is_empty() {
        out.push_str(&format!("\n{} (no matching state file):\n", red("Orphans")));
        for b in &o.bridges {
            out.push_str(&format!("  bridge {b}\n"));
        }
        for v in &o.veths {
            out.push_str(&format!("  veth   {v}\n"));
        }
        for n in &o.netns {
            out.push_str(&format!("  netns  {n}\n"));
        }
        out.push_str("Run `nlink-lab destroy --orphans` to clean up.\n");
    }
    if !o.stale.is_empty() {
        out.push_str(&format!(
            "\n{} (state file with missing namespaces):\n",
            yellow("Stale labs")
        ));
        for s in &o.stale {
            out.push_str(&format!(
                "  {}  (missing: {})\n",
                s.name,
                s.missing_namespaces.join(", ")
            ));
        }
        out.push_str("Run `nlink-lab destroy LAB` for each.\n");
    }
    if o.is_empty() {
        out.push_str("\nNo orphans or stale labs.\n");
    }
    out
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let labs = nlink_lab::RunningLab::list()?;
    let current = nlink_lab::internal::state::context_get(caller_uid());
    let orphans = if args.scan {
        Some(find_orphans(&labs).await)
    } else {
        None
    };
    let rows = labs
        .iter()
        .map(|l| LabRow {
            lab: l.name.clone(),
            nodes: l.node_count,
            created_at: l.created_at.clone(),
            source: nlink_lab::internal::state::source_of(&l.name).and_then(|s| s.path),
            current: current.as_deref() == Some(l.name.as_str()),
        })
        .collect();
    let legacy = nlink_lab::internal::state::legacy_labs()
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let untagged = orphans.as_ref().map_or(0, |o| o.untagged_ignored);
    emit(
        ctx,
        &LsReport {
            labs: rows,
            orphans,
            legacy,
        },
    )?;
    if ctx.verbose && !ctx.json && untagged > 0 {
        eprintln!("{untagged} untagged namespace(s) ignored (not created by nlink-lab).");
    }
    Ok(())
}
