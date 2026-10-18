//! `nlink-lab wait-for`.

use crate::ctx::{Ctx, require_root};
use nlink_lab::wait::WaitCondition;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
    /// With --exec or --file too, all must hold, within one --timeout.
    #[arg(long, value_name = "[IP:]PORT", value_parser = crate::args::tcp_target)]
    pub tcp: Option<(String, u16)>,

    /// Wait for command to succeed (exit 0).
    #[arg(long = "exec")]
    pub exec_cmd: Option<String>,

    /// Wait for file to exist.
    #[arg(long)]
    pub file: Option<String>,

    /// How long to wait for all of them.
    #[arg(short, long, value_name = "DURATION", default_value = "30s", value_parser = crate::args::duration)]
    pub timeout: std::time::Duration,

    /// How often to probe.
    #[arg(short, long, value_name = "DURATION", default_value = "500ms", value_parser = crate::args::interval)]
    pub interval: std::time::Duration,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        tcp,
        exec_cmd,
        file,
        timeout,
        interval,
    } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;

    let mut conditions = Vec::new();
    if let Some((ip, port)) = tcp {
        conditions.push(WaitCondition::Tcp { ip, port });
    }
    if let Some(cmd) = exec_cmd {
        conditions.push(WaitCondition::Exec(cmd));
    }
    if let Some(path) = file {
        conditions.push(WaitCondition::File(path));
    }
    if conditions.is_empty() {
        return Err(nlink_lab::Error::invalid_argument(
            "one of --tcp, --exec, or --file is required".to_string(),
        ));
    }
    let result = running
        .wait_all(
            &node,
            &conditions,
            timeout,
            interval,
            &nlink_lab::OpCtx::default(),
        )
        .await;

    match result {
        Ok(()) => {
            if !ctx.quiet {
                eprintln!("ready");
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}
