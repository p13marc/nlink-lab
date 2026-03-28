//! Zenoh message types for nlink-lab pub/sub and query/reply.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::metrics::MetricsSnapshot;

// ─── Pub/Sub messages (backend → clients) ────────────

/// Full topology update (published on startup and changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyUpdate {
    pub lab_name: String,
    pub timestamp: u64,
    pub node_count: usize,
    pub link_count: usize,
    /// Serialized topology (JSON).
    pub topology_json: String,
}

/// Backend health/liveness status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub lab_name: String,
    pub timestamp: u64,
    pub node_count: usize,
    pub namespace_count: usize,
    pub container_count: usize,
    pub pid_count: usize,
    pub uptime_secs: u64,
}

/// Lab event (interface state change, process exit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabEvent {
    pub lab_name: String,
    pub timestamp: u64,
    pub kind: LabEventKind,
}

/// Lab event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LabEventKind {
    InterfaceUp {
        node: String,
        interface: String,
    },
    InterfaceDown {
        node: String,
        interface: String,
    },
    ProcessExited {
        node: String,
        pid: u32,
        exit_code: i32,
    },
}

// ─── Query/Reply messages (clients → backend) ────────

/// Request to execute a command in a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub node: String,
    pub cmd: String,
    pub args: Vec<String>,
}

/// Response from command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Request to modify impairment on an interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentRequest {
    pub node: String,
    pub interface: String,
    pub delay: Option<String>,
    pub jitter: Option<String>,
    pub loss: Option<String>,
    pub corrupt: Option<String>,
    pub reorder: Option<String>,
}

/// Response from impairment modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentResponse {
    pub success: bool,
    pub message: String,
}

/// Request for lab status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {}

/// Lab status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub lab_name: String,
    pub node_count: usize,
    pub namespace_count: usize,
    pub container_count: usize,
    pub uptime_secs: u64,
    pub nodes: Vec<String>,
}
