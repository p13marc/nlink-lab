//! Error types for nlink-lab operations.

use std::path::PathBuf;

/// Result type for nlink-lab operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during lab operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML parsing error.
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Underlying nlink error.
    #[error("netlink error: {0}")]
    Nlink(#[from] nlink::Error),

    /// Topology validation failed.
    #[error("validation failed: {0}")]
    Validation(String),

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

    /// Deploy failed.
    #[error("deploy failed: {0}")]
    DeployFailed(String),

    /// State file error.
    #[error("state error: {message} (path: {path})")]
    State { message: String, path: PathBuf },
}

/// Rich NLL parse error with source context for miette rendering.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{message}")]
pub struct NllDiagnostic {
    /// Error message.
    pub message: String,

    /// Source code for context.
    #[source_code]
    pub src: miette::NamedSource<String>,

    /// Span pointing to the error location.
    #[label("{label}")]
    pub span: miette::SourceSpan,

    /// Label for the error span.
    pub label: String,

    /// Help text.
    #[help]
    pub help: Option<String>,
}

impl Error {
    /// Create an invalid topology error.
    pub fn invalid_topology(message: impl Into<String>) -> Self {
        Self::InvalidTopology(message.into())
    }

    /// Create a deploy failed error.
    pub fn deploy_failed(message: impl Into<String>) -> Self {
        Self::DeployFailed(message.into())
    }

    /// Check if this is a "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. } | Self::NodeNotFound { .. })
    }
}
