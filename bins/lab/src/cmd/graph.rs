//! `nlink-lab graph`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};
use crate::render::{topology_to_dot, topology_to_mermaid};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Set a `param` value (repeatable): --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Emit a Mermaid `graph LR` block instead of DOT (renders inline
    /// in Forgejo/GitHub markdown).
    #[arg(long)]
    pub mermaid: bool,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        mermaid,
    } = args;
    let topo = parse_topology(&topology, &params)?;
    if mermaid {
        print!("{}", topology_to_mermaid(&topo));
    } else {
        print!("{}", topology_to_dot(&topo));
    }
    Ok(())
}
