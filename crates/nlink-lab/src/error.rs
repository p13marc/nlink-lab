//! Error types for nlink-lab operations.
#![allow(unused_assignments)] // false positives from thiserror/miette derive macros

use std::path::PathBuf;

/// Result type for nlink-lab operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during lab operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Underlying nlink error.
    #[error("netlink error: {0}")]
    Nlink(#[from] nlink::Error),

    /// Topology validation failed (single message).
    #[error("validation failed: {0}")]
    Validation(String),

    /// Topology validation failed with structured issues.
    #[error("validation failed ({count} error{s})", count = .0.len(), s = if .0.len() == 1 { "" } else { "s" })]
    ValidationErrors(Vec<crate::validator::ValidationIssue>),

    /// Lab already exists.
    #[error("lab already exists: {name}")]
    AlreadyExists { name: String },

    /// Lab not found.
    #[error("lab not found: {name}")]
    NotFound { name: String },

    /// Node not found in topology.
    #[error("node not found: {name}")]
    NodeNotFound { name: String },

    /// Invalid endpoint format.
    #[error("invalid endpoint '{endpoint}': expected 'node:interface' format")]
    InvalidEndpoint { endpoint: String },

    /// NLL parse error (plain message, no source context).
    #[error("NLL parse error: {0}")]
    NllParse(String),

    /// NLL parse error with source context for rich diagnostics.
    #[error("{}", .0)]
    NllDiagnostic(#[from] NllDiagnostic),

    /// Invalid topology file.
    #[error("invalid topology: {0}")]
    InvalidTopology(String),

    /// Namespace operation failed.
    #[error("{op} namespace '{ns}': {detail}")]
    Namespace {
        op: &'static str,
        ns: String,
        detail: String,
    },

    /// Netlink link/address/interface operation failed.
    #[error("{op} on node '{node}': {detail}")]
    NetlinkOp {
        op: String,
        node: String,
        detail: String,
    },

    /// Route configuration failed.
    #[error("add route '{dest}' on node '{node}': {detail}")]
    Route {
        dest: String,
        node: String,
        detail: String,
    },

    /// Firewall (nftables) operation failed.
    #[error("apply firewall on node '{node}': {detail}")]
    Firewall { node: String, detail: String },

    /// Container runtime operation failed.
    #[error("{op} container '{name}': {detail}")]
    Container {
        op: &'static str,
        name: String,
        detail: String,
    },

    /// Generic deploy failure (for cases that don't fit specific variants).
    #[error("deploy failed: {0}")]
    DeployFailed(String),

    /// State file error.
    #[error("{op} state: {detail} (path: {path})")]
    State {
        op: &'static str,
        detail: String,
        path: PathBuf,
    },
}

/// Rich NLL parse error with source context for miette rendering.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{message}")]
pub struct NllDiagnostic {
    pub message: String,
    #[source_code]
    pub src: miette::NamedSource<String>,
    #[label("{label}")]
    pub span: miette::SourceSpan,
    pub label: String,
    #[help]
    pub help: Option<String>,
}

impl Error {
    /// Create an invalid topology error.
    pub fn invalid_topology(message: impl Into<String>) -> Self {
        Self::InvalidTopology(message.into())
    }

    /// Create a deploy failed error (generic catch-all).
    pub fn deploy_failed(message: impl Into<String>) -> Self {
        Self::DeployFailed(message.into())
    }

    /// Check if this is a "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. } | Self::NodeNotFound { .. })
    }
}

