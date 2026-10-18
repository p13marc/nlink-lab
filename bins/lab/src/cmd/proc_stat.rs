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

    /// Process ID as the node sees it: the host PID on a namespace node,
    /// the container's own PID on a container node. Omit it and pass
    /// `--all`, `--pid` or `--pidfile-glob` instead to sample more than
    /// one process per call. Every record carries both `host_pid` and
    /// `ns_pid`.
    pub pid: Option<u32>,

    /// Sample this PID, as the node sees it (repeatable). Output is a
    /// JSON array even for a single `--pid`, unlike the positional form.
    #[arg(long = "pid", value_name = "PID", conflicts_with = "pid")]
    pub pid_flag: Vec<u32>,

    /// Sample every process in the node's network namespace, found by
    /// walking the host's `/proc`. A process that exits mid-sample is
    /// left out.
    #[arg(long, conflicts_with_all = ["pid", "pid_flag"])]
    pub all: bool,

    /// Sample the processes the node's pidfiles name: every file matching
    /// this absolute path, whose file name may hold `*` and `?`
    /// (`'/run/*.pid'`). Read from the host, through the container's root
    /// on a container node. Each record's `name` is the pidfile's stem;
    /// a stale pidfile is skipped.
    #[arg(
        long,
        value_name = "PATTERN",
        conflicts_with_all = ["pid", "pid_flag", "all"]
    )]
    pub pidfile_glob: Option<String>,

    /// Sample at this interval (`2s`), emitting one record per
    /// sample. NDJSON (one JSON object per line, or one array per line
    /// in the multi-process modes) when combined with `--json`. Stops
    /// on Ctrl-C.
    #[arg(long, value_name = "DURATION", value_parser = crate::args::interval)]
    pub watch: Option<std::time::Duration>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        pid,
        pid_flag,
        all,
        pidfile_glob,
        watch,
    } = args;
    if pid.is_none() && pid_flag.is_empty() && !all && pidfile_glob.is_none() {
        return Err(nlink_lab::Error::deploy_failed(
            "name a PID, or pass --pid <PID> (repeatable), --all or --pidfile-glob <PATTERN>",
        ));
    }
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Single sample = one shot then exit. --watch = NDJSON
    // (or text) stream until Ctrl-C.
    let interval = watch;
    loop {
        // `--all` and `--pidfile-glob` are re-resolved every tick: a
        // sampled lab churns, and the point of the flags is to follow the
        // node rather than a list of PIDs frozen at the first sample.
        let stats = if all {
            running.proc_stat_all(&node)?
        } else if let Some(pattern) = &pidfile_glob {
            running.proc_stat_pidfiles(&node, pattern)?
        } else if let Some(p) = pid {
            running.proc_stat_many(&node, &[p])?
        } else {
            running.proc_stat_many(&node, &pid_flag)?
        };
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
                // The node's own PID first; the host's too where it differs
                // (a container node), and the pidfile's name where known.
                let host = if stat.host_pid != stat.ns_pid {
                    format!(" host_pid={}", stat.host_pid)
                } else {
                    String::new()
                };
                let name = stat
                    .name
                    .as_deref()
                    .map(|n| format!(" name={n}"))
                    .unwrap_or_default();
                println!(
                    "node={} pid={}{host}{name} cmd={} state={} rss={}kB pss={}kB vsz={}kB fds={} \
                     user_ticks={} kernel_ticks={}",
                    stat.node,
                    stat.ns_pid,
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
