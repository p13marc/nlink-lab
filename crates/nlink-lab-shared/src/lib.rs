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

pub mod messages;
pub mod metrics;
pub mod topics;
