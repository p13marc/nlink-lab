//! `nlink-lab daemon`.

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Metrics collection interval.
    #[arg(short, long, value_name = "DURATION", default_value = "2s", value_parser = crate::args::interval)]
    pub interval: std::time::Duration,

    /// Zenoh mode: peer or client.
    #[arg(long, default_value = "peer")]
    pub zenoh_mode: String,

    /// Zenoh listen endpoint.
    #[arg(long)]
    pub zenoh_listen: Option<String>,

    /// Zenoh connect endpoint.
    #[arg(long)]
    pub zenoh_connect: Option<String>,

    /// Also serve /metrics (OpenMetrics) and /api/v1/{snapshot,health,topology}
    /// over HTTP on this address, e.g. 127.0.0.1:9464 (#64).
    #[arg(long, value_name = "ADDR")]
    pub http: Option<std::net::SocketAddr>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        interval,
        zenoh_mode,
        zenoh_listen,
        zenoh_connect,
        http,
    } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    // The real backend (nlink-lab-backend as a library): RPC
    // queryables, events, sockdiag flows — the CLI used to run a
    // private fork that ignored every flag below (#43).
    let opts = nlink_lab_backend::BackendOpts {
        interval,
        zenoh_mode: zenoh_mode.parse()?,
        zenoh_listen: zenoh_listen.into_iter().collect(),
        zenoh_connect: zenoh_connect.into_iter().collect(),
        http,
    };
    if !ctx.quiet {
        println!(
            "Starting Zenoh backend for lab '{}' ({} nodes, every {:?})",
            lab,
            running.namespace_count(),
            interval
        );
    }
    nlink_lab_backend::run(running, opts).await?;
    Ok(())
}
