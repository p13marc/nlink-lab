//! Value parsers for the subcommands' flags: one grammar per kind of
//! value, so `--timeout`, `--interval` and `--duration` all read `30s`,
//! and a bad value is a usage error (exit 64) before anything runs.

use std::time::Duration;

/// A duration with its unit: `500ms`, `30s`, `1.5m`, `2h` (`ns`, `us`,
/// `ms`, `s`, `m`, `h`). A bare number is refused: `--timeout 30` read
/// seconds in one command and milliseconds in another.
pub fn duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    if t.parse::<f64>().is_ok() {
        return Err(format!("`{t}` has no unit: write `{t}s` or `{t}ms`"));
    }
    nlink_lab::helpers::parse_duration(t)
        .map_err(|_| format!("`{t}` is not a duration (a number and a unit: 500ms, 30s, 2m, 1h)"))
}

/// A duration of at least a millisecond: an interval of zero spins.
pub fn interval(s: &str) -> Result<Duration, String> {
    let d = duration(s)?;
    if d < Duration::from_millis(1) {
        return Err(format!("`{s}` is too short for an interval (1ms at least)"));
    }
    Ok(d)
}

/// A 32-bit value, in decimal or `0x` hex: a mark.
pub fn u32_dec_or_hex(s: &str) -> Result<u32, String> {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("'{s}' is not a 32-bit number (decimal or 0x hex)"))
}

/// A netem seed: an integer in `0..=i64::MAX` (#337).
pub fn seed(s: &str) -> Result<u64, String> {
    nlink_lab::seed::parse(s)
}

/// `KEY=VALUE` (`--env`, `--set`). The key is not empty; the value may be.
pub fn key_value(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err(format!("`{s}` is not KEY=VALUE")),
    }
}

/// `NODE:IFACE`.
pub fn endpoint(s: &str) -> Result<String, String> {
    nlink_lab::EndpointRef::parse(s)
        .map(|e| format!("{}:{}", e.node, e.iface))
        .ok_or_else(|| format!("`{s}` is not an endpoint (NODE:IFACE, like r1:eth0)"))
}

/// A signal: `TERM`, `SIGKILL`, `stop`, ….
pub fn signal(s: &str) -> Result<nlink_lab::Signal, String> {
    nlink_lab::parse_signal(s).map_err(|e| e.to_string())
}

/// A TCP target: `PORT` (on 127.0.0.1), `IP:PORT` or `[IPv6]:PORT`.
pub fn tcp_target(s: &str) -> Result<(String, u16), String> {
    nlink_lab::wait::parse_tcp_target(s).map_err(|e| e.to_string())
}

/// A byte size: `65536`, `64K`, `16M`, `1G` (powers of 1000).
pub fn byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num_str, mul) = if let Some(rest) = s.strip_suffix(['G', 'g']) {
        (rest, 1_000_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['M', 'm']) {
        (rest, 1_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['K', 'k']) {
        (rest, 1_000u64)
    } else {
        (s, 1u64)
    };
    let n: u64 = num_str
        .parse()
        .map_err(|e| format!("invalid byte size {s:?}: {e}"))?;
    // overflowed (a panic in a debug build, a wrapped size otherwise)
    n.checked_mul(mul)
        .ok_or_else(|| format!("byte size {s:?} is too large"))
}

/// A regular expression (`--wait-log`).
pub fn regex(s: &str) -> Result<regex::Regex, String> {
    nlink_lab::wait::log_pattern(s).map_err(|e| e.to_string())
}

/// A root qdisc: `tbf,rate=10mbit,burst=32kb,limit=100kb`, `fq_codel`, ….
pub fn qdisc(s: &str) -> Result<nlink_lab::types::QdiscKind, String> {
    nlink_lab::types::QdiscKind::parse_spec(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_need_a_unit() {
        assert_eq!(duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(duration(" 30s "), Ok(Duration::from_secs(30)));
        assert_eq!(duration("1.5m"), Ok(Duration::from_secs(90)));
        let e = duration("30").unwrap_err();
        assert!(e.contains("`30s`"), "{e}");
        assert!(duration("soon").unwrap_err().contains("not a duration"));
        assert!(duration("-1s").is_err());
        assert!(interval("0s").is_err());
        assert_eq!(interval("1ms"), Ok(Duration::from_millis(1)));
    }

    #[test]
    fn small_grammars() {
        assert_eq!(key_value("A=b=c"), Ok(("A".into(), "b=c".into())));
        assert_eq!(key_value("A="), Ok(("A".into(), String::new())));
        assert!(key_value("=x").is_err() && key_value("A").is_err());
        assert_eq!(endpoint("r1:eth0"), Ok("r1:eth0".into()));
        assert!(endpoint("r1").is_err());
        assert!(signal("SIGSTOP").is_ok() && signal("nope").is_err());
        assert_eq!(tcp_target("8080"), Ok(("127.0.0.1".into(), 8080)));
        assert_eq!(byte_size("64K"), Ok(64_000));
        assert!(byte_size("99999999999G").is_err());
        assert!(qdisc("tbf,rate=1mbit").is_err());
    }
}
