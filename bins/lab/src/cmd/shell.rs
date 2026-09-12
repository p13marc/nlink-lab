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
        eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
        eprintln!("Available nodes: {}", node_names.join(", "));
        std::process::exit(1);
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
        std::process::exit(status.code().unwrap_or(1));
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
        std::process::exit(status.code().unwrap_or(1));
    }
}
