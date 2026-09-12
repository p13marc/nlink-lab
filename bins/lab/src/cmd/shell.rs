//! `nlink-lab shell`.

use crate::ctx::{Ctx, require_root};
use crate::util::nsenter_shell_args;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,

    /// Node name.
    pub node: String,

    /// Shell to use (default: /bin/sh).
    #[arg(long, default_value = "/bin/sh")]
    pub shell: String,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node, shell } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Validate node exists
    let node_names: Vec<&str> = running.node_names().collect();
    if !node_names.contains(&node.as_str()) {
        eprintln!("Available nodes: {}", node_names.join(", "));
        return Err(nlink_lab::Error::NodeNotFound { name: node });
    }
    if let Some(container) = running.container_for(&node) {
        let rt = running.runtime_binary().unwrap_or("docker");
        let status = std::process::Command::new(rt)
            .args(["exec", "-it", &container.id, &shell])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("exec failed: {e}")))?;
        pass_through_status(status);
        Ok(())
    } else {
        let ns = running.namespace_for(&node)?;
        let args = nsenter_shell_args(ns, &shell);
        let status = std::process::Command::new("nsenter")
            .args(&args)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("nsenter failed: {e}")))?;
        pass_through_status(status);
        Ok(())
    }
}

/// The shell's exit status becomes ours (signal death → 128+signo).
fn pass_through_status(status: std::process::ExitStatus) {
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1);
    if code != 0 {
        crate::output::set_exit_code(u8::try_from(code.clamp(0, 255)).unwrap_or(1));
    }
}
