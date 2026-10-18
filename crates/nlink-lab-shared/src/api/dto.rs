//! Plain data carried by the API: what a client shows and edits.

use serde::{Deserialize, Serialize};

/// An impairment (netem), every knob optional: what `impair` sets and the
/// GUI's editor edits. Values are in NLL's units (`"10ms"`, `"1%"`,
/// `"100mbit"`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImpairmentSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrupt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reorder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_correlation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_correlation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
}

/// Which way an impairment acts on an endpoint's link: out of it (its
/// egress), into it (the peer's egress), or both.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Out,
    In,
    Both,
}

/// A process's output stream.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    #[default]
    Stdout,
    Stderr,
    Both,
}

/// One running lab, as a list shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LabSummary {
    pub lab: String,
    #[serde(default)]
    pub nodes: usize,
    #[serde(default)]
    pub containers: usize,
    #[serde(default)]
    pub links: usize,
    /// RFC 3339.
    #[serde(default)]
    pub created_at: String,
    /// `running`, or `stale` when namespaces it claims are gone.
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub impaired: usize,
    #[serde(default)]
    pub partitioned: usize,
    #[serde(default)]
    pub processes: usize,
    /// sha256 of the lab's topology: changes when an `apply` changes it.
    #[serde(default)]
    pub revision: String,
}

/// A tracked background process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessDto {
    pub node: String,
    pub pid: u32,
    pub alive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// A snapshot of a lab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnapshotDto {
    pub name: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub nodes: usize,
    #[serde(default)]
    pub links: usize,
}

/// The impairment of one endpoint as it is: what the topology declares,
/// what `impair` set at runtime, and whether it is partitioned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EndpointImpairment {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<ImpairmentSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<ImpairmentSpec>,
    #[serde(default)]
    pub partitioned: bool,
}

/// Where a lab's topology came from, sent with a deploy: the file's text
/// and the files it imports (keyed by their path relative to it), so the
/// daemon never reads a client's files by path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceBundle {
    /// The entry file's path, as the client knows it (for messages only).
    #[serde(default)]
    pub path: String,
    pub text: String,
    #[serde(default)]
    pub imports: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub params: Vec<(String, String)>,
}

/// Options of a deploy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeployOptions {
    /// Destroy a lab of that name first.
    #[serde(default)]
    pub force: bool,
    /// Skip the `validate { … }` assertions.
    #[serde(default)]
    pub skip_assertions: bool,
    /// A failed assertion fails the deploy.
    #[serde(default)]
    pub strict: bool,
    /// Appended to the lab's name and prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// Options of an apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyOptions {
    /// Drop runtime impairments and partitions.
    #[serde(default)]
    pub reset_impairments: bool,
    #[serde(default)]
    pub strict: bool,
}

/// A condition a `wait_for` (or a spawn) waits for. Conditions given
/// together must all hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WaitSpec {
    /// `IP:PORT` that accepts a TCP connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp: Option<String>,
    /// A command that exits 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    /// A file that exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// A port a process of the node listens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening: Option<u16>,
    /// A regex a (spawned) process's log line matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    #[serde(default)]
    pub timeout_ms: u64,
}

/// What a subscription delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Labs appearing, changing, going away.
    Labs,
    /// The lab's lifecycle events (deployed, applied, impaired, …).
    Lifecycle,
    /// Interfaces going up or down, processes exiting.
    Runtime,
    /// Metrics snapshots, every interval.
    Metrics,
    /// A new topology after an apply or restore.
    Topology,
    /// Progress of operations.
    Ops,
    /// Changes nobody made through nlink-lab (hand edits).
    Drift,
}

/// An operation the daemon runs or ran (`list_ops`): a deploy, an apply,
/// a scenario…
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpInfo {
    pub op: String,
    /// `deploy`, `apply`, `destroy`, …
    pub kind: String,
    /// The lab it is about; empty for the host's (`reap_orphans`).
    pub lab: String,
    /// Who asked: `user (uid)`.
    pub actor: String,
    pub state: OpState,
    /// RFC 3339.
    pub queued_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where an operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    /// Behind others on its lab.
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// It failed and what it had done was undone.
    RolledBack,
}

impl OpState {
    /// Whether it has ended.
    pub fn is_final(self) -> bool {
        !matches!(self, OpState::Queued | OpState::Running)
    }
}
