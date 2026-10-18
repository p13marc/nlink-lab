//! Exit codes: 0 ok · 1 error · 2 a negative result (validation, a failed
//! assertion, drift, lint findings) · 64 a usage error · 124 timeout ·
//! 130 interrupted · `exec`/`shell` pass the child's code through.

/// Process exit status chosen by a subcommand that completed its work
/// but wants a non-zero status (a child's exit code, an assertion
/// failure, …). `cmd::dispatch` keeps returning `Result<()>`; arms call
/// [`set_exit_code`] instead of `std::process::exit` so buffered
/// output and destructors still run.
pub static EXIT_CODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_VALIDATION: u8 = 2;
/// A command line that does not parse, a removed command (sysexits'
/// `EX_USAGE`). clap's own 2 collided with "validation failed".
pub const EXIT_USAGE: u8 = 64;
pub const EXIT_TIMEOUT: u8 = 124;

pub fn set_exit_code(code: u8) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
}

/// The exit code of a failed command: the API's ([`nlink_lab::Error::to_api`]).
pub fn exit_code_for(err: &nlink_lab::Error) -> u8 {
    err.to_api().exit_code
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
        // A wait that gives up (`wait`, `wait-for`) is a timeout too.
        assert_eq!(
            exit_code_for(&nlink_lab::Error::TimedOut("timeout waiting".into())),
            EXIT_TIMEOUT
        );
        // interrupted
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Cancelled("deploy".into())),
            130
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::NotFound { name: "l".into() }),
            EXIT_FAILURE
        );
        // a bad argument is an error, not a validation failure (#209)
        assert_eq!(
            exit_code_for(&nlink_lab::Error::invalid_argument("--set x")),
            EXIT_FAILURE
        );
    }
}
