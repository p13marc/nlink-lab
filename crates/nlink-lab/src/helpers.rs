//! Value parsing and validation helpers.
//!
//! Convert human-friendly strings from topology files into values
//! that nlink APIs expect.

use std::net::IpAddr;
use std::time::Duration;

use crate::error::{Error, Result};

/// Parse a CIDR string like "10.0.0.1/24" into (IpAddr, prefix_len).
pub fn parse_cidr(s: &str) -> Result<(IpAddr, u8)> {
    let (addr_str, prefix_str) = s.rsplit_once('/').ok_or_else(|| {
        Error::invalid_topology(format!("invalid CIDR '{s}': missing '/' separator"))
    })?;
    let addr: IpAddr = addr_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid CIDR '{s}': {e}")))?;
    let prefix: u8 = prefix_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid CIDR '{s}': bad prefix: {e}")))?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(Error::invalid_topology(format!(
            "invalid CIDR '{s}': prefix {prefix} exceeds maximum {max}"
        )));
    }
    Ok((addr, prefix))
}

/// Compute the network address from an IP and prefix length.
/// E.g., network_address(10.0.1.5, 24) → 10.0.1.0
pub fn network_address(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let mask = if prefix == 0 {
                0u32
            } else {
                !0u32 << (32 - prefix)
            };
            IpAddr::V4(std::net::Ipv4Addr::from(u32::from(v4) & mask))
        }
        IpAddr::V6(v6) => {
            let mask = if prefix == 0 {
                0u128
            } else {
                !0u128 << (128 - prefix)
            };
            IpAddr::V6(std::net::Ipv6Addr::from(u128::from(v6) & mask))
        }
    }
}

/// Duration unit suffixes and their length in nanoseconds.
///
/// Ordered longest-suffix-first so that `ms` wins over `s` and `m`, and
/// `us`/`ns` win over `s`. Both the micro sign (U+00B5) and the Greek
/// small mu (U+03BC) are accepted for microseconds.
const DURATION_UNITS: &[(&str, f64)] = &[
    ("ns", 1.0),
    ("us", 1e3),
    ("µs", 1e3),
    ("μs", 1e3),
    ("ms", 1e6),
    ("s", 1e9),
    ("m", 60e9),
    ("h", 3_600e9),
];

/// Parse a duration string like "10ms", "100us", "1s", "500ns", "2m", "1h".
///
/// Accepted units: `ns`, `us` (or `µs`), `ms`, `s`, `m`, `h`. A bare
/// number with no unit is rejected — there is no implicit default unit.
/// Values must be finite and non-negative; anything that would overflow a
/// [`Duration`] (roughly 584 years) is an error. Fractional values are
/// rounded to the nearest nanosecond.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let Some((val_str, per_unit_ns)) = DURATION_UNITS
        .iter()
        .find_map(|(unit, ns)| s.strip_suffix(unit).map(|v| (v, *ns)))
    else {
        return Err(Error::invalid_topology(format!(
            "invalid duration '{s}': expected suffix ns, us, ms, s, m, or h"
        )));
    };
    let n: f64 = val_str
        .trim()
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid duration '{s}': {e}")))?;
    if n.is_nan() {
        return Err(Error::invalid_topology(format!(
            "invalid duration '{s}': not a number"
        )));
    }
    if n < 0.0 {
        return Err(Error::invalid_topology(format!(
            "invalid duration '{s}': must not be negative"
        )));
    }
    let nanos = n * per_unit_ns;
    if !nanos.is_finite() || nanos > u64::MAX as f64 {
        return Err(Error::invalid_topology(format!(
            "invalid duration '{s}': too large (max {} seconds)",
            u64::MAX / 1_000_000_000
        )));
    }
    // `as u64` saturates, but the range check above already guarantees
    // the value fits.
    Ok(Duration::from_nanos(nanos.round() as u64))
}

/// Parse a percentage string like "0.1%", "5%" into f64 (0.1, 5.0).
///
/// The value must be a finite number in `0..=100`; negatives, NaN, and
/// infinities are rejected.
pub fn parse_percent(s: &str) -> Result<f64> {
    let s = s.trim();
    let val_str = s.strip_suffix('%').ok_or_else(|| {
        Error::invalid_topology(format!("invalid percentage '{s}': missing '%' suffix"))
    })?;
    let val: f64 = val_str
        .trim()
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid percentage '{s}': {e}")))?;
    if !val.is_finite() {
        return Err(Error::invalid_topology(format!(
            "invalid percentage '{s}': not a finite number"
        )));
    }
    if val < 0.0 {
        return Err(Error::invalid_topology(format!(
            "invalid percentage '{s}': must not be negative"
        )));
    }
    if val > 100.0 {
        return Err(Error::invalid_topology(format!(
            "invalid percentage '{s}': value must be 0-100"
        )));
    }
    Ok(val)
}

/// Rate unit suffixes and their size in bits per second.
///
/// Conventions (matching the NLL lexer's `RATE` token and `tc(8)`):
///
/// - `bit`, `kbit`, `mbit`, `gbit`, `tbit` — bits/s with decimal SI
///   multipliers (`k` = 1000, ...), exactly as `tc` reads them.
/// - `byte`, `kbyte`, `mbyte`, `gbyte`, `tbyte` and `bps`, `kbps`,
///   `mbps`, `gbps`, `tbps` — **bytes**/s (×8), the `tc` meaning of
///   `bps`.
/// - bare `k`, `m`, `g`, `t`, `p` — bits/s with decimal SI multipliers,
///   so `100m` means 100 mbit/s. (This is an NLL shorthand; `tc` itself
///   has no bare-letter units and treats a bare number as bytes/s.)
///
/// Ordered longest-suffix-first so `kbit` wins over `bit`, `bit` over
/// `t`, and so on. Matching is case-insensitive.
const RATE_UNITS: &[(&str, u64)] = &[
    ("kbyte", 8_000),
    ("mbyte", 8_000_000),
    ("gbyte", 8_000_000_000),
    ("tbyte", 8_000_000_000_000),
    ("byte", 8),
    ("kbit", 1_000),
    ("mbit", 1_000_000),
    ("gbit", 1_000_000_000),
    ("tbit", 1_000_000_000_000),
    ("bit", 1),
    ("kbps", 8_000),
    ("mbps", 8_000_000),
    ("gbps", 8_000_000_000),
    ("tbps", 8_000_000_000_000),
    ("bps", 8),
    ("k", 1_000),
    ("m", 1_000_000),
    ("g", 1_000_000_000),
    ("t", 1_000_000_000_000),
    ("p", 1_000_000_000_000_000),
];

/// Parse a rate string like "100mbit", "1gbit", "10kbit", "1mbyte", "100m"
/// into bits per second.
///
/// See `RATE_UNITS` (private table) for the accepted suffixes and the bit/byte
/// convention. A bare number with no unit is rejected. The value must be
/// finite and non-negative, and the product must fit in a `u64`.
pub fn parse_rate_bps(s: &str) -> Result<u64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let Some((val_str, per_unit_bits)) = RATE_UNITS
        .iter()
        .find_map(|(unit, bits)| lower.strip_suffix(unit).map(|v| (v, *bits)))
    else {
        return Err(Error::invalid_topology(format!(
            "invalid rate '{s}': expected suffix bit, kbit, mbit, gbit, tbit, \
             byte, kbyte, mbyte, gbyte, tbyte, bps, kbps, mbps, gbps, tbps, \
             or k, m, g, t, p"
        )));
    };
    let val_str = val_str.trim();
    let too_large =
        || Error::invalid_topology(format!("invalid rate '{s}': exceeds {} bit/s", u64::MAX));

    // Integer values (the only form the NLL lexer produces) are multiplied
    // exactly; anything else goes through f64 for fractional support.
    if let Ok(n) = val_str.parse::<u64>() {
        return n.checked_mul(per_unit_bits).ok_or_else(too_large);
    }
    let n: f64 = val_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
    if !n.is_finite() {
        return Err(Error::invalid_topology(format!(
            "invalid rate '{s}': not a finite number"
        )));
    }
    if n < 0.0 {
        return Err(Error::invalid_topology(format!(
            "invalid rate '{s}': must not be negative"
        )));
    }
    let bits = n * per_unit_bits as f64;
    if bits >= u64::MAX as f64 {
        return Err(too_large());
    }
    Ok(bits.round() as u64)
}

/// Check if an IP address falls within a subnet.
pub fn ip_in_subnet(ip: IpAddr, network: IpAddr, prefix_len: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            if prefix_len == 0 {
                return true;
            }
            if prefix_len > 32 {
                return false;
            }
            let mask = u32::MAX << (32 - prefix_len);
            (u32::from(ip) & mask) == (u32::from(net) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            if prefix_len == 0 {
                return true;
            }
            if prefix_len > 128 {
                return false;
            }
            let ip_bits = u128::from(ip);
            let net_bits = u128::from(net);
            let mask = u128::MAX << (128 - prefix_len);
            (ip_bits & mask) == (net_bits & mask)
        }
        _ => false, // v4 vs v6 mismatch
    }
}

/// Validate a Linux interface name.
///
/// Rules: 1-15 characters, no '/' or whitespace, not "." or "..".
pub fn validate_interface_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Validation("interface name is empty".into()));
    }
    if name.len() > 15 {
        return Err(Error::Validation(format!(
            "interface name '{name}' is {} chars (max 15)",
            name.len()
        )));
    }
    if name == "." || name == ".." {
        return Err(Error::Validation(format!(
            "interface name '{name}' is reserved"
        )));
    }
    if name.contains('/') || name.contains(char::is_whitespace) {
        return Err(Error::Validation(format!(
            "interface name '{name}' contains invalid characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cidr_v4() {
        let (ip, prefix) = parse_cidr("10.0.0.1/24").unwrap();
        assert_eq!(ip, "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(prefix, 24);
    }

    #[test]
    fn test_parse_cidr_v6() {
        let (ip, prefix) = parse_cidr("::1/128").unwrap();
        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(prefix, 128);
    }

    #[test]
    fn test_parse_cidr_missing_prefix() {
        assert!(parse_cidr("10.0.0.1").is_err());
    }

    #[test]
    fn test_parse_cidr_bad_ip() {
        assert!(parse_cidr("999.0.0.1/24").is_err());
    }

    #[test]
    fn test_parse_cidr_prefix_too_large() {
        assert!(parse_cidr("10.0.0.1/33").is_err());
        assert!(parse_cidr("::1/129").is_err());
    }

    #[test]
    fn test_parse_cidr_v4_max() {
        let (_, prefix) = parse_cidr("10.0.0.1/32").unwrap();
        assert_eq!(prefix, 32);
    }

    #[test]
    fn test_parse_duration_ms() {
        assert_eq!(parse_duration("10ms").unwrap(), Duration::from_millis(10));
    }

    #[test]
    fn test_parse_duration_us() {
        assert_eq!(parse_duration("100us").unwrap(), Duration::from_micros(100));
    }

    #[test]
    fn test_parse_duration_s() {
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
    }

    #[test]
    fn test_parse_duration_ns() {
        assert_eq!(parse_duration("500ns").unwrap(), Duration::from_nanos(500));
    }

    #[test]
    fn test_parse_duration_fractional() {
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
    }

    #[test]
    fn test_parse_duration_minutes_hours() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5m").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("0.5h").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn test_parse_duration_micro_sign() {
        assert_eq!(parse_duration("100µs").unwrap(), Duration::from_micros(100));
        assert_eq!(parse_duration("100μs").unwrap(), Duration::from_micros(100));
    }

    #[test]
    fn test_parse_duration_zero_and_whitespace() {
        assert_eq!(parse_duration("0s").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("0ms").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_duration("  10 ms  ").unwrap(),
            Duration::from_millis(10)
        );
        assert_eq!(parse_duration("+10ms").unwrap(), Duration::from_millis(10));
    }

    #[test]
    fn test_parse_duration_fractional_ns_rounds() {
        assert_eq!(parse_duration("0.1ms").unwrap(), Duration::from_micros(100));
        assert_eq!(parse_duration("1.5ns").unwrap(), Duration::from_nanos(2));
        assert_eq!(parse_duration("2.5us").unwrap(), Duration::from_nanos(2500));
    }

    #[test]
    fn test_parse_duration_bad() {
        // No implicit default unit for bare numbers.
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10xyz").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("s").is_err());
        assert!(parse_duration("ms").is_err());
    }

    // Issue #15: these used to panic inside `Duration::from_secs_f64`.
    #[test]
    fn test_parse_duration_negative() {
        for input in ["-1s", "-1ms", "-0.5us", "-10ns", "-1m", "-1h"] {
            let err = parse_duration(input).unwrap_err();
            assert!(
                err.to_string().contains("must not be negative"),
                "{input}: {err}"
            );
        }
    }

    #[test]
    fn test_parse_duration_non_finite() {
        for input in ["infs", "-infs", "nans", "NaNms", "inf ms", "infinityh"] {
            assert!(parse_duration(input).is_err(), "{input} should be rejected");
        }
    }

    #[test]
    fn test_parse_duration_out_of_range() {
        for input in ["1e30s", "1e300ms", "1e20s", "1e10h"] {
            let err = parse_duration(input).unwrap_err();
            assert!(err.to_string().contains("too large"), "{input}: {err}");
        }
        // Just under the u64-nanosecond ceiling still parses.
        assert!(parse_duration("18446744073ns").is_ok());
        assert!(parse_duration("18446744073s").is_ok());
        assert!(parse_duration("18446744074s").is_err());
    }

    #[test]
    fn test_parse_percent() {
        assert!((parse_percent("0.1%").unwrap() - 0.1).abs() < f64::EPSILON);
        assert!((parse_percent("5%").unwrap() - 5.0).abs() < f64::EPSILON);
        assert!((parse_percent("100%").unwrap() - 100.0).abs() < f64::EPSILON);
        assert_eq!(parse_percent("0%").unwrap(), 0.0);
        assert_eq!(parse_percent(" 50 % ").unwrap(), 50.0);
    }

    #[test]
    fn test_parse_percent_bad() {
        assert!(parse_percent("5").is_err());
        assert!(parse_percent("abc%").is_err());
        assert!(parse_percent("%").is_err());
    }

    #[test]
    fn test_parse_percent_bounds() {
        assert!(parse_percent("100.0%").is_ok());
        let err = parse_percent("100.001%").unwrap_err();
        assert!(err.to_string().contains("0-100"), "{err}");
        assert!(parse_percent("101%").is_err());
        assert!(parse_percent("1e3%").is_err());
        let err = parse_percent("-1%").unwrap_err();
        assert!(err.to_string().contains("negative"), "{err}");
        assert!(parse_percent("-0.001%").is_err());
    }

    #[test]
    fn test_parse_percent_non_finite() {
        for input in ["nan%", "NaN%", "inf%", "-inf%", "infinity%"] {
            let err = parse_percent(input).unwrap_err();
            assert!(err.to_string().contains("finite"), "{input}: {err}");
        }
    }

    #[test]
    fn test_parse_rate_bps() {
        assert_eq!(parse_rate_bps("100mbit").unwrap(), 100_000_000);
        assert_eq!(parse_rate_bps("1gbit").unwrap(), 1_000_000_000);
        assert_eq!(parse_rate_bps("10kbit").unwrap(), 10_000);
        assert_eq!(parse_rate_bps("1000bit").unwrap(), 1000);
        assert_eq!(parse_rate_bps("2tbit").unwrap(), 2_000_000_000_000);
    }

    #[test]
    fn test_parse_rate_bps_bytes() {
        // `bps` and `byte` families are bytes per second (×8), as in tc(8).
        assert_eq!(parse_rate_bps("1bps").unwrap(), 8);
        assert_eq!(parse_rate_bps("1kbps").unwrap(), 8_000);
        assert_eq!(parse_rate_bps("1mbps").unwrap(), 8_000_000);
        assert_eq!(parse_rate_bps("1gbps").unwrap(), 8_000_000_000);
        assert_eq!(parse_rate_bps("1tbps").unwrap(), 8_000_000_000_000);
        assert_eq!(parse_rate_bps("1byte").unwrap(), 8);
        assert_eq!(parse_rate_bps("1kbyte").unwrap(), 8_000);
        assert_eq!(parse_rate_bps("1mbyte").unwrap(), 8_000_000);
        assert_eq!(parse_rate_bps("1gbyte").unwrap(), 8_000_000_000);
        assert_eq!(parse_rate_bps("1tbyte").unwrap(), 8_000_000_000_000);
    }

    #[test]
    fn test_parse_rate_bps_bare_si() {
        // Bare SI letters are bits per second (NLL shorthand: 100m = 100mbit).
        assert_eq!(parse_rate_bps("1k").unwrap(), 1_000);
        assert_eq!(parse_rate_bps("100m").unwrap(), 100_000_000);
        assert_eq!(parse_rate_bps("1g").unwrap(), 1_000_000_000);
        assert_eq!(parse_rate_bps("1t").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_rate_bps("1p").unwrap(), 1_000_000_000_000_000);
    }

    #[test]
    fn test_parse_rate_bps_case_and_fractional() {
        assert_eq!(parse_rate_bps("100Mbit").unwrap(), 100_000_000);
        assert_eq!(parse_rate_bps("1GBit").unwrap(), 1_000_000_000);
        assert_eq!(parse_rate_bps("1.5mbit").unwrap(), 1_500_000);
        assert_eq!(parse_rate_bps("0.5kbyte").unwrap(), 4_000);
        assert_eq!(parse_rate_bps(" 10 kbit ").unwrap(), 10_000);
        assert_eq!(parse_rate_bps("0bit").unwrap(), 0);
    }

    #[test]
    fn test_parse_rate_bps_bad() {
        assert!(parse_rate_bps("100").is_err());
        assert!(parse_rate_bps("abc").is_err());
        assert!(parse_rate_bps("").is_err());
        assert!(parse_rate_bps("mbit").is_err());
        assert!(parse_rate_bps("10xbit").is_err());
        assert!(parse_rate_bps("-1mbit").is_err());
        assert!(parse_rate_bps("-0.5mbit").is_err());
        assert!(parse_rate_bps("nanmbit").is_err());
        assert!(parse_rate_bps("infbit").is_err());
    }

    #[test]
    fn test_parse_rate_bps_overflow() {
        // u64::MAX bit/s is representable; one more is not.
        assert_eq!(parse_rate_bps("18446744073709551615bit").unwrap(), u64::MAX);
        assert!(parse_rate_bps("18446744073709551616bit").is_err());
        let err = parse_rate_bps("18446744073709552p").unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
        assert!(parse_rate_bps("1e30mbit").is_err());
        assert!(parse_rate_bps("3000000000gbyte").is_err());
        // Fractional overflow goes through the f64 path.
        assert!(parse_rate_bps("18446744073709551615.5bit").is_err());
    }

    #[test]
    fn test_ip_in_subnet_v4() {
        let net: IpAddr = "10.0.0.0".parse().unwrap();
        let ip1: IpAddr = "10.0.0.5".parse().unwrap();
        let ip2: IpAddr = "10.0.1.5".parse().unwrap();
        assert!(ip_in_subnet(ip1, net, 24));
        assert!(!ip_in_subnet(ip2, net, 24));
        assert!(ip_in_subnet(ip2, net, 16));
    }

    #[test]
    fn test_ip_in_subnet_v6() {
        let net: IpAddr = "fd00::".parse().unwrap();
        let ip1: IpAddr = "fd00::1".parse().unwrap();
        let ip2: IpAddr = "fd01::1".parse().unwrap();
        assert!(ip_in_subnet(ip1, net, 64));
        assert!(!ip_in_subnet(ip2, net, 64));
    }

    #[test]
    fn test_ip_in_subnet_mismatch() {
        let v4: IpAddr = "10.0.0.0".parse().unwrap();
        let v6: IpAddr = "::1".parse().unwrap();
        assert!(!ip_in_subnet(v6, v4, 24));
    }
}
