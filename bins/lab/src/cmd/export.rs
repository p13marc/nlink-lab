//! `nlink-lab export`: a running lab's topology, as it is now (after
//! `apply` and `edit`), as NLL, TOML or JSON.

use std::path::PathBuf;

use crate::ctx::{Ctx, LabArg};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: LabArg,

    /// Write it here (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// As NLL (the default is TOML; `--format json` gives JSON).
    #[arg(long)]
    pub nll: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let lab = args.lab.resolve()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let topo = running.topology();
    let content = if args.nll {
        nlink_lab::render::try_render(topo)?
    } else if ctx.json {
        format!("{}\n", serde_json::to_string_pretty(topo)?)
    } else {
        toml::to_string_pretty(topo)
            .map_err(|e| nlink_lab::Error::invalid_topology(format!("serialize: {e}")))?
    };
    match args.output {
        Some(path) => {
            std::fs::write(&path, &content)?;
            if !ctx.quiet {
                eprintln!("Exported '{lab}' to {}", path.display());
            }
        }
        None => print!("{content}"),
    }
    Ok(())
}
