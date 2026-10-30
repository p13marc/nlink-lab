//! The types nlink-lab's daemon and its clients share.
//!
//! - [`api`]: the daemon API (`docs/API.md`): requests and their roles,
//!   responses, stream items and notifications, `OpEvent` progress, the
//!   error document, DTOs, and the unix-socket frame codec.
//! - [`messages`], [`metrics`], [`topics`]: lab events and metrics
//!   snapshots, and the Zenoh topics `nlink-labd` publishes them on.
//!
//! ```text
//! nlink-labd (root)                     clients
//! ├── the API: unix socket, ssh        ├── nlink-lab top --daemon
//! │   dial-stdio, Zenoh bridge         ├── nlink-lab-gui (desktop app)
//! └── Zenoh topics (events, metrics)   └── nlink-lab-client, external tools
//!          │                                     │
//!          └──────── nlink-lab-shared types ─────┘
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

pub mod api;
pub mod messages;
pub mod metrics;
pub mod topics;

/// Current wire-format version stamped into every top-level message.
///
/// `0` on a decoded message means "sent by a peer that predates
/// versioning" — the `wire_version` field was missing and defaulted.
pub const WIRE_VERSION: u32 = 1;
