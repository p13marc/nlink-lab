//! A client of the nlink-lab daemon API (`nlink-labd`, `docs/API.md`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), nlink_lab_client::ClientError> {
//! use nlink_lab_client::{Client, Endpoint};
//! use nlink_lab_shared::api::Request;
//!
//! let client = Client::connect(&Endpoint::from_env(), "my-tool/0.1").await?;
//! let labs = client.list_labs().await?;
//! // a long call streams events before its end
//! let mut call = client.start(Request::Destroy { lab: "wan".into() }).await?;
//! while let Some(event) = call.next().await {
//!     println!("{event:?}");
//! }
//! # Ok(()) }
//! ```
//!
//! One connection carries any number of calls at once: every frame is
//! tagged with its call's id, and a reader task routes each to its
//! [`Call`]. The version is checked once, when the connection opens.

mod call;
mod conn;
mod endpoint;

pub use call::{Call, CallEvent};
pub use conn::{Client, Welcome};
pub use endpoint::{DEFAULT_SOCKET, Endpoint};

use nlink_lab_shared::api::{ApiError, FrameError};

/// What a client call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("bad frame from the daemon: {0}")]
    Frame(#[from] FrameError),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The daemon answered with an error.
    #[error("{0}")]
    Api(Box<ApiError>),
    #[error("the daemon speaks API {server}, this client {client}")]
    VersionMismatch { server: String, client: String },
    #[error("the connection to the daemon closed")]
    Closed,
}

impl ClientError {
    /// The CLI's exit code for it: the daemon's for an API error, 1
    /// otherwise.
    pub fn exit_code(&self) -> u8 {
        match self {
            ClientError::Api(e) => e.exit_code,
            _ => 1,
        }
    }
}
