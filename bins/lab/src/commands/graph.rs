use crate::rendering::{topology_to_ascii, topology_to_dot};
use std::path::PathBuf;

pub(crate) fn run_graph(topology: PathBuf) -> nlink_lab::Result<()> {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    print!("{}", topology_to_dot(&topo));
    Ok(())
}

pub(crate) fn run_render(
    topology: PathBuf,
    dot: bool,
    ascii: bool,
    json: bool,
) -> nlink_lab::Result<()> {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&topo)?);
    } else if dot {
        print!("{}", topology_to_dot(&topo));
    } else if ascii {
        print!("{}", topology_to_ascii(&topo));
    } else {
        print!("{}", nlink_lab::render::render(&topo));
    }
    Ok(())
}
