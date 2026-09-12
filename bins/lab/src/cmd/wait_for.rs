//! `nlink-lab wait-for`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,

    /// Node name.
    pub node: String,

    /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
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

    let result = if let Some(ref tcp_addr) = tcp {
        let (ip, port) = if let Some((ip, port_str)) = tcp_addr.rsplit_once(':') {
            (
                ip.to_string(),
                port_str.parse::<u16>().map_err(|e| {
                    nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                })?,
            )
        } else {
            (
                "127.0.0.1".to_string(),
                tcp_addr.parse::<u16>().map_err(|e| {
                    nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                })?,
            )
        };
        running
            .wait_for_tcp(&node, &ip, port, timeout, interval)
            .await
    } else if let Some(ref cmd) = exec_cmd {
        running.wait_for_exec(&node, cmd, timeout, interval).await
    } else if let Some(ref path) = file {
        running.wait_for_file(&node, path, timeout, interval).await
    } else {
        return Err(nlink_lab::Error::invalid_topology(
            "one of --tcp, --exec, or --file is required".to_string(),
        ));
    };

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
