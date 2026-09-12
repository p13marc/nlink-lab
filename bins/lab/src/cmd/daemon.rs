//! `nlink-lab daemon`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (must be deployed).
    pub lab: String,

    /// Metrics collection interval in seconds.
    #[arg(short, long, default_value = "2")]
    pub interval: u64,

    /// Zenoh mode: peer or client.
    #[arg(long, default_value = "peer")]
    pub zenoh_mode: String,

    /// Zenoh listen endpoint.
    #[arg(long)]
    pub zenoh_listen: Option<String>,

    /// Zenoh connect endpoint.
    #[arg(long)]
    pub zenoh_connect: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        interval,
        zenoh_mode,
        zenoh_listen,
        zenoh_connect,
    } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // The real backend (nlink-lab-backend as a library): RPC
    // queryables, events, sockdiag flows — the CLI used to run a
    // private fork that ignored every flag below (#43).
    let opts = nlink_lab_backend::BackendOpts {
        interval: std::time::Duration::from_secs(interval),
        zenoh_mode: zenoh_mode.parse()?,
        zenoh_listen: zenoh_listen.into_iter().collect(),
        zenoh_connect: zenoh_connect.into_iter().collect(),
    };
    if !ctx.quiet {
        println!(
            "Starting Zenoh backend for lab '{}' ({} nodes, every {}s)",
            lab,
            running.namespace_count(),
            interval
        );
    }
    nlink_lab_backend::run(running, opts).await?;
    Ok(())
}
