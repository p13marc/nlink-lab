use crate::color::{green, red, yellow};
use crate::daemon::run_daemon_inline;
use crate::output::{print_deploy_summary, print_topology_summary};
use crate::util::check_root;
use std::path::PathBuf;
use std::time::Instant;

pub(crate) async fn run(
    topology: PathBuf,
    dry_run: bool,
    force: bool,
    daemon: bool,
    skip_validate: bool,
    quiet: bool,
) -> nlink_lab::Result<()> {
    let mut topo = nlink_lab::parser::parse_file(&topology)?;
    if skip_validate {
        topo.assertions.clear();
    }
    let result = topo.validate();

    // Print warnings
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }

    if result.has_errors() {
        eprintln!("Validation failed for {:?}:", topo.lab.name);
        for e in result.errors() {
            eprintln!("  {} {e}", red("ERROR"));
        }
        return Err(nlink_lab::Error::Validation("see errors above".into()));
    }

    if dry_run {
        println!("Topology {:?} is valid", topo.lab.name);
        print_topology_summary(&topo);
        return Ok(());
    }

    // Handle --force: destroy existing lab first
    if force && nlink_lab::state::exists(&topo.lab.name) {
        let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
        lab.destroy().await?;
    }

    check_root();

    let start = Instant::now();
    let lab = topo.deploy().await?;
    let elapsed = start.elapsed();

    println!(
        "{} Lab {:?} deployed in {:.0?}",
        green("OK"),
        topo.lab.name,
        elapsed
    );
    print_deploy_summary(&topo);

    if !quiet {
        let first_node = topo
            .nodes
            .keys()
            .next()
            .map(|s| s.as_str())
            .unwrap_or("node");
        println!();
        println!("Next steps:");
        println!(
            "  nlink-lab status {}          # inspect lab",
            topo.lab.name
        );
        println!(
            "  nlink-lab exec {} {} -- ip addr",
            topo.lab.name, first_node
        );
        println!(
            "  nlink-lab shell {} {}        # interactive shell",
            topo.lab.name, first_node
        );
        println!("  nlink-lab destroy {}         # tear down", topo.lab.name);
    }

    if daemon {
        run_daemon_inline(&lab).await?;
    }
    Ok(())
}
