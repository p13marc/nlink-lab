//! What the CLI prints: exit codes, the `--json` error envelope, styles,
//! tables, the typed reports and their JSON schemas.

pub mod error;
pub mod exit;
pub mod schemas;
pub mod style;
pub mod table;

pub use error::error_json;
pub use exit::*;
pub use schemas::all_schemas;
pub use table::Table;

use crate::ctx::Ctx;

/// `--format`: what a command prints its result as.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// For people: aligned tables, colours on a terminal.
    #[default]
    Text,
    /// One pretty-printed JSON document.
    Json,
    /// One compact JSON document per line; what streams always print.
    Ndjson,
}

/// A command's result, printed as text or as one JSON document that
/// carries a `schema_version`.
pub trait Report: serde::Serialize {
    /// The document's `schema_version`: bumped when a field changes
    /// meaning or goes away.
    const VERSION: u32 = 1;

    /// The text form, lines ending in `\n`.
    fn text(&self) -> String;

    /// With `--quiet`, what a list prints instead: one id per line.
    fn quiet(&self) -> Option<String> {
        None
    }
}

/// What `--json` prints for a [`Report`]: its fields after a
/// `schema_version`.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct Document<'a, R: schemars::JsonSchema> {
    pub schema_version: u32,
    #[serde(flatten)]
    pub report: &'a R,
}

/// Print `doc` as the JSON document `--json` (pretty) or `--format
/// ndjson` (one line) asks for, after a `schema_version` of 1: for a
/// command whose text is not a [`Report`].
pub fn emit_json<R: serde::Serialize + schemars::JsonSchema>(
    ctx: &Ctx,
    doc: &R,
) -> nlink_lab::Result<()> {
    let doc = Document {
        schema_version: 1,
        report: doc,
    };
    if ctx.format == Format::Ndjson {
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&doc)?);
    }
    Ok(())
}

/// Print `report` on stdout in the format `ctx` asks for.
pub fn emit<R: Report + schemars::JsonSchema>(ctx: &Ctx, report: &R) -> nlink_lab::Result<()> {
    let doc = Document {
        schema_version: R::VERSION,
        report,
    };
    match ctx.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&doc)?),
        Format::Ndjson => println!("{}", serde_json::to_string(&doc)?),
        Format::Text => match report.quiet().filter(|_| ctx.quiet) {
            Some(ids) => anstream::print!("{ids}"),
            None => anstream::print!("{}", report.text()),
        },
    }
    Ok(())
}

pub use nlink_lab::internal::ops::view::DryRunReport;

/// Human rendering of a layered diff plus removal ops.
pub fn print_layered(layered: &nlink_lab::internal::diff::LayeredDiff, removals: &[String]) {
    let text = layered.to_string();
    print!("{text}");
    if !text.is_empty() && !text.ends_with('\n') {
        println!();
    }
    if !removals.is_empty() {
        println!("Removals:");
        for r in removals {
            println!("  - {r}");
        }
    }
}

/// `validate --list-rules --json`.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct RuleList {
    pub rules: Vec<RuleInfo>,
}

/// `validate --json` envelope.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab validate --json")]
pub struct ValidateReport<'a> {
    pub lab: &'a str,
    /// `false` when any error-level issue was found (exit 2).
    pub valid: bool,
    pub nodes: usize,
    pub links: usize,
    pub networks: usize,
    pub errors: usize,
    pub warnings: usize,
    pub issues: &'a [nlink_lab::ValidationIssue],
}

/// One entry of `validate --list-rules --json`.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab validate --list-rules --json (array element)")]
pub struct RuleInfo {
    pub rule: &'static str,
    pub severity: nlink_lab::Severity,
}
