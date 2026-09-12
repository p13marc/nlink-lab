//! `nlink-lab render`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};
use crate::render::{topology_to_ascii, topology_to_dot};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,
    /// Output as DOT graph (for Graphviz).
    #[arg(long)]
    pub dot: bool,
    /// Output as ASCII diagram.
    #[arg(long)]
    pub ascii: bool,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        dot,
        ascii,
        params,
    } = args;
    let topo = parse_topology(&topology, &params)?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&topo)?);
    } else if dot {
        print!("{}", topology_to_dot(&topo));
    } else if ascii {
        print!("{}", topology_to_ascii(&topo));
    } else {
        print!("{}", nlink_lab::render::try_render(&topo)?);
    }
    Ok(())
}
