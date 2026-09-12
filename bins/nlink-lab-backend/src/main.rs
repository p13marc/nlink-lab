//! nlink-lab-backend: standalone Zenoh backend daemon.
//!
//! Thin clap wrapper over [`nlink_lab_backend::run`]; the `nlink-lab
//! daemon` CLI arm calls the same library entry point.

// `nlink_lab::Error` (wrapped by `nlink_lab_backend::Error`) is deliberately
// unboxed; match the allow the library, CLI and backend lib already carry.
#![allow(clippy::result_large_err)]

use std::time::Duration;

use clap::Parser;
use nlink_lab_backend::{BackendOpts, ZenohMode};
use tracing::info;

#[derive(Parser)]
#[command(
    name = "nlink-lab-backend",
    about = "Zenoh backend daemon for nlink-lab"
)]
struct Cli {
    /// Lab name (must be deployed).
    lab: String,

    /// Metrics collection interval in seconds.
    #[arg(short, long, default_value = "2")]
    interval: u64,

    /// Zenoh mode: peer or client.
    #[arg(long, default_value = "peer")]
    zenoh_mode: ZenohMode,

    /// Zenoh listen endpoint (repeatable), e.g. tcp/0.0.0.0:7447.
    #[arg(long)]
    zenoh_listen: Vec<String>,

    /// Zenoh connect endpoint (repeatable), e.g. tcp/127.0.0.1:7447.
    #[arg(long)]
    zenoh_connect: Vec<String>,
}

impl From<Cli> for BackendOpts {
    fn from(cli: Cli) -> Self {
        Self {
            interval: Duration::from_secs(cli.interval),
            zenoh_mode: cli.zenoh_mode,
            zenoh_listen: cli.zenoh_listen,
            zenoh_connect: cli.zenoh_connect,
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    if let Err(e) = run(cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), nlink_lab_backend::Error> {
    let lab = nlink_lab::RunningLab::load(&cli.lab)?;
    info!(lab = cli.lab, nodes = lab.namespace_count(), "loaded lab");
    nlink_lab_backend::run(lab, cli.into()).await
}
