//! Process exit-code policy and the `--json` error envelope.

/// Process exit status chosen by a subcommand that completed its work
/// but wants a non-zero status (a child's exit code, an assertion
/// failure, …). `cmd::dispatch` keeps returning `Result<()>`; arms call
/// [`set_exit_code`] instead of `std::process::exit` so buffered
/// output and destructors still run.
pub static EXIT_CODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Exit codes: 0 ok · 1 error · 2 validation / assertion / drift
/// failure · 124 timeout · `exec`/`shell` pass the child's code through.
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_VALIDATION: u8 = 2;
pub const EXIT_TIMEOUT: u8 = 124;

pub fn set_exit_code(code: u8) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
}

pub fn exit_code_for(err: &nlink_lab::Error) -> u8 {
    match err {
        nlink_lab::Error::Validation(_)
        | nlink_lab::Error::ValidationErrors(_)
        | nlink_lab::Error::InvalidTopology(_) => EXIT_VALIDATION,
        nlink_lab::Error::Timeout(_) => EXIT_TIMEOUT,
        _ => EXIT_FAILURE,
    }
}

/// Plan 158b Phase 3 — render an `nlink_lab::Error` as a
/// structured JSON envelope for the `--json` paths. Walks the
/// `std::error::Error::source` chain to build `error_chain`,
/// and surfaces `errno` / `ext_ack` / `ext_ack_offset` from any
/// `nlink::Error::Kernel` / `KernelWithContext` found in the
/// chain (via the inherent accessors added in Plan 158b Phase 1).
pub fn render_error_json(err: &nlink_lab::Error) -> serde_json::Value {
    let mut chain: Vec<String> = Vec::new();
    let mut src: &dyn std::error::Error = err;
    loop {
        chain.push(src.to_string());
        match src.source() {
            Some(next) => src = next,
            None => break,
        }
    }
    let mut envelope = serde_json::json!({
        "error": err.to_string(),
        "error_chain": chain,
        "errno": err.errno(),
        "ext_ack": err.ext_ack(),
        "ext_ack_offset": err.ext_ack_offset(),
        "exit_code": exit_code_for(err),
    });
    // Validation failures carry the structured issues so `--json`
    // consumers do not have to scrape "see errors above".
    if let nlink_lab::Error::ValidationErrors(issues) = err {
        envelope["issues"] = serde_json::to_value(issues).unwrap_or(serde_json::Value::Null);
    }
    envelope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_policy() {
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Validation("x".into())),
            EXIT_VALIDATION
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Timeout(std::time::Duration::from_secs(
                1
            ))),
            EXIT_TIMEOUT
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::NotFound { name: "l".into() }),
            EXIT_FAILURE
        );
    }
}
