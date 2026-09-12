//! `nlink-lab diff`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};

#[derive(clap::Args)]
pub struct Args {
    /// First topology file.
    pub a: PathBuf,

    /// Second topology file.
    pub b: PathBuf,

    /// Set a `param` value (repeatable) for both files: --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { a, b, params } = args;
    let topo_a = parse_topology(&a, &params)?;
    let topo_b = parse_topology(&b, &params)?;
    let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
    if ctx.json {
        // For JSON, output a simple summary
        println!(
            "{}",
            serde_json::json!({
                "nodes_added": diff.nodes_added,
                "nodes_removed": diff.nodes_removed,
                "links_added": diff.links_added.len(),
                "links_removed": diff.links_removed.len(),
                "impairments_changed": diff.impairments_changed.len(),
                "impairments_added": diff.impairments_added.len(),
                "impairments_removed": diff.impairments_removed.len(),
                "total_changes": diff.change_count(),
            })
        );
    } else if diff.is_empty() {
        println!("No differences.");
    } else {
        println!("Diff: {} → {}", a.display(), b.display());
        print!("{diff}");
        println!("\n{} change(s)", diff.change_count());
    }
    Ok(())
}
