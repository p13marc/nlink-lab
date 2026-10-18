//! The nlink-lab daemon API (#233): one set of requests, responses and
//! events, carried by two transports, a unix socket (see [`frame`]) and
//! Zenoh. The CLI, the desktop app and other clients speak it through
//! `nlink-lab-client`.
//!
//! Every call is a stream: zero or more [`StreamItem`]s, then one end,
//! the [`Response`] or an [`ApiError`]. A unary call simply has no items.
//!
//! The version is negotiated once, when a connection opens
//! ([`ClientMsg::Hello`] / [`ServerMsg::Welcome`]): a different major
//! version is refused, a newer minor only adds requests and fields
//! (announced as `features`). Messages carry no per-message version.

pub mod dto;
pub mod error;
pub mod event;
pub mod frame;
pub mod request;
pub mod response;

pub use dto::*;
pub use error::{ApiError, ErrorCode};
pub use event::{Notification, OpEvent, OpOutcome, OpPhase, StreamItem, WaitKind};
pub use frame::{ClientMsg, DataStream, Frame, FrameError, FrameKind, MAX_FRAME, ServerMsg};
pub use request::{MethodMeta, OnDisconnect, Request, Role};
pub use response::Response;

/// The API version: `(major, minor)`. A client and a daemon of different
/// majors refuse each other at the handshake.
pub const API_VERSION: (u32, u32) = (1, 0);

/// `API_VERSION` as written in the handshake: `"1.0"`.
pub fn api_version_string() -> String {
    format!("{}.{}", API_VERSION.0, API_VERSION.1)
}

/// The major version of a handshake's `"MAJOR.MINOR"`, if it parses.
pub fn api_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}
