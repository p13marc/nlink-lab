use crate::color::{red, yellow};
use crate::util::check_root;
use std::path::PathBuf;
use std::time::Instant;

pub(crate) async fn run(topology: PathBuf, dry_run: bool) -> nlink_lab::Result<()> {
    let desired = nlink_lab::parser::parse_file(&topology)?;
    let result = desired.validate();
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }
    if result.has_errors() {
        for e in result.errors() {
            eprintln!("  {} {e}", red("ERROR"));
        }
        return Err(nlink_lab::Error::Validation("see errors above".into()));
    }

    // Load current topology from running lab state
    let lab_name = &desired.lab.name;
    if !nlink_lab::state::exists(lab_name) {
        return Err(nlink_lab::Error::NotFound {
            name: format!("{lab_name} (deploy first, then apply changes)"),
        });
    }
    let mut running = nlink_lab::RunningLab::load(lab_name)?;
    let current = running.topology();

    let diff = nlink_lab::diff_topologies(current, &desired);

    if diff.is_empty() {
        println!("No changes to apply.");
        return Ok(());
    }

    println!("Changes for lab '{lab_name}':");
    print!("{diff}");
    println!("{} change(s)", diff.change_count());

    if dry_run {
        println!("\n(dry run — no changes applied)");
        return Ok(());
    }

    check_root();
    let start = Instant::now();
    nlink_lab::apply_diff(&mut running, &desired, &diff).await?;
    let elapsed = start.elapsed();

    println!(
        "\nApplied {} change(s) in {:.0?}",
        diff.change_count(),
        elapsed
    );
    Ok(())
}
