//! The JSON schemas `docs-gen --schemas` writes (`docs/json-schemas/`).

use super::{DryRunReport, RuleInfo, ValidateReport};

/// The schema of what nlink-lab *prints* as `T`: generated for the
/// serialize contract, so a field it leaves out when empty
/// (`skip_serializing_if`) is not required. `schema_for!` describes what
/// deserializes, which required every elided layer of the `apply --check`
/// envelope.
pub fn output_schema_for<T: schemars::JsonSchema>() -> schemars::Schema {
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
            "deploy",
            output_schema_for::<super::Document<'static, crate::cmd::deploy::DeployReport>>(),
        ),
        (
            "ls",
            output_schema_for::<super::Document<'static, crate::cmd::ls::LsReport>>(),
        ),
        (
            "status",
            output_schema_for::<super::Document<'static, crate::cmd::status::StatusReport>>(),
        ),
        (
            "use",
            output_schema_for::<super::Document<'static, crate::cmd::use_lab::UseReport>>(),
        ),
        (
            "ps",
            output_schema_for::<super::Document<'static, crate::cmd::ps::PsReport>>(),
        ),
        (
            "stats",
            output_schema_for::<super::Document<'static, crate::cmd::stats::StatsReport>>(),
        ),
        (
            "stats-procs",
            output_schema_for::<super::Document<'static, crate::cmd::stats::ProcsReport>>(),
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
            output_schema_for::<super::Document<'static, crate::cmd::snapshot::SnapshotList>>(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
