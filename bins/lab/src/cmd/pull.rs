//! `nlink-lab pull`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Set a `param` value (repeatable): --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { topology, params } = args;
    let topo = parse_topology(&topology, &params)?;
    let progress = crate::progress::Progress::new(ctx);
    let op = progress.op("pull", &topo.lab.name);
    let pulled = nlink_lab::ops::mutate::pull_images(&topo, &op).await;
    progress.finish();
    match pulled?.len() {
        0 => println!("No container images in topology."),
        n => println!("{n} image(s) pulled"),
    }
    Ok(())
}
