//! What a call streams before it ends: operation progress, notifications
//! of a subscription.

use serde::{Deserialize, Serialize};

use super::{ApiError, LabSummary};

/// One step of an operation's progress (deploy, apply, destroy, restore,
/// scenario, benchmark, test). `seq` orders them; `ts_ms` is the Unix time
/// in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpEvent {
    /// The operation's id: it is re-attached by it.
    pub op: String,
    pub lab: String,
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub phase: OpPhase,
}

/// What happened. `Step`s are coalesced (one per 100 ms at most); the
/// first and the last of a stage always come.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum OpPhase {
    /// Waiting behind `position` other operations on the lab.
    Queued { position: usize },
    /// `kind` is `deploy`, `apply`, …; `actor` who asked (`user (uid)`).
    Started {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
    },
    /// The plan: its stages with how many ops each, and the removals.
    Planned {
        stages: Vec<(String, usize)>,
        ops: usize,
        #[serde(default)]
        removals: usize,
    },
    /// A stage begins (`index` of `of`, from 1), with its op count.
    Stage {
        stage: String,
        index: usize,
        of: usize,
        ops: usize,
    },
    /// `done` of `total` ops of the whole plan; the latest one.
    Step {
        done: usize,
        total: usize,
        describe: String,
    },
    /// A wait starts: for `what`, until `deadline_ms` from now at most.
    Waiting {
        what: WaitKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<u64>,
    },
    /// How a wait is going (`done` of `total`, e.g. FRR adjacencies).
    WaitProgress {
        what: WaitKind,
        done: usize,
        total: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A message: `level` is `info` or `warn`.
    Log { level: String, message: String },
    /// A `validate { … }` assertion's result.
    Assertion {
        description: String,
        passed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A scenario action at `time_ms` into the scenario.
    ScenarioAction {
        time_ms: u64,
        description: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// The operation failed or was cancelled: undoing `entries` changes.
    RollingBack { reason: String, entries: usize },
    /// The end.
    Finished { outcome: OpOutcome, elapsed_ms: u64 },
}

/// What a wait waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitKind {
    ImagePull { image: String },
    ContainerStart { node: String },
    Healthcheck { node: String },
    FrrConvergence,
    WifiAssociation,
    AssertionRetries,
    Readiness { node: String },
}

/// How an operation ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OpOutcome {
    Succeeded,
    Failed {
        error: ApiError,
    },
    Cancelled,
    /// It failed and what it had done was undone.
    RolledBack {
        error: ApiError,
    },
}

/// A notification of a subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "notification", rename_all = "snake_case")]
pub enum Notification {
    /// Labs that appeared, changed (counts, health) or went away.
    LabsChanged {
        #[serde(default)]
        added: Vec<LabSummary>,
        #[serde(default)]
        updated: Vec<LabSummary>,
        #[serde(default)]
        removed: Vec<String>,
    },
    /// A lifecycle event of the lab, as `events.ndjson` records it.
    Lifecycle {
        lab: String,
        event: serde_json::Value,
    },
    /// An interface went up or down, a process exited.
    Runtime {
        lab: String,
        event: serde_json::Value,
    },
    /// A change nobody made through nlink-lab.
    Drift {
        lab: String,
        node: String,
        detail: serde_json::Value,
    },
    /// The latest metrics of the lab (`crate::metrics::MetricsSnapshot`).
    Metrics {
        lab: String,
        snapshot: crate::metrics::MetricsSnapshot,
    },
    /// The lab's topology changed: fetch it again (`get_topology`).
    TopologyChanged { lab: String, revision: String },
    /// Progress of an operation on the lab.
    OpProgress { event: OpEvent },
}

/// An item a call streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum StreamItem {
    Op {
        event: OpEvent,
    },
    Notification {
        notification: Notification,
    },
    /// The call started what it is about: an operation, a process.
    Started {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// A slow reader missed `dropped` items.
    Lagged {
        dropped: u64,
    },
}
