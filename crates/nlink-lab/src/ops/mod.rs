//! Operations: what the CLI, the daemon and the desktop app all run, with
//! one context for their progress and cancellation (#236).

mod ctx;

pub use ctx::{CancelToken, OpCtx, ProgressSink};
