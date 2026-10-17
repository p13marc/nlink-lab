//! `nlink-lab spawn`.

use std::path::PathBuf;

use crate::ctx::{Ctx, require_root};
use nlink_lab::wait::WaitCondition;

/// Stream selector for `nlink-lab spawn --wait-log-stream`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum WaitLogStream {
    Stdout,
    Stderr,
    Both,
}

impl From<WaitLogStream> for nlink_lab::LogStream {
    fn from(s: WaitLogStream) -> Self {
        match s {
            WaitLogStream::Stdout => nlink_lab::LogStream::Stdout,
            WaitLogStream::Stderr => nlink_lab::LogStream::Stderr,
            WaitLogStream::Both => nlink_lab::LogStream::Both,
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Directory for stdout/stderr log files (default: lab state dir).
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// Set an environment variable (repeatable: -e KEY=VALUE).
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE", value_parser = crate::args::key_value)]
    pub env_vars: Vec<(String, String)>,

    /// Working directory for the spawned process (chdir before exec).
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<PathBuf>,

    /// Wait for TCP port after spawn (e.g., "127.0.0.1:8080" or "8080").
    ///
    /// The probe runs inside the node's namespace, so `127.0.0.1:<port>`
    /// only matches a service that bound to the loopback interface. If
    /// your service binds to a specific node IP (e.g., the interface
    /// address), pass that address here instead of `127.0.0.1`.
    #[arg(long, value_name = "[IP:]PORT", value_parser = crate::args::tcp_target)]
    pub wait_tcp: Option<(String, u16)>,

    /// Wait for a stdout/stderr line matching REGEX before returning.
    ///
    /// Useful for services that signal readiness via a log line
    /// rather than a port (e.g., `[STARTED] tunnel established`).
    /// Combinable with --wait-tcp; both must succeed before spawn
    /// returns. Fails the spawn on timeout.
    #[arg(long, value_parser = crate::args::regex, value_name = "REGEX")]
    pub wait_log: Option<regex::Regex>,

    /// Which stream to monitor for --wait-log: stdout, stderr, or
    /// both. Default: both.
    #[arg(long, value_name = "STREAM", default_value = "both")]
    pub wait_log_stream: WaitLogStream,

    /// Wait until the spawned process has a TCP listener on PORT
    /// inside its namespace. Reads `/proc/<pid>/net/tcp{,6}` —
    /// no actual `connect(2)` is attempted, so this works for
    /// services that bind to non-routable addresses or that
    /// would log connection-refused on probe attempts. Combinable
    /// with --wait-tcp / --wait-log (all AND-compose). Round-5
    /// §2.4.
    #[arg(long, value_name = "PORT")]
    pub wait_port: Option<u16>,

    /// Wait until the spawned process's open-fd count has been
    /// stable for DURATION (`2s`). Heuristic — prefer --wait-log or
    /// --wait-port when a deterministic signal is available. A
    /// process can open more files later in its lifecycle.
    /// Round-5 §2.4.
    #[arg(long, value_name = "DURATION", value_parser = crate::args::duration)]
    pub wait_fd_stable: Option<std::time::Duration>,

    /// Timeout for --wait-tcp / --wait-log / --wait-port /
    /// --wait-fd-stable together.
    #[arg(long, value_name = "DURATION", default_value = "30s", value_parser = crate::args::duration)]
    pub wait_timeout: std::time::Duration,

    /// Command and arguments.
    #[arg(trailing_var_arg = true, required = true)]
    pub cmd: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        log_dir,
        env_vars,
        workdir,
        wait_tcp,
        wait_log,
        wait_log_stream,
        wait_port,
        wait_fd_stable,
        wait_timeout,
        cmd,
    } = args;
    let lab = lab.resolve()?;
    // Every argument is checked (by clap) before the process starts: a
    // bad `--wait-log` regex used to start it, print its PID and fail,
    // and a retry started a second one (#209).
    require_root()?;
    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    crate::cmd::common::require_node(&running, &node)?;
    let args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    let opts = nlink_lab::SpawnOpts {
        log_dir: log_dir.as_deref(),
        workdir: workdir.as_deref(),
        env: &env_refs,
    };
    let pid = running.spawn_with_logs_with_opts(&node, &args, opts)?;
    running.save_state()?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "pid": pid,
                // Explicit alias for `pid`: equal today because nlink-lab
                // doesn't use CLONE_NEWPID (host_pid == ns_pid). See
                // ARCHITECTURE.md "Process & namespace model". Round-5 §2.1.
                "host_pid": pid,
                "node": node,
                "command": cmd.join(" "),
            })
        );
    } else {
        println!("PID: {pid}");
    }

    // Readiness signals AND-compose: all must hold, within one
    // --wait-timeout.
    let mut conditions = Vec::new();
    if let Some((ip, port)) = wait_tcp {
        conditions.push(WaitCondition::Tcp { ip, port });
    }
    if let Some(pattern) = wait_log {
        conditions.push(WaitCondition::Log {
            pid,
            pattern,
            stream: wait_log_stream.into(),
        });
    }
    if let Some(port) = wait_port {
        conditions.push(WaitCondition::Listening {
            port,
            pid: Some(pid),
        });
    }
    if let Some(stable_for) = wait_fd_stable {
        conditions.push(WaitCondition::FdStable { pid, stable_for });
    }
    if !conditions.is_empty() {
        let progress = crate::progress::Progress::new(ctx);
        let waited = running
            .wait_all(
                &node,
                &conditions,
                wait_timeout,
                std::time::Duration::from_millis(500),
                &progress.op("wait", &lab),
            )
            .await;
        progress.finish();
        waited?;
        if !ctx.quiet {
            eprintln!("ready");
        }
    }

    Ok(())
}
