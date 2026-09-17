//! `nlink-lab proc-stat`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Process ID (host PID — same as ns PID; see ARCHITECTURE.md).
    /// Omit it and pass `--all` or `--pid` instead to sample more than
    /// one process per call.
    pub pid: Option<u32>,

    /// Sample this PID (repeatable). Output is a JSON array even for a
    /// single `--pid`, unlike the positional form.
    #[arg(long = "pid", value_name = "PID", conflicts_with = "pid")]
    pub pid_flag: Vec<u32>,

    /// Sample every process in the node's network namespace, found by
    /// walking `/proc`. Reports host PIDs, which for a container node
    /// are not the PIDs seen inside it.
    #[arg(long, conflicts_with_all = ["pid", "pid_flag"])]
    pub all: bool,

    /// Sample every SECS seconds, emitting one record per
    /// sample. NDJSON (one JSON object per line, or one array per line
    /// in the multi-process modes) when combined with `--json`. Stops
    /// on Ctrl-C.
    #[arg(long, value_name = "SECS")]
    pub watch: Option<f64>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        pid,
        pid_flag,
        all,
        watch,
    } = args;
    if pid.is_none() && pid_flag.is_empty() && !all {
        return Err(nlink_lab::Error::deploy_failed(
            "name a PID, or pass --pid <PID> (repeatable) or --all",
        ));
    }
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Single sample = one shot then exit. --watch = NDJSON
    // (or text) stream until Ctrl-C.
    let interval = watch.map(std::time::Duration::from_secs_f64);
    loop {
        // `--all` is re-resolved every tick: a sampled lab churns, and
        // the point of the flag is to follow the node rather than a
        // list of PIDs frozen at the first sample.
        let pids = if all {
            running.node_pids(&node)?
        } else if let Some(p) = pid {
            vec![p]
        } else {
            pid_flag.clone()
        };
        let stats = running.proc_stat_many(&node, &pids)?;
        if ctx.json {
            // A bare positional PID keeps the single-object shape it
            // has always had; the multi-process modes are an array,
            // even when they matched one process or none.
            if pid.is_some() {
                for stat in &stats {
                    println!("{}", serde_json::to_string(stat)?);
                }
            } else {
                println!("{}", serde_json::to_string(&stats)?);
            }
        } else {
            for stat in &stats {
                println!(
                    "node={} pid={} cmd={} state={} rss={}kB pss={}kB vsz={}kB fds={} \
                     user_ticks={} kernel_ticks={}",
                    stat.node,
                    stat.host_pid,
                    stat.command,
                    stat.state,
                    opt(stat.rss_kb),
                    opt(stat.pss_kb),
                    opt(stat.vsz_kb),
                    stat.fd_count,
                    stat.cpu_user_ticks,
                    stat.cpu_kernel_ticks,
                );
            }
        }
        match interval {
            Some(d) => tokio::time::sleep(d).await,
            None => break,
        }
    }
    Ok(())
}

/// `None` renders as `?`, the way the single-process output always has.
fn opt(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}
