//! Process exit-code policy and the `--json` error envelope.

/// Process exit status chosen by a subcommand that completed its work
/// but wants a non-zero status (a child's exit code, an assertion
/// failure, …). `cmd::dispatch` keeps returning `Result<()>`; arms call
/// [`set_exit_code`] instead of `std::process::exit` so buffered
/// output and destructors still run.
pub static EXIT_CODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Exit codes: 0 ok · 1 error · 2 validation / assertion / drift
/// failure · 124 timeout · `exec`/`shell` pass the child's code through.
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_VALIDATION: u8 = 2;
pub const EXIT_TIMEOUT: u8 = 124;

pub fn set_exit_code(code: u8) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
}

pub fn exit_code_for(err: &nlink_lab::Error) -> u8 {
    match err {
        nlink_lab::Error::Validation(_)
        | nlink_lab::Error::ValidationErrors(_)
        | nlink_lab::Error::InvalidTopology(_) => EXIT_VALIDATION,
        nlink_lab::Error::Timeout(_) | nlink_lab::Error::TimedOut(_) => EXIT_TIMEOUT,
        _ => EXIT_FAILURE,
    }
}

/// The `--json` error envelope of an NLL parse error: the message, the
/// file, and the 1-based line and column the span starts at.
pub fn render_diagnostic_json(diag: &nlink_lab::error::NllDiagnostic) -> serde_json::Value {
    let text = diag.src.inner();
    let offset = diag.span.offset().min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    serde_json::json!({
        "error": diag.message,
        "kind": "parse",
        "file": diag.src.name(),
        "line": line,
        "column": column,
        "label": diag.label,
        "help": diag.help,
        "exit_code": EXIT_VALIDATION,
    })
}

/// Plan 158b Phase 3 — render an `nlink_lab::Error` as a
/// structured JSON envelope for the `--json` paths. Walks the
/// `std::error::Error::source` chain to build `error_chain`,
/// and surfaces `errno` / `ext_ack` / `ext_ack_offset` from any
/// `nlink::Error::Kernel` / `KernelWithContext` found in the
/// chain (via the inherent accessors added in Plan 158b Phase 1).
pub fn render_error_json(err: &nlink_lab::Error) -> serde_json::Value {
    let mut chain: Vec<String> = Vec::new();
    let mut src: &dyn std::error::Error = err;
    loop {
        chain.push(src.to_string());
        match src.source() {
            Some(next) => src = next,
            None => break,
        }
    }
    let mut envelope = serde_json::json!({
        "error": err.to_string(),
        "error_chain": chain,
        "errno": err.errno(),
        "ext_ack": err.ext_ack(),
        "ext_ack_offset": err.ext_ack_offset(),
        "exit_code": exit_code_for(err),
    });
    // What nlink says about a netlink failure (#177): what kind of
    // failure, the object and the namespace it concerns, and nlink's
    // own context, innermost last. `null` when no netlink call failed.
    let nl = err.nlink_error();
    envelope["kind"] = nl
        .and_then(|e| serde_json::to_value(e.root_cause().kind()).ok())
        .unwrap_or(serde_json::Value::Null);
    envelope["object"] = nl
        .and_then(|e| e.root_cause().object())
        .and_then(|o| serde_json::to_value(o).ok())
        .unwrap_or(serde_json::Value::Null);
    envelope["netns"] = nl
        .and_then(|e| e.netns().or(e.root_cause().netns()))
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null);
    envelope["context"] = nl
        .map(|e| serde_json::Value::from(e.context().to_vec()))
        .unwrap_or(serde_json::Value::Null);
    // Validation failures carry the structured issues so `--json`
    // consumers do not have to scrape "see errors above".
    if let nlink_lab::Error::ValidationErrors(issues) = err {
        envelope["issues"] = serde_json::to_value(issues).unwrap_or(serde_json::Value::Null);
    }
    envelope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_envelope_carries_what_nlink_says() {
        let kernel = nlink::Error::from_errno_ext_ack(16, Some("busy".into()), Some(36));
        let e = nlink_lab::Error::nlink("failed to apply nftables config on 'b'", kernel);
        let v = render_error_json(&e);
        assert_eq!(v["errno"], 16);
        assert_eq!(v["ext_ack"], "busy");
        assert_eq!(v["ext_ack_offset"], 36);
        assert_eq!(v["kind"], "busy", "{v}");
        assert!(v["error"].as_str().unwrap().starts_with("failed to apply"));
        assert!(v["context"].is_array());
        // and nothing invented for an error nlink did not raise
        let v = render_error_json(&nlink_lab::Error::deploy_failed("no runtime"));
        assert!(v["kind"].is_null() && v["errno"].is_null(), "{v}");
    }

    #[test]
    fn the_check_envelope_schema_does_not_require_elided_layers() {
        // An empty layer is left out of the JSON, so a schema that
        // requires it rejects every real `apply --check --json`.
        let schema = output_schema_for::<DryRunReport<'static>>();
        let required: Vec<&str> = schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            required,
            ["schema_version", "lab", "no_op", "change_count"],
            "{required:?}"
        );
    }

    #[test]
    fn exit_code_policy() {
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Validation("x".into())),
            EXIT_VALIDATION
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Timeout(std::time::Duration::from_secs(
                1
            ))),
            EXIT_TIMEOUT
        );
        // A wait that gives up (`wait`, `wait-for`) is a timeout too.
        assert_eq!(
            exit_code_for(&nlink_lab::Error::TimedOut("timeout waiting".into())),
            EXIT_TIMEOUT
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::NotFound { name: "l".into() }),
            EXIT_FAILURE
        );
        // a bad argument is an error, not a validation failure (#209)
        assert_eq!(
            exit_code_for(&nlink_lab::Error::invalid_argument("--set x")),
            EXIT_FAILURE
        );
    }
}

/// The `apply --dry-run --json` / `apply --check --json` / `verify --json`
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

/// Human rendering of the plan's other work ([`DryRunReport::ops`]).
pub fn print_plan_ops(ops: &[String]) {
    if !ops.is_empty() {
        println!("Plan:");
        for op in ops {
            println!("  + {op}");
        }
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

/// `status --scan --json` envelope.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab status --scan --json")]
pub struct StatusScanReport<'a> {
    pub labs: &'a [nlink_lab::state::LabInfo],
    pub orphans: &'a crate::host_scan::Orphans,
}

/// The schema of what nlink-lab *prints* as `T`: generated for the
/// serialize contract, so a field it leaves out when empty
/// (`skip_serializing_if`) is not required. `schema_for!` describes what
/// deserializes, which required every elided layer of the `apply --check`
/// envelope.
fn output_schema_for<T: schemars::JsonSchema>() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Every JSON schema `docs-gen --schemas` writes: file stem → schema.
pub fn all_schemas() -> Vec<(&'static str, schemars::Schema)> {
    vec![
        (
            "layered-diff.v3",
            output_schema_for::<DryRunReport<'static>>(),
        ),
        ("validate", output_schema_for::<ValidateReport<'static>>()),
        ("validate-rules", output_schema_for::<Vec<RuleInfo>>()),
        (
            "status-list",
            output_schema_for::<Vec<nlink_lab::state::LabInfo>>(),
        ),
        (
            "status-scan",
            output_schema_for::<StatusScanReport<'static>>(),
        ),
        ("ps", output_schema_for::<Vec<nlink_lab::ProcessInfo>>()),
        (
            "proc-stat",
            output_schema_for::<nlink_lab::proc_stat::ProcStat>(),
        ),
        (
            "proc-stat-list",
            output_schema_for::<Vec<nlink_lab::proc_stat::ProcStat>>(),
        ),
        (
            "doctor",
            output_schema_for::<crate::cmd::doctor::DoctorReport>(),
        ),
        (
            "metrics-snapshot",
            output_schema_for::<nlink_lab_shared::metrics::MetricsSnapshot>(),
        ),
        (
            "snapshot-list",
            output_schema_for::<Vec<nlink_lab::state::SnapshotInfo>>(),
        ),
        (
            "events",
            output_schema_for::<Vec<crate::cmd::events::Event>>(),
        ),
        (
            "lint",
            output_schema_for::<crate::cmd::lint::LintReport<'static>>(),
        ),
        // the daemon API (docs/API.md): what clients send and receive
        (
            "api-client.v1",
            output_schema_for::<nlink_lab_shared::api::ClientMsg>(),
        ),
        (
            "api-server.v1",
            output_schema_for::<nlink_lab_shared::api::ServerMsg>(),
        ),
    ]
}
