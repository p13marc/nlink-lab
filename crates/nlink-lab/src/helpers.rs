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

/// Parse a duration string like "10ms", "100us", "1s", "500ns".
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if let Some(val) = s.strip_suffix("ms") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid duration '{s}': {e}")))?;
        Ok(Duration::from_secs_f64(n / 1000.0))
    } else if let Some(val) = s.strip_suffix("us") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid duration '{s}': {e}")))?;
        Ok(Duration::from_secs_f64(n / 1_000_000.0))
    } else if let Some(val) = s.strip_suffix("ns") {
        let n: u64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid duration '{s}': {e}")))?;
        Ok(Duration::from_nanos(n))
    } else if let Some(val) = s.strip_suffix('s') {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid duration '{s}': {e}")))?;
        Ok(Duration::from_secs_f64(n))
    } else {
        Err(Error::invalid_topology(format!(
            "invalid duration '{s}': expected suffix ms, us, ns, or s"
        )))
    }
}

/// Parse a percentage string like "0.1%", "5%" into f64 (0.1, 5.0).
pub fn parse_percent(s: &str) -> Result<f64> {
    let s = s.trim();
    let val_str = s.strip_suffix('%').ok_or_else(|| {
        Error::invalid_topology(format!("invalid percentage '{s}': missing '%' suffix"))
    })?;
    let val: f64 = val_str
        .trim()
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid percentage '{s}': {e}")))?;
    if !(0.0..=100.0).contains(&val) {
        return Err(Error::invalid_topology(format!(
            "invalid percentage '{s}': value must be 0-100"
        )));
    }
    Ok(val)
}

/// Parse a rate string like "100mbit", "1gbit", "10kbit" into bits per second.
pub fn parse_rate_bps(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(val) = s.strip_suffix("gbit") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 1_000_000_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("gbps") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 8_000_000_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("mbit") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 1_000_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("mbps") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 8_000_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("kbit") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 1_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("kbps") {
        let n: f64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok((n * 8_000.0) as u64)
    } else if let Some(val) = s.strip_suffix("bit") {
        let n: u64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok(n)
    } else if let Some(val) = s.strip_suffix("bps") {
        let n: u64 = val
            .trim()
            .parse()
            .map_err(|e| Error::invalid_topology(format!("invalid rate '{s}': {e}")))?;
        Ok(n * 8)
    } else {
        Err(Error::invalid_topology(format!(
            "invalid rate '{s}': expected suffix bit, kbit, mbit, gbit, bps, kbps, mbps, or gbps"
        )))
    }
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
    fn test_parse_duration_bad() {
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10xyz").is_err());
    }

    #[test]
    fn test_parse_percent() {
        assert!((parse_percent("0.1%").unwrap() - 0.1).abs() < f64::EPSILON);
        assert!((parse_percent("5%").unwrap() - 5.0).abs() < f64::EPSILON);
        assert!((parse_percent("100%").unwrap() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_percent_bad() {
        assert!(parse_percent("5").is_err());
        assert!(parse_percent("abc%").is_err());
        assert!(parse_percent("101%").is_err());
        assert!(parse_percent("-1%").is_err());
    }

    #[test]
    fn test_parse_rate_bps() {
        assert_eq!(parse_rate_bps("100mbit").unwrap(), 100_000_000);
        assert_eq!(parse_rate_bps("1gbit").unwrap(), 1_000_000_000);
        assert_eq!(parse_rate_bps("10kbit").unwrap(), 10_000);
        assert_eq!(parse_rate_bps("1000bit").unwrap(), 1000);
    }

    #[test]
    fn test_parse_rate_bps_bytes() {
        assert_eq!(parse_rate_bps("1mbps").unwrap(), 8_000_000);
        assert_eq!(parse_rate_bps("1gbps").unwrap(), 8_000_000_000);
    }

    #[test]
    fn test_parse_rate_bps_bad() {
        assert!(parse_rate_bps("100").is_err());
        assert!(parse_rate_bps("abc").is_err());
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
