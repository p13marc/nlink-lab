use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> nlink_lab::Result<()> {
    match cli.command {
        Commands::Deploy { topology, dry_run } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            if dry_run {
                println!("Topology '{}' parsed successfully", topo.lab.name);
                println!("  Nodes: {}", topo.nodes.len());
                println!("  Links: {}", topo.links.len());
                println!("  Impairments: {}", topo.impairments.len());
                println!("  (dry run — nothing deployed)");
            } else {
                // TODO: validate + deploy
                println!("Deploy not yet implemented. Use --dry-run to validate parsing.");
            }
            Ok(())
        }

        Commands::Destroy { name } => {
            // TODO: load state, destroy
            println!("Destroy not yet implemented for lab '{name}'.");
            Ok(())
        }

        Commands::Status { name } => {
            match name {
                Some(name) => {
                    // TODO: load state, show details
                    println!("Status not yet implemented for lab '{name}'.");
                }
                None => {
                    // TODO: list running labs
                    println!("No running labs (status not yet implemented).");
                }
            }
            Ok(())
        }

        Commands::Exec { lab, node, cmd } => {
            // TODO: load state, spawn in namespace
            let _ = (lab, node, cmd);
            println!("Exec not yet implemented.");
            Ok(())
        }

        Commands::Validate { topology } => {
            let topo = nlink_lab::parser::parse_file(&topology)?;
            // TODO: run validator
            println!("Topology '{}' parsed successfully", topo.lab.name);
            println!("  Nodes: {}", topo.nodes.len());
            println!("  Links: {}", topo.links.len());
            println!("  Profiles: {}", topo.profiles.len());
            println!("  Networks: {}", topo.networks.len());
            println!("  Impairments: {}", topo.impairments.len());
            println!("  Rate limits: {}", topo.rate_limits.len());
            Ok(())
        }
    }
}
