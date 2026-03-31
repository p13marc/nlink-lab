mod apply;
mod capture;
mod containers;
mod daemon_cmd;
mod deploy;
mod destroy;
mod diagnose;
mod diff;
mod exec;
mod export;
mod graph;
mod impair;
mod init;
mod inspect;
mod metrics;
mod process;
mod shell;
mod status;
mod test;
mod validate;
mod wait;

use crate::cli::{Cli, Commands};

pub(crate) async fn dispatch(cli: Cli) -> nlink_lab::Result<()> {
    let json = cli.json;
    let quiet = cli.quiet;

    match cli.command {
        Commands::Deploy {
            topology,
            dry_run,
            force,
            daemon,
            skip_validate,
        } => deploy::run(topology, dry_run, force, daemon, skip_validate, quiet).await,

        Commands::Apply { topology, dry_run } => apply::run(topology, dry_run).await,

        Commands::Destroy { name, force, all } => destroy::run(name, force, all).await,

        Commands::Status { name } => status::run(name, json),

        Commands::Exec { lab, node, cmd } => exec::run(lab, node, cmd),

        Commands::Validate { topology } => validate::run(topology),

        Commands::Test {
            path,
            junit,
            tap,
            fail_fast,
        } => test::run(path, junit, tap, fail_fast).await,

        Commands::Impair {
            lab,
            endpoint,
            show,
            delay,
            jitter,
            loss,
            rate,
            clear,
        } => impair::run(lab, endpoint, show, delay, jitter, loss, rate, clear).await,

        Commands::Graph { topology } => graph::run_graph(topology),

        Commands::Render {
            topology,
            dot,
            ascii,
        } => graph::run_render(topology, dot, ascii, json),

        Commands::Shell { lab, node, shell } => shell::run(lab, node, shell),

        Commands::Ps { lab } => process::run_ps(lab, json),

        Commands::Kill { lab, pid } => process::run_kill(lab, pid),

        Commands::Diagnose { lab, node } => diagnose::run(lab, node, json).await,

        Commands::Capture {
            lab,
            endpoint,
            write,
            count,
            filter,
        } => capture::run(lab, endpoint, write, count, filter),

        Commands::Diff { a, b } => diff::run(a, b, json),

        Commands::Export { lab, output } => export::run(lab, output, json),

        Commands::Inspect { lab } => inspect::run(lab, json),

        Commands::Containers { lab } => containers::run_list(lab, json),

        Commands::Logs {
            lab,
            node,
            follow,
            tail,
        } => containers::run_logs(lab, node, follow, tail),

        Commands::Pull { topology } => containers::run_pull(topology),

        Commands::Stats { lab } => containers::run_stats(lab),

        Commands::Restart { lab, node } => containers::run_restart(lab, node),

        Commands::Daemon {
            lab,
            interval,
            zenoh_mode,
            zenoh_listen,
            zenoh_connect,
        } => daemon_cmd::run(lab, interval, zenoh_mode, zenoh_listen, zenoh_connect).await,

        Commands::Metrics {
            lab,
            node,
            format,
            count,
            zenoh_connect,
        } => metrics::run(lab, node, format, count, zenoh_connect).await,

        Commands::Init {
            template,
            list,
            output,
            format,
            name,
            force,
        } => init::run(template, list, output, format, name, force),

        Commands::Wait { name, timeout } => wait::run(name, timeout).await,

        Commands::Completions { .. } => Ok(()), // handled in main()
    }
}
