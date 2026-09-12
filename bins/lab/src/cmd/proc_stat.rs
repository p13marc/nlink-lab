//! `nlink-lab proc-stat`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,

    /// Node name.
    pub node: String,

    /// Process ID (host PID — same as ns PID; see ARCHITECTURE.md).
    pub pid: u32,

    /// Sample every SECS seconds, emitting one record per
    /// sample. NDJSON (one JSON object per line) when combined
    /// with `--json`. Stops on Ctrl-C.
    #[arg(long, value_name = "SECS")]
    pub watch: Option<f64>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        pid,
        watch,
    } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Single sample = one shot then exit. --watch = NDJSON
    // (or text) stream until Ctrl-C.
    let interval = watch.map(std::time::Duration::from_secs_f64);
    loop {
        let stat = running.proc_stat(&node, pid)?;
        if ctx.json {
            println!("{}", serde_json::to_string(&stat)?);
        } else {
            println!(
                "pid={} cmd={} state={} rss={}kB vsz={}kB fds={} \
                 user_ticks={} kernel_ticks={}",
                stat.host_pid,
                stat.command,
                stat.state,
                stat.rss_kb
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into()),
                stat.vsz_kb
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into()),
                stat.fd_count,
                stat.cpu_user_ticks,
                stat.cpu_kernel_ticks,
            );
        }
        match interval {
            Some(d) => tokio::time::sleep(d).await,
            None => break,
        }
    }
    Ok(())
}
