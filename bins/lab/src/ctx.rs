//! Per-invocation context shared by the subcommand handlers: global
//! flags, colour helpers, topology parsing with `--set`, and the
//! root / capability check.

use std::io::IsTerminal;

/// Global flags shared by every subcommand handler.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    /// `--json`: machine-readable output where supported.
    pub json: bool,
    /// `--quiet`: suppress informational output.
    pub quiet: bool,
    /// `--verbose`: extra detail (also raises the tracing level).
    pub verbose: bool,
}

// ─── Color helpers ───────────────────────────────────────

fn use_color() -> bool {
    std::env::var("NO_COLOR").is_err() && std::io::stderr().is_terminal()
}

pub fn green(s: &str) -> String {
    if use_color() {
        format!("\x1b[32m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn red(s: &str) -> String {
    if use_color() {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn yellow(s: &str) -> String {
    if use_color() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    if use_color() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Turn a failed validation into the structured error (exit 2, issues
/// in the JSON envelope), printing the human-readable list first.
pub fn validation_failed(lab: &str, result: &nlink_lab::ValidationResult) -> nlink_lab::Error {
    eprintln!("Validation failed for {lab:?}:");
    for e in result.errors() {
        eprintln!("  {} {e}", red("ERROR"));
    }
    nlink_lab::Error::ValidationErrors(result.errors().cloned().collect())
}

/// Parse repeated `--set KEY=VALUE` strings into `(key, value)` pairs.
pub fn parse_set_params(params: &[String]) -> nlink_lab::Result<Vec<(String, String)>> {
    params
        .iter()
        .map(|p| {
            let (key, value) = p.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "invalid --set format: '{p}' (expected KEY=VALUE)"
                ))
            })?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Parse a topology file, optionally with CLI `--set` parameters.
pub fn parse_topology(
    path: &std::path::Path,
    params: &[String],
) -> nlink_lab::Result<nlink_lab::Topology> {
    let cli_params = parse_set_params(params)?;

    if cli_params.is_empty() {
        nlink_lab::parser::parse_file(path)
    } else {
        nlink_lab::parser::parse_file_with_params(path, &cli_params)
    }
}

const CAP_NET_ADMIN: u64 = 12;
const CAP_SYS_ADMIN: u64 = 21;

/// Whether a `CapEff` bitmask (hex, as in `/proc/self/status`) grants
/// both capabilities nlink-lab needs.
fn cap_eff_has_net_and_sys_admin(hex: &str) -> bool {
    u64::from_str_radix(hex.trim(), 16)
        .map(|bits| bits & (1 << CAP_NET_ADMIN) != 0 && bits & (1 << CAP_SYS_ADMIN) != 0)
        .unwrap_or(false)
}

/// Fail fast, before any namespace/netlink work, unless this process is
/// root or holds CAP_NET_ADMIN *and* CAP_SYS_ADMIN. The old check only
/// warned and accepted any capability bit at all (#44).
pub fn require_root() -> nlink_lab::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }
    let ok = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("CapEff:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .map(cap_eff_has_net_and_sys_admin)
            })
        })
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(nlink_lab::Error::deploy_failed(
            "this command needs root (sudo), a SUID binary, or CAP_NET_ADMIN+CAP_SYS_ADMIN \
             (see `just install-caps`)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_eff_requires_both_admin_bits() {
        // CAP_NET_ADMIN (12) + CAP_SYS_ADMIN (21)
        assert!(cap_eff_has_net_and_sys_admin("0000000000201000"));
        // full root set
        assert!(cap_eff_has_net_and_sys_admin("000001ffffffffff"));
        // only CAP_NET_ADMIN
        assert!(!cap_eff_has_net_and_sys_admin("0000000000001000"));
        // only CAP_CHOWN — used to pass the old `!= 0` check
        assert!(!cap_eff_has_net_and_sys_admin("0000000000000001"));
        assert!(!cap_eff_has_net_and_sys_admin("garbage"));
    }
}
