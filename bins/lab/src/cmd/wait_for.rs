//! `nlink-lab wait-for`.

use crate::ctx::{Ctx, require_root};
use nlink_lab::wait::WaitCondition;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
    /// With --exec or --file too, all must hold, within one --timeout.
    #[arg(long)]
    pub tcp: Option<String>,

    /// Wait for command to succeed (exit 0).
    #[arg(long = "exec")]
    pub exec_cmd: Option<String>,

    /// Wait for file to exist.
    #[arg(long)]
    pub file: Option<String>,

    /// Timeout in seconds (default: 30).
    #[arg(short, long, default_value = "30")]
    pub timeout: u64,

    /// Poll interval in milliseconds (default: 500).
    #[arg(long, default_value = "500")]
    pub interval: u64,
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
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let timeout = std::time::Duration::from_secs(timeout);
    let interval = std::time::Duration::from_millis(interval);

    let mut conditions = Vec::new();
    if let Some(tcp_addr) = &tcp {
        let (ip, port) = nlink_lab::wait::parse_tcp_target(tcp_addr)?;
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
