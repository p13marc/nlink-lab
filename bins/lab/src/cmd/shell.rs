//! `nlink-lab shell`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Shell to use (default: /bin/sh).
    #[arg(long, default_value = "/bin/sh")]
    pub shell: String,
}

pub fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, node, shell } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    crate::cmd::common::require_node(&running, &node)?;
    // `exec -it` on a container node, the namespace (with its /etc/netns
    // overlay where the host allows one) on a namespace node (#179).
    let mode = nlink_lab::ExecMode {
        stdin: true,
        tty: running.container_for(&node).is_some(),
        ..Default::default()
    };
    let mut prepared =
        running.prepare_exec(&node, &shell, &[], &nlink_lab::ExecOpts::default(), mode)?;
    prepared
        .command()
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let status = prepared
        .spawn()?
        .child
        .wait()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("shell in '{node}': {e}")))?;
    pass_through_status(status);
    Ok(())
}

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
