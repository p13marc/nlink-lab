use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy a lab from a topology file.
    Deploy {
        /// Path to the topology TOML file.
        topology: PathBuf,

        /// Validate only, don't actually deploy.
        #[arg(long)]
        dry_run: bool,

        /// Destroy existing lab with same name before deploying.
        #[arg(long)]
        force: bool,
    },

    /// Tear down a running lab.
    Destroy {
        /// Lab name.
        name: String,
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
        /// Path to the topology TOML file.
        topology: PathBuf,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env(),
        )
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new().unwrap();
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> nlink_lab::Result<()> {
    match cli.command {
        Commands::Deploy {
            topology,
            dry_run,
            force,
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
            let _lab = topo.deploy().await?;
            let elapsed = start.elapsed();

            println!(
                "Lab {:?} deployed in {:.0?}",
                topo.lab.name, elapsed
            );
            print_deploy_summary(&topo);
            Ok(())
        }

        Commands::Destroy { name } => {
            check_root();
            let lab = nlink_lab::RunningLab::load(&name)?;
            let node_count = lab.namespace_count();
            lab.destroy().await?;
            println!("Lab {name:?} destroyed ({node_count} namespaces removed)");
            Ok(())
        }

        Commands::Status { name } => match name {
            None => {
                let labs = nlink_lab::RunningLab::list()?;
                if labs.is_empty() {
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
                println!("Lab: {}", lab.name());
                println!("Nodes: {}", lab.namespace_count());
                let topo = lab.topology();
                println!("Links: {}", topo.links.len());
                println!("Impairments: {}", topo.impairments.len());
                let node_names: Vec<&str> = lab.node_names().collect();
                println!("  {}", node_names.join(", "));
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

fn check_root() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("warning: nlink-lab typically requires root or CAP_NET_ADMIN");
    }
}
