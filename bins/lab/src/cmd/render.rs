//! `nlink-lab render`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};
use crate::render::{topology_to_ascii, topology_to_dot, topology_to_mermaid};

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
    /// Output as a Mermaid `graph LR` block (renders inline in
    /// Forgejo/GitHub markdown).
    #[arg(long, conflicts_with_all = ["dot", "ascii"])]
    pub mermaid: bool,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        dot,
        ascii,
        mermaid,
        params,
    } = args;
    let topo = parse_topology(&topology, &params)?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&topo)?);
    } else if dot {
        print!("{}", topology_to_dot(&topo));
    } else if ascii {
        print!("{}", topology_to_ascii(&topo));
    } else if mermaid {
        print!("{}", topology_to_mermaid(&topo));
    } else {
        print!("{}", nlink_lab::render::try_render(&topo)?);
    }
    Ok(())
}
