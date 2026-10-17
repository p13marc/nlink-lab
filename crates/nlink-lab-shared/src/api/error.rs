//! The error every call can end with, the same for every transport, and
//! the same as the CLI's `--json` error envelope.

use serde::{Deserialize, Serialize};

/// What kind of failure: what a client branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No such lab, node, process, snapshot.
    NotFound,
    AlreadyExists,
    /// Another operation holds the lab.
    Locked,
    Busy,
    /// A bad request: a malformed argument, an unknown method.
    InvalidArgument,
    /// The topology does not validate.
    Validation,
    Timeout,
    Cancelled,
    /// The caller may not do this.
    PermissionDenied,
    /// Client and daemon speak different API majors.
    VersionMismatch,
    Unsupported,
    /// The kernel refused (netlink, nftables).
    Kernel,
    Io,
    Internal,
}

/// A failed call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApiError {
    pub code: ErrorCode,
    /// The whole message, as the CLI prints it.
    pub message: String,
    /// The error and its causes, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    /// The kernel's errno, when the kernel refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errno: Option<i32>,
    /// The kernel's extended ack text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext_ack: Option<String>,
    /// The namespace the failing call ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netns: Option<String>,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Validation issues, for `Validation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues: Option<serde_json::Value>,
    /// The CLI's exit code for it: 1 error, 2 validation, 124 timeout,
    /// 130 cancelled.
    pub exit_code: u8,
}

impl ApiError {
    /// An error of `code` with `message` and the exit code that goes with it.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let exit_code = match code {
            ErrorCode::Validation => 2,
            ErrorCode::Timeout => 124,
            ErrorCode::Cancelled => 130,
            _ => 1,
        };
        Self {
            code,
            message: message.into(),
            chain: Vec::new(),
            errno: None,
            ext_ack: None,
            netns: None,
            hint: None,
            issues: None,
            exit_code,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}
