use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
struct Cli {
    /// Output JSON instead of human-readable text (for status, diagnose, ps).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy a lab from a topology file (.nll).
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
    },

    /// Apply topology changes to a running lab.
    Apply {
        /// Path to the updated topology file (.nll).
        topology: PathBuf,

        /// Show what would change without applying.
        #[arg(long)]
        dry_run: bool,
    },

    /// Tear down a running lab.
    Destroy {
        /// Lab name.
        name: String,

        /// Continue cleanup even if some resources are already gone.
        #[arg(long)]
        force: bool,
    },

    /// Show running labs or details of a specific lab.
    Status {
        /// Lab name (omit to list all).
        name: Option<String>,
    },

    /// Run a command in a lab node.
    Exec {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// Validate a topology file without deploying.
    Validate {
        /// Path to the topology file (.nll).
        topology: PathBuf,
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
    },

    /// Print topology as DOT graph.
    Graph {
        /// Path to the topology file (.nll).
        topology: PathBuf,
    },

    /// List processes running in a lab.
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

    /// Capture packets on an interface (tcpdump).
    Capture {
        /// Lab name.
        lab: String,

        /// Endpoint (e.g., "router:eth0").
        endpoint: String,

        /// Write to pcap file.
        #[arg(short, long)]
        write: Option<PathBuf>,

        /// Capture N packets then stop.
        #[arg(short, long)]
        count: Option<u32>,

        /// BPF filter expression (e.g., "tcp port 80").
        #[arg(short, long)]
        filter: Option<String>,
    },

    /// Wait for a lab to be ready.
    Wait {
        /// Lab name.
        name: String,

        /// Timeout in seconds (default: 30).
        #[arg(short, long, default_value = "30")]
        timeout: u64,
    },

    /// Compare two topology files and show differences.
    Diff {
        /// First topology file (or lab name with --lab).
        a: PathBuf,

        /// Second topology file.
        b: PathBuf,
    },

    /// Export a running lab's topology as serialized data.
    Export {
        /// Lab name.
        lab: String,

        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env(),
        )
        .init();

    let cli = Cli::parse();

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
            let report = miette::Report::new(diag);
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> nlink_lab::Result<()> {
    let json = cli.json;
    match cli.command {
        Commands::Deploy {
            topology,
            dry_run,
            force,
            daemon,
        } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            let result = topo.validate();

            // Print warnings
            for w in result.warnings() {
                eprintln!("  WARN  {w}");
            }

            if result.has_errors() {
                eprintln!("Validation failed for {:?}:", topo.lab.name);
                for e in result.errors() {
                    eprintln!("  ERROR {e}");
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            if dry_run {
                println!("Topology {:?} is valid", topo.lab.name);
                print_topology_summary(&topo);
                return Ok(());
            }

            // Handle --force: destroy existing lab first
            if force && nlink_lab::state::exists(&topo.lab.name) {
                let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
                lab.destroy().await?;
            }

            check_root();

            let start = Instant::now();
            let lab = topo.deploy().await?;
            let elapsed = start.elapsed();

            println!(
                "Lab {:?} deployed in {:.0?}",
                topo.lab.name, elapsed
            );
            print_deploy_summary(&topo);

            if daemon {
                run_daemon_inline(&lab).await?;
            }
            Ok(())
        }

        Commands::Apply { topology, dry_run } => {
            let desired = nlink_lab::parser::parse_file(&topology)?;
            let result = desired.validate();
            for w in result.warnings() {
                eprintln!("  WARN  {w}");
            }
            if result.has_errors() {
                for e in result.errors() {
                    eprintln!("  ERROR {e}");
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

            if diff.is_empty() {
                println!("No changes to apply.");
                return Ok(());
            }

            println!("Changes for lab '{lab_name}':");
            print!("{diff}");
            println!("{} change(s)", diff.change_count());

            if dry_run {
                println!("\n(dry run — no changes applied)");
                return Ok(());
            }

            check_root();
            let start = Instant::now();
            nlink_lab::apply_diff(&mut running, &desired, &diff).await?;
            let elapsed = start.elapsed();

            println!("\nApplied {} change(s) in {:.0?}", diff.change_count(), elapsed);
            Ok(())
        }

        Commands::Destroy { name, force } => {
            check_root();
            match nlink_lab::RunningLab::load(&name) {
                Ok(lab) => {
                    let node_count = lab.namespace_count();
                    lab.destroy().await?;
                    println!("Lab {name:?} destroyed ({node_count} namespaces removed)");
                }
                Err(e) if force => {
                    // Force cleanup: try to delete namespaces by prefix pattern
                    eprintln!("warning: state not found, attempting force cleanup: {e}");
                    force_cleanup(&name).await;
                    println!("Lab {name:?} force-cleaned");
                }
                Err(e) => return Err(e),
            }
            Ok(())
        }

        Commands::Status { name } => match name {
            None => {
                let labs = nlink_lab::RunningLab::list()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&labs)?);
                } else if labs.is_empty() {
                    println!("No running labs.");
                } else {
                    println!("{:<18} {:<6} {}", "NAME", "NODES", "CREATED");
                    for info in labs {
                        println!(
                            "{:<18} {:<6} {}",
                            info.name, info.node_count, info.created_at
                        );
                    }
                }
                Ok(())
            }
            Some(name) => {
                let lab = nlink_lab::RunningLab::load(&name)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(lab.topology())?);
                } else {
                    println!("Lab: {}", lab.name());
                    println!("Nodes: {}", lab.namespace_count());
                    let topo = lab.topology();
                    println!("Links: {}", topo.links.len());
                    println!("Impairments: {}", topo.impairments.len());
                    let node_names: Vec<&str> = lab.node_names().collect();
                    println!("  {}", node_names.join(", "));
                }
                Ok(())
            }
        },

        Commands::Exec { lab, node, cmd } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
            let output = running.exec(&node, &cmd[0], &args)?;

            print!("{}", output.stdout);
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }

            if output.exit_code != 0 {
                std::process::exit(output.exit_code);
            }
            Ok(())
        }

        Commands::Validate { topology } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            let result = topo.validate();

            for w in result.warnings() {
                eprintln!("  WARN  {w}");
            }

            if result.has_errors() {
                eprintln!("Validation failed for {:?}:", topo.lab.name);
                for e in result.errors() {
                    eprintln!("  ERROR {e}");
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            println!("Topology {:?} is valid", topo.lab.name);
            print_topology_summary(&topo);
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
        } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;

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

            if clear {
                running.clear_impairment(&endpoint).await?;
                println!("Cleared impairment on {endpoint}");
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
            Ok(())
        }

        Commands::Graph { topology } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            print!("{}", topology_to_dot(&topo));
            Ok(())
        }

        Commands::Ps { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let procs = running.process_status();
            if json {
                println!("{}", serde_json::to_string_pretty(&procs)?);
            } else if procs.is_empty() {
                println!("No tracked processes.");
            } else {
                println!("{:<12} {:<8} {}", "NODE", "PID", "STATUS");
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
            Ok(())
        }

        Commands::Capture {
            lab,
            endpoint,
            write,
            count,
            filter,
        } => {
            check_root();
            let running = nlink_lab::RunningLab::load(&lab)?;
            let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
                nlink_lab::Error::InvalidEndpoint {
                    endpoint: endpoint.clone(),
                }
            })?;

            let mut args = vec!["-i".to_string(), ep.iface.clone(), "-nn".to_string()];
            if let Some(file) = &write {
                args.push("-w".to_string());
                args.push(file.to_string_lossy().into_owned());
            }
            if let Some(n) = count {
                args.push("-c".to_string());
                args.push(n.to_string());
            }
            if let Some(f) = &filter {
                args.push(f.clone());
            }

            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let output = running.exec(&ep.node, "tcpdump", &arg_refs)?;
            print!("{}", output.stdout);
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            Ok(())
        }

        Commands::Diff { a, b } => {
            let topo_a = nlink_lab::parser::parse_file(&a)?;
            let topo_b = nlink_lab::parser::parse_file(&b)?;
            let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
            if json {
                // For JSON, output a simple summary
                println!("{}", serde_json::json!({
                    "nodes_added": diff.nodes_added,
                    "nodes_removed": diff.nodes_removed,
                    "links_added": diff.links_added.len(),
                    "links_removed": diff.links_removed.len(),
                    "impairments_changed": diff.impairments_changed.len(),
                    "impairments_added": diff.impairments_added.len(),
                    "impairments_removed": diff.impairments_removed.len(),
                    "total_changes": diff.change_count(),
                }));
            } else if diff.is_empty() {
                println!("No differences.");
            } else {
                println!(
                    "Diff: {} → {}",
                    a.display(),
                    b.display()
                );
                print!("{diff}");
                println!(
                    "\n{} change(s)",
                    diff.change_count()
                );
            }
            Ok(())
        }

        Commands::Export { lab, output } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let content = if json {
                serde_json::to_string_pretty(running.topology())?
            } else {
                toml::to_string_pretty(running.topology())
                    .map_err(|e| nlink_lab::Error::invalid_topology(format!("serialize: {e}")))?
            };
            match output {
                Some(path) => {
                    std::fs::write(&path, &content)?;
                    eprintln!("Exported to {}", path.display());
                }
                None => print!("{content}"),
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
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}")))?;
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
                                    if let Some(filter) = &node {
                                        if node_name != filter { continue; }
                                    }
                                    let metrics = &snapshot.nodes[node_name];
                                    for iface in &metrics.interfaces {
                                        let errors = iface.rx_errors + iface.tx_errors;
                                        let drops = iface.rx_dropped + iface.tx_dropped + iface.tc_drops as u64;
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

                            if let Some(max) = count {
                                if samples >= max {
                                    break;
                                }
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
                println!("{:<15} {:<5} {:<5} {}", "TEMPLATE", "NODES", "LINKS", "DESCRIPTION");
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
            let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
            loop {
                if nlink_lab::state::exists(&name) {
                    println!("Lab '{name}' is ready.");
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(nlink_lab::Error::invalid_topology(format!(
                        "timeout waiting for lab '{name}' after {timeout}s"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
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
                if let Some(payload) = query.payload() {
                    if let Ok(req) = serde_json::from_slice::<ExecRequest>(&payload.to_bytes()) {
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
        nodes.insert(diag.node.clone(), NodeMetrics { interfaces: iface_metrics, issues });
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

fn check_root() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("warning: nlink-lab typically requires root or CAP_NET_ADMIN");
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

    // Also clean up state directory
    let _ = nlink_lab::state::remove(name);
}
