//! Subcommand handlers — one module per `nlink-lab <subcommand>`, each
//! exporting `pub struct Args` (the clap arguments referenced from
//! [`crate::cli::Commands`]) and `run(ctx, args)`. [`dispatch`] is the
//! single match that routes a parsed command to its handler.

pub mod apply;
pub mod capture;
pub mod containers;
pub mod daemon;
pub mod deploy;
pub mod destroy;
pub mod diagnose;
pub mod diff;
pub mod docs_gen;
pub mod doctor;
pub mod exec;
pub mod export;
pub mod graph;
pub mod impair;
pub mod import;
pub mod init;
pub mod inspect;
pub mod ip;
pub mod kill;
pub mod logs;
pub mod metrics;
pub mod proc_stat;
pub mod ps;
pub mod pull;
pub mod render;
pub mod restart;
pub mod scenario;
pub mod shell;
pub mod spawn;
pub mod stats;
pub mod status;
pub mod test;
pub mod validate;
pub mod verify;
pub mod wait;
pub mod wait_for;
pub mod watch;

use crate::cli::Commands;
use crate::ctx::Ctx;

/// Route a parsed subcommand to its handler.
pub async fn dispatch(ctx: &Ctx, cmd: Commands) -> nlink_lab::Result<()> {
    match cmd {
        Commands::Deploy(args) => deploy::run(ctx, args).await,
        Commands::Apply(args) => apply::run(ctx, args).await,
        Commands::Destroy(args) => destroy::run(ctx, args).await,
        Commands::Status(args) => status::run(ctx, args).await,
        Commands::Exec(args) => exec::run(ctx, args),
        Commands::Spawn(args) => spawn::run(ctx, args).await,
        Commands::Validate(args) => validate::run(ctx, args),
        Commands::Verify(args) => verify::run(ctx, args).await,
        Commands::Doctor(args) => doctor::run(ctx, args).await,
        Commands::Test(args) => test::run(ctx, args).await,
        Commands::Impair(args) => impair::run(ctx, args).await,
        Commands::Scenario(args) => scenario::run(ctx, args).await,
        Commands::DocsGen(args) => docs_gen::run(ctx, args),
        Commands::Graph(args) => graph::run(ctx, args),
        Commands::Render(args) => render::run(ctx, args),
        Commands::Shell(args) => shell::run(ctx, args),
        Commands::Ps(args) => ps::run(ctx, args),
        Commands::Kill(args) => kill::run(ctx, args),
        Commands::ProcStat(args) => proc_stat::run(ctx, args).await,
        Commands::Diagnose(args) => diagnose::run(ctx, args).await,
        Commands::Capture(args) => capture::run(ctx, args),
        Commands::Wait(args) => wait::run(ctx, args).await,
        Commands::Watch(args) => watch::run(ctx, args).await,
        Commands::WaitFor(args) => wait_for::run(ctx, args).await,
        Commands::Ip(args) => ip::run(ctx, args),
        Commands::Diff(args) => diff::run(ctx, args),
        Commands::Export(args) => export::run(ctx, args),
        Commands::Import(args) => import::run(ctx, args).await,
        Commands::Inspect(args) => inspect::run(ctx, args),
        Commands::Containers(args) => containers::run(ctx, args),
        Commands::Logs(args) => logs::run(ctx, args),
        Commands::Pull(args) => pull::run(ctx, args),
        Commands::Stats(args) => stats::run(ctx, args),
        Commands::Restart(args) => restart::run(ctx, args),
        Commands::Completions { .. } => {
            // Already handled before async runtime
            Ok(())
        }
        Commands::Daemon(args) => daemon::run(ctx, args).await,
        Commands::Metrics(args) => metrics::run(ctx, args).await,
        Commands::Init(args) => init::run(ctx, args),
    }
}
