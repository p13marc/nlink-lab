//! `nlink-lab logs`.

use crate::ctx::Ctx;
use crate::util::{tail_follow, tail_lines};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,
    /// Node name (for container logs).
    pub node: Option<String>,
    /// Process ID (for background process logs).
    #[arg(long)]
    pub pid: Option<u32>,
    /// Show stderr instead of stdout (with --pid).
    #[arg(long)]
    pub stderr: bool,
    /// Stream logs in tail -F style. Works for container nodes (via
    /// the runtime) and for tracked background processes (via
    /// `--pid`). Re-opens the file on rotation/truncation. Stops on
    /// Ctrl-C.
    #[arg(long)]
    pub follow: bool,
    /// Show last N lines.
    #[arg(long)]
    pub tail: Option<u32>,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        pid,
        stderr,
        follow,
        tail,
    } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;

    // Process logs mode (--pid)
    if let Some(pid) = pid {
        let (stdout_path, stderr_path) = running.log_paths(pid).ok_or_else(|| {
            nlink_lab::Error::deploy_failed(format!("no log files found for PID {pid}"))
        })?;
        let path = std::path::Path::new(if stderr { stderr_path } else { stdout_path });
        // `--tail N` reads only the tail of the file (a service log
        // can be gigabytes); without it the whole file is streamed.
        let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let initial: String = match tail {
            Some(n) => tail_lines(path, n as usize).map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
            })?,
            None => std::fs::read_to_string(path).map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
            })?,
        };
        if !initial.is_empty() {
            print!("{initial}");
            if !initial.ends_with('\n') {
                println!();
            }
        }
        if follow {
            // tail -F semantics: resume reading from current EOF,
            // poll, and reopen if the file is rotated/truncated.
            tail_follow(path, file_len)?;
        }
        return Ok(());
    }

    // Container logs mode (node name)
    let node = node.ok_or_else(|| {
        nlink_lab::Error::invalid_topology("either a node name or --pid is required")
    })?;
    let container = running.container_for(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Logs are only available for container nodes."
        ))
    })?;
    let rt = running.runtime_binary().unwrap_or("docker");
    let mut args = vec!["logs".to_string()];
    if follow {
        args.push("--follow".to_string());
    }
    if let Some(n) = tail {
        args.push("--tail".to_string());
        args.push(n.to_string());
    }
    args.push(container.id.clone());
    let status = std::process::Command::new(rt)
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("logs failed: {e}")))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
