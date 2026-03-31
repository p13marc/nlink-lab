//! Built-in IP computation functions for the NLL DSL.
//!
//! Provides `subnet(base, prefix_len, index)` and `host(cidr, host_number)`
//! following Terraform's `cidrsubnet`/`cidrhost` pattern.
//!
//! All math uses `std::net::Ipv4Addr` — no external dependencies.

use std::net::Ipv4Addr;

use crate::error::{Error, Result};

/// Evaluate a built-in function call.
///
/// Supported functions:
/// - `subnet("base/prefix", new_prefix, index)` → `"ip/new_prefix"`
/// - `host("base/prefix", host_number)` → `"ip"`
pub fn eval_function(name: &str, args: &[String]) -> Result<String> {
    match name {
        "subnet" => eval_subnet(args),
        "host" => eval_host(args),
        other => Err(Error::invalid_topology(format!(
            "unknown function '{other}'"
        ))),
    }
}

/// `subnet(base_cidr, new_prefix_len, index)` → CIDR string.
///
/// Carve subnet #`index` with prefix `/new_prefix_len` from `base_cidr`.
///
/// ```text
/// subnet("10.0.0.0/16", 24, 18)  → "10.0.18.0/24"
/// subnet("10.0.0.0/8", 16, 2)    → "10.2.0.0/16"
/// ```
fn eval_subnet(args: &[String]) -> Result<String> {
    if args.len() != 3 {
        return Err(Error::invalid_topology(
            "subnet() requires 3 arguments: base_cidr, new_prefix, index",
        ));
    }

    let base_str = args[0].trim().trim_matches('"');
    let (base_ip, base_prefix) = parse_cidr_parts(base_str)?;
    let new_prefix: u8 = args[1]
        .trim()
        .parse()
        .map_err(|_| Error::invalid_topology(format!("invalid prefix length: {}", args[1])))?;
    let index: u32 = args[2]
        .trim()
        .parse()
        .map_err(|_| Error::invalid_topology(format!("invalid index: {}", args[2])))?;

    if new_prefix <= base_prefix {
        return Err(Error::invalid_topology(format!(
            "subnet(): new prefix /{new_prefix} must be longer than base /{base_prefix}"
        )));
    }

    let additional_bits = new_prefix - base_prefix;
    let max_subnets = 1u32 << additional_bits;
    if index >= max_subnets {
        return Err(Error::invalid_topology(format!(
            "subnet(): index {index} exceeds max {max_subnets} subnets (/{base_prefix} → /{new_prefix})"
        )));
    }

    let base_u32 = u32::from(base_ip);
    let host_bits = 32 - new_prefix;
    let subnet_ip = base_u32 + (index << host_bits);
    let result_ip = Ipv4Addr::from(subnet_ip);

    Ok(format!("{result_ip}/{new_prefix}"))
}

/// `host(cidr, host_number)` → IP string.
///
/// Get host #`host_number` from a subnet (1-based).
///
/// ```text
/// host("10.0.18.0/24", 1)    → "10.0.18.1"
/// host("10.0.18.0/24", 254)  → "10.0.18.254"
/// host("172.16.0.0/30", 2)   → "172.16.0.2"
/// ```
fn eval_host(args: &[String]) -> Result<String> {
    if args.len() != 2 {
        return Err(Error::invalid_topology(
            "host() requires 2 arguments: cidr, host_number",
        ));
    }

    let cidr_str = args[0].trim().trim_matches('"');
    let (base_ip, prefix) = parse_cidr_parts(cidr_str)?;
    let host_num: u32 = args[1]
        .trim()
        .parse()
        .map_err(|_| Error::invalid_topology(format!("invalid host number: {}", args[1])))?;

    let host_bits = 32 - prefix;
    let max_hosts = (1u32 << host_bits) - 2; // exclude network and broadcast
    if host_num == 0 || host_num > max_hosts {
        return Err(Error::invalid_topology(format!(
            "host(): host number {host_num} out of range 1..{max_hosts} for /{prefix}"
        )));
    }

    let base_u32 = u32::from(base_ip);
    let result_ip = Ipv4Addr::from(base_u32 + host_num);

    Ok(result_ip.to_string())
}

/// Parse "ip/prefix" into (Ipv4Addr, u8).
fn parse_cidr_parts(s: &str) -> Result<(Ipv4Addr, u8)> {
    let (ip_str, prefix_str) = s
        .split_once('/')
        .ok_or_else(|| Error::invalid_topology(format!("invalid CIDR '{s}': missing '/'")))?;
    let ip: Ipv4Addr = ip_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid IP '{ip_str}': {e}")))?;
    let prefix: u8 = prefix_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid prefix '/{prefix_str}': {e}")))?;
    if prefix > 32 {
        return Err(Error::invalid_topology(format!(
            "prefix /{prefix} exceeds 32"
        )));
    }
    Ok((ip, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &[&str]) -> Result<String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        eval_function(name, &args)
    }

    // ── subnet() ──────────────────────────────────────────

    #[test]
    fn test_subnet_basic() {
        assert_eq!(
            call("subnet", &["10.0.0.0/16", "24", "18"]).unwrap(),
            "10.0.18.0/24"
        );
    }

    #[test]
    fn test_subnet_from_8() {
        assert_eq!(
            call("subnet", &["10.0.0.0/8", "16", "2"]).unwrap(),
            "10.2.0.0/16"
        );
    }

    #[test]
    fn test_subnet_index_0() {
        assert_eq!(
            call("subnet", &["10.0.0.0/16", "24", "0"]).unwrap(),
            "10.0.0.0/24"
        );
    }

    #[test]
    fn test_subnet_slash30() {
        assert_eq!(
            call("subnet", &["192.168.0.0/24", "30", "3"]).unwrap(),
            "192.168.0.12/30"
        );
    }

    #[test]
    fn test_subnet_172() {
        assert_eq!(
            call("subnet", &["172.100.0.0/16", "24", "5"]).unwrap(),
            "172.100.5.0/24"
        );
    }

    #[test]
    fn test_subnet_overflow() {
        let result = call("subnet", &["10.0.0.0/24", "26", "5"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_subnet_prefix_too_short() {
        let result = call("subnet", &["10.0.0.0/24", "16", "0"]);
        assert!(result.is_err());
    }

    // ── host() ──────────────────────────────────────────

    #[test]
    fn test_host_basic() {
        assert_eq!(call("host", &["10.0.18.0/24", "1"]).unwrap(), "10.0.18.1");
    }

    #[test]
    fn test_host_last() {
        assert_eq!(
            call("host", &["10.0.18.0/24", "254"]).unwrap(),
            "10.0.18.254"
        );
    }

    #[test]
    fn test_host_slash30() {
        assert_eq!(call("host", &["172.16.0.0/30", "1"]).unwrap(), "172.16.0.1");
        assert_eq!(call("host", &["172.16.0.0/30", "2"]).unwrap(), "172.16.0.2");
    }

    #[test]
    fn test_host_zero_invalid() {
        assert!(call("host", &["10.0.0.0/24", "0"]).is_err());
    }

    #[test]
    fn test_host_overflow() {
        assert!(call("host", &["10.0.0.0/24", "255"]).is_err());
    }

    #[test]
    fn test_host_quoted_cidr() {
        // Arguments may come with quotes from the parser
        assert_eq!(
            call("host", &["\"10.0.18.0/24\"", "1"]).unwrap(),
            "10.0.18.1"
        );
    }

    // ── Unknown function ──────────────────────────────────

    #[test]
    fn test_unknown_function() {
        assert!(call("bogus", &["1", "2"]).is_err());
    }
}
