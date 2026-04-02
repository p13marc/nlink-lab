use std::path::PathBuf;

pub(crate) fn run(a: PathBuf, b: PathBuf, json: bool) -> nlink_lab::Result<()> {
    let topo_a = nlink_lab::parser::parse_file(&a)?;
    let topo_b = nlink_lab::parser::parse_file(&b)?;
    let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
    if json {
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
