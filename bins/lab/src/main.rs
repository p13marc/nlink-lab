#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
// The `cmd::dispatch()` CLI dispatcher awaits the full deploy/apply future graph
// inline; its layout computation exceeds the default query-depth limit
// (compile-time only, no runtime effect).
#![recursion_limit = "256"]

mod cli;
mod cmd;
mod ctx;
#[cfg(test)]
mod docs_gate;
mod host_scan;
mod output;
mod removed;
mod render;
mod util;

use clap::{CommandFactory, FromArgMatches, Parser};
use std::process::ExitCode;

use cli::Commands;
use ctx::Ctx;
use output::style::ColorWhen;
use output::{EXIT_CODE, EXIT_FAILURE, EXIT_USAGE, Format, error_json, exit_code_for};

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
pub(crate) struct Cli {
    /// Output format: `text`, `json` (one document), or `ndjson` (one
    /// compact document per line). JSON documents carry a
    /// `schema_version`; schemas are under `docs/json-schemas/`.
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    format: Option<Format>,

    /// Shorthand for `--format json`.
    #[arg(long, global = true, conflicts_with = "format")]
    json: bool,

    /// When to colour the output: `auto` (a terminal, unless `NO_COLOR`
    /// is set), `always`, `never`. Decided per stream.
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value_t)]
    color: ColorWhen,

    /// More detail: `-v` shows deployment steps and info logs, `-vv`
    /// debug logs.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Suppress informational output (errors still go to stderr); list
    /// commands print only ids.
    ///
    /// Recommended for scripted/automated use; the default human-readable
    /// output is intended for interactive shells.
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

/// The CLI's clap command, with its grouped top-level help.
pub(crate) fn command() -> clap::Command {
    let cmd = Cli::command();
    let template = cli::grouped_help(&cmd);
    cmd.help_template(template)
}

/// A command line clap refused: help and version go to stdout with exit
/// 0; a removed command says what replaced it; anything else is clap's
/// message, or the JSON envelope under `--json`, with exit 64.
fn usage_error(e: clap::Error, args: &[std::ffi::OsString]) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    let removed = (e.kind() == ErrorKind::InvalidSubcommand)
        .then(|| removed::subcommand(args).and_then(|n| removed::find(removed::REMOVED, n)))
        .flatten();
    let wants_json = args
        .iter()
        .any(|a| a == "--json" || a == "--format=json" || a == "--format=ndjson")
        || args
            .windows(2)
            .any(|w| w[0] == "--format" && (w[1] == "json" || w[1] == "ndjson"));
    let message = match removed {
        Some(r) => format!("`nlink-lab {}` was removed: use `{}`", r.name, r.instead),
        None => e.render().to_string().trim_end().to_string(),
    };
    if wants_json {
        let mut api = nlink_lab::api::ApiError::new(
            nlink_lab::api::ErrorCode::InvalidArgument,
            message
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("error: "),
        );
        api.exit_code = EXIT_USAGE;
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&api).unwrap_or_else(|_| message.clone())
        );
    } else if removed.is_some() {
        anstream::eprintln!("{} {message}", output::style::red("error:"));
    } else {
        let _ = e.print();
    }
    ExitCode::from(EXIT_USAGE)
}

fn main() -> ExitCode {
    // Dynamic completion callback (`COMPLETE=bash nlink-lab`): answers the
    // shell and exits before normal argument parsing.
    clap_complete::CompleteEnv::with_factory(command).complete();

    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // read before parsing, so clap's own help and errors follow it too
    let color = ColorWhen::from_args(&args);
    color.apply();
    // NLL diagnostics follow it too (miette decides alone otherwise)
    if color != ColorWhen::Auto {
        let on = color == ColorWhen::Always;
        let _ = miette::set_hook(Box::new(move |_| {
            Box::new(miette::MietteHandlerOpts::new().color(on).build())
        }));
    }
    let cli = match command()
        .color(color.clap())
        .try_get_matches_from(&args)
        .and_then(|m| Cli::from_arg_matches(&m))
    {
        Ok(cli) => cli,
        Err(e) => return usage_error(e, &args),
    };

    // Tracing level: `RUST_LOG` if set, else warn (info with -v, debug
    // with -vv). `from_default_env` alone meant error: every warning was
    // hidden (#209).
    let env_filter = match cli.verbose {
        0 => {
            // nlink's connection logs every error ack at warn, the ones its
            // caller expects and handles too (`impair --partition` printed
            // two ENOENTs): what fails reaches the user as the command's error
            // (#223)
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,nlink::netlink::connection=error")
            })
        }
        1 => tracing_subscriber::EnvFilter::new("info"),
        _ => tracing_subscriber::EnvFilter::new("debug"),
    };
    // Log lines go to stderr: on stdout they corrupted `--json` documents,
    // and `lsp`'s JSON-RPC framing.
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .with_ansi(color != ColorWhen::Never)
        .init();

    // Handle completions synchronously (no runtime needed)
    if let Commands::Completions { shell } = &cli.command {
        clap_complete::generate(*shell, &mut command(), "nlink-lab", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }

    let format = match (cli.json, cli.format) {
        (_, Some(f)) => f,
        (true, None) => Format::Json,
        (false, None) => Format::Text,
    };
    let ctx = Ctx {
        format,
        json: format != Format::Text,
        quiet: cli.quiet,
        verbose: cli.verbose > 0,
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
        // Under `--json` a failure is the API's error document on stderr,
        // stdout staying clean for the tools that pipe it.
        Err(e) if ctx.json => {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&error_json(&e))
                    .unwrap_or_else(|_| format!("error: {e}"))
            );
            ExitCode::from(exit_code_for(&e))
        }
        Err(nlink_lab::Error::NllDiagnostic(diag)) => {
            // NLL diagnostics get their own rich miette renderer.
            let report = miette::Report::new(*diag);
            eprintln!("{report:?}");
            ExitCode::from(output::EXIT_VALIDATION)
        }
        Err(e) => {
            anstream::eprintln!("{} {e}", output::style::red("error:"));
            if let Some(hint) = e.to_api().hint {
                anstream::eprintln!("  {} {hint}", output::style::bold("hint:"));
            }
            ExitCode::from(exit_code_for(&e))
        }
    }
}
