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

/// The `apply --dry-run --json` / `verify --json`
/// envelope (schema v3, `docs/json-schemas/layered-diff.v3.schema.json`).
#[derive(serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab apply --dry-run / --check / verify --json (v3)")]
pub struct DryRunReport<'a> {
    /// Schema marker: `3`. v3 dropped the v1 `diff` / `layered_summary`
    /// fields (Plan 160 / 0.7.0) — use `topology` / `network` /
    /// `nftables` / `wireguard` / `removals`. `topology` and `wireguard`
    /// were added later and are additive, so the marker did not move.
    pub schema_version: u32,
    pub lab: &'a str,
    pub no_op: bool,
    pub change_count: usize,
    /// Lab-graph changes: nodes, links, routes, sysctls, nftables,
    /// impairments, per-pair `impair` matrices, `qdisc` blocks, rate
    /// limits — what the topology files differ in. The kernel side of a
    /// traffic-control change is in `network` (the qdiscs and trees it
    /// declares), with any drift from it (#108). Elided when empty.
    #[serde(skip_serializing_if = "nlink_lab::diff::TopologyDiff::is_empty")]
    pub topology: &'a nlink_lab::diff::TopologyDiff,
    /// Typed per-node `NetworkConfig` diff. Empty map elided.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub network: &'a std::collections::BTreeMap<String, nlink_lab::diff::ConfigDiff>,
    /// Typed per-node `NftablesDiff`. Empty map elided.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub nftables: &'a std::collections::BTreeMap<String, nlink_lab::diff::NftablesDiff>,
    /// Typed per-node `WireguardConfigDiff`: peers, keys, endpoints and
    /// allowed-ips that differ from the declared tunnels (public keys
    /// only; private and preshared keys as presence flags). Empty map
    /// elided. Additive, so the marker stays `3`. Free-form in the
    /// schema: nlink 0.32 derives one that does not match the JSON (keys
    /// as byte arrays, presence flags as keys; marcpardo/nlink#876).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    pub wireguard: &'a std::collections::BTreeMap<String, nlink_lab::diff::WireguardConfigDiff>,
    /// Removal ops from the plan diff (deleted nodes, links, VRF-table
    /// routes, …), one description per op. Elided when empty.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub removals: &'a [String],
    /// The plan's other work, one description per op: what neither the
    /// lab graph nor the declarative layers show, such as a container
    /// re-created because its options changed, or a process restarted
    /// (#204). Elided when empty; additive, so the marker stays `3`.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub ops: &'a [String],
}

impl<'a> DryRunReport<'a> {
    pub fn new(
        lab: &'a str,
        layered: &'a nlink_lab::diff::LayeredDiff,
        removals: &'a [String],
    ) -> Self {
        Self {
            schema_version: 3,
            lab,
            no_op: layered.is_empty() && removals.is_empty(),
            change_count: layered.change_count() + removals.len(),
            topology: &layered.topology,
            network: &layered.network,
            nftables: &layered.nftables,
            wireguard: &layered.wireguard,
            removals,
            ops: &[],
        }
    }

    /// With the plan's other work ([`DryRunReport::ops`]).
    pub fn with_ops(mut self, ops: &'a [String]) -> Self {
        self.no_op &= ops.is_empty();
        self.change_count += ops.len();
        self.ops = ops;
        self
    }
}

/// Human rendering of a layered diff plus removal ops.
pub fn print_layered(layered: &nlink_lab::diff::LayeredDiff, removals: &[String]) {
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
