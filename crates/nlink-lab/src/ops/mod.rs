//! Operations: what the CLI, the daemon and the desktop app all run, with
//! one context for their progress and cancellation (#236), and the views
//! and read calls they share (#233).

mod ctx;
pub mod read;
pub mod view;

pub use ctx::{CancelToken, OpCtx, ProgressSink};
