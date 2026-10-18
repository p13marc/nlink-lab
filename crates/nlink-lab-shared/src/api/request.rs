//! Everything a client can ask the daemon.

use serde::{Deserialize, Serialize};

use super::{
    ApplyOptions, DeployOptions, Direction, ImpairmentSpec, LogStream, SourceBundle, Topic,
    WaitSpec,
};

/// A request: `{"method": "...", "params": {...}}`.
///
/// Topologies travel as JSON (`nlink_lab::Topology`); the daemon
/// redacts the secrets of every topology it sends unless asked not to
/// (`redact: false`, operators only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    // ── session ──
    Ping,
    /// The daemon's version, host and the caller's identity and role.
    WhoAmI,
    DaemonStatus,

    // ── reading ──
    ListLabs,
    GetLab {
        lab: String,
    },
    GetTopology {
        lab: String,
        #[serde(default = "yes")]
        redact: bool,
    },
    ListProcesses {
        lab: String,
    },
    GetImpairments {
        lab: String,
    },
    ListSnapshots {
        lab: String,
    },
    /// Lifecycle events from `since` (a cursor from an earlier read), at
    /// most `limit`, of these `kinds` (all when empty).
    ReadEvents {
        lab: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        #[serde(default)]
        kinds: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    GetMetrics {
        lab: String,
    },
    Diagnose {
        lab: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
    },
    NodeAddresses {
        lab: String,
        node: String,
    },
    ProcStat {
        lab: String,
        node: String,
        #[serde(default)]
        pids: Vec<u32>,
    },
    Stats {
        lab: String,
    },
    ListOps,
    /// Follow an operation started earlier (by this or another client):
    /// its events so far, then the rest.
    AttachOp {
        op: String,
    },
    /// A stream of notifications: of one lab, or of all with `lab: null`.
    Subscribe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lab: Option<String>,
        topics: Vec<Topic>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
    },

    // ── lifecycle (long-running: they stream `OpEvent`s) ──
    Deploy {
        topology: serde_json::Value,
        #[serde(default)]
        options: DeployOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<SourceBundle>,
    },
    /// The deploy plan, without deploying.
    PlanDeploy {
        topology: serde_json::Value,
    },
    Apply {
        lab: String,
        topology: serde_json::Value,
        #[serde(default)]
        options: ApplyOptions,
    },
    /// What an apply would change, without changing it.
    CheckApply {
        lab: String,
        topology: serde_json::Value,
        #[serde(default)]
        options: ApplyOptions,
    },
    /// Drift between the lab and its topology.
    Verify {
        lab: String,
    },
    Destroy {
        lab: String,
    },
    /// Orphans and stale labs on the host.
    ScanHost,
    ReapOrphans,
    Restart {
        lab: String,
        node: String,
    },
    Snapshot {
        lab: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    DeleteSnapshot {
        lab: String,
        name: String,
    },
    Restore {
        lab: String,
        name: String,
        #[serde(default)]
        dry_run: bool,
    },
    RunScenario {
        lab: String,
        name: String,
    },
    RunBenchmark {
        lab: String,
        name: String,
    },
    RunAssertions {
        lab: String,
    },
    PullImages {
        topology: serde_json::Value,
    },
    CancelOp {
        op: String,
    },

    // ── runtime changes ──
    Impair {
        lab: String,
        endpoint: String,
        #[serde(default)]
        direction: Direction,
        spec: ImpairmentSpec,
    },
    ClearImpairment {
        lab: String,
        endpoint: String,
    },
    Partition {
        lab: String,
        endpoint: String,
    },
    Heal {
        lab: String,
        endpoint: String,
    },
    /// A network's per-pair impairment from `src` to `dst`; `None` clears it.
    SetPairImpairment {
        lab: String,
        network: String,
        src: String,
        dst: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spec: Option<ImpairmentSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate_cap: Option<String>,
    },
    Spawn {
        lab: String,
        node: String,
        argv: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<WaitSpec>,
    },
    Kill {
        lab: String,
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },

    // ── interactive (streams of data) ──
    /// Run a command; its output streams as data, then `Exited`. With
    /// `stdin`, the client streams input; with `tty`, it runs on a pty.
    Exec {
        lab: String,
        node: String,
        argv: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        #[serde(default)]
        stdin: bool,
        #[serde(default)]
        tty: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// An interactive shell on a pty of `rows` × `cols`.
    Shell {
        lab: String,
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
        rows: u16,
        cols: u16,
    },
    Logs {
        lab: String,
        pid: u32,
        #[serde(default)]
        stream: LogStream,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<usize>,
        #[serde(default)]
        follow: bool,
    },
    /// Capture packets on an endpoint; the pcap streams as data.
    Capture {
        lab: String,
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    WaitFor {
        lab: String,
        node: String,
        cond: WaitSpec,
    },
    /// The lab as an `.nlz` archive, streamed as data.
    Export {
        lab: String,
    },
}

fn yes() -> bool {
    true
}

/// Who may call a method.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Reads only.
    Viewer,
    /// Changes labs, runs commands in them.
    Operator,
}

/// What happens to a call when its client goes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnDisconnect {
    /// It is cancelled (interactive calls: their processes are stopped).
    Cancel,
    /// It goes on; `attach_op` finds it again (lifecycle operations).
    Continue,
}

/// What a method is: who may call it, whether it changes a lab, whether
/// it is a long operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodMeta {
    pub role: Role,
    pub mutating: bool,
    pub long_running: bool,
    pub on_disconnect: OnDisconnect,
}

impl Request {
    /// The method's name: `"list_labs"`, `"deploy"`, …
    pub fn method(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_default()
    }

    /// The lab the request is about, when it is about one.
    pub fn lab(&self) -> Option<&str> {
        use Request::*;
        match self {
            GetLab { lab }
            | GetTopology { lab, .. }
            | ListProcesses { lab }
            | GetImpairments { lab }
            | ListSnapshots { lab }
            | ReadEvents { lab, .. }
            | GetMetrics { lab }
            | Diagnose { lab, .. }
            | NodeAddresses { lab, .. }
            | ProcStat { lab, .. }
            | Stats { lab }
            | Apply { lab, .. }
            | CheckApply { lab, .. }
            | Verify { lab }
            | Destroy { lab }
            | Restart { lab, .. }
            | Snapshot { lab, .. }
            | DeleteSnapshot { lab, .. }
            | Restore { lab, .. }
            | RunScenario { lab, .. }
            | RunBenchmark { lab, .. }
            | RunAssertions { lab }
            | Impair { lab, .. }
            | ClearImpairment { lab, .. }
            | Partition { lab, .. }
            | Heal { lab, .. }
            | SetPairImpairment { lab, .. }
            | Spawn { lab, .. }
            | Kill { lab, .. }
            | Exec { lab, .. }
            | Shell { lab, .. }
            | Logs { lab, .. }
            | Capture { lab, .. }
            | WaitFor { lab, .. }
            | Export { lab } => Some(lab),
            Subscribe { lab, .. } => lab.as_deref(),
            Ping
            | WhoAmI
            | DaemonStatus
            | ListLabs
            | ListOps
            | AttachOp { .. }
            | Deploy { .. }
            | PlanDeploy { .. }
            | ScanHost
            | ReapOrphans
            | PullImages { .. }
            | CancelOp { .. } => None,
        }
    }

    /// Who may call it and how it behaves. Exhaustive on purpose: a new
    /// request does not compile without its role.
    pub fn meta(&self) -> MethodMeta {
        use Request::*;
        let read = MethodMeta {
            role: Role::Viewer,
            mutating: false,
            long_running: false,
            on_disconnect: OnDisconnect::Cancel,
        };
        let change = MethodMeta {
            role: Role::Operator,
            mutating: true,
            ..read
        };
        let operation = MethodMeta {
            long_running: true,
            on_disconnect: OnDisconnect::Continue,
            ..change
        };
        let interactive = MethodMeta {
            role: Role::Operator,
            mutating: false,
            long_running: true,
            on_disconnect: OnDisconnect::Cancel,
        };
        match self {
            Ping
            | WhoAmI
            | DaemonStatus
            | ListLabs
            | GetLab { .. }
            | ListProcesses { .. }
            | GetImpairments { .. }
            | ListSnapshots { .. }
            | ReadEvents { .. }
            | GetMetrics { .. }
            | Diagnose { .. }
            | NodeAddresses { .. }
            | ProcStat { .. }
            | Stats { .. }
            | ListOps
            | PlanDeploy { .. }
            | CheckApply { .. }
            | Verify { .. }
            | ScanHost => read,
            // the unredacted topology holds secrets
            GetTopology { redact, .. } => MethodMeta {
                role: if *redact {
                    Role::Viewer
                } else {
                    Role::Operator
                },
                ..read
            },
            AttachOp { .. } | Subscribe { .. } => MethodMeta {
                long_running: true,
                ..read
            },
            Deploy { .. }
            | Apply { .. }
            | Destroy { .. }
            | Restart { .. }
            | Restore { .. }
            | RunScenario { .. }
            | RunBenchmark { .. }
            | RunAssertions { .. }
            | PullImages { .. }
            | ReapOrphans => operation,
            Snapshot { .. }
            | DeleteSnapshot { .. }
            | CancelOp { .. }
            | Impair { .. }
            | ClearImpairment { .. }
            | Partition { .. }
            | Heal { .. }
            | SetPairImpairment { .. }
            | Spawn { .. }
            | Kill { .. } => change,
            // a wait's `exec` condition runs a command
            Exec { .. } | Shell { .. } | Capture { .. } | Export { .. } | WaitFor { .. } => {
                interactive
            }
            // the logs are readable in the state root by the group anyway
            Logs { .. } => MethodMeta {
                role: Role::Viewer,
                ..interactive
            },
        }
    }
}
