//! Zenoh message types for nlink-lab pub/sub and query/reply.
//!
//! All fields are `#[serde(default)]` and the top-level messages carry a
//! `wire_version` (see the crate docs for the compatibility contract).
//! The `Default` impls of versioned messages stamp
//! [`WIRE_VERSION`](crate::WIRE_VERSION) so `Msg { field, ..Default::default() }`
//! produces a correctly-tagged message; a *decoded* message whose sender
//! omitted the field reads `0`.

use serde::{Deserialize, Serialize};

use crate::WIRE_VERSION;

/// Implements `Default` with `wire_version: WIRE_VERSION` and every other
/// listed field at its type's default.
macro_rules! versioned_default {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl Default for $ty {
            fn default() -> Self {
                Self {
                    wire_version: WIRE_VERSION,
                    $($field: Default::default(),)*
                }
            }
        }
    };
}

// ─── Pub/Sub messages (backend → clients) ────────────

/// Full topology update (published on startup and changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyUpdate {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub lab_name: String,
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub link_count: usize,
    /// Serialized topology (JSON).
    #[serde(default)]
    pub topology_json: String,
}

versioned_default!(TopologyUpdate {
    lab_name,
    timestamp,
    node_count,
    link_count,
    topology_json,
});

/// Backend health/liveness status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub lab_name: String,
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub namespace_count: usize,
    #[serde(default)]
    pub container_count: usize,
    #[serde(default)]
    pub pid_count: usize,
    #[serde(default)]
    pub uptime_secs: u64,
}

versioned_default!(HealthStatus {
    lab_name,
    timestamp,
    node_count,
    namespace_count,
    container_count,
    pid_count,
    uptime_secs,
});

/// Lab event (interface state change, process exit).
///
/// `kind` has no sensible default, so it is the one wire field that is
/// required; everything else defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabEvent {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub lab_name: String,
    #[serde(default)]
    pub timestamp: u64,
    pub kind: LabEventKind,
}

impl LabEvent {
    /// Build an event stamped with the current [`WIRE_VERSION`].
    pub fn new(lab_name: impl Into<String>, timestamp: u64, kind: LabEventKind) -> Self {
        Self {
            wire_version: WIRE_VERSION,
            lab_name: lab_name.into(),
            timestamp,
            kind,
        }
    }
}

/// Lab event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LabEventKind {
    InterfaceUp {
        #[serde(default)]
        node: String,
        #[serde(default)]
        interface: String,
    },
    InterfaceDown {
        #[serde(default)]
        node: String,
        #[serde(default)]
        interface: String,
    },
    /// A background process tracked by the lab (`nlink-lab spawn` or an
    /// `exec` block) was alive at the previous sample and is gone now.
    ProcessExited {
        #[serde(default)]
        node: String,
        #[serde(default)]
        pid: u32,
        /// Exit status when the daemon could collect it (it is the
        /// process's parent — e.g. `deploy --daemon`); a signal death is
        /// reported shell-style as `128 + signo`. `None` when the process
        /// was reaped by someone else, so no status was observable.
        #[serde(default)]
        exit_code: Option<i32>,
    },
}

// ─── Query/Reply messages (clients → backend) ────────

/// Request to execute a command in a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecRequest {
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Response from command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

versioned_default!(ExecResponse {
    success,
    exit_code,
    stdout,
    stderr,
});

/// Request to modify impairment on an interface.
///
/// Netem knobs that are `None` are left unset on the interface. A
/// request with every knob `None` and `clear == false` is rejected by
/// the backend rather than guessed at — set `clear` to remove the
/// impairment entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentRequest {
    /// Wire-format version of the sender (`0` = pre-versioning client).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub interface: String,
    #[serde(default)]
    pub delay: Option<String>,
    #[serde(default)]
    pub jitter: Option<String>,
    #[serde(default)]
    pub loss: Option<String>,
    /// Bandwidth cap (e.g. `"100mbit"`), applied as netem `rate`.
    #[serde(default)]
    pub rate: Option<String>,
    #[serde(default)]
    pub corrupt: Option<String>,
    #[serde(default)]
    pub reorder: Option<String>,
    /// Remove the impairment on `node:interface` instead of setting one.
    /// The netem knobs are ignored when this is `true`.
    #[serde(default)]
    pub clear: bool,
}

versioned_default!(ImpairmentRequest {
    node,
    interface,
    delay,
    jitter,
    loss,
    rate,
    corrupt,
    reorder,
    clear,
});

impl ImpairmentRequest {
    /// `true` when no netem knob is set (regardless of `clear`).
    pub fn is_empty(&self) -> bool {
        self.delay.is_none()
            && self.jitter.is_none()
            && self.loss.is_none()
            && self.rate.is_none()
            && self.corrupt.is_none()
            && self.reorder.is_none()
    }
}

/// Response from impairment modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpairmentResponse {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub message: String,
}

versioned_default!(ImpairmentResponse { success, message });

/// Lab status response. The status RPC takes no request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Wire-format version of the sender (`0` = pre-versioning backend).
    #[serde(default)]
    pub wire_version: u32,
    #[serde(default)]
    pub lab_name: String,
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub namespace_count: usize,
    #[serde(default)]
    pub container_count: usize,
    #[serde(default)]
    pub uptime_secs: u64,
    #[serde(default)]
    pub nodes: Vec<String>,
}

versioned_default!(StatusResponse {
    lab_name,
    node_count,
    namespace_count,
    container_count,
    uptime_secs,
    nodes,
});

#[cfg(test)]
mod tests {
    use super::*;

    /// A document from a backend that predates every optional field
    /// (`{}`) still decodes, with `wire_version == 0`.
    #[test]
    fn empty_documents_deserialize_with_defaults() {
        let t: TopologyUpdate = serde_json::from_str("{}").unwrap();
        assert_eq!(t.wire_version, 0);
        assert!(t.lab_name.is_empty());
        assert_eq!(t.node_count, 0);

        let h: HealthStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(h.wire_version, 0);
        assert_eq!(h.container_count, 0);

        let e: ExecResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(e.wire_version, 0);
        assert!(!e.success);

        let r: ExecRequest = serde_json::from_str("{}").unwrap();
        assert!(r.args.is_empty());

        let i: ImpairmentRequest = serde_json::from_str("{}").unwrap();
        assert!(i.is_empty());
        assert!(!i.clear);
        assert!(i.rate.is_none());

        let ir: ImpairmentResponse = serde_json::from_str("{}").unwrap();
        assert!(ir.message.is_empty());

        let s: StatusResponse = serde_json::from_str("{}").unwrap();
        assert!(s.nodes.is_empty());
    }

    /// `LabEvent` requires only `kind`; the variant payload fields default.
    #[test]
    fn lab_event_deserializes_with_only_kind() {
        let ev: LabEvent = serde_json::from_str(r#"{"kind":{"type":"InterfaceUp"}}"#).unwrap();
        assert_eq!(ev.wire_version, 0);
        assert!(matches!(ev.kind, LabEventKind::InterfaceUp { .. }));

        let ev: LabEvent =
            serde_json::from_str(r#"{"kind":{"type":"ProcessExited","node":"a","pid":7}}"#)
                .unwrap();
        match ev.kind {
            LabEventKind::ProcessExited {
                node,
                pid,
                exit_code,
            } => {
                assert_eq!(node, "a");
                assert_eq!(pid, 7);
                assert_eq!(exit_code, None);
            }
            other => panic!("unexpected kind {other:?}"),
        }

        assert!(serde_json::from_str::<LabEvent>("{}").is_err());
    }

    /// Fields a newer sender adds are ignored by an older receiver.
    #[test]
    fn unknown_fields_are_ignored() {
        let h: HealthStatus =
            serde_json::from_str(r#"{"lab_name":"x","future_field":{"a":[1,2]}}"#).unwrap();
        assert_eq!(h.lab_name, "x");

        let i: ImpairmentRequest =
            serde_json::from_str(r#"{"node":"r","interface":"eth0","duplicate":"1%"}"#).unwrap();
        assert_eq!(i.node, "r");

        let ev: LabEvent = serde_json::from_str(
            r#"{"kind":{"type":"InterfaceDown","node":"a","interface":"eth0","reason":"carrier"},"extra":true}"#,
        )
        .unwrap();
        assert!(matches!(ev.kind, LabEventKind::InterfaceDown { .. }));
    }

    /// `Default` stamps the current version; serialization carries it.
    #[test]
    fn defaults_stamp_current_wire_version() {
        let s = StatusResponse {
            lab_name: "lab".into(),
            ..Default::default()
        };
        assert_eq!(s.wire_version, WIRE_VERSION);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(&format!("\"wire_version\":{WIRE_VERSION}")));

        let ev = LabEvent::new(
            "lab",
            1,
            LabEventKind::InterfaceUp {
                node: "a".into(),
                interface: "eth0".into(),
            },
        );
        assert_eq!(ev.wire_version, WIRE_VERSION);
        assert_eq!(ExecResponse::default().wire_version, WIRE_VERSION);
        assert_eq!(ImpairmentRequest::default().wire_version, WIRE_VERSION);
    }

    #[test]
    fn impairment_request_rate_and_clear_round_trip() {
        let req = ImpairmentRequest {
            node: "r".into(),
            interface: "eth0".into(),
            rate: Some("100mbit".into()),
            ..Default::default()
        };
        assert!(!req.is_empty());
        let json = serde_json::to_string(&req).unwrap();
        let back: ImpairmentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rate.as_deref(), Some("100mbit"));
        assert!(!back.clear);

        let clear: ImpairmentRequest =
            serde_json::from_str(r#"{"node":"r","interface":"eth0","clear":true}"#).unwrap();
        assert!(clear.clear);
        assert!(clear.is_empty());
    }
}
