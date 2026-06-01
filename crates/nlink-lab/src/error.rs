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
    NllDiagnostic(Box<NllDiagnostic>),

    /// Invalid topology file.
    #[error("invalid topology: {0}")]
    InvalidTopology(String),

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
    #[error("deploy failed: {0}")]
    DeployFailed(String),

    /// Command exceeded its `--timeout`. The CLI maps this to exit
    /// code 124, matching `coreutils timeout(1)`. Surfaced from
    /// `RunningLab::exec_with_opts` and `exec_attached_with_opts`
    /// when `ExecOpts::timeout` is set and elapses before the child
    /// exits.
    #[error("command timed out after {0:?}")]
    Timeout(std::time::Duration),

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
    /// `nlink-feedback.md` item #4.
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
