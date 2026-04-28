#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]

use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

// ─── Color helpers ───────────────────────────────────────

fn use_color() -> bool {
    std::env::var("NO_COLOR").is_err() && atty::is(atty::Stream::Stderr)
}

fn green(s: &str) -> String {
    if use_color() {
        format!("\x1b[32m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn red(s: &str) -> String {
    if use_color() {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn yellow(s: &str) -> String {
    if use_color() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn bold(s: &str) -> String {
    if use_color() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
struct Cli {
    /// Output JSON instead of human-readable text (where supported).
    ///
    /// Supported by: `deploy`, `status`, `inspect`, `spawn`, `exec`, `ps`,
    /// `diagnose`, `render`. JSON Schemas for the high-traffic shapes
    /// (deploy/status/spawn/ps) live under `docs/json-schemas/`.
    #[arg(long, global = true)]
    json: bool,

    /// Verbose output (show deployment steps, tracing info).
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress informational output (errors still go to stderr).
    ///
    /// Recommended for scripted/automated use; the default human-readable
    /// output is intended for interactive shells.
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy a lab from a topology file (.nll).
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "name": str, "nodes": int, "links": int, "deploy_time_ms": int }
    ///
    /// Combined with `--unique`, the `name` field is the chosen unique
    /// lab name (original name + PID suffix). Useful for scripted
    /// teardown.
    Deploy {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Validate only, don't actually deploy.
        #[arg(long)]
        dry_run: bool,

        /// Destroy existing lab with same name before deploying.
        #[arg(long)]
        force: bool,

        /// Start the Zenoh backend daemon after deploying.
        #[arg(long)]
        daemon: bool,

        /// Skip validate block assertions after deploy.
        #[arg(long)]
        skip_validate: bool,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Append suffix to lab name (for parallel test safety).
        #[arg(long)]
        suffix: Option<String>,

        /// Auto-generate unique lab name suffix (appends PID).
        #[arg(long)]
        unique: bool,
    },

    /// Apply topology changes to a running lab.
    ///
    /// Reconciles the live lab state to match an updated NLL,
    /// issuing only the deltas. Add `--check` to fail on any drift
    /// (a CI gate). Add `--json --dry-run` for machine-parseable
    /// diff output.
    Apply {
        /// Path to the updated topology file (.nll).
        topology: PathBuf,

        /// Show what would change without applying.
        #[arg(long)]
        dry_run: bool,

        /// Drift check — exit non-zero if the live lab differs from
        /// the NLL. Useful as a CI gate. Implies --dry-run.
        #[arg(long)]
        check: bool,
    },

    /// Tear down a running lab.
    Destroy {
        /// Lab name (omit with --all or --orphans).
        name: Option<String>,

        /// Continue cleanup even if some resources are already gone.
        #[arg(long)]
        force: bool,

        /// Destroy all running labs.
        #[arg(long)]
        all: bool,

        /// Also reap mgmt bridges / veths / namespaces with no state file
        /// (left behind by a crashed deploy). Implies best-effort cleanup;
        /// can be combined with --all or used on its own.
        #[arg(long)]
        orphans: bool,
    },

    /// Show running labs or details of a specific lab.
    ///
    /// JSON OUTPUT (with `--json`, no lab name):
    ///   [ { "name": str, "node_count": int, "created_at": str }, ... ]
    ///
    /// JSON OUTPUT (with `--json --scan`):
    ///   { "labs": [ ... ],
    ///     "orphans": { "bridges": [str], "veths": [str], "netns": [str],
    ///                  "stale": [ { "name": str,
    ///                               "missing_namespaces": [str] } ] } }
    ///
    /// JSON OUTPUT (with `--json <lab>`):
    ///   topology object for the lab + an `addresses` field per node.
    Status {
        /// Lab name (omit to list all).
        name: Option<String>,

        /// Also scan the host for mgmt bridges / namespaces with no
        /// matching state file (orphans), and labs whose state file
        /// claims namespaces no longer present on the host (stale).
        #[arg(long)]
        scan: bool,
    },

    /// Run a command in a lab node.
    Exec {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Set environment variables (can be repeated: --env KEY=VALUE).
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,

        /// Working directory for the command. For namespace nodes this is
        /// `chdir()` on the host filesystem; for container nodes it's passed
        /// as `-w <path>` to docker/podman.
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,

        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// Spawn a background process in a lab node.
    ///
    /// Stdout/stderr are captured to per-process log files at:
    ///
    ///   $XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}
    ///
    /// (defaults to `~/.local/state` if `XDG_STATE_HOME` is unset). The
    /// path is stable; consumers can read it directly, or use
    /// `nlink-lab logs <lab> --pid <pid>`.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "command": str, "node": str, "pid": int }
    Spawn {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Directory for stdout/stderr log files (default: lab state dir).
        #[arg(long)]
        log_dir: Option<PathBuf>,

        /// Set environment variables (can be repeated: --env KEY=VALUE).
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,

        /// Working directory for the spawned process (chdir before exec).
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,

        /// Wait for TCP port after spawn (e.g., "127.0.0.1:8080" or "8080").
        ///
        /// The probe runs inside the node's namespace, so `127.0.0.1:<port>`
        /// only matches a service that bound to the loopback interface. If
        /// your service binds to a specific node IP (e.g., the interface
        /// address), pass that address here instead of `127.0.0.1`.
        #[arg(long)]
        wait_tcp: Option<String>,

        /// Timeout for --wait-tcp in seconds (default: 30).
        #[arg(long, default_value = "30")]
        wait_timeout: u64,

        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// Validate a topology file without deploying.
    Validate {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Show resolved IP addresses for all interfaces.
        #[arg(long)]
        show_ips: bool,
    },

    /// Run topology tests: deploy, validate, destroy.
    Test {
        /// Topology file or directory of .nll files.
        path: PathBuf,

        /// Write JUnit XML results to file.
        #[arg(long)]
        junit: Option<PathBuf>,

        /// Write TAP output to stdout.
        #[arg(long)]
        tap: bool,

        /// Stop on first failure.
        #[arg(long)]
        fail_fast: bool,
    },

    /// Modify link impairment at runtime.
    Impair {
        /// Lab name.
        lab: String,

        /// Endpoint (e.g., "router:eth0"). Not required with --show.
        endpoint: Option<String>,

        /// Show current impairments on all interfaces.
        #[arg(long)]
        show: bool,

        /// Delay (e.g., "10ms").
        #[arg(long)]
        delay: Option<String>,

        /// Jitter (e.g., "2ms").
        #[arg(long)]
        jitter: Option<String>,

        /// Packet loss (e.g., "0.1%").
        #[arg(long)]
        loss: Option<String>,

        /// Rate limit (e.g., "100mbit").
        #[arg(long)]
        rate: Option<String>,

        /// Remove impairment.
        #[arg(long)]
        clear: bool,

        /// Egress delay (applied to named endpoint).
        #[arg(long)]
        out_delay: Option<String>,

        /// Egress jitter.
        #[arg(long)]
        out_jitter: Option<String>,

        /// Egress packet loss.
        #[arg(long)]
        out_loss: Option<String>,

        /// Egress rate limit.
        #[arg(long)]
        out_rate: Option<String>,

        /// Ingress delay (applied to peer endpoint).
        #[arg(long)]
        in_delay: Option<String>,

        /// Ingress jitter.
        #[arg(long)]
        in_jitter: Option<String>,

        /// Ingress packet loss.
        #[arg(long)]
        in_loss: Option<String>,

        /// Ingress rate limit.
        #[arg(long)]
        in_rate: Option<String>,

        /// Simulate a network partition (save impairments, apply 100% loss).
        #[arg(long)]
        partition: bool,

        /// Restore pre-partition impairments.
        #[arg(long)]
        heal: bool,
    },

    /// Print topology as DOT graph.
    Graph {
        /// Path to the topology file (.nll).
        topology: PathBuf,
    },

    /// Render a topology file with all loops, variables, and imports expanded.
    Render {
        /// Path to the topology file (.nll).
        topology: PathBuf,
        /// Output as DOT graph (for Graphviz).
        #[arg(long)]
        dot: bool,
        /// Output as ASCII diagram.
        #[arg(long)]
        ascii: bool,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Open an interactive shell in a lab node.
    Shell {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Shell to use (default: /bin/sh).
        #[arg(long, default_value = "/bin/sh")]
        shell: String,
    },

    /// List background processes (alive and exited) tracked by `spawn`.
    ///
    /// Exited processes remain in the listing with `alive: false` so
    /// post-mortem inspection (which log files? when did they exit?) is
    /// possible. They are pruned only when the lab is destroyed. Consumers
    /// polling "is X still running?" must check the `alive` field, not
    /// just look up the PID.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   [ { "node": str, "pid": int, "alive": bool,
    ///       "stdout_log": str | null, "stderr_log": str | null }, ... ]
    Ps {
        /// Lab name.
        lab: String,
    },

    /// Kill a tracked background process.
    Kill {
        /// Lab name.
        lab: String,

        /// Process ID to kill.
        pid: u32,
    },

    /// Run diagnostics on a lab.
    Diagnose {
        /// Lab name.
        lab: String,

        /// Node name (omit to diagnose all).
        node: Option<String>,
    },

    /// Capture packets on an interface using netring.
    Capture {
        /// Lab name.
        lab: String,

        /// Endpoint (e.g., "router:eth0").
        endpoint: String,

        /// Write to pcap file (default: print summaries to stdout).
        #[arg(short, long)]
        write: Option<PathBuf>,

        /// Capture N packets then stop.
        #[arg(short, long)]
        count: Option<u64>,

        /// BPF filter expression (e.g., "tcp port 80").
        #[arg(short, long)]
        filter: Option<String>,

        /// Stop after N seconds.
        #[arg(long)]
        duration: Option<f64>,

        /// Snap length -- truncate packets to N bytes.
        #[arg(long, default_value = "262144")]
        snap_len: u32,
    },

    /// Wait for a lab to be ready.
    Wait {
        /// Lab name.
        name: String,

        /// Timeout in seconds (default: 30).
        #[arg(short, long, default_value = "30")]
        timeout: u64,
    },

    /// Wait for a service or condition inside a lab node.
    WaitFor {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
        #[arg(long)]
        tcp: Option<String>,

        /// Wait for command to succeed (exit 0).
        #[arg(long = "exec")]
        exec_cmd: Option<String>,

        /// Wait for file to exist.
        #[arg(long)]
        file: Option<String>,

        /// Timeout in seconds (default: 30).
        #[arg(short, long, default_value = "30")]
        timeout: u64,

        /// Poll interval in milliseconds (default: 500).
        #[arg(long, default_value = "500")]
        interval: u64,
    },

    /// Show IP addresses assigned to a node.
    Ip {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Filter by interface name.
        #[arg(long)]
        iface: Option<String>,

        /// Show CIDR notation (include prefix length).
        #[arg(long)]
        cidr: bool,
    },

    /// Compare two topology files and show differences.
    Diff {
        /// First topology file (or lab name with --lab).
        a: PathBuf,

        /// Second topology file.
        b: PathBuf,
    },

    /// Export a running lab's topology as serialized data.
    ///
    /// By default, dumps the rendered topology as TOML/JSON to stdout
    /// or `--output FILE`. With `--archive`, produces a portable
    /// `.nlz` lab archive (tar.gz with manifest + topology + params
    /// + rendered + checksums) suitable for sharing repros.
    Export {
        /// Lab name (or path to an .nll file with --archive).
        lab: String,

        /// Output file (default: stdout for plain export, ./<lab>.nlz with --archive).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Produce a portable `.nlz` archive instead of plain TOML/JSON.
        #[arg(long)]
        archive: bool,

        /// (with --archive) Include live state (PIDs, ns names) for inspection.
        #[arg(long, requires = "archive")]
        include_running_state: bool,

        /// (with --archive) Skip the rendered.toml snapshot.
        #[arg(long, requires = "archive")]
        no_rendered: bool,

        /// (with --archive) NLL `param` overrides recorded in the archive.
        #[arg(long = "set", value_name = "KEY=VALUE", requires = "archive")]
        set_params: Vec<String>,
    },

    /// Import a `.nlz` lab archive.
    ///
    /// Verifies checksums, extracts to `./<lab-name>/` (or `-d DIR`),
    /// and validates the topology. Pass `--no-deploy` to extract +
    /// validate without deploying; `--no-reparse` to use the bundled
    /// rendered.toml directly (useful when the archive was produced
    /// by a newer nlink-lab whose NLL syntax we don't fully understand).
    Import {
        /// Path to a `.nlz` archive.
        archive: PathBuf,

        /// Extract to this directory. Default: ./<lab-name>/
        #[arg(short = 'd', long)]
        dir: Option<PathBuf>,

        /// Extract + validate only; don't deploy.
        #[arg(long)]
        no_deploy: bool,

        /// Use the archive's rendered.toml as-is, skip re-parsing the NLL.
        #[arg(long)]
        no_reparse: bool,
    },

    /// Show comprehensive lab details, OR summarize a `.nlz` archive.
    ///
    /// If LAB is a path ending in `.nlz`, summarizes the archive
    /// (manifest + node/link/network counts) without extracting.
    /// Otherwise, behaves as before — runs against a deployed lab.
    Inspect {
        /// Lab name, or path to a `.nlz` archive.
        lab: String,
    },

    /// List container nodes in a running lab.
    Containers {
        /// Lab name.
        lab: String,
    },

    /// Show container logs or per-process logs from `nlink-lab spawn`.
    ///
    /// Without `--pid`: shows the container's stdout/stderr (node must be
    /// a container).  With `--pid`: shows the per-process log file written
    /// by `spawn`. Per-process log files live at:
    ///
    ///   $XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}
    ///
    /// (defaults to `~/.local/state` if `XDG_STATE_HOME` is unset). The
    /// path is stable; consumers can read it directly.
    Logs {
        /// Lab name.
        lab: String,
        /// Node name (for container logs).
        node: Option<String>,
        /// Process ID (for background process logs).
        #[arg(long)]
        pid: Option<u32>,
        /// Show stderr instead of stdout (with --pid).
        #[arg(long)]
        stderr: bool,
        /// Stream logs in tail -F style. Works for container nodes (via
        /// the runtime) and for tracked background processes (via
        /// `--pid`). Re-opens the file on rotation/truncation. Stops on
        /// Ctrl-C.
        #[arg(long)]
        follow: bool,
        /// Show last N lines.
        #[arg(long)]
        tail: Option<u32>,
    },

    /// Pre-pull all container images from a topology.
    Pull {
        /// Path to the topology file (.nll).
        topology: PathBuf,
    },

    /// Show container resource usage.
    Stats {
        /// Lab name.
        lab: String,
    },

    /// Restart a container node.
    Restart {
        /// Lab name.
        lab: String,
        /// Node name (must be a container node).
        node: String,
    },

    /// Generate shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Start the Zenoh backend daemon for a running lab.
    Daemon {
        /// Lab name (must be deployed).
        lab: String,

        /// Metrics collection interval in seconds.
        #[arg(short, long, default_value = "2")]
        interval: u64,

        /// Zenoh mode: peer or client.
        #[arg(long, default_value = "peer")]
        zenoh_mode: String,

        /// Zenoh listen endpoint.
        #[arg(long)]
        zenoh_listen: Option<String>,

        /// Zenoh connect endpoint.
        #[arg(long)]
        zenoh_connect: Option<String>,
    },

    /// Stream live metrics from a lab via Zenoh (no root required).
    Metrics {
        /// Lab name.
        lab: String,

        /// Filter to specific node.
        #[arg(short, long)]
        node: Option<String>,

        /// Output format: table (default), json.
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Number of samples then exit.
        #[arg(short, long)]
        count: Option<usize>,

        /// Zenoh connect endpoint.
        #[arg(long)]
        zenoh_connect: Option<String>,
    },

    /// Create a topology file from a built-in template.
    Init {
        /// Template name (e.g., "router", "spine-leaf"). Use --list to see all.
        template: Option<String>,

        /// List available templates.
        #[arg(long)]
        list: bool,

        /// Output directory (default: current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output format.
        #[arg(short, long, default_value = "nll")]
        format: String,

        /// Override the lab name.
        #[arg(short, long)]
        name: Option<String>,

        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Set tracing level based on --verbose flag (default: warn, verbose: info)
    let env_filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new("info")
    } else {
        tracing_subscriber::EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Handle completions synchronously (no runtime needed)
    if let Commands::Completions { shell } = &cli.command {
        clap_complete::generate(
            *shell,
            &mut Cli::command(),
            "nlink-lab",
            &mut std::io::stdout(),
        );
        return ExitCode::SUCCESS;
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(nlink_lab::Error::NllDiagnostic(diag)) => {
            let report = miette::Report::new(*diag);
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parse a topology file, optionally with CLI `--set` parameters.
fn parse_topology(
    path: &std::path::Path,
    params: &[String],
) -> nlink_lab::Result<nlink_lab::Topology> {
    let cli_params: Vec<(String, String)> = params
        .iter()
        .map(|p| {
            let (key, value) = p.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "invalid --set format: '{p}' (expected KEY=VALUE)"
                ))
            })?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect::<nlink_lab::Result<Vec<_>>>()?;

    if cli_params.is_empty() {
        nlink_lab::parser::parse_file(path)
    } else {
        nlink_lab::parser::parse_file_with_params(path, &cli_params)
    }
}

async fn run(cli: Cli) -> nlink_lab::Result<()> {
    let json = cli.json;
    let quiet = cli.quiet;
    let _verbose = cli.verbose;
    match cli.command {
        Commands::Deploy {
            topology,
            dry_run,
            force,
            daemon,
            skip_validate,
            params,
            suffix,
            unique,
        } => {
            let mut topo = parse_topology(&topology, &params)?;
            if unique {
                topo.lab.name = format!("{}-{}", topo.lab.name, std::process::id());
            } else if let Some(ref sfx) = suffix {
                topo.lab.name = format!("{}-{sfx}", topo.lab.name);
            }
            if skip_validate {
                topo.assertions.clear();
            }
            let result = topo.validate();

            // Print warnings
            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }

            if result.has_errors() {
                eprintln!("Validation failed for {:?}:", topo.lab.name);
                for e in result.errors() {
                    eprintln!("  {} {e}", red("ERROR"));
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            if dry_run {
                println!("Topology {:?} is valid", topo.lab.name);
                print_topology_summary(&topo);
                return Ok(());
            }

            // Handle --force: destroy existing lab first
            if force {
                if nlink_lab::state::exists(&topo.lab.name) {
                    let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
                    lab.destroy().await?;
                } else {
                    // Best-effort cleanup of orphaned resources (no state file)
                    force_cleanup(&topo.lab.name).await;
                }
            }

            check_root();

            let start = Instant::now();
            let lab = topo.deploy().await?;
            let elapsed = start.elapsed();

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": topo.lab.name,
                        "nodes": topo.nodes.len(),
                        "links": topo.links.len(),
                        "deploy_time_ms": elapsed.as_millis() as u64,
                    })
                );
            } else {
                println!(
                    "{} Lab {:?} deployed in {:.0?}",
                    green("OK"),
                    topo.lab.name,
                    elapsed
                );
                print_deploy_summary(&topo);

                if !quiet {
                    let first_node = topo
                        .nodes
                        .keys()
                        .next()
                        .map(|s| s.as_str())
                        .unwrap_or("node");
                    println!();
                    println!("Next steps:");
                    println!(
                        "  nlink-lab status {}          # inspect lab",
                        topo.lab.name
                    );
                    println!(
                        "  nlink-lab exec {} {} -- ip addr",
                        topo.lab.name, first_node
                    );
                    println!(
                        "  nlink-lab shell {} {}        # interactive shell",
                        topo.lab.name, first_node
                    );
                    println!("  nlink-lab destroy {}         # tear down", topo.lab.name);
                }
            }

            if daemon {
                run_daemon_inline(&lab).await?;
            }
            Ok(())
        }

        Commands::Apply {
            topology,
            dry_run,
            check,
        } => {
            // --check implies --dry-run.
            let dry_run = dry_run || check;

            let desired = nlink_lab::parser::parse_file(&topology)?;
            let result = desired.validate();
            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }
            if result.has_errors() {
                for e in result.errors() {
                    eprintln!("  {} {e}", red("ERROR"));
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            // Load current topology from running lab state
            let lab_name = &desired.lab.name;
            if !nlink_lab::state::exists(lab_name) {
                return Err(nlink_lab::Error::NotFound {
                    name: format!("{lab_name} (deploy first, then apply changes)"),
                });
            }
            let mut running = nlink_lab::RunningLab::load(lab_name)?;
            let current = running.topology();

            let diff = nlink_lab::diff_topologies(current, &desired);

            // JSON dry-run output for CI consumption.
            if json && dry_run {
                #[derive(serde::Serialize)]
                struct DryRunReport<'a> {
                    lab: &'a str,
                    no_op: bool,
                    change_count: usize,
                    diff: &'a nlink_lab::TopologyDiff,
                }
                let report = DryRunReport {
                    lab: lab_name,
                    no_op: diff.is_empty(),
                    change_count: diff.change_count(),
                    diff: &diff,
                };
                println!("{}", serde_json::to_string_pretty(&report)?);
                if check && !diff.is_empty() {
                    return Err(nlink_lab::Error::Validation(format!(
                        "drift detected: {} change(s) needed to converge",
                        diff.change_count(),
                    )));
                }
                return Ok(());
            }

            if diff.is_empty() {
                if !quiet {
                    println!("No changes to apply.");
                }
                return Ok(());
            }

            // --check: exit non-zero if any drift.
            if check {
                if !quiet {
                    println!("Drift detected for lab '{lab_name}':");
                    print!("{diff}");
                    println!("{} change(s) needed to converge", diff.change_count());
                }
                return Err(nlink_lab::Error::Validation(format!(
                    "drift detected: {} change(s) needed to converge",
                    diff.change_count(),
                )));
            }

            if !quiet {
                println!("Changes for lab '{lab_name}':");
                print!("{diff}");
                println!("{} change(s)", diff.change_count());
            }

            if dry_run {
                if !quiet {
                    println!("\n(dry run — no changes applied)");
                }
                return Ok(());
            }

            check_root();
            let start = Instant::now();
            nlink_lab::apply_diff(&mut running, &desired, &diff).await?;
            let elapsed = start.elapsed();

            if !quiet {
                println!(
                    "\nApplied {} change(s) in {:.0?}",
                    diff.change_count(),
                    elapsed
                );
            }
            Ok(())
        }

        Commands::Destroy {
            name,
            force,
            all,
            orphans,
        } => {
            check_root();
            if all {
                let labs = nlink_lab::RunningLab::list()?;
                if labs.is_empty() && !orphans {
                    println!("No running labs.");
                    return Ok(());
                }
                for info in &labs {
                    match nlink_lab::RunningLab::load(&info.name) {
                        Ok(lab) => {
                            lab.destroy().await?;
                            println!("Destroyed '{}'", info.name);
                        }
                        Err(_) if force => {
                            force_cleanup(&info.name).await;
                            println!("Force-cleaned '{}'", info.name);
                        }
                        Err(e) => eprintln!("Failed to destroy '{}': {e}", info.name),
                    }
                }
                if !labs.is_empty() {
                    println!("{} lab(s) destroyed", labs.len());
                }
                if orphans {
                    reap_orphans(&labs).await;
                }
                return Ok(());
            }
            if orphans && name.is_none() {
                // `destroy --orphans` alone: reap without touching state-backed labs.
                let labs = nlink_lab::RunningLab::list()?;
                reap_orphans(&labs).await;
                return Ok(());
            }
            let name = name.ok_or_else(|| {
                nlink_lab::Error::deploy_failed("lab name required (or use --all/--orphans)")
            })?;
            match nlink_lab::RunningLab::load(&name) {
                Ok(lab) => {
                    let node_count = lab.namespace_count();
                    let topo = lab.topology();
                    let container_count = topo.nodes.values().filter(|n| n.image.is_some()).count();
                    let link_count = topo.links.len();
                    let process_count = lab.process_status().iter().filter(|p| p.alive).count();
                    lab.destroy().await?;
                    println!("Lab {name:?} destroyed:");
                    println!("  Nodes:       {node_count}");
                    if container_count > 0 {
                        println!("  Containers:  {container_count} stopped and removed");
                    }
                    println!("  Links:       {link_count}");
                    if process_count > 0 {
                        println!("  Processes:   {process_count} killed");
                    }
                }
                Err(e) if force => {
                    eprintln!("warning: state not found, attempting force cleanup: {e}");
                    force_cleanup(&name).await;
                    println!("Lab {name:?} force-cleaned");
                }
                Err(nlink_lab::Error::NotFound { .. }) => {
                    // Idempotent: destroying a non-existent lab is a no-op
                }
                Err(e) => return Err(e),
            }
            Ok(())
        }

        Commands::Status { name, scan } => match name {
            None => {
                let labs = nlink_lab::RunningLab::list()?;
                let orphans = if scan {
                    find_orphans(&labs)
                } else {
                    Orphans::default()
                };
                if json {
                    if scan {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "labs": labs,
                                "orphans": orphans,
                            }))?
                        );
                    } else {
                        println!("{}", serde_json::to_string_pretty(&labs)?);
                    }
                } else if labs.is_empty() {
                    println!("No running labs.");
                } else {
                    println!("{:<18} {:<6} CREATED", "NAME", "NODES");
                    for info in labs {
                        println!(
                            "{:<18} {:<6} {}",
                            info.name, info.node_count, info.created_at
                        );
                    }
                }
                if scan && !json && !orphans.is_empty() {
                    let has_orphans = !orphans.bridges.is_empty()
                        || !orphans.veths.is_empty()
                        || !orphans.netns.is_empty();
                    if has_orphans {
                        println!();
                        println!("Orphans detected (no matching state file):");
                        for b in &orphans.bridges {
                            println!("  bridge {b}");
                        }
                        for v in &orphans.veths {
                            println!("  veth   {v}");
                        }
                        for n in &orphans.netns {
                            println!("  netns  {n}");
                        }
                        println!();
                        println!("Run `nlink-lab destroy --orphans` to clean up.");
                    }
                    if !orphans.stale.is_empty() {
                        println!();
                        println!("Stale labs detected (state file with missing resources):");
                        for s in &orphans.stale {
                            println!(
                                "  {}  (missing: {})",
                                s.name,
                                s.missing_namespaces.join(", ")
                            );
                        }
                        println!();
                        println!(
                            "Run `nlink-lab destroy <lab>` to clean up each stale state file."
                        );
                    }
                }
                Ok(())
            }
            Some(name) => {
                let lab = nlink_lab::RunningLab::load(&name)?;
                if json {
                    let mut output = serde_json::to_value(lab.topology())?;
                    // Add resolved addresses per node (including mgmt0)
                    if let Some(nodes) = output.get_mut("nodes")
                        && let Some(nodes_obj) = nodes.as_object_mut()
                    {
                        for node_name in nodes_obj.keys().cloned().collect::<Vec<_>>() {
                            if let Ok(addrs) = lab.node_addresses(&node_name)
                                && !addrs.is_empty()
                                && let Some(n) = nodes_obj.get_mut(&node_name)
                                && let Some(o) = n.as_object_mut()
                            {
                                o.insert("addresses".to_string(), serde_json::json!(addrs));
                            }
                        }
                    }
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    let topo = lab.topology();
                    println!("Lab: {}", lab.name());
                    println!(
                        "Nodes: {}  Links: {}  Impairments: {}",
                        lab.namespace_count(),
                        topo.links.len(),
                        topo.impairments.len()
                    );
                    println!();
                    println!("  {:<20} {:<12} IMAGE", "NODE", "TYPE");
                    let mut names: Vec<&String> = topo.nodes.keys().collect();
                    names.sort();
                    for name in names {
                        let node = &topo.nodes[name];
                        let kind = if node.image.is_some() {
                            "container"
                        } else {
                            "namespace"
                        };
                        let image = node.image.as_deref().unwrap_or("--");
                        println!("  {:<20} {:<12} {}", name, kind, image);
                    }
                }
                Ok(())
            }
        },

        Commands::Exec {
            lab,
            node,
            env_vars,
            workdir,
            cmd,
        } => {
            check_root();
            // Prepend env vars to command: env K=V K=V ... cmd args
            let cmd = if env_vars.is_empty() {
                cmd
            } else {
                let mut full = vec!["env".to_string()];
                full.extend(env_vars);
                full.extend(cmd);
                full
            };

            if cli.json {
                // In JSON mode, wrap ALL errors as JSON output
                let result = (|| -> nlink_lab::Result<serde_json::Value> {
                    let running = nlink_lab::RunningLab::load(&lab)?;
                    let node_names: Vec<&str> = running.node_names().collect();
                    if !node_names.contains(&node.as_str()) {
                        return Err(nlink_lab::Error::NodeNotFound { name: node.clone() });
                    }
                    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
                    let start = Instant::now();
                    let output = running.exec_in(&node, &cmd[0], &args, workdir.as_deref())?;
                    let duration_ms = start.elapsed().as_millis() as u64;
                    Ok(serde_json::json!({
                        "exit_code": output.exit_code,
                        "stdout": output.stdout,
                        "stderr": output.stderr,
                        "duration_ms": duration_ms,
                    }))
                })();
                match result {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "error": e.to_string(),
                                "exit_code": null,
                                "stdout": "",
                                "stderr": "",
                                "duration_ms": 0,
                            })
                        );
                    }
                }
                return Ok(());
            }

            // Non-JSON path: stream stdio live so long-running commands
            // (services, tail -f, ping) show output as it's produced.
            // Scripts that want captured/structured output should use
            // `--json`, which still buffers into the structured response.
            let running = nlink_lab::RunningLab::load(&lab)?;
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
            let code = running.exec_attached_in(&node, &cmd[0], &args, workdir.as_deref())?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }

        Commands::Spawn {
            lab,
            node,
            log_dir,
            env_vars,
            workdir,
            wait_tcp,
            wait_timeout,
            cmd,
        } => {
            check_root();
            // Prepend env vars to command
            let cmd = if env_vars.is_empty() {
                cmd
            } else {
                let mut full = vec!["env".to_string()];
                full.extend(env_vars);
                full.extend(cmd);
                full
            };
            let mut running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            let args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let pid =
                running.spawn_with_logs_in(&node, &args, log_dir.as_deref(), workdir.as_deref())?;
            running.save_state()?;

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "pid": pid,
                        "node": node,
                        "command": cmd.join(" "),
                    })
                );
            } else {
                println!("PID: {pid}");
            }

            // Wait for TCP port if requested
            if let Some(ref tcp_addr) = wait_tcp {
                let timeout = std::time::Duration::from_secs(wait_timeout);
                let interval = std::time::Duration::from_millis(500);
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
                if !cli.quiet {
                    eprintln!("ready");
                }
            }

            Ok(())
        }

        Commands::Validate {
            topology,
            params,
            show_ips,
        } => {
            let topo = parse_topology(&topology, &params)?;
            let result = topo.validate();

            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }

            if result.has_errors() {
                eprintln!("Validation failed for {:?}:", topo.lab.name);
                for e in result.errors() {
                    eprintln!("  {} {e}", red("ERROR"));
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            println!("Topology {:?} is valid", topo.lab.name);
            print_topology_summary(&topo);

            if show_ips {
                println!("\n  Addresses:");
                // From links
                for link in &topo.links {
                    if let Some(ref addrs) = link.addresses {
                        for (i, ep_str) in link.endpoints.iter().enumerate() {
                            println!("    {:<24} {} (link)", ep_str, addrs[i]);
                        }
                    }
                }
                // From network ports
                for (net_name, network) in &topo.networks {
                    for member in &network.members {
                        if let Some(ep) = nlink_lab::EndpointRef::parse(member) {
                            // Port keys can be either "node:iface" or "node"
                            let port = network
                                .ports
                                .get(member)
                                .or_else(|| network.ports.get(&ep.node));
                            if let Some(port) = port {
                                for addr in &port.addresses {
                                    println!(
                                        "    {:<24} {} (network {:?})",
                                        member, addr, net_name
                                    );
                                }
                            }
                        }
                    }
                }
                // From node interfaces (loopback, etc.)
                for (name, node) in &topo.nodes {
                    for (iface, cfg) in &node.interfaces {
                        for addr in &cfg.addresses {
                            println!("    {name}:{iface:<18} {addr} (interface)");
                        }
                    }
                }
            }
            Ok(())
        }

        Commands::Test {
            path,
            junit,
            tap,
            fail_fast,
        } => {
            check_root();

            // Collect .nll files
            let files: Vec<PathBuf> = if path.is_dir() {
                let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "nll"))
                    .collect();
                entries.sort();
                entries
            } else {
                vec![path.clone()]
            };

            if files.is_empty() {
                eprintln!("No .nll files found in {}", path.display());
                return Ok(());
            }

            let mut all_results = Vec::new();
            let mut any_failed = false;

            for file in &files {
                eprint!("Testing {} ... ", file.display());
                match nlink_lab::test_runner::run_test(file).await {
                    Ok(result) => {
                        let pass_count = result.assertions.iter().filter(|a| a.passed).count();
                        let total = result.assertions.len();
                        if result.passed {
                            eprintln!(
                                "{} ({pass_count}/{total} assertions, {}ms)",
                                green("PASS"),
                                result.total_ms
                            );
                        } else {
                            eprintln!(
                                "{} ({pass_count}/{total} assertions, {}ms)",
                                red("FAIL"),
                                result.total_ms
                            );
                            for a in &result.assertions {
                                if !a.passed {
                                    eprintln!(
                                        "  {} {}{}",
                                        red("FAIL"),
                                        a.description,
                                        a.detail
                                            .as_ref()
                                            .map(|d| format!(": {d}"))
                                            .unwrap_or_default()
                                    );
                                }
                            }
                            any_failed = true;
                        }
                        all_results.push(result);
                    }
                    Err(e) => {
                        eprintln!("{}: {e}", red("ERROR"));
                        any_failed = true;
                        if fail_fast {
                            break;
                        }
                    }
                }
                if any_failed && fail_fast {
                    break;
                }
            }

            // Output formats
            if let Some(junit_path) = &junit {
                let xml = nlink_lab::test_runner::format_junit(&all_results);
                std::fs::write(junit_path, &xml)?;
                eprintln!("JUnit results written to {}", junit_path.display());
            }

            if tap {
                print!("{}", nlink_lab::test_runner::format_tap(&all_results));
            }

            if any_failed {
                std::process::exit(1);
            }
            Ok(())
        }

        Commands::Impair {
            lab,
            endpoint,
            show,
            delay,
            jitter,
            loss,
            rate,
            clear,
            out_delay,
            out_jitter,
            out_loss,
            out_rate,
            in_delay,
            in_jitter,
            in_loss,
            in_rate,
            partition,
            heal,
        } => {
            check_root();
            let mut running = nlink_lab::RunningLab::load(&lab)?;

            if show {
                for node_name in running.node_names() {
                    let output = running.exec(node_name, "tc", &["qdisc", "show"])?;
                    if !output.stdout.trim().is_empty() {
                        println!("--- {node_name} ---");
                        println!("{}", output.stdout.trim());
                    }
                }
                return Ok(());
            }

            let endpoint = endpoint.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("endpoint required (use --show to inspect)")
            })?;

            if partition {
                running.partition(&endpoint).await?;
                println!("Partitioned {endpoint}");
            } else if heal {
                running.heal(&endpoint).await?;
                println!("Healed {endpoint}");
            } else if clear {
                running.clear_impairment(&endpoint).await?;
                println!("Cleared impairment on {endpoint}");
            } else {
                let has_directional = out_delay.is_some()
                    || out_jitter.is_some()
                    || out_loss.is_some()
                    || out_rate.is_some()
                    || in_delay.is_some()
                    || in_jitter.is_some()
                    || in_loss.is_some()
                    || in_rate.is_some();
                let has_symmetric =
                    delay.is_some() || jitter.is_some() || loss.is_some() || rate.is_some();

                if has_directional && has_symmetric {
                    return Err(nlink_lab::Error::invalid_topology(
                        "cannot mix --delay/--loss with --out-delay/--in-delay",
                    ));
                }

                if has_directional {
                    let egress = nlink_lab::Impairment {
                        delay: out_delay,
                        jitter: out_jitter,
                        loss: out_loss,
                        rate: out_rate,
                        ..Default::default()
                    };
                    let ingress = nlink_lab::Impairment {
                        delay: in_delay,
                        jitter: in_jitter,
                        loss: in_loss,
                        rate: in_rate,
                        ..Default::default()
                    };

                    if egress != nlink_lab::Impairment::default() {
                        running.set_impairment(&endpoint, &egress).await?;
                        println!("Updated egress impairment on {endpoint}");
                    }
                    if ingress != nlink_lab::Impairment::default() {
                        let peer = running.peer_endpoint(&endpoint)?;
                        running.set_impairment(&peer, &ingress).await?;
                        println!("Updated ingress impairment on {endpoint} (via {peer})");
                    }
                } else {
                    let impairment = nlink_lab::Impairment {
                        delay,
                        jitter,
                        loss,
                        rate,
                        ..Default::default()
                    };
                    running.set_impairment(&endpoint, &impairment).await?;
                    println!("Updated impairment on {endpoint}");
                }
            }
            Ok(())
        }

        Commands::Graph { topology } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            print!("{}", topology_to_dot(&topo));
            Ok(())
        }

        Commands::Render {
            topology,
            dot,
            ascii,
            params,
        } => {
            let topo = parse_topology(&topology, &params)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&topo)?);
            } else if dot {
                print!("{}", topology_to_dot(&topo));
            } else if ascii {
                print!("{}", topology_to_ascii(&topo));
            } else {
                print!("{}", nlink_lab::render::render(&topo));
            }
            Ok(())
        }

        Commands::Shell { lab, node, shell } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            if let Some(container) = running.container_for(&node) {
                let rt = running.runtime_binary().unwrap_or("docker");
                let status = std::process::Command::new(rt)
                    .args(["exec", "-it", &container.id, &shell])
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("exec failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            } else {
                let ns = running.namespace_for(&node)?;
                let args = nsenter_shell_args(ns, &shell);
                let status = std::process::Command::new("nsenter")
                    .args(&args)
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("nsenter failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Commands::Ps { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let procs = running.process_status();
            if json {
                println!("{}", serde_json::to_string_pretty(&procs)?);
            } else if procs.is_empty() {
                println!("No tracked processes.");
            } else {
                println!("{:<12} {:<8} STATUS", "NODE", "PID");
                for p in &procs {
                    let status = if p.alive { "running" } else { "dead" };
                    println!("{:<12} {:<8} {}", p.node, p.pid, status);
                }
            }
            Ok(())
        }

        Commands::Kill { lab, pid } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            running.kill_process(pid)?;
            println!("Killed process {pid}");
            Ok(())
        }

        Commands::Diagnose { lab, node } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            let results = running.diagnose(node.as_deref()).await?;
            if json {
                let json_results: Vec<serde_json::Value> = results.iter().map(|diag| {
                    serde_json::json!({
                        "node": diag.node,
                        "interfaces": diag.interfaces.iter().map(|iface| {
                            serde_json::json!({
                                "name": iface.name,
                                "state": format!("{:?}", iface.state),
                                "mtu": iface.mtu,
                                "rx_bytes": iface.stats.rx_bytes(),
                                "tx_bytes": iface.stats.tx_bytes(),
                                "issues": iface.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                            })
                        }).collect::<Vec<_>>(),
                        "issues": diag.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json_results)?);
            } else {
                for diag in &results {
                    println!("── {} ──", diag.node);
                    for iface in &diag.interfaces {
                        let status = if iface.issues.is_empty() {
                            "OK"
                        } else {
                            "WARN"
                        };
                        println!(
                            "  [{status:<4}] {:<12} state={:<6} mtu={:<5} rx={} tx={}",
                            iface.name,
                            format!("{:?}", iface.state),
                            iface.mtu.unwrap_or(0),
                            iface.stats.rx_bytes(),
                            iface.stats.tx_bytes(),
                        );
                        for issue in &iface.issues {
                            println!("         {issue}");
                        }
                    }
                    for issue in &diag.issues {
                        println!("  [WARN] {issue}");
                    }
                }
            }
            Ok(())
        }

        Commands::Capture {
            lab,
            endpoint,
            write,
            count,
            filter,
            duration,
            snap_len,
        } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
                nlink_lab::Error::InvalidEndpoint {
                    endpoint: endpoint.clone(),
                }
            })?;

            let ns_name = running.namespace_for(&ep.node)?.to_string();

            let bpf = match &filter {
                Some(expr) => Some(nlink_lab::capture::compile_bpf_filter(expr)?),
                None => None,
            };

            let config = nlink_lab::capture::CaptureConfig {
                interface: ep.iface.clone(),
                snap_len,
                count,
                duration: duration.map(std::time::Duration::from_secs_f64),
                bpf_filter: bpf,
                profile: netring::RingProfile::LowMemory,
            };

            static CAPTURE_SHUTDOWN: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            CAPTURE_SHUTDOWN.store(false, std::sync::atomic::Ordering::Relaxed);
            // Handle both SIGINT (Ctrl-C) and SIGTERM (`kill`, `timeout(1)`)
            // so the capture loop can exit cleanly and print the summary
            // line. SIGKILL is uncatchable; per-packet pcap flushes
            // (capture.rs) protect data integrity in that case.
            unsafe {
                extern "C" fn handler(_: libc::c_int) {
                    CAPTURE_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let h = handler as *const () as libc::sighandler_t;
                libc::signal(libc::SIGINT, h);
                libc::signal(libc::SIGTERM, h);
            }

            let result = if let Some(path) = write {
                let file = std::fs::File::create(&path)?;
                nlink_lab::capture::run_capture(&ns_name, &config, Some(file), &CAPTURE_SHUTDOWN)?
            } else {
                nlink_lab::capture::run_capture::<std::fs::File>(
                    &ns_name,
                    &config,
                    None,
                    &CAPTURE_SHUTDOWN,
                )?
            };

            if !cli.quiet {
                eprintln!(
                    "\n{} packets captured ({} received by kernel, {} dropped)",
                    result.packets_captured, result.stats.packets, result.stats.drops,
                );
            }
            Ok(())
        }

        Commands::Diff { a, b } => {
            let topo_a = nlink_lab::parser::parse_file(&a)?;
            let topo_b = nlink_lab::parser::parse_file(&b)?;
            let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
            if json {
                // For JSON, output a simple summary
                println!(
                    "{}",
                    serde_json::json!({
                        "nodes_added": diff.nodes_added,
                        "nodes_removed": diff.nodes_removed,
                        "links_added": diff.links_added.len(),
                        "links_removed": diff.links_removed.len(),
                        "impairments_changed": diff.impairments_changed.len(),
                        "impairments_added": diff.impairments_added.len(),
                        "impairments_removed": diff.impairments_removed.len(),
                        "total_changes": diff.change_count(),
                    })
                );
            } else if diff.is_empty() {
                println!("No differences.");
            } else {
                println!("Diff: {} → {}", a.display(), b.display());
                print!("{diff}");
                println!("\n{} change(s)", diff.change_count());
            }
            Ok(())
        }

        Commands::Export {
            lab,
            output,
            archive,
            include_running_state,
            no_rendered,
            set_params,
        } => {
            if archive {
                use nlink_lab::portability::{ArchiveSource, ExportOptions, export_archive};
                let lab_path = std::path::Path::new(&lab);
                let source = if lab_path.extension().and_then(|s| s.to_str()) == Some("nll")
                    || lab_path.exists()
                {
                    ArchiveSource::Nll {
                        path: lab_path.into(),
                    }
                } else {
                    ArchiveSource::Lab { name: lab.clone() }
                };

                let params: Vec<(String, String)> = set_params
                    .iter()
                    .map(|p| {
                        let (k, v) = p.split_once('=').ok_or_else(|| {
                            nlink_lab::Error::invalid_topology(format!(
                                "invalid --set format: '{p}' (expected KEY=VALUE)"
                            ))
                        })?;
                        Ok((k.to_string(), v.to_string()))
                    })
                    .collect::<nlink_lab::Result<Vec<_>>>()?;

                let out_path = output.unwrap_or_else(|| {
                    let basename = match &source {
                        ArchiveSource::Lab { name } => name.clone(),
                        ArchiveSource::Nll { path } => path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("lab")
                            .to_string(),
                    };
                    PathBuf::from(format!("{basename}.nlz"))
                });

                let opts = ExportOptions {
                    include_running_state,
                    no_rendered,
                    params,
                };
                export_archive(source, &out_path, opts)?;
                if !quiet {
                    eprintln!("Archive written to {}", out_path.display());
                }
            } else {
                let running = nlink_lab::RunningLab::load(&lab)?;
                let content = if json {
                    serde_json::to_string_pretty(running.topology())?
                } else {
                    toml::to_string_pretty(running.topology()).map_err(|e| {
                        nlink_lab::Error::invalid_topology(format!("serialize: {e}"))
                    })?
                };
                match output {
                    Some(path) => {
                        std::fs::write(&path, &content)?;
                        if !quiet {
                            eprintln!("Exported to {}", path.display());
                        }
                    }
                    None => print!("{content}"),
                }
            }
            Ok(())
        }

        Commands::Import {
            archive,
            dir,
            no_deploy,
            no_reparse,
        } => {
            use nlink_lab::portability::import_archive;
            let report = import_archive(&archive, dir.as_deref(), no_reparse)?;
            if !quiet {
                eprintln!(
                    "Extracted lab '{}' to {} (format v{}, exported by {})",
                    report.manifest.lab_name,
                    report.extracted_to.display(),
                    report.manifest.format_version,
                    report.manifest.exported_by,
                );
            }
            if no_deploy {
                if !quiet {
                    eprintln!("(--no-deploy: skipping deploy)");
                }
                return Ok(());
            }
            // Deploy the imported topology. We re-read the extracted
            // topology.nll so the import path matches what `deploy`
            // would do for a regular file.
            let topology_path = report.extracted_to.join("topology.nll");
            let topo = nlink_lab::parser::parse_file(&topology_path)?;
            let lab = topo.deploy().await?;
            if !quiet {
                eprintln!("Deployed lab '{}'", lab.name());
            }
            Ok(())
        }

        Commands::Daemon {
            lab,
            interval: _interval,
            zenoh_mode: _zenoh_mode,
            zenoh_listen: _zenoh_listen,
            zenoh_connect: _zenoh_connect,
        } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            println!(
                "Starting Zenoh backend for lab '{}' ({} nodes)",
                lab,
                running.namespace_count(),
            );
            run_daemon_inline(&running).await
        }

        Commands::Metrics {
            lab,
            node,
            format: fmt,
            count,
            zenoh_connect,
        } => {
            let mut zenoh_config = zenoh::Config::default();
            if let Some(connect) = &zenoh_connect {
                zenoh_config
                    .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
                    .map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}"))
                    })?;
            }

            let session = zenoh::open(zenoh_config).await.map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
            })?;

            let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
            let subscriber = session.declare_subscriber(&topic).await.map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("subscribe to '{topic}': {e}"))
            })?;

            eprintln!("Subscribing to metrics for lab '{lab}'... (Ctrl-C to stop)");

            let mut samples = 0usize;
            loop {
                tokio::select! {
                    Ok(sample) = subscriber.recv_async() => {
                        let payload = sample.payload().to_bytes();
                        if let Ok(snapshot) = serde_json::from_slice::<nlink_lab_shared::metrics::MetricsSnapshot>(&payload) {
                            samples += 1;

                            if fmt == "json" {
                                println!("{}", serde_json::to_string(&snapshot).unwrap_or_default());
                            } else {
                                // Clear screen for table mode
                                print!("\x1B[2J\x1B[H");
                                println!(
                                    "lab: {}  |  nodes: {}  |  sample: #{}",
                                    snapshot.lab_name,
                                    snapshot.nodes.len(),
                                    samples,
                                );
                                println!();
                                println!(
                                    "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>8}",
                                    "NODE", "IFACE", "STATE", "RX rate", "TX rate", "ERRORS", "DROPS"
                                );
                                println!("{}", "─".repeat(78));

                                let mut node_names: Vec<&String> = snapshot.nodes.keys().collect();
                                node_names.sort();
                                for node_name in node_names {
                                    if let Some(filter) = &node
                                        && node_name != filter { continue; }
                                    let metrics = &snapshot.nodes[node_name];
                                    for iface in &metrics.interfaces {
                                        let errors = iface.rx_errors + iface.tx_errors;
                                        let drops = iface.rx_dropped + iface.tx_dropped + iface.tc_drops;
                                        let drop_warn = if drops > 0 { " !" } else { "" };
                                        println!(
                                            "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>7}{}",
                                            node_name,
                                            iface.name,
                                            iface.state,
                                            nlink_lab_shared::metrics::format_rate(iface.rx_bps),
                                            nlink_lab_shared::metrics::format_rate(iface.tx_bps),
                                            errors,
                                            drops,
                                            drop_warn,
                                        );
                                    }
                                    for issue in &metrics.issues {
                                        println!("  [WARN] {node_name}: {issue}");
                                    }
                                }
                            }

                            if let Some(max) = count
                                && samples >= max {
                                    break;
                                }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            }
            Ok(())
        }

        Commands::Init {
            template,
            list,
            output,
            format: _,
            name,
            force,
        } => {
            if list || template.is_none() {
                println!(
                    "{:<15} {:<5} {:<5} DESCRIPTION",
                    "TEMPLATE", "NODES", "LINKS"
                );
                println!("{}", "─".repeat(70));
                for t in nlink_lab::templates::list() {
                    println!(
                        "{:<15} {:<5} {:<5} {}",
                        t.name, t.node_count, t.link_count, t.description
                    );
                }
                return Ok(());
            }

            let template_name = template.unwrap();
            let t = nlink_lab::templates::get(&template_name).ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "unknown template '{template_name}'. Use --list to see available templates"
                ))
            })?;

            let nll_content = nlink_lab::templates::render(t, name.as_deref());
            let out_dir = output.unwrap_or_else(|| PathBuf::from("."));
            let lab_name = name.as_deref().unwrap_or(t.name);

            let path = out_dir.join(format!("{lab_name}.nll"));
            if path.exists() && !force {
                return Err(nlink_lab::Error::AlreadyExists {
                    name: format!("{} (use --force to overwrite)", path.display()),
                });
            }
            std::fs::write(&path, &nll_content)?;
            println!(
                "Created {} ({} nodes, {} links)",
                path.display(),
                t.node_count,
                t.link_count
            );

            Ok(())
        }

        Commands::Wait { name, timeout } => {
            let start = Instant::now();
            let deadline = start + std::time::Duration::from_secs(timeout);
            eprint!("Waiting for lab '{name}'...");
            loop {
                if nlink_lab::state::exists(&name) {
                    eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    eprintln!(" timeout after {timeout}s");
                    return Err(nlink_lab::Error::invalid_topology(format!(
                        "timeout waiting for lab '{name}' after {timeout}s"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        Commands::WaitFor {
            lab,
            node,
            tcp,
            exec_cmd,
            file,
            timeout,
            interval,
        } => {
            check_root();
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
                    if !cli.quiet {
                        eprintln!("ready");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }

        Commands::Ip {
            lab,
            node,
            iface,
            cidr,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let addrs = running.node_addresses(&node)?;

            if let Some(ref iface_name) = iface {
                let iface_addrs = addrs.get(iface_name).ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "interface '{iface_name}' not found on node '{node}'"
                    ))
                })?;

                if json {
                    println!("{}", serde_json::to_string_pretty(&iface_addrs)?);
                } else if let Some(first) = iface_addrs.first() {
                    if cidr {
                        println!("{first}");
                    } else {
                        println!("{}", first.split('/').next().unwrap_or(first));
                    }
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&addrs)?);
            } else {
                for (iface_name, iface_addrs) in &addrs {
                    for addr in iface_addrs {
                        if cidr {
                            println!("{iface_name}: {addr}");
                        } else {
                            println!("{iface_name}: {}", addr.split('/').next().unwrap_or(addr));
                        }
                    }
                }
            }
            Ok(())
        }

        Commands::Inspect { lab } => {
            // If the argument looks like a `.nlz` archive path, do
            // archive inspection instead of lab inspection.
            let lab_path = std::path::Path::new(&lab);
            if lab.ends_with(".nlz") || (lab_path.exists() && !nlink_lab::state::exists(&lab)) {
                use nlink_lab::portability::inspect_archive;
                let summary = inspect_archive(lab_path)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                } else {
                    let m = &summary.manifest;
                    println!("Archive:       {}", lab_path.display());
                    println!("Lab:           {}", m.lab_name);
                    println!("Format:        v{}", m.format_version);
                    println!("Exported by:   {} on {}", m.exported_by, m.exported_at);
                    println!(
                        "Platform:      {} {} / {}",
                        m.platform.os, m.platform.kernel, m.platform.arch,
                    );
                    println!("State:         {:?}", m.deploy_state);
                    if let Some(n) = summary.node_count {
                        println!("Nodes:         {n}");
                    }
                    if let Some(n) = summary.link_count {
                        println!("Links:         {n}");
                    }
                    if let Some(n) = summary.network_count {
                        println!("Networks:      {n}");
                    }
                    println!("Files:");
                    println!("  topology:    {}", m.files.topology);
                    if let Some(f) = &m.files.params {
                        println!("  params:      {f}");
                    }
                    if let Some(f) = &m.files.rendered {
                        println!("  rendered:    {f}");
                    }
                    if let Some(f) = &m.files.state {
                        println!("  state:       {f}");
                    }
                }
                return Ok(());
            }

            let running = nlink_lab::RunningLab::load(&lab)?;
            let topo = running.topology();

            if json {
                println!("{}", serde_json::to_string_pretty(topo)?);
                return Ok(());
            }

            // Header
            println!("{}", bold(&format!("Lab: {}", running.name())));
            println!(
                "Nodes: {}  Links: {}  Impairments: {}",
                running.namespace_count(),
                topo.links.len(),
                topo.impairments.len()
            );

            // Node table
            println!(
                "\n  {:<20} {:<12} {}",
                bold("NODE"),
                bold("TYPE"),
                bold("IMAGE")
            );
            let mut names: Vec<&String> = topo.nodes.keys().collect();
            names.sort();
            for name in &names {
                let node = &topo.nodes[*name];
                let kind = if node.image.is_some() {
                    "container"
                } else {
                    "namespace"
                };
                let image = node.image.as_deref().unwrap_or("--");
                println!("  {:<20} {:<12} {}", name, kind, image);
            }

            // Links
            if !topo.links.is_empty() {
                println!("\n  {:<40} {}", bold("LINK"), bold("ADDRESSES"));
                for link in &topo.links {
                    let addrs = link
                        .addresses
                        .as_ref()
                        .map(|a| format!("{} -- {}", a[0], a[1]))
                        .unwrap_or_else(|| "--".to_string());
                    println!(
                        "  {:<40} {}",
                        format!("{} -- {}", link.endpoints[0], link.endpoints[1]),
                        addrs
                    );
                }
            }

            // Impairments
            if !topo.impairments.is_empty() {
                println!("\n  {}", bold("IMPAIRMENTS"));
                for (ep, imp) in &topo.impairments {
                    let mut parts = Vec::new();
                    if let Some(d) = &imp.delay {
                        parts.push(format!("delay={d}"));
                    }
                    if let Some(j) = &imp.jitter {
                        parts.push(format!("jitter={j}"));
                    }
                    if let Some(l) = &imp.loss {
                        parts.push(format!("loss={l}"));
                    }
                    if let Some(r) = &imp.rate {
                        parts.push(format!("rate={r}"));
                    }
                    println!("  {:<24} {}", ep, parts.join("  "));
                }
            }

            // Processes
            let procs: Vec<_> = running
                .process_status()
                .into_iter()
                .filter(|p| p.alive)
                .collect();
            if !procs.is_empty() {
                println!("\n  {}", bold("PROCESSES"));
                for p in &procs {
                    println!("  {:<16} pid={}", p.node, p.pid);
                }
            }

            Ok(())
        }

        Commands::Containers { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if containers.is_empty() {
                println!("No container nodes in lab '{lab}'.");
            } else if json {
                let data: Vec<serde_json::Value> = containers.iter().map(|(name, state)| {
                    serde_json::json!({ "node": name, "image": state.image, "id": state.id, "pid": state.pid })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                println!(
                    "  {:<16} {:<20} {:<14} PID",
                    "NODE", "IMAGE", "CONTAINER ID"
                );
                let mut entries: Vec<_> = containers.iter().collect();
                entries.sort_by_key(|(name, _)| (*name).clone());
                for (name, state) in entries {
                    let short_id = if state.id.len() > 12 {
                        &state.id[..12]
                    } else {
                        &state.id
                    };
                    println!(
                        "  {:<16} {:<20} {:<14} {}",
                        name, state.image, short_id, state.pid
                    );
                }
            }
            Ok(())
        }

        Commands::Logs {
            lab,
            node,
            pid,
            stderr,
            follow,
            tail,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;

            // Process logs mode (--pid)
            if let Some(pid) = pid {
                let (stdout_path, stderr_path) = running.log_paths(pid).ok_or_else(|| {
                    nlink_lab::Error::deploy_failed(format!("no log files found for PID {pid}"))
                })?;
                let path = if stderr { stderr_path } else { stdout_path };
                let content = std::fs::read_to_string(path).map_err(|e| {
                    nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
                })?;
                let initial: String = if let Some(n) = tail {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = lines.len().saturating_sub(n as usize);
                    lines[start..].join("\n")
                } else {
                    content.clone()
                };
                if !initial.is_empty() {
                    print!("{initial}");
                    if !initial.ends_with('\n') {
                        println!();
                    }
                }
                if follow {
                    // tail -F semantics: resume reading from current EOF,
                    // poll, and reopen if the file is rotated/truncated.
                    tail_follow(std::path::Path::new(path), content.len() as u64)?;
                }
                return Ok(());
            }

            // Container logs mode (node name)
            let node = node.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("either a node name or --pid is required")
            })?;
            let container = running.container_for(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' is not a container. Logs are only available for container nodes."
                ))
            })?;
            let rt = running.runtime_binary().unwrap_or("docker");
            let mut args = vec!["logs".to_string()];
            if follow {
                args.push("--follow".to_string());
            }
            if let Some(n) = tail {
                args.push("--tail".to_string());
                args.push(n.to_string());
            }
            args.push(container.id.clone());
            let status = std::process::Command::new(rt)
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("logs failed: {e}")))?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }

        Commands::Pull { topology } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            let images: std::collections::BTreeSet<&str> = topo
                .nodes
                .values()
                .filter_map(|n| n.image.as_deref())
                .collect();
            if images.is_empty() {
                println!("No container images in topology.");
            } else {
                let rt = nlink_lab::container::Runtime::detect()?;
                for image in &images {
                    eprint!("Pulling {image}...");
                    rt.pull_image(image)?;
                    eprintln!(" done");
                }
                println!("{} image(s) pulled", images.len());
            }
            Ok(())
        }

        Commands::Stats { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if containers.is_empty() {
                println!("No container nodes in lab '{lab}'.");
            } else {
                let rt = running.runtime_binary().unwrap_or("docker");
                let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
                let output = std::process::Command::new(rt)
                    .args([
                        "stats",
                        "--no-stream",
                        "--format",
                        "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}",
                    ])
                    .args(&ids)
                    .output()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("stats failed: {e}")))?;
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            Ok(())
        }

        Commands::Restart { lab, node } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            let container = running.container_for(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' is not a container. Restart is only available for container nodes."
                ))
            })?;
            let rt = running.runtime_binary().unwrap_or("docker");
            eprint!("Restarting '{node}'...");
            let status = std::process::Command::new(rt)
                .args(["restart", &container.id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("restart failed: {e}")))?;
            if status.success() {
                eprintln!(" done");
            } else {
                eprintln!(" failed");
                std::process::exit(1);
            }
            Ok(())
        }

        Commands::Completions { .. } => {
            // Already handled before async runtime
            Ok(())
        }
    }
}

async fn run_daemon_inline(lab: &nlink_lab::RunningLab) -> nlink_lab::Result<()> {
    use nlink_lab_shared::{messages::*, topics};
    use std::time::Duration;

    let zenoh_config = zenoh::Config::default();
    let session = zenoh::open(zenoh_config).await.map_err(|e| {
        nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
    })?;

    let lab_name = lab.name().to_string();
    let start_time = Instant::now();

    let topo_publisher = session
        .declare_publisher(topics::topology(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;
    let health_publisher = session
        .declare_publisher(topics::health(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;
    let snapshot_publisher = session
        .declare_publisher(topics::metrics_snapshot(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publisher: {e}")))?;

    let exec_queryable = session
        .declare_queryable(topics::rpc_exec(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("queryable: {e}")))?;
    let status_queryable = session
        .declare_queryable(topics::rpc_status(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("queryable: {e}")))?;

    // Publish initial topology
    let topo_json = serde_json::to_string(lab.topology())?;
    let topo_update = TopologyUpdate {
        lab_name: lab_name.clone(),
        timestamp: now_unix(),
        node_count: lab.topology().nodes.len(),
        link_count: lab.topology().links.len(),
        topology_json: topo_json,
    };
    topo_publisher
        .put(serde_json::to_vec(&topo_update).unwrap())
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("publish: {e}")))?;

    let _token = session
        .liveliness()
        .declare_token(topics::health(&lab_name))
        .await
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("liveliness: {e}")))?;

    eprintln!("Backend daemon running (Ctrl-C to stop)");

    let mut health_interval = tokio::time::interval(Duration::from_secs(10));
    let mut metrics_interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                if let Ok(diags) = lab.diagnose(None).await {
                    let snapshot = diags_to_snapshot(&lab_name, &diags);
                    // Per-interface metrics
                    for (node_name, node_metrics) in &snapshot.nodes {
                        for iface in &node_metrics.interfaces {
                            let topic = topics::metrics_iface(&lab_name, node_name, &iface.name);
                            if let Ok(json) = serde_json::to_vec(iface) {
                                let _ = session.put(&topic, json).await;
                            }
                        }
                    }
                    // Full snapshot
                    if let Ok(json) = serde_json::to_vec(&snapshot) {
                        let _ = snapshot_publisher.put(json).await;
                    }
                }
            }
            _ = health_interval.tick() => {
                let status = HealthStatus {
                    lab_name: lab_name.clone(),
                    timestamp: now_unix(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: 0,
                    pid_count: lab.process_status().len(),
                    uptime_secs: start_time.elapsed().as_secs(),
                };
                if let Ok(json) = serde_json::to_vec(&status) {
                    let _ = health_publisher.put(json).await;
                }
            }
            Ok(query) = exec_queryable.recv_async() => {
                if let Some(payload) = query.payload()
                    && let Ok(req) = serde_json::from_slice::<ExecRequest>(&payload.to_bytes()) {
                        let args: Vec<&str> = req.args.iter().map(|s| s.as_str()).collect();
                        let resp = match lab.exec(&req.node, &req.cmd, &args) {
                            Ok(output) => ExecResponse {
                                success: output.exit_code == 0,
                                exit_code: output.exit_code,
                                stdout: output.stdout,
                                stderr: output.stderr,
                            },
                            Err(e) => ExecResponse {
                                success: false,
                                exit_code: -1,
                                stdout: String::new(),
                                stderr: e.to_string(),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&resp) {
                            let _ = query.reply(topics::rpc_exec(&lab_name), json).await;
                        }
                    }
            }
            Ok(query) = status_queryable.recv_async() => {
                let resp = StatusResponse {
                    lab_name: lab_name.clone(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: 0,
                    uptime_secs: start_time.elapsed().as_secs(),
                    nodes: lab.node_names().map(|s| s.to_string()).collect(),
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = query.reply(topics::rpc_status(&lab_name), json).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nShutting down daemon");
                break;
            }
        }
    }

    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn diags_to_snapshot(
    lab_name: &str,
    diags: &[nlink_lab::NodeDiagnostic],
) -> nlink_lab_shared::metrics::MetricsSnapshot {
    use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};
    let mut nodes = std::collections::HashMap::new();
    for diag in diags {
        let iface_metrics: Vec<InterfaceMetrics> = diag
            .interfaces
            .iter()
            .map(|iface| InterfaceMetrics {
                name: iface.name.clone(),
                state: format!("{:?}", iface.state),
                rx_bps: iface.rates.rx_bps,
                tx_bps: iface.rates.tx_bps,
                rx_pps: iface.rates.rx_pps,
                tx_pps: iface.rates.tx_pps,
                rx_errors: iface.stats.rx_errors(),
                tx_errors: iface.stats.tx_errors(),
                rx_dropped: iface.stats.rx_dropped(),
                tx_dropped: iface.stats.tx_dropped(),
                tc_drops: iface.tc.as_ref().map_or(0, |tc| tc.drops),
                tc_qlen: iface.tc.as_ref().map_or(0, |tc| tc.qlen),
            })
            .collect();
        let issues: Vec<String> = diag.issues.iter().map(|i| i.to_string()).collect();
        nodes.insert(
            diag.node.clone(),
            NodeMetrics {
                interfaces: iface_metrics,
                issues,
            },
        );
    }
    MetricsSnapshot {
        lab_name: lab_name.to_string(),
        timestamp: now_unix(),
        nodes,
    }
}

fn print_topology_summary(topo: &nlink_lab::Topology) {
    println!("  Nodes:       {}", topo.nodes.len());
    println!("  Links:       {}", topo.links.len());
    println!("  Profiles:    {}", topo.profiles.len());
    println!("  Networks:    {}", topo.networks.len());
    println!("  Impairments: {}", topo.impairments.len());
    println!("  Rate limits: {}", topo.rate_limits.len());
}

fn print_deploy_summary(topo: &nlink_lab::Topology) {
    let node_names: Vec<&str> = topo.nodes.keys().map(|s| s.as_str()).collect();
    println!("  Nodes:       {}", node_names.join(", "));
    println!("  Links:       {} point-to-point", topo.links.len());
    if !topo.impairments.is_empty() {
        println!("  Impairments: {}", topo.impairments.len());
    }
    let bg_count: usize = topo
        .nodes
        .values()
        .flat_map(|n| &n.exec)
        .filter(|e| e.background)
        .count();
    if bg_count > 0 {
        println!("  Processes:   {} background", bg_count);
    }
}

fn topology_to_dot(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;

    let mut out = format!("graph {:?} {{\n", topo.lab.name);
    out += "  rankdir=LR;\n";
    out += "  node [shape=box];\n";

    for link in &topo.links {
        let a = EndpointRef::parse(&link.endpoints[0]).unwrap();
        let b = EndpointRef::parse(&link.endpoints[1]).unwrap();

        let mut label_parts = Vec::new();
        if let Some(addrs) = &link.addresses {
            label_parts.push(format!("{} / {}", addrs[0], addrs[1]));
        }
        if let Some(mtu) = link.mtu {
            label_parts.push(format!("MTU {mtu}"));
        }
        // Check for impairment
        if let Some(imp) = topo.impairments.get(&link.endpoints[0]) {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss={l}"));
            }
            if !parts.is_empty() {
                label_parts.push(parts.join(" "));
            }
        }

        let label = label_parts.join("\\n");
        if label.is_empty() {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface
            );
        } else {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\", label=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface, label
            );
        }
    }

    out += "}\n";
    out
}

fn topology_to_ascii(topo: &nlink_lab::Topology) -> String {
    use std::collections::HashSet;

    let mut out = String::new();
    out.push_str(&format!("Lab: {}\n", topo.lab.name));
    if let Some(desc) = &topo.lab.description {
        out.push_str(&format!("  {desc}\n"));
    }
    out.push('\n');

    out.push_str("Nodes:\n");
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    for name in &nodes {
        let node = &topo.nodes[*name];
        let kind = if node.image.is_some() {
            " [container]"
        } else {
            ""
        };
        out.push_str(&format!("  {name}{kind}\n"));
    }

    out.push_str("\nLinks:\n");
    let mut shown: HashSet<String> = HashSet::new();
    for link in &topo.links {
        let key = format!("{} -- {}", link.endpoints[0], link.endpoints[1]);
        if shown.insert(key.clone()) {
            let mut parts = vec![format!("  {}", key)];
            if let Some(addrs) = &link.addresses {
                parts.push(format!("{} -- {}", addrs[0], addrs[1]));
            }
            if let Some(mtu) = link.mtu {
                parts.push(format!("mtu={mtu}"));
            }
            out.push_str(&format!("{}\n", parts.join("  ")));
        }
    }

    if !topo.assertions.is_empty() {
        out.push_str("\nAssertions:\n");
        for a in &topo.assertions {
            match a {
                nlink_lab::types::Assertion::Reach { from, to } => {
                    out.push_str(&format!("  reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::NoReach { from, to } => {
                    out.push_str(&format!("  no-reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::TcpConnect {
                    from,
                    to,
                    port,
                    timeout,
                    retries,
                    interval,
                } => {
                    let t = timeout
                        .as_deref()
                        .map(|t| format!(" timeout {t}"))
                        .unwrap_or_default();
                    let r = retries.map(|r| format!(" retries {r}")).unwrap_or_default();
                    let i = interval
                        .as_deref()
                        .map(|i| format!(" interval {i}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  tcp-connect {from} -> {to}:{port}{t}{r}{i}\n"));
                }
                nlink_lab::types::Assertion::LatencyUnder {
                    from,
                    to,
                    max,
                    samples,
                } => {
                    let s = samples.map(|s| format!(" samples {s}")).unwrap_or_default();
                    out.push_str(&format!("  latency-under {from} -> {to} < {max}{s}\n"));
                }
                nlink_lab::types::Assertion::RouteHas {
                    node,
                    destination,
                    via,
                    dev,
                } => {
                    let v = via
                        .as_deref()
                        .map(|v| format!(" via {v}"))
                        .unwrap_or_default();
                    let d = dev
                        .as_deref()
                        .map(|d| format!(" dev {d}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  route-has {node} {destination}{v}{d}\n"));
                }
                nlink_lab::types::Assertion::DnsResolves {
                    from,
                    name,
                    expected_ip,
                } => {
                    out.push_str(&format!("  dns-resolves {from} {name} -> {expected_ip}\n"));
                }
            }
        }
    }

    out
}

fn check_root() {
    if unsafe { libc::geteuid() } != 0 {
        // Check if we have effective capabilities via /proc/self/status
        let has_caps = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find(|l| l.starts_with("CapEff:")).map(|l| {
                    let hex = l.split_whitespace().nth(1).unwrap_or("0");
                    u64::from_str_radix(hex, 16).unwrap_or(0) != 0
                })
            })
            .unwrap_or(false);
        if !has_caps {
            eprintln!(
                "warning: nlink-lab requires root, SUID, or capabilities (CAP_NET_ADMIN+CAP_SYS_ADMIN)"
            );
        }
    }
}

/// Best-effort cleanup when state is missing: delete namespaces matching the lab prefix.
async fn force_cleanup(name: &str) {
    // Try to list and delete namespaces matching the lab prefix.
    // Use ip netns since we don't have direct nlink dependency in the CLI.
    let prefix = format!("{name}-");
    if let Ok(output) = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let ns_name = line.split_whitespace().next().unwrap_or("");
            if ns_name.starts_with(&prefix) {
                let result = std::process::Command::new("ip")
                    .args(["netns", "delete", ns_name])
                    .status();
                match result {
                    Ok(s) if s.success() => eprintln!("  deleted namespace '{ns_name}'"),
                    _ => eprintln!("  warning: failed to delete namespace '{ns_name}'"),
                }
            }
        }
    }

    // Clean up root-namespace mgmt veth peers first (may be orphaned if bridge
    // was already deleted or namespaces were deleted before the bridge).
    // Veth peers are named nm{hash6}{idx} where hash is from mgmt_bridge_name.
    let bridge_name = nlink_lab::mgmt_bridge_name_for(name);
    // Veth peers are named nm{hash8}{idx} — same hash as bridge (strip "nl" prefix)
    let veth_prefix = format!("nm{}", &bridge_name[2..]);
    if let Ok(output) = std::process::Command::new("ip")
        .args(["-o", "link", "show"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(ifname) = line.split(':').nth(1).map(|s| s.trim()) {
                let ifname = ifname.split('@').next().unwrap_or(ifname);
                if ifname.starts_with(veth_prefix.as_str()) {
                    let _ = std::process::Command::new("ip")
                        .args(["link", "delete", ifname])
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
        }
    }

    // Clean up root-namespace management bridge.
    let result = std::process::Command::new("ip")
        .args(["link", "delete", &bridge_name])
        .stderr(std::process::Stdio::null())
        .status();
    if let Ok(s) = result
        && s.success()
    {
        eprintln!("  deleted mgmt bridge '{bridge_name}'");
    }

    // Also clean up state directory
    let _ = nlink_lab::state::remove(name);
}

/// Resources on the host that look like lab-owned state but have no matching
/// `state.json` — usually left behind by a crashed deploy.
#[derive(Debug, Default, serde::Serialize)]
struct Orphans {
    /// Root-namespace mgmt bridges (`nl{hash8}`).
    bridges: Vec<String>,
    /// Root-namespace mgmt veth peers (`nm{hash8}{idx}`).
    veths: Vec<String>,
    /// Named network namespaces whose prefix doesn't match any known lab.
    netns: Vec<String>,
    /// Labs whose state file claims namespaces that no longer exist on the
    /// host — the mirror case of the above (state with no resources). Most
    /// commonly caused by a reboot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stale: Vec<StaleLab>,
}

/// A state-backed lab with one or more namespaces missing from the host.
#[derive(Debug, Clone, serde::Serialize)]
struct StaleLab {
    /// Lab name.
    name: String,
    /// Namespaces claimed by `state.json` that are absent from `ip netns list`.
    missing_namespaces: Vec<String>,
}

impl Orphans {
    fn is_empty(&self) -> bool {
        self.bridges.is_empty()
            && self.veths.is_empty()
            && self.netns.is_empty()
            && self.stale.is_empty()
    }
}

/// Scan the host for lab-owned resources without a matching state file, and
/// state-backed labs whose resources are gone from the host.
///
/// Detection rules for *orphans* (resource with no state):
/// - Interfaces matching `^nl[0-9a-f]{8}$` are mgmt bridges; orphan if the
///   hash doesn't match any known lab's `mgmt_bridge_name_for`.
/// - Interfaces starting with `nm` + 8 hex + digits are mgmt veth peers;
///   orphan if the hash portion doesn't match any known lab.
/// - Named netns whose prefix matches a known lab are skipped; remaining
///   lab-shaped names (containing a hyphen) are reported.
///
/// Detection rule for *stale* (state with no resources): for each known lab,
/// compare the namespaces it claims in `state.json` against the host's
/// current `ip netns list`. Any missing namespace marks the lab stale.
fn find_orphans(known: &[nlink_lab::state::LabInfo]) -> Orphans {
    let ifnames: Vec<String> = list_ip_links();
    let netns: Vec<String> = list_netns();
    let mut orphans = classify_orphans(&ifnames, &netns, known);

    let lab_namespaces: Vec<(String, Vec<String>)> = known
        .iter()
        .filter_map(|info| {
            nlink_lab::state::load_namespace_names(&info.name)
                .ok()
                .map(|ns| (info.name.clone(), ns))
        })
        .collect();
    orphans.stale = classify_stale(&lab_namespaces, &netns);
    orphans
}

/// Pure stale-lab classifier.
///
/// For each `(lab_name, claimed_namespaces)` pair, return a [`StaleLab`] if
/// any claimed namespace is absent from `netns_present`. Labs with all
/// namespaces present are omitted. Claimed namespaces are deduplicated and
/// sorted in the output so results are stable for tests.
fn classify_stale(labs: &[(String, Vec<String>)], netns_present: &[String]) -> Vec<StaleLab> {
    let present: std::collections::HashSet<&str> =
        netns_present.iter().map(|s| s.as_str()).collect();
    let mut out = Vec::new();
    for (name, claimed) in labs {
        let mut missing: Vec<String> = claimed
            .iter()
            .filter(|ns| !present.contains(ns.as_str()))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            out.push(StaleLab {
                name: name.clone(),
                missing_namespaces: missing,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn list_ip_links() -> Vec<String> {
    let output = match std::process::Command::new("ip")
        .args(["-o", "link", "show"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            line.split(':')
                .nth(1)
                .map(|s| s.trim().split('@').next().unwrap_or("").to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn list_netns() -> Vec<String> {
    let output = match std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pure classification — given host state and known labs, emit orphans.
fn classify_orphans(
    ifnames: &[String],
    netns: &[String],
    known: &[nlink_lab::state::LabInfo],
) -> Orphans {
    let mut known_bridges: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut known_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut known_prefixes: Vec<String> = Vec::new();
    for info in known {
        let bridge = nlink_lab::mgmt_bridge_name_for(&info.name);
        if bridge.len() > 2 {
            known_hashes.insert(bridge[2..].to_string());
        }
        known_bridges.insert(bridge);
        known_prefixes.push(format!("{}-", info.name));
    }

    let mut orphans = Orphans::default();
    for ifname in ifnames {
        // Mgmt bridge: `nl` + 8 hex chars, total 10.
        if ifname.len() == 10
            && ifname.starts_with("nl")
            && ifname[2..].chars().all(|c| c.is_ascii_hexdigit())
            && !known_bridges.contains(ifname)
        {
            orphans.bridges.push(ifname.clone());
            continue;
        }
        // Mgmt veth peer: `nm` + 8 hex + 1+ digits.
        if ifname.len() >= 11
            && ifname.starts_with("nm")
            && ifname[2..10].chars().all(|c| c.is_ascii_hexdigit())
            && ifname[10..].chars().all(|c| c.is_ascii_digit())
            && !known_hashes.contains(&ifname[2..10])
        {
            orphans.veths.push(ifname.clone());
        }
    }

    for ns in netns {
        if known_prefixes.iter().any(|p| ns.starts_with(p.as_str())) {
            continue;
        }
        if ns.contains('-') {
            orphans.netns.push(ns.clone());
        }
    }

    orphans
}

/// Best-effort cleanup of orphan resources found by [`find_orphans`].
async fn reap_orphans(known: &[nlink_lab::state::LabInfo]) {
    let orphans = find_orphans(known);
    if orphans.is_empty() {
        println!("No orphans detected.");
        return;
    }
    // Netns first: deleting a namespace reaps the veths inside it.
    for ns in &orphans.netns {
        let r = std::process::Command::new("ip")
            .args(["netns", "delete", ns])
            .status();
        match r {
            Ok(s) if s.success() => println!("  deleted namespace '{ns}'"),
            _ => eprintln!("  warning: failed to delete namespace '{ns}'"),
        }
    }
    for v in &orphans.veths {
        let _ = std::process::Command::new("ip")
            .args(["link", "delete", v])
            .stderr(std::process::Stdio::null())
            .status();
        println!("  deleted veth '{v}'");
    }
    for b in &orphans.bridges {
        let r = std::process::Command::new("ip")
            .args(["link", "delete", b])
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(s) = r
            && s.success()
        {
            println!("  deleted mgmt bridge '{b}'");
        }
    }
}

/// Follow `path` from `start_offset`, writing each new chunk to `out`
/// until `should_continue()` returns false or an I/O error occurs.
/// Handles file truncation/rotation by reopening from offset 0 when the
/// file shrinks below the last-read position.
///
/// Production callers use `|| true` for `should_continue` and exit on
/// SIGINT (Ctrl-C terminates the process as usual). Tests can pass a
/// closure that stops after a deterministic number of iterations.
fn tail_follow_to<W: std::io::Write>(
    path: &std::path::Path,
    start_offset: u64,
    out: &mut W,
    should_continue: impl Fn() -> bool,
) -> nlink_lab::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("failed to open log file: {e}")))?;
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("seek on log file: {e}")))?;
    let mut pos = start_offset;
    let mut buf = [0u8; 8192];
    while should_continue() {
        match file.read(&mut buf) {
            Ok(0) => {
                let meta = std::fs::metadata(path).ok();
                if let Some(m) = meta
                    && m.len() < pos
                {
                    file = std::fs::File::open(path).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("reopen log file: {e}"))
                    })?;
                    pos = 0;
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(n) => {
                out.write_all(&buf[..n]).ok();
                out.flush().ok();
                pos += n as u64;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "read from log file: {e}"
                )));
            }
        }
    }
    Ok(())
}

/// Wrapper used by the CLI: runs forever (until Ctrl-C) and writes to
/// stdout.
fn tail_follow(path: &std::path::Path, start_offset: u64) -> nlink_lab::Result<()> {
    let mut stdout = std::io::stdout();
    tail_follow_to(path, start_offset, &mut stdout, || true)
}

/// Build the argv to pass to `nsenter` for entering a lab node's network
/// namespace and exec'ing a shell.
///
/// Must emit `--net=<path>` as a single argument; splitting it into two
/// (`--net`, `<path>`) makes nsenter treat `--net` as "enter target's netns"
/// and then look for a target it never got, failing with
/// "neither filename nor target pid supplied for ns/net".
fn nsenter_shell_args(ns: &str, shell: &str) -> Vec<String> {
    vec![
        format!("--net=/var/run/netns/{ns}"),
        "--".into(),
        shell.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab::state::LabInfo;

    /// Each JSON Schema under `docs/json-schemas/` must be valid JSON.
    /// Catches accidental hand-edit corruption (trailing comma, etc.) at
    /// CI time — we don't validate the schema language itself, just
    /// parseability. Keep the file list in sync when adding schemas.
    #[test]
    fn json_schemas_parse() {
        let schemas = [
            include_str!("../../../docs/json-schemas/deploy.schema.json"),
            include_str!("../../../docs/json-schemas/status-list.schema.json"),
            include_str!("../../../docs/json-schemas/status-scan.schema.json"),
            include_str!("../../../docs/json-schemas/spawn.schema.json"),
            include_str!("../../../docs/json-schemas/ps.schema.json"),
        ];
        for s in schemas {
            let _: serde_json::Value = serde_json::from_str(s)
                .expect("JSON Schema file failed to parse — see file list above");
        }
    }

    #[test]
    fn nsenter_shell_args_uses_equals_form() {
        let args = nsenter_shell_args("mylab-router", "/bin/bash");
        assert_eq!(
            args,
            vec![
                "--net=/var/run/netns/mylab-router".to_string(),
                "--".to_string(),
                "/bin/bash".to_string(),
            ]
        );
        // Guard against the split `--net` regression: no argument may be
        // exactly `--net`, which nsenter interprets as the flag alone.
        assert!(
            !args.iter().any(|a| a == "--net"),
            "bare --net would be misparsed by nsenter"
        );
    }

    fn info(name: &str) -> LabInfo {
        LabInfo {
            name: name.to_string(),
            node_count: 0,
            created_at: String::new(),
        }
    }

    #[test]
    fn classify_orphans_reports_unknown_mgmt_bridge() {
        let known = vec![info("keep")];
        let keep_bridge = nlink_lab::mgmt_bridge_name_for("keep");
        let orphan_bridge = nlink_lab::mgmt_bridge_name_for("gone");
        let ifnames = vec![keep_bridge.clone(), orphan_bridge.clone(), "eth0".into()];
        let orphans = classify_orphans(&ifnames, &[], &known);
        assert_eq!(orphans.bridges, vec![orphan_bridge]);
        assert!(orphans.veths.is_empty());
    }

    #[test]
    fn classify_orphans_skips_known_mgmt_veths() {
        let known = vec![info("keep")];
        let keep_hash = &nlink_lab::mgmt_bridge_name_for("keep")[2..];
        let gone_hash = &nlink_lab::mgmt_bridge_name_for("gone")[2..];
        let ifnames = vec![
            format!("nm{keep_hash}0"),
            format!("nm{gone_hash}0"),
            format!("nm{gone_hash}42"),
            "lo".into(),
            "eth0".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &known);
        assert_eq!(orphans.veths.len(), 2);
        assert!(orphans.veths.iter().all(|v| v.contains(gone_hash)));
    }

    #[test]
    fn classify_orphans_ignores_system_netns() {
        // Bare system netns names (no hyphen) are never flagged.
        let orphans = classify_orphans(&[], &["default".into(), "init".into()], &[]);
        assert!(orphans.netns.is_empty());
    }

    #[test]
    fn classify_orphans_reports_unknown_netns() {
        let known = vec![info("keep")];
        let netns = vec![
            "keep-router".into(),
            "keep-mgmt".into(),
            "stale-mgmt".into(),
            "stale-node1".into(),
        ];
        let orphans = classify_orphans(&[], &netns, &known);
        assert_eq!(orphans.netns.len(), 2);
        assert!(orphans.netns.contains(&"stale-mgmt".to_string()));
        assert!(orphans.netns.contains(&"stale-node1".to_string()));
    }

    #[test]
    fn tail_follow_reads_appended_data() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"initial\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Append after a short delay from a background thread.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_w)
                .unwrap();
            f.write_all(b"appended\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            // Stop after roughly 1 second of polling (4×250ms sleeps).
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 6
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("appended"),
            "expected appended content, got: {captured:?}"
        );
        assert!(
            !captured.contains("initial"),
            "should not re-read data before start_offset: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_follow_handles_truncation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"old content that will be truncated\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Truncate then write fresh content.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::File::create(&path_w).unwrap(); // truncates
            f.write_all(b"fresh\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 8
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("fresh"),
            "expected post-truncation content, got: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_stale_flags_lab_with_missing_namespace() {
        let labs = vec![(
            "des-3m".into(),
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ],
        )];
        // Host sees nothing — classic WSL-restart case.
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "des-3m");
        assert_eq!(
            stale[0].missing_namespaces,
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ]
        );
    }

    #[test]
    fn classify_stale_ignores_healthy_labs() {
        let labs = vec![(
            "healthy".into(),
            vec!["healthy-a".to_string(), "healthy-b".to_string()],
        )];
        let present = vec!["healthy-a".into(), "healthy-b".into()];
        assert!(classify_stale(&labs, &present).is_empty());
    }

    #[test]
    fn classify_stale_reports_partial_loss() {
        let labs = vec![(
            "partial".into(),
            vec!["partial-a".to_string(), "partial-b".to_string()],
        )];
        let present = vec!["partial-a".into()];
        let stale = classify_stale(&labs, &present);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].missing_namespaces, vec!["partial-b".to_string()]);
    }

    #[test]
    fn classify_stale_orders_deterministically() {
        // Two stale labs provided out of alphabetical order — output should
        // be sorted so test assertions and `--json` output are stable.
        let labs = vec![
            ("zebra".into(), vec!["zebra-a".to_string()]),
            ("alpha".into(), vec!["alpha-a".to_string()]),
        ];
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].name, "alpha");
        assert_eq!(stale[1].name, "zebra");
    }

    #[test]
    fn classify_orphans_ignores_non_lab_interfaces() {
        // Random interface names should not trigger detection.
        let ifnames = vec![
            "eth0".into(),
            "docker0".into(),
            "br-abc".into(),
            "wlp3s0".into(),
            // nl-prefixed but wrong length / non-hex — not a mgmt bridge.
            "nlmonitor".into(),
            "nl1234".into(),
            // nm-prefixed but no trailing digits.
            "nmabcdef01".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &[]);
        assert!(
            orphans.bridges.is_empty() && orphans.veths.is_empty(),
            "false positives: {orphans:?}"
        );
    }
}
