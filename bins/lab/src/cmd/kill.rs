//! `nlink-lab kill`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Process ID to kill.
    pub pid: u32,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, pid } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    running.kill_process(pid)?;
    println!("Killed process {pid}");
    Ok(())
}
