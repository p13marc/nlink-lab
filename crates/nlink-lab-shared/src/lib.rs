//! Shared types for nlink-lab Zenoh communication.
//!
//! This crate defines the message types and topic helpers used by the
//! nlink-lab backend daemon and its clients (CLI metrics, TopoViewer GUI).
//!
//! # Architecture
//!
//! ```text
//! Frontend (unprivileged)          Backend (CAP_NET_ADMIN)
//! ├── nlink-lab metrics CLI        ├── nlink-lab daemon
//! ├── TopoViewer GUI               │
//! └── External tools               └── Publishes via Zenoh
//!     │                                 │
//!     └── nlink-lab-shared types ───────┘
//! ```
//!
//! # Wire compatibility
//!
//! Every field of every wire struct is `#[serde(default)]`, and every
//! top-level message carries a `wire_version` stamped with
//! [`WIRE_VERSION`] by the sender. Together these give:
//!
//! - **Forward compatibility** — a client built against an older
//!   schema ignores fields it does not know (serde's default
//!   behaviour) and fills fields the sender omitted with their
//!   defaults.
//! - **Backward compatibility** — a newer client decoding a message
//!   from an older backend sees `wire_version == 0` (the field was
//!   absent) and can special-case it.
//!
//! Bump [`WIRE_VERSION`] only for a change an old peer could
//! *misinterpret* (a renamed field, a changed unit); purely additive
//! fields need no bump.

pub mod messages;
pub mod metrics;
pub mod topics;

/// Current wire-format version stamped into every top-level message.
///
/// `0` on a decoded message means "sent by a peer that predates
/// versioning" — the `wire_version` field was missing and defaulted.
pub const WIRE_VERSION: u32 = 1;
