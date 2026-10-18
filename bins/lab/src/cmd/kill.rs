//! `nlink-lab kill`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// The tracked processes to stop (PIDs `spawn` printed).
    #[arg(required = true, add = crate::ctx::pid_completer())]
    pub pids: Vec<u32>,

    /// Send this signal instead of the TERM-then-KILL sequence: TERM, KILL,
    /// STOP, CONT, HUP, INT, USR1, USR2 (with or without the SIG prefix).
    /// STOP/CONT freeze and thaw a process in place -- a "TCP answers,
    /// application is dead" peer for resilience tests.
    #[arg(long, short = 's', value_parser = crate::args::signal)]
    pub signal: Option<nlink_lab::Signal>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, pids, signal } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let mut results = Vec::new();
    for pid in pids {
        let action = match signal {
            None => {
                if running.kill_process(pid)? {
                    "killed"
                } else {
                    "already-exited"
                }
            }
            Some(signal) => {
                running.signal_process(pid, signal)?;
                "signalled"
            }
        };
        if !ctx.json && !ctx.quiet {
            match (action, signal) {
                ("killed", _) => println!("Killed process {pid}"),
                ("already-exited", _) => println!("Process {pid} had already exited"),
                (_, Some(s)) => println!("Sent SIG{} to process {pid}", s.name()),
                _ => {}
            }
        }
        results.push(serde_json::json!({
            "pid": pid,
            "action": action,
            "signal": signal.map(|s| s.name()),
        }));
    }
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({ "schema_version": 1, "lab": lab, "processes": results })
        );
    }
    Ok(())
}
