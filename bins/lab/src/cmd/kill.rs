//! `nlink-lab kill`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Process ID to kill.
    pub pid: u32,

    /// Send this signal instead of the TERM-then-KILL sequence: TERM, KILL,
    /// STOP, CONT, HUP, INT, USR1, USR2 (with or without the SIG prefix).
    /// STOP/CONT freeze and thaw a process in place -- a "TCP answers,
    /// application is dead" peer for resilience tests.
    #[arg(long, short = 's')]
    pub signal: Option<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, pid, signal } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    match signal {
        None => {
            running.kill_process(pid)?;
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({ "lab": lab, "pid": pid, "action": "killed" })
                );
            } else {
                println!("Killed process {pid}");
            }
        }
        Some(name) => {
            let signal = nlink_lab::parse_signal(&name)?;
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
