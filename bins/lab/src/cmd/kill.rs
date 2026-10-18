//! `nlink-lab kill`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Process ID to kill.
    pub pid: u32,

    /// Send this signal instead of the TERM-then-KILL sequence: TERM, KILL,
    /// STOP, CONT, HUP, INT, USR1, USR2 (with or without the SIG prefix).
    /// STOP/CONT freeze and thaw a process in place -- a "TCP answers,
    /// application is dead" peer for resilience tests.
    #[arg(long, short = 's', value_parser = crate::args::signal)]
    pub signal: Option<nlink_lab::Signal>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, pid, signal } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    match signal {
        None => {
            let killed = running.kill_process(pid)?;
            let action = if killed { "killed" } else { "already-exited" };
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({ "lab": lab, "pid": pid, "action": action })
                );
            } else if killed {
                println!("Killed process {pid}");
            } else {
                println!("Process {pid} had already exited");
            }
        }
        Some(signal) => {
            running.signal_process(pid, signal)?;
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({ "lab": lab, "pid": pid, "action": "signalled", "signal": signal.name() })
                );
            } else {
                println!("Sent SIG{} to process {pid}", signal.name());
            }
        }
    }
    Ok(())
}
