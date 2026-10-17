//! `nlink-lab pull`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Set a `param` value (repeatable): --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value)]
    pub params: Vec<(String, String)>,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { topology, params } = args;
    let topo = parse_topology(&topology, &params)?;
    let images: std::collections::BTreeSet<&str> = topo
        .nodes
        .values()
        .filter_map(|n| n.image.as_deref())
        .collect();
    if images.is_empty() {
        println!("No container images in topology.");
    } else {
        // the runtime the deploy will use: detection preferred podman
        // over a lab's `runtime "docker"`, and pulled into the wrong store
        // (#209)
        let rt = nlink_lab::container::Runtime::new(&topo.lab.runtime.clone().unwrap_or_default())?;
        for image in &images {
            eprint!("Pulling {image}...");
            rt.pull_image(image)?;
            eprintln!(" done");
        }
        println!("{} image(s) pulled", images.len());
    }
    Ok(())
}
