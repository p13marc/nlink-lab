//! `nlink-lab ps`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,

    /// Hide processes whose tracked PID has exited (alive == false).
    /// Useful for "is X still running?" polling loops where exited
    /// post-mortem entries would otherwise be misread as alive.
    #[arg(long)]
    pub alive_only: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, alive_only } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let procs = if alive_only {
        running.process_status_alive_only()
    } else {
        running.process_status()
    };
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&procs)?);
    } else if procs.is_empty() {
        println!("No tracked processes.");
    } else {
        println!("{:<12} {:<8} STATUS", "NODE", "PID");
        for p in &procs {
            let status = if p.alive { "running" } else { "dead" };
            println!("{:<12} {:<8} {}", p.node, p.pid, status);
        }
    }
    Ok(())
}
