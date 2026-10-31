//! `nlink-lab stats`: what nodes use (cgroup v2: CPU time, memory, PIDs),
//! or with `--pid`/`--procs`/`--pidfile-glob` what a node's processes
//! use (`/proc`: RSS, PSS, fds, CPU ticks).

use std::time::Duration;

use nlink_lab::internal::ops::view::NodeStats;

use crate::ctx::{Ctx, require_root};
use crate::output::{Report, Table, emit, table::terminal_width};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// One node (default: every node with a cgroup: containers and
    /// cpu/memory-limited namespace nodes).
    #[arg(add = crate::ctx::node_completer())]
    pub node: Option<String>,

    /// Sample this process of NODE, by the PID the node sees (the host's
    /// on a namespace node, the container's own on a container node;
    /// repeatable).
    #[arg(long = "pid", value_name = "PID", requires = "node", add = crate::ctx::pid_completer())]
    pub pids: Vec<u32>,

    /// Sample every process in NODE's network namespace.
    #[arg(long, requires = "node", conflicts_with = "pids")]
    pub procs: bool,

    /// Sample the processes NODE's pidfiles name: every file matching
    /// this absolute path, whose file name may hold `*` and `?`
    /// (`'/run/*.pid'`). Each record's `name` is the pidfile's stem; a
    /// stale pidfile is skipped.
    #[arg(
        long,
        value_name = "PATTERN",
        requires = "node",
        conflicts_with_all = ["pids", "procs"]
    )]
    pub pidfile_glob: Option<String>,

    /// Sample again at this interval until Ctrl-C (`--format ndjson`:
    /// one document per sample).
    #[arg(long, value_name = "DURATION", value_parser = crate::args::interval)]
    pub watch: Option<Duration>,
}

/// `stats`'s result without process flags.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct StatsReport {
    pub lab: String,
    pub nodes: Vec<NodeStats>,
}

impl Report for StatsReport {
    fn text(&self) -> String {
        if self.nodes.is_empty() {
            return format!(
                "No container nodes or cpu/memory-limited namespace nodes in lab '{}'.\n",
                self.lab
            );
        }
        let mut t = Table::new(&["NODE", "KIND", "CPU(s)", "MEM", "MEM MAX", "PIDS"])
            .right(2)
            .right(3)
            .right(4)
            .right(5);
        for n in &self.nodes {
            t.row([
                n.node.clone(),
                n.kind.to_string(),
                format!("{:.2}", n.usage.cpu_usec as f64 / 1_000_000.0),
                mib(n.usage.memory_bytes),
                n.usage.memory_max.map(mib).unwrap_or_else(|| "max".into()),
                n.usage.pids.to_string(),
            ]);
        }
        t.render(terminal_width())
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1}M", bytes as f64 / 1_048_576.0)
}

/// `stats NODE --pid/--procs/--pidfile-glob`'s result.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ProcsReport {
    pub lab: String,
    pub node: String,
    pub processes: Vec<nlink_lab::internal::proc_stat::ProcStat>,
}

impl Report for ProcsReport {
    fn text(&self) -> String {
        if self.processes.is_empty() {
            return format!("No matching process in node '{}'.\n", self.node);
        }
        let opt = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let mut t = Table::new(&[
            "PID", "STATE", "RSS kB", "PSS kB", "FDS", "USER", "SYS", "COMMAND",
        ])
        .right(0)
        .right(2)
        .right(3)
        .right(4)
        .right(5)
        .right(6);
        for p in &self.processes {
            // the node's own PID; the host's too where it differs (a
            // container node), and the pidfile's name where known
            let mut command = p.command.clone();
            if p.host_pid != p.ns_pid {
                command = format!("{command} (host pid {})", p.host_pid);
            }
            if let Some(name) = &p.name {
                command = format!("[{name}] {command}");
            }
            t.row([
                p.ns_pid.to_string(),
                p.state.clone(),
                opt(p.rss_kb),
                opt(p.pss_kb),
                p.fd_count.to_string(),
                p.cpu_user_ticks.to_string(),
                p.cpu_kernel_ticks.to_string(),
                command,
            ]);
        }
        t.render(terminal_width())
    }

    fn quiet(&self) -> Option<String> {
        Some(
            self.processes
                .iter()
                .map(|p| format!("{}\n", p.ns_pid))
                .collect(),
        )
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let lab = args.lab.resolve()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let procs = args.procs || !args.pids.is_empty() || args.pidfile_glob.is_some();
    if let Some(node) = &args.node {
        crate::cmd::common::require_node(&running, node)?;
    }
    if procs {
        // /proc/<pid>/fd is mode 0700: root reads every process's
        require_root()?;
    }
    loop {
        if procs {
            let node = args.node.clone().unwrap_or_default();
            // `--procs` and `--pidfile-glob` are re-resolved every sample:
            // a lab churns, and the point is to follow the node, not a list
            // of PIDs frozen at the first one.
            let processes = if args.procs {
                running.proc_stat_all(&node)?
            } else if let Some(pattern) = &args.pidfile_glob {
                running.proc_stat_pidfiles(&node, pattern)?
            } else {
                running.proc_stat_many(&node, &args.pids)?
            };
            emit(
                ctx,
                &ProcsReport {
                    lab: lab.clone(),
                    node,
                    processes,
                },
            )?;
        } else {
            emit(
                ctx,
                &StatsReport {
                    lab: lab.clone(),
                    nodes: nlink_lab::internal::ops::view::node_stats(
                        &running,
                        args.node.as_deref(),
                    ),
                },
            )?;
        }
        match args.watch {
            Some(d) => tokio::time::sleep(d).await,
            None => return Ok(()),
        }
    }
}
