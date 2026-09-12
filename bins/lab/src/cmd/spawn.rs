//! `nlink-lab spawn`.

use std::path::PathBuf;

use crate::ctx::{Ctx, require_root};
use crate::util::parse_env_pairs;

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
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Directory for stdout/stderr log files (default: lab state dir).
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// Set environment variables (can be repeated: --env KEY=VALUE).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env_vars: Vec<String>,

    /// Working directory for the spawned process (chdir before exec).
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<PathBuf>,

    /// Wait for TCP port after spawn (e.g., "127.0.0.1:8080" or "8080").
    ///
    /// The probe runs inside the node's namespace, so `127.0.0.1:<port>`
    /// only matches a service that bound to the loopback interface. If
    /// your service binds to a specific node IP (e.g., the interface
    /// address), pass that address here instead of `127.0.0.1`.
    #[arg(long)]
    pub wait_tcp: Option<String>,

    /// Wait for a stdout/stderr line matching REGEX before returning.
    ///
    /// Useful for services that signal readiness via a log line
    /// rather than a port (e.g., `[STARTED] tunnel established`).
    /// Combinable with --wait-tcp; both must succeed before spawn
    /// returns. Fails the spawn on timeout.
    #[arg(long, value_name = "REGEX")]
    pub wait_log: Option<String>,

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
    /// stable for SECS seconds. Heuristic — prefer --wait-log or
    /// --wait-port when a deterministic signal is available. A
    /// process can open more files later in its lifecycle.
    /// Round-5 §2.4.
    #[arg(long, value_name = "SECS")]
    pub wait_fd_stable: Option<f64>,

    /// Timeout for --wait-tcp / --wait-log / --wait-port /
    /// --wait-fd-stable in seconds (default: 30).
    #[arg(long, default_value = "30")]
    pub wait_timeout: u64,

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
    require_root()?;
    let env_pairs = parse_env_pairs(&env_vars)?;
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut running = nlink_lab::RunningLab::load(&lab)?;
    // Validate node exists
    let node_names: Vec<&str> = running.node_names().collect();
    if !node_names.contains(&node.as_str()) {
        eprintln!("Available nodes: {}", node_names.join(", "));
        return Err(nlink_lab::Error::NodeNotFound { name: node });
    }
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

    // Wait for readiness signal(s). --wait-tcp and --wait-log are
    // independent and AND-composed: both must succeed before
    // spawn returns. Either one can fail the spawn via timeout.
    let timeout = std::time::Duration::from_secs(wait_timeout);
    let interval = std::time::Duration::from_millis(500);
    if let Some(ref tcp_addr) = wait_tcp {
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
            .await?;
    }
    if let Some(ref re_src) = wait_log {
        let pattern = regex::Regex::new(re_src).map_err(|e| {
            nlink_lab::Error::invalid_topology(format!("invalid --wait-log regex {re_src:?}: {e}"))
        })?;
        running
            .wait_for_log_line(pid, &pattern, wait_log_stream.into(), timeout, interval)
            .await?;
    }
    if let Some(port) = wait_port {
        running
            .wait_for_port(&node, pid, port, timeout, interval)
            .await?;
    }
    if let Some(secs) = wait_fd_stable {
        let stable_for = std::time::Duration::from_secs_f64(secs);
        running
            .wait_for_fd_stable(&node, pid, stable_for, timeout, interval)
            .await?;
    }
    if (wait_tcp.is_some() || wait_log.is_some() || wait_port.is_some() || wait_fd_stable.is_some())
        && !ctx.quiet
    {
        eprintln!("ready");
    }

    Ok(())
}
