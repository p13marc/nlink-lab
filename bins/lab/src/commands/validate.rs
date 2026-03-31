use crate::color::{red, yellow};
use crate::output::print_topology_summary;
use std::path::PathBuf;

pub(crate) fn run(topology: PathBuf) -> nlink_lab::Result<()> {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    let result = topo.validate();

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

    println!("Topology {:?} is valid", topo.lab.name);
    print_topology_summary(&topo);
    Ok(())
}
