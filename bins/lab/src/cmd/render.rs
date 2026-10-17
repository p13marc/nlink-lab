//! `nlink-lab render`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};
use crate::render::{topology_to_ascii, topology_to_dot, topology_to_mermaid};

/// `--diagram`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagram {
    /// A Graphviz DOT graph (`| dot -Tsvg`).
    Dot,
    /// A Mermaid `graph LR` block (renders inline in Forgejo/GitHub
    /// markdown).
    Mermaid,
    /// A text drawing.
    Ascii,
}

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// A diagram of it instead of the flat NLL.
    #[arg(long, value_enum)]
    pub diagram: Option<Diagram>,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        diagram,
        params,
    } = args;
    let topo = parse_topology(&topology, &params)?;
    match diagram {
        Some(Diagram::Dot) => print!("{}", topology_to_dot(&topo)),
        Some(Diagram::Mermaid) => print!("{}", topology_to_mermaid(&topo)),
        Some(Diagram::Ascii) => print!("{}", topology_to_ascii(&topo)),
        None if ctx.json => println!("{}", serde_json::to_string_pretty(&topo)?),
        None => print!("{}", nlink_lab::render::try_render(&topo)?),
    }
    Ok(())
}
