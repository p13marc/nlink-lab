#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
// The `cmd::dispatch()` CLI dispatcher awaits the full deploy/apply future graph
// inline; its layout computation exceeds the default query-depth limit
// (compile-time only, no runtime effect).
#![recursion_limit = "256"]

mod cli;
mod cmd;
mod ctx;
mod host_scan;
mod output;
mod render;
mod util;

use clap::{CommandFactory, Parser};
use std::process::ExitCode;

use cli::Commands;
use ctx::Ctx;
use output::{EXIT_CODE, EXIT_FAILURE, EXIT_VALIDATION, exit_code_for, render_error_json};

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
pub(crate) struct Cli {
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

fn main() -> ExitCode {
    // Dynamic completion callback (`COMPLETE=bash nlink-lab`): answers the
    // shell and exits before normal argument parsing.
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

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

    // Plan 158b Phase 3 — when `--json` is on, surface terminal
    // errors as a structured envelope on stderr (still going to
    // stderr to keep stdout clean for tools piping JSON output).
    let want_json_errors = cli.json;
    let ctx = Ctx {
        json: cli.json,
        quiet: cli.quiet,
        verbose: cli.verbose,
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    match rt.block_on(cmd::dispatch(&ctx, cli.command)) {
        Ok(()) => ExitCode::from(EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst)),
        Err(nlink_lab::Error::NllDiagnostic(diag)) => {
            // NLL diagnostics get their own rich miette renderer
            // even under --json; the structured envelope below is
            // for kernel/runtime errors.
            let report = miette::Report::new(*diag);
            eprintln!("{report:?}");
            ExitCode::from(EXIT_VALIDATION)
        }
        Err(e) if want_json_errors => {
            let envelope = render_error_json(&e);
            // Pretty-printing keeps the schema obvious in
            // interactive use; tools that want compact output can
            // re-serialize with serde_json::to_string.
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| format!("error: {e}"))
            );
            ExitCode::from(exit_code_for(&e))
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(exit_code_for(&e))
        }
    }
}
