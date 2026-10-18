//! `nlink-lab wait`: for a lab to be deployed, or for conditions in one
//! of its nodes (a TCP listener, a command, a file, a listening port, a
//! log line, a stable fd count), all within one `--timeout`.

use std::time::{Duration, Instant};

use nlink_lab::wait::WaitCondition;

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// The node the conditions are checked in. Without it, wait for the
    /// lab to be deployed (name it: -L, NLINK_LAB or `use`).
    #[arg(add = crate::ctx::node_completer())]
    pub node: Option<String>,

    /// A TCP listener: `PORT` (on 127.0.0.1), `IP:PORT`, `[IPv6]:PORT`.
    #[arg(long, value_name = "[IP:]PORT", value_parser = crate::args::tcp_target, requires = "node")]
    pub tcp: Option<(String, u16)>,

    /// A command that exits 0 (run with `sh -c`).
    #[arg(long = "exec", value_name = "CMD", requires = "node")]
    pub exec_cmd: Option<String>,

    /// A file that exists.
    #[arg(long, value_name = "PATH", requires = "node")]
    pub file: Option<String>,

    /// A TCP listener on PORT, of any process of the node (of `--pid`'s
    /// when it is given). Reads the socket table, connects to nothing.
    #[arg(long, value_name = "PORT", requires = "node")]
    pub listening: Option<u16>,

    /// A line of the `--pid` process's stdout or stderr that matches.
    #[arg(long, value_name = "REGEX", value_parser = crate::args::regex, requires = "pid")]
    pub log: Option<regex::Regex>,

    /// The `--pid` process's open-fd count, unchanged this long (a
    /// heuristic: prefer `--log` or `--listening`).
    #[arg(long, value_name = "DURATION", value_parser = crate::args::duration, requires = "pid")]
    pub fd_stable: Option<Duration>,

    /// The spawned process `--log`, `--fd-stable` and `--listening` look
    /// at (a PID `spawn` printed).
    #[arg(long, value_name = "PID", requires = "node")]
    pub pid: Option<u32>,

    /// How long to wait for all of them.
    #[arg(short, long, value_name = "DURATION", default_value = "30s", value_parser = crate::args::duration)]
    pub timeout: Duration,

    /// How often to look.
    #[arg(short, long, value_name = "DURATION", default_value = "500ms", value_parser = crate::args::interval)]
    pub interval: Duration,
}

/// Wait for the lab's state file: its deploy finished.
async fn lab_deployed(ctx: &Ctx, name: &str, timeout: Duration) -> nlink_lab::Result<()> {
    let start = Instant::now();
    let deadline = start + timeout;
    if !ctx.quiet {
        eprint!("Waiting for lab '{name}'...");
    }
    loop {
        if nlink_lab::state::exists(name) {
            if !ctx.quiet {
                eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            if !ctx.quiet {
                eprintln!(" timeout after {timeout:?}");
            }
            return Err(nlink_lab::Error::TimedOut(format!(
                "timeout waiting for lab '{name}' after {timeout:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Some(node) = args.node else {
        // The lab may not exist yet, so it is named: by -L, NLINK_LAB or
        // `use` (the only running lab is deployed already, which is what
        // was asked).
        let (name, _) = crate::ctx::resolve_lab(args.lab.lab.as_deref()).map_err(|e| {
            nlink_lab::Error::invalid_argument(format!(
                "name the lab to wait for: -L LAB, NLINK_LAB=LAB or `nlink-lab use LAB` ({e})"
            ))
        })?;
        return lab_deployed(ctx, &name, args.timeout).await;
    };
    let lab = args.lab.resolve()?;
    let mut conditions = Vec::new();
    if let Some((ip, port)) = args.tcp {
        conditions.push(WaitCondition::Tcp { ip, port });
    }
    if let Some(cmd) = args.exec_cmd {
        conditions.push(WaitCondition::Exec(cmd));
    }
    if let Some(path) = args.file {
        conditions.push(WaitCondition::File(path));
    }
    if let Some(port) = args.listening {
        conditions.push(WaitCondition::Listening {
            port,
            pid: args.pid,
        });
    }
    if let (Some(pattern), Some(pid)) = (args.log, args.pid) {
        conditions.push(WaitCondition::Log {
            pid,
            pattern,
            stream: nlink_lab::LogStream::Both,
        });
    }
    if let (Some(stable_for), Some(pid)) = (args.fd_stable, args.pid) {
        conditions.push(WaitCondition::FdStable { pid, stable_for });
    }
    if conditions.is_empty() {
        return Err(nlink_lab::Error::invalid_argument(
            "what to wait for in the node: --tcp, --exec, --file, --listening, --log or --fd-stable",
        ));
    }
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    crate::cmd::common::require_node(&running, &node)?;
    let progress = crate::progress::Progress::new(ctx);
    let result = running
        .wait_all(
            &node,
            &conditions,
            args.timeout,
            args.interval,
            &progress.op("wait", &lab),
        )
        .await;
    progress.finish();
    result?;
    if !ctx.quiet {
        eprintln!("ready");
    }
    Ok(())
}
