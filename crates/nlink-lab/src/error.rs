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

    /// An nlink call failed while nlink-lab was doing `context` (#177).
    /// nlink's error is the source, so `--json` reports its kind, errno,
    /// extended ack, object and namespace, which a message formatted
    /// into a string lost. Build it with [`Error::nlink`].
    #[error("{context}: {source}")]
    NlinkOp {
        context: String,
        #[source]
        source: nlink::Error,
    },

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

    /// NLL parse error with the byte span it was raised at. Turned into
    /// an [`NllDiagnostic`] (with the source attached) by the parse entry
    /// points; the span is data, not text to re-parse. `span.start` is
    /// the offset, `span.end - span.start` the width of the offending
    /// token (0 at end of input).
    #[error("NLL parse error: {message}")]
    NllParseAt {
        message: String,
        span: std::ops::Range<usize>,
    },

    /// NLL parse error with source context for rich diagnostics.
    #[error("{}", .0)]
    NllDiagnostic(Box<NllDiagnostic>),

    /// Invalid topology file.
    #[error("invalid topology: {0}")]
    InvalidTopology(String),

    /// A command-line argument the caller got wrong (`--set noequals`, an
    /// unknown template, a bad regex). The CLI exits 1 for it, as for any
    /// other error; it used to be an `InvalidTopology`, exit 2 (#209).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Namespace operation failed. Plan 158b — the underlying
    /// `nlink::Error` is preserved on `#[source]` so
    /// `err.ext_ack()` walks through to the kernel's
    /// `NLMSGERR_ATTR_MSG` text.
    #[error("{op} namespace '{ns}'")]
    Namespace {
        op: &'static str,
        ns: String,
        #[source]
        source: nlink::Error,
    },

    /// Packet capture error.
    #[error("capture failed: {0}")]
    Capture(String),

    /// Generic deploy failure (for cases that don't fit specific variants).
    /// A failure described by its message alone. Named for the deploy, its
    /// first user; it is every command's now, so it prints only the
    /// message (#177).
    #[error("{0}")]
    DeployFailed(String),

    /// Command exceeded its `--timeout`. The CLI maps this to exit
    /// code 124, matching `coreutils timeout(1)`. Surfaced from
    /// `RunningLab::exec_with_opts` and `exec_attached_with_opts`
    /// when `ExecOpts::timeout` is set and elapses before the child
    /// exits.
    #[error("command timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// A wait gave up at its deadline: `wait`, `wait-for`, `spawn
    /// --wait-*`. The message says what it waited for. The CLI maps it to
    /// exit code 124, like [`Error::Timeout`].
    #[error("{0}")]
    TimedOut(String),

    /// State file error.
    #[error("{op} state: {detail} (path: {path})")]
    State {
        op: &'static str,
        detail: String,
        path: PathBuf,
    },

    /// Another process holds the lab's lock: a deploy, apply or destroy
    /// is running (#234). It was a "deploy failed".
    #[error("lab '{name}' is locked by another process")]
    Locked { name: String },

    /// The lab is busy with another operation and cannot take this one now.
    #[error("{0}")]
    Busy(String),

    /// The operation was cancelled; what it had done was rolled back.
    #[error("cancelled: {0}")]
    Cancelled(String),

    /// The state root (or a file in it) cannot be read or written by this
    /// user (#234). `hint` says what to do: run as root, or join the
    /// `nlink-lab` group to read.
    #[error("permission denied: {path}: {hint}")]
    PermissionDenied { path: PathBuf, hint: String },

    /// The operation needs privileges this process does not have: root,
    /// or CAP_NET_ADMIN + CAP_SYS_ADMIN.
    #[error("{0}")]
    Privileges(String),

    /// The container runtime (docker or podman) failed or is missing.
    #[error("container runtime: {0}")]
    Runtime(String),

    /// Something nlink-lab does not do (yet), or not for this kind of node.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The lab's state is in a per-user root an older nlink-lab used
    /// (#234): `doctor --fix` moves it into the state root.
    #[error(
        "lab '{name}' is in the old state directory {path}: run `sudo nlink-lab doctor --fix` to move it"
    )]
    MovedState { name: String, path: PathBuf },

    /// What the daemon answered a call with (#233): reported as it is,
    /// exit code included.
    #[error("{0}")]
    Api(Box<nlink_lab_shared::api::ApiError>),
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

impl From<NllDiagnostic> for Error {
    fn from(diag: NllDiagnostic) -> Self {
        Self::NllDiagnostic(Box::new(diag))
    }
}

/// Plan 158c — route bare `IpAddr` parse failures from the `?`
/// operator into [`Error::InvalidTopology`]. The previous
/// `.map_err(|e| Error::invalid_topology(format!("…: {e}")))`
/// ceremony at every parse site collapses to a bare `?` when
/// the surrounding `Result<_, Error>` context is descriptive
/// enough (e.g. inside a function whose name is itself the
/// context).
impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Self {
        Self::InvalidTopology(format!("invalid IP address: {e}"))
    }
}

/// Same shape for integer parses (port numbers, prefix
/// lengths, mark values, etc.).
impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Self::InvalidTopology(format!("invalid integer: {e}"))
    }
}

impl Error {
    /// An NLL parse error pointing at a byte span of the source.
    pub fn at(span: std::ops::Range<usize>, message: impl Into<String>) -> Self {
        Error::NllParseAt {
            message: message.into(),
            span,
        }
    }

    /// Create an invalid topology error.
    pub fn invalid_topology(message: impl Into<String>) -> Self {
        Self::InvalidTopology(message.into())
    }

    /// Create an invalid argument error.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    /// Create a deploy failed error (generic catch-all).
    pub fn deploy_failed(message: impl Into<String>) -> Self {
        Self::DeployFailed(message.into())
    }

    /// An nlink error, with what nlink-lab was doing when it happened
    /// (`"failed to apply nftables config on 'b'"`). Prefer it to
    /// formatting the error into [`Self::deploy_failed`], which keeps only
    /// its text (#177).
    pub fn nlink(context: impl Into<String>, source: nlink::Error) -> Self {
        Self::NlinkOp {
            context: context.into(),
            source,
        }
    }

    /// Check if this is a "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. } | Self::NodeNotFound { .. })
    }

    /// Walk the source chain looking for the first
    /// `nlink::Error`. Returns `None` if no kernel error is in
    /// the chain (e.g. `Self::Validation`,
    /// `Self::InvalidTopology`).
    ///
    /// Once the first `nlink::Error` is located, the
    /// `ext_ack`/`errno`/`ext_ack_offset` accessors below
    /// delegate to `nlink::Error::root_cause()` (Plan 187 §2.2
    /// upstream) to drill to the deepest kernel layer — which
    /// transparently unwraps `Box<nlink::Error>` sources, side-
    /// stepping the `downcast_ref` trap described in
    /// `docs/upstream/nlink-feedback.md` item #4.
    /// The first nlink error in this error's source chain, if any.
    pub fn nlink_error(&self) -> Option<&nlink::Error> {
        self.first_nlink_error()
    }

    fn first_nlink_error(&self) -> Option<&nlink::Error> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                return Some(e);
            }
            src = src.source()?;
        }
    }

    /// Return the kernel `NLMSGERR_ATTR_MSG` ext_ack string
    /// from this error's source chain, if any.
    ///
    /// Plan 158b — the typed-source variants
    /// ([`Self::Namespace`], [`Self::Nlink`]) carry the
    /// underlying `nlink::Error` on `#[source]`, so the
    /// accessor finds it even when the top-level error is one
    /// of our wrapper variants. For (still legacy) stringified
    /// call sites that route through [`Self::DeployFailed`],
    /// `ext_ack` is no longer recoverable — its text is
    /// flattened into the human-readable string at
    /// construction time.
    ///
    /// Plan 159f — delegates to `nlink::Error::root_cause()` so
    /// the deepest kernel layer wins, and `Box<nlink::Error>`
    /// sources are unwrapped transparently.
    pub fn ext_ack(&self) -> Option<&str> {
        self.first_nlink_error()?.root_cause().ext_ack()
    }

    /// Companion to [`Self::ext_ack`] — returns the offset
    /// (if any) into the request payload where the kernel
    /// said the rejected attribute lives.
    pub fn ext_ack_offset(&self) -> Option<u32> {
        self.first_nlink_error()?.root_cause().ext_ack_offset()
    }

    /// This error as the daemon API reports it (#233), with the same exit
    /// code the CLI uses: 2 for a topology that does not parse or
    /// validate, 124 for a timeout, 130 for a cancel, 1 otherwise.
    pub fn to_api(&self) -> nlink_lab_shared::api::ApiError {
        use nlink_lab_shared::api::{ApiError, ErrorCode};
        let code = match self {
            Error::Api(api) => return (**api).clone(),
            Error::Io(e) => match e.kind() {
                std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
                std::io::ErrorKind::NotFound => ErrorCode::NotFound,
                _ => ErrorCode::Io,
            },
            Error::Json(_) | Error::DeployFailed(_) | Error::Runtime(_) => ErrorCode::Internal,
            Error::Nlink(_) | Error::NlinkOp { .. } | Error::Namespace { .. } => ErrorCode::Kernel,
            Error::Validation(_)
            | Error::ValidationErrors(_)
            | Error::InvalidTopology(_)
            | Error::NllParse(_)
            | Error::NllParseAt { .. }
            | Error::NllDiagnostic(_) => ErrorCode::Validation,
            Error::AlreadyExists { .. } => ErrorCode::AlreadyExists,
            Error::NotFound { .. } | Error::NodeNotFound { .. } | Error::MovedState { .. } => {
                ErrorCode::NotFound
            }
            Error::InvalidEndpoint { .. } | Error::InvalidArgument(_) => ErrorCode::InvalidArgument,
            Error::Capture(_) | Error::State { .. } => ErrorCode::Io,
            Error::Timeout(_) | Error::TimedOut(_) => ErrorCode::Timeout,
            Error::Locked { .. } => ErrorCode::Locked,
            Error::Busy(_) => ErrorCode::Busy,
            Error::Cancelled(_) => ErrorCode::Cancelled,
            Error::PermissionDenied { .. } | Error::Privileges(_) => ErrorCode::PermissionDenied,
            Error::Unsupported(_) => ErrorCode::Unsupported,
        };
        let mut api = ApiError::new(code, self.to_string());
        let mut src: &dyn std::error::Error = self;
        loop {
            api.chain.push(src.to_string());
            match src.source() {
                Some(next) => src = next,
                None => break,
            }
        }
        api.errno = self.errno();
        api.ext_ack = self.ext_ack().map(str::to_string);
        api.netns = self
            .nlink_error()
            .and_then(|e| e.netns().or(e.root_cause().netns()))
            .map(str::to_string);
        api.hint = match self {
            Error::PermissionDenied { hint, .. } => Some(hint.clone()),
            Error::MovedState { .. } => Some("sudo nlink-lab doctor --fix".into()),
            _ => None,
        };
        match self {
            Error::ValidationErrors(issues) => api.issues = serde_json::to_value(issues).ok(),
            // where in the file, as one issue shaped like the validator's
            Error::NllDiagnostic(d) => {
                let text = d.src.inner();
                let offset = d.span.offset().min(text.len());
                let before = &text[..offset];
                let line = before.matches('\n').count() + 1;
                let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
                api.message = d.message.clone();
                api.issues = Some(serde_json::json!([{
                    "severity": "error",
                    "rule": "parse",
                    "message": d.message,
                    "location": format!("{}:{line}:{column}", d.src.name()),
                }]));
                api.hint = d.help.clone();
            }
            _ => {}
        }
        // What nlink says about a netlink failure (#177): what kind of
        // failure, the object it concerns, and nlink's own context,
        // innermost last.
        api.nlink = self.nlink_error().map(|e| {
            let root = e.root_cause();
            serde_json::json!({
                "kind": serde_json::to_value(root.kind()).ok(),
                "object": root.object().and_then(|o| serde_json::to_value(o).ok()),
                "context": e.context().to_vec(),
                "ext_ack_offset": self.ext_ack_offset(),
            })
        });
        api
    }

    /// Return the kernel errno from the source chain, if any.
    /// nlink 0.19 normalizes errno via `.abs()`, so the value
    /// is always the positive errno number regardless of how
    /// the underlying `nlink::Error` was constructed.
    pub fn errno(&self) -> Option<i32> {
        self.first_nlink_error()?.root_cause().errno()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_api_codes_and_exit_codes() {
        use nlink_lab_shared::api::ErrorCode;
        let api = Error::NotFound { name: "x".into() }.to_api();
        assert_eq!((api.code, api.exit_code), (ErrorCode::NotFound, 1));
        assert_eq!(api.message, "lab not found: x");
        let v = Error::Validation("bad".into()).to_api();
        assert_eq!((v.code, v.exit_code), (ErrorCode::Validation, 2));
        let t = Error::TimedOut("waited".into()).to_api();
        assert_eq!(t.exit_code, 124);
        let c = Error::Cancelled("deploy".into()).to_api();
        assert_eq!(c.exit_code, 130);
        let m = Error::MovedState {
            name: "x".into(),
            path: "/old".into(),
        }
        .to_api();
        assert!(m.hint.unwrap().contains("doctor --fix"));
        let kernel = nlink::Error::from_errno_ext_ack(16, Some("busy".into()), None);
        let k = Error::nlink("apply nftables on 'b'", kernel).to_api();
        assert_eq!(k.code, ErrorCode::Kernel);
        assert_eq!(k.errno, Some(16));
        assert_eq!(k.ext_ack.as_deref(), Some("busy"));
        assert!(k.chain.len() >= 2, "{:?}", k.chain);
    }

    /// An nlink failure keeps its errno and extended ack under the
    /// context it happened in, and prints no "deploy failed" (#177).
    #[test]
    fn nlink_op_keeps_the_kernel_error() {
        let kernel = nlink::Error::from_errno_ext_ack(16, Some("busy".into()), Some(36));
        let e = Error::nlink("failed to apply nftables config on 'b'", kernel);
        assert_eq!(e.errno(), Some(16));
        assert_eq!(e.ext_ack(), Some("busy"));
        assert_eq!(e.ext_ack_offset(), Some(36));
        let text = e.to_string();
        assert!(
            text.starts_with("failed to apply nftables config on 'b': "),
            "{text}"
        );
        assert!(
            !Error::deploy_failed("x")
                .to_string()
                .contains("deploy failed")
        );
    }

    #[test]
    fn ext_ack_walks_through_namespace_variant() {
        // 0.19 normalizes errno via `.abs()`, so `errno()` returns
        // the positive errno regardless of input sign.
        let kernel = nlink::Error::from_errno_ext_ack(
            1,
            Some("netlink: Could not process attribute".into()),
            Some(16),
        );
        let lab_err = Error::Namespace {
            op: "create",
            ns: "ns-test".into(),
            source: kernel,
        };
        assert_eq!(lab_err.errno(), Some(1));
        assert_eq!(
            lab_err.ext_ack(),
            Some("netlink: Could not process attribute")
        );
        assert_eq!(lab_err.ext_ack_offset(), Some(16));
    }

    #[test]
    fn ext_ack_walks_through_nlink_from_variant() {
        let kernel =
            nlink::Error::from_errno_ext_ack(17, Some("netlink: duplicate link".into()), None);
        let lab_err: Error = kernel.into();
        assert_eq!(lab_err.errno(), Some(17));
        assert_eq!(lab_err.ext_ack(), Some("netlink: duplicate link"));
    }

    #[test]
    fn ext_ack_none_when_no_kernel_in_chain() {
        let lab_err = Error::Validation("bad name".into());
        assert_eq!(lab_err.ext_ack(), None);
        assert_eq!(lab_err.errno(), None);
        assert_eq!(lab_err.ext_ack_offset(), None);
    }

    #[test]
    fn ext_ack_none_for_legacy_deploy_failed_string() {
        // DeployFailed flattens the source at construction; the
        // typed chain is lost. Documented limitation — callers
        // should prefer typed-source variants for new code.
        let lab_err = Error::deploy_failed("apply firewall failed: kernel error EPERM");
        assert_eq!(lab_err.ext_ack(), None);
    }

    #[test]
    fn from_addr_parse_error_routes_to_invalid_topology() {
        let parse_err = "not-an-ip".parse::<std::net::IpAddr>().unwrap_err();
        let lab_err: Error = parse_err.into();
        assert!(matches!(lab_err, Error::InvalidTopology(_)));
        let rendered = lab_err.to_string();
        assert!(
            rendered.contains("invalid IP address"),
            "expected 'invalid IP address' prefix, got: {rendered}"
        );
    }

    #[test]
    fn from_parse_int_error_routes_to_invalid_topology() {
        let parse_err = "abc".parse::<u32>().unwrap_err();
        let lab_err: Error = parse_err.into();
        assert!(matches!(lab_err, Error::InvalidTopology(_)));
        let rendered = lab_err.to_string();
        assert!(
            rendered.contains("invalid integer"),
            "expected 'invalid integer' prefix, got: {rendered}"
        );
    }

    #[test]
    fn question_mark_propagates_addr_parse_error() {
        // Verify the `?` operator works in a fn returning
        // Result<_, Error> — the documented use case.
        fn parse_one(s: &str) -> Result<std::net::IpAddr> {
            Ok(s.parse()?)
        }
        let err = parse_one("not-an-ip").unwrap_err();
        assert!(matches!(err, Error::InvalidTopology(_)));
    }

    #[test]
    fn display_includes_source_chain_text() {
        let kernel = nlink::Error::from_errno_ext_ack(1, Some("netlink: foo".into()), None);
        // The wire format of the kernel error already includes
        // ext_ack via its Display impl (Plan 182 in nlink). When
        // wrapped, our wrapper renders the variant context and
        // the source chain is reachable via std::error::Error.
        let lab_err = Error::Namespace {
            op: "create",
            ns: "ns-test".into(),
            source: kernel,
        };
        let rendered = format!("{lab_err}");
        assert!(
            rendered.contains("create namespace 'ns-test'"),
            "wrapper context expected in Display: {rendered}"
        );
    }

    /// Plan 159f — guard against the Box<nlink::Error> source
    /// trap. Today no nlink-lab variant boxes its source, but
    /// `root_cause` (via upstream `chain_walk`) transparently
    /// unwraps boxed sources, so a future refactor that boxes
    /// (e.g. to mitigate `result_large_err`) would not regress
    /// `ext_ack`/`errno` retrieval. We construct the boxed
    /// chain by wrapping the kernel error in `Error::Nlink` —
    /// `thiserror`'s `#[from] nlink::Error` field gives us a
    /// chain that `first_nlink_error` walks in one step.
    #[test]
    fn root_cause_drills_through_nlink_chain_to_kernel_layer() {
        let kernel =
            nlink::Error::from_errno_ext_ack(42, Some("netlink: deep ext_ack".into()), Some(8));
        let lab_err: Error = kernel.into();
        // first_nlink_error walks to the nlink::Error layer;
        // root_cause then drills (transparently unboxing) to
        // the deepest kernel-bearing layer.
        assert_eq!(lab_err.errno(), Some(42));
        assert_eq!(lab_err.ext_ack(), Some("netlink: deep ext_ack"));
        assert_eq!(lab_err.ext_ack_offset(), Some(8));
    }

    /// Same as above but through the typed `Error::Namespace`
    /// variant — confirms both code paths (From-conversion and
    /// `#[source]` field) reach the kernel layer via
    /// `root_cause`.
    #[test]
    fn root_cause_drills_through_namespace_variant() {
        let kernel =
            nlink::Error::from_errno_ext_ack(13, Some("netlink: namespace wrap".into()), None);
        let lab_err = Error::Namespace {
            op: "create",
            ns: "rc-test".into(),
            source: kernel,
        };
        assert_eq!(lab_err.errno(), Some(13));
        assert_eq!(lab_err.ext_ack(), Some("netlink: namespace wrap"));
    }
}
