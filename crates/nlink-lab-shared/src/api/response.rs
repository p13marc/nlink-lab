//! What a call ends with when it succeeds.

use serde::{Deserialize, Serialize};

use super::{EndpointImpairment, LabSummary, OpInfo, ProcessDto, SnapshotDto};
use crate::metrics::MetricsSnapshot;

/// The result of a call. Each request has the variant named after what
/// it returns; `Ok` for those that return nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong,
    WhoAmI {
        server: String,
        host: String,
        api: String,
        uid: u32,
        user: String,
        role: super::Role,
        #[serde(default)]
        features: Vec<String>,
    },
    DaemonStatus {
        server: String,
        uptime_ms: u64,
        labs: usize,
        clients: usize,
        #[serde(default)]
        legacy_labs: Vec<String>,
    },
    Labs {
        labs: Vec<LabSummary>,
    },
    Lab {
        lab: LabSummary,
        detail: serde_json::Value,
    },
    Topology {
        lab: String,
        revision: String,
        topology: serde_json::Value,
    },
    Processes {
        processes: Vec<ProcessDto>,
    },
    Impairments {
        endpoints: Vec<EndpointImpairment>,
    },
    Snapshots {
        snapshots: Vec<SnapshotDto>,
    },
    /// `next` is the cursor to read on from.
    Events {
        events: Vec<serde_json::Value>,
        next: u64,
    },
    Metrics {
        snapshot: MetricsSnapshot,
    },
    /// Free-form reports with a documented shape (diagnose, addresses,
    /// proc-stat, stats, plans, diffs, scenario and benchmark results).
    Report {
        kind: String,
        report: serde_json::Value,
    },
    Ops {
        ops: Vec<OpInfo>,
    },
    /// The client left a long operation running (`detach`, or it went
    /// away): `attach_op` follows it again.
    Detached {
        op: String,
    },
    /// A lifecycle operation finished; `lab` is the lab it made or changed.
    Done {
        op: String,
        lab: String,
        #[serde(default)]
        report: serde_json::Value,
    },
    Spawned {
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr: Option<String>,
    },
    /// An exec, a shell or a wait's command ended.
    Exited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
        duration_ms: u64,
        #[serde(default)]
        timed_out: bool,
    },
    /// The exec's output, for a buffered exec.
    Output {
        stdout: String,
        stderr: String,
        exit_code: i32,
        duration_ms: u64,
    },
}
