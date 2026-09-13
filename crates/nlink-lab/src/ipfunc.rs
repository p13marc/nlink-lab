//! Built-in IP computation functions for the NLL DSL.
//!
//! Provides `subnet(base, prefix_len, index)` and `host(cidr, host_number)`
//! following Terraform's `cidrsubnet`/`cidrhost` pattern.
//!
//! All math is done on `u128` so IPv4 and IPv6 share one implementation
//! (`std::net` only — no external dependencies).
//!
//! Every argument is user input, so every arithmetic step is range-checked
//! and returns [`Error::InvalidTopology`] rather than panicking (issue #15):
//! prefix lengths are bounded to `0..=32` (IPv4) or `0..=128` (IPv6) on
//! both the base CIDR and the requested prefix, shifts use `checked_shl`,
//! and the base address is masked to its network address before any offset
//! is added.

use std::net::IpAddr;

use crate::error::{Error, Result};
use crate::helpers::{addr_bits, ip_from_bits, ip_to_bits, network_address};

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
/// The base address is normalised to its network address first, so
/// `subnet("10.0.0.5/24", 26, 1)` and `subnet("10.0.0.0/24", 26, 1)`
/// both yield `10.0.0.64/26`.
///
/// ```text
/// subnet("10.0.0.0/16", 24, 18)  → "10.0.18.0/24"
/// subnet("10.0.0.0/8", 16, 2)    → "10.2.0.0/16"
/// subnet("fd00::/48", 64, 1)     → "fd00:0:0:1::/64"
/// ```
///
/// Errors (never panics):
/// - `new_prefix` outside the family's range or not longer than the base prefix
/// - `index >= 2^(new_prefix - base_prefix)`
fn eval_subnet(args: &[String]) -> Result<String> {
    if args.len() != 3 {
        return Err(Error::invalid_topology(
            "subnet() requires 3 arguments: base_cidr, new_prefix, index",
        ));
    }

    let base_str = args[0].trim().trim_matches('"');
    let (base_ip, base_prefix) = parse_cidr_parts(base_str)?;
    let bits = addr_bits(base_ip);
    let new_prefix = parse_prefix_len(&args[1], "subnet(): new prefix", bits)?;
    let index: u64 = args[2].trim().parse().map_err(|_| {
        Error::invalid_topology(format!(
            "subnet(): invalid index '{}': expected a non-negative integer",
            args[2].trim()
        ))
    })?;

    if new_prefix <= base_prefix {
        return Err(Error::invalid_topology(format!(
            "subnet(): new prefix /{new_prefix} must be longer than base /{base_prefix}"
        )));
    }

    // 1..=bits because base_prefix < new_prefix <= bits. A shift by the
    // full width is undefined, so `checked_shl` (None = unbounded).
    let additional_bits = u32::from(new_prefix - base_prefix);
    let max_subnets = 1u128.checked_shl(additional_bits);
    if let Some(max) = max_subnets
        && u128::from(index) >= max
    {
        return Err(Error::invalid_topology(format!(
            "subnet(): index {index} exceeds max {max} subnets (/{base_prefix} → /{new_prefix})"
        )));
    }

    // 0..=bits-1 because new_prefix >= 1.
    let host_bits = u32::from(bits - new_prefix);
    let family = if bits == 32 { "IPv4" } else { "IPv6" };
    let overflow = || {
        Error::invalid_topology(format!(
            "subnet(): subnet {index} of {base_str} at /{new_prefix} overflows the {family} address space"
        ))
    };
    let offset = u128::from(index)
        .checked_shl(host_bits)
        .filter(|o| *o >> host_bits == u128::from(index))
        .ok_or_else(overflow)?;
    let network = ip_to_bits(network_address(base_ip, base_prefix));
    let subnet_ip = network
        .checked_add(offset)
        .filter(|v| bits == 128 || *v <= u128::from(u32::MAX))
        .ok_or_else(overflow)?;
    let result_ip = ip_from_bits(bits, subnet_ip);

    Ok(format!("{result_ip}/{new_prefix}"))
}

/// `host(cidr, host_number)` → IP string.
///
/// Get the `host_number`-th usable address (1-based) of a subnet. The base
/// address is normalised to its network address first. The usable range
/// follows the same policy as Python's `ipaddress.IPv4Network.hosts()`:
///
/// - `/0`–`/30` (IPv6 `/0`–`/126`): network and broadcast/anycast are
///   excluded, so host `n` is `network + n` with
///   `1 <= n <= 2^(bits-prefix) - 2`.
/// - `/31` (RFC 3021 point-to-point) and IPv6 `/127`: both addresses are
///   usable, so host `1` is the network address and host `2` is
///   `network + 1`.
/// - `/32` and `/128`: exactly one usable address, host `1`, which is the
///   address itself.
///
/// ```text
/// host("10.0.18.0/24", 1)    → "10.0.18.1"
/// host("10.0.18.0/24", 254)  → "10.0.18.254"
/// host("172.16.0.0/30", 2)   → "172.16.0.2"
/// host("10.0.0.0/31", 2)     → "10.0.0.1"
/// host("10.0.0.7/32", 1)     → "10.0.0.7"
/// host("fd00:0:0:1::/64", 1) → "fd00:0:0:1::1"
/// ```
///
/// Errors (never panics): `host_number` of `0` or beyond the usable range.
fn eval_host(args: &[String]) -> Result<String> {
    if args.len() != 2 {
        return Err(Error::invalid_topology(
            "host() requires 2 arguments: cidr, host_number",
        ));
    }

    let cidr_str = args[0].trim().trim_matches('"');
    let (base_ip, prefix) = parse_cidr_parts(cidr_str)?;
    let bits = addr_bits(base_ip);
    let host_num: u64 = args[1].trim().parse().map_err(|_| {
        Error::invalid_topology(format!(
            "host(): invalid host number '{}': expected a positive integer",
            args[1].trim()
        ))
    })?;

    // 0..=bits; a full-width shift is undefined, so saturate (only /0
    // on IPv6 gets there and no lab has 2^128 hosts).
    let host_bits = u32::from(bits - prefix);
    let block_size = 1u128.checked_shl(host_bits).unwrap_or(u128::MAX);
    // (usable host count, offset of host #1 from the network address)
    let (max_hosts, first_offset) = if prefix == bits {
        (1u128, 0u128)
    } else if prefix == bits - 1 {
        (2u128, 0u128)
    } else {
        (block_size - 2, 1u128)
    };
    if host_num == 0 || u128::from(host_num) > max_hosts {
        return Err(Error::invalid_topology(format!(
            "host(): host number {host_num} out of range 1..={max_hosts} for {cidr_str} (/{prefix})"
        )));
    }

    let family = if bits == 32 { "IPv4" } else { "IPv6" };
    let network = ip_to_bits(network_address(base_ip, prefix));
    let addr = network
        .checked_add(u128::from(host_num) - 1 + first_offset)
        .filter(|v| bits == 128 || *v <= u128::from(u32::MAX))
        .ok_or_else(|| {
            Error::invalid_topology(format!(
                "host(): host {host_num} of {cidr_str} overflows the {family} address space"
            ))
        })?;
    let result_ip = ip_from_bits(bits, addr);

    Ok(result_ip.to_string())
}

/// Parse a prefix-length argument (`"24"`) into `0..=bits`.
///
/// `what` names the argument in the error message, e.g.
/// `"subnet(): new prefix"`.
fn parse_prefix_len(raw: &str, what: &str, bits: u8) -> Result<u8> {
    let raw = raw.trim();
    let prefix: u32 = raw.parse().map_err(|_| {
        Error::invalid_topology(format!(
            "{what}: invalid prefix length '{raw}': expected an integer 0..={bits}"
        ))
    })?;
    if prefix > u32::from(bits) {
        return Err(Error::invalid_topology(format!(
            "{what}: prefix length /{prefix} exceeds {bits}"
        )));
    }
    Ok(prefix as u8)
}

/// Parse "ip/prefix" into (IpAddr, u8), with the prefix bounded to the
/// address family's width.
fn parse_cidr_parts(s: &str) -> Result<(IpAddr, u8)> {
    let (ip_str, prefix_str) = s
        .rsplit_once('/')
        .ok_or_else(|| Error::invalid_topology(format!("invalid CIDR '{s}': missing '/'")))?;
    let ip: IpAddr = ip_str
        .parse()
        .map_err(|e| Error::invalid_topology(format!("invalid IP '{ip_str}': {e}")))?;
    let prefix = parse_prefix_len(prefix_str, &format!("invalid CIDR '{s}'"), addr_bits(ip))?;
    Ok((ip, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &[&str]) -> Result<String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        eval_function(name, &args)
    }

    /// Assert that a call fails with `InvalidTopology` whose message
    /// contains `needle`.
    fn assert_err_contains(result: Result<String>, needle: &str) {
        match result {
            Err(Error::InvalidTopology(msg)) => {
                assert!(
                    msg.contains(needle),
                    "expected error containing {needle:?}, got {msg:?}"
                );
            }
            Err(other) => panic!("expected InvalidTopology, got {other:?}"),
            Ok(v) => panic!("expected error containing {needle:?}, got Ok({v:?})"),
        }
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
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "26", "5"]),
            "index 5 exceeds max 4",
        );
    }

    #[test]
    fn test_subnet_prefix_too_short() {
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "16", "0"]),
            "must be longer than base",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "24", "0"]),
            "must be longer than base",
        );
    }

    // Issue #15: `subnet("0.0.0.0/0", 32, 0)` shifted 1u32 by 32.
    #[test]
    fn test_subnet_base_prefix_0_to_32() {
        assert_eq!(
            call("subnet", &["0.0.0.0/0", "32", "0"]).unwrap(),
            "0.0.0.0/32"
        );
        assert_eq!(
            call("subnet", &["0.0.0.0/0", "32", "4294967295"]).unwrap(),
            "255.255.255.255/32"
        );
        assert_err_contains(
            call("subnet", &["0.0.0.0/0", "32", "4294967296"]),
            "index 4294967296 exceeds max 4294967296",
        );
    }

    #[test]
    fn test_subnet_base_prefix_0_to_1() {
        assert_eq!(
            call("subnet", &["0.0.0.0/0", "1", "0"]).unwrap(),
            "0.0.0.0/1"
        );
        assert_eq!(
            call("subnet", &["0.0.0.0/0", "1", "1"]).unwrap(),
            "128.0.0.0/1"
        );
        assert_err_contains(
            call("subnet", &["0.0.0.0/0", "1", "2"]),
            "index 2 exceeds max 2",
        );
    }

    #[test]
    fn test_subnet_new_prefix_31_and_32() {
        assert_eq!(
            call("subnet", &["10.0.0.0/24", "31", "127"]).unwrap(),
            "10.0.0.254/31"
        );
        assert_eq!(
            call("subnet", &["10.0.0.0/24", "32", "255"]).unwrap(),
            "10.0.0.255/32"
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "32", "256"]),
            "index 256 exceeds max 256",
        );
    }

    // Issue #15: `subnet("10.0.0.0/24", 200, 0)` shifted by 176 and
    // `32 - new_prefix` underflowed for any new_prefix > 32.
    #[test]
    fn test_subnet_new_prefix_out_of_range() {
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "33", "0"]),
            "subnet(): new prefix: prefix length /33 exceeds 32",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "200", "0"]),
            "prefix length /200 exceeds 32",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "300", "0"]),
            "prefix length /300 exceeds 32",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "-1", "0"]),
            "invalid prefix length '-1'",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/24", "abc", "0"]),
            "invalid prefix length 'abc'",
        );
    }

    #[test]
    fn test_subnet_base_prefix_out_of_range() {
        assert_err_contains(
            call("subnet", &["10.0.0.0/33", "34", "0"]),
            "invalid CIDR '10.0.0.0/33': prefix length /33 exceeds 32",
        );
    }

    #[test]
    fn test_subnet_index_not_a_number() {
        assert_err_contains(
            call("subnet", &["10.0.0.0/16", "24", "-1"]),
            "invalid index '-1'",
        );
        assert_err_contains(
            call("subnet", &["10.0.0.0/16", "24", "18446744073709551616"]),
            "invalid index",
        );
    }

    // Issue #15: `subnet("255.255.255.255/0", 1, 1)` overflowed
    // `base_u32 + (index << host_bits)`. The base is now masked to its
    // network address, so the result is well-defined.
    #[test]
    fn test_subnet_base_with_host_bits_is_masked() {
        assert_eq!(
            call("subnet", &["255.255.255.255/0", "1", "1"]).unwrap(),
            "128.0.0.0/1"
        );
        assert_eq!(
            call("subnet", &["10.0.0.5/24", "26", "1"]).unwrap(),
            "10.0.0.64/26"
        );
        assert_eq!(
            call("subnet", &["255.255.255.255/24", "32", "255"]).unwrap(),
            "255.255.255.255/32"
        );
    }

    #[test]
    fn test_subnet_wrong_arity() {
        assert_err_contains(
            call("subnet", &["10.0.0.0/16", "24"]),
            "requires 3 arguments",
        );
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
        assert_err_contains(
            call("host", &["172.16.0.0/30", "3"]),
            "host number 3 out of range 1..=2",
        );
    }

    #[test]
    fn test_host_zero_invalid() {
        assert_err_contains(
            call("host", &["10.0.0.0/24", "0"]),
            "host number 0 out of range 1..=254",
        );
    }

    #[test]
    fn test_host_overflow() {
        assert_err_contains(
            call("host", &["10.0.0.0/24", "255"]),
            "host number 255 out of range 1..=254",
        );
    }

    // RFC 3021: both addresses of a /31 are usable.
    #[test]
    fn test_host_slash31() {
        assert_eq!(call("host", &["10.0.0.0/31", "1"]).unwrap(), "10.0.0.0");
        assert_eq!(call("host", &["10.0.0.0/31", "2"]).unwrap(), "10.0.0.1");
        assert_err_contains(
            call("host", &["10.0.0.0/31", "0"]),
            "host number 0 out of range 1..=2",
        );
        assert_err_contains(
            call("host", &["10.0.0.0/31", "3"]),
            "host number 3 out of range 1..=2",
        );
    }

    // Issue #15: `host("10.0.0.1/32", 1)` computed `1 - 2` in u32.
    #[test]
    fn test_host_slash32() {
        assert_eq!(call("host", &["10.0.0.1/32", "1"]).unwrap(), "10.0.0.1");
        assert_eq!(
            call("host", &["255.255.255.255/32", "1"]).unwrap(),
            "255.255.255.255"
        );
        assert_err_contains(
            call("host", &["10.0.0.1/32", "0"]),
            "host number 0 out of range 1..=1",
        );
        assert_err_contains(
            call("host", &["10.0.0.1/32", "2"]),
            "host number 2 out of range 1..=1",
        );
    }

    // Prefix 0: `1u32 << 32` used to overflow.
    #[test]
    fn test_host_slash0() {
        assert_eq!(call("host", &["0.0.0.0/0", "1"]).unwrap(), "0.0.0.1");
        assert_eq!(
            call("host", &["0.0.0.0/0", "4294967294"]).unwrap(),
            "255.255.255.254"
        );
        assert_err_contains(
            call("host", &["0.0.0.0/0", "4294967295"]),
            "host number 4294967295 out of range 1..=4294967294",
        );
    }

    #[test]
    fn test_host_prefix_out_of_range() {
        assert_err_contains(
            call("host", &["10.0.0.0/33", "1"]),
            "invalid CIDR '10.0.0.0/33': prefix length /33 exceeds 32",
        );
        assert_err_contains(
            call("host", &["10.0.0.0/999", "1"]),
            "prefix length /999 exceeds 32",
        );
        assert_err_contains(
            call("host", &["10.0.0.0/x", "1"]),
            "invalid prefix length 'x'",
        );
    }

    #[test]
    fn test_host_number_not_a_number() {
        assert_err_contains(
            call("host", &["10.0.0.0/24", "-1"]),
            "invalid host number '-1'",
        );
        assert_err_contains(
            call("host", &["10.0.0.0/24", "1.5"]),
            "invalid host number '1.5'",
        );
    }

    // The base is masked to its network address before adding the host
    // offset, so a base with host bits set cannot overflow.
    #[test]
    fn test_host_base_with_host_bits_is_masked() {
        assert_eq!(call("host", &["10.0.18.5/24", "1"]).unwrap(), "10.0.18.1");
        assert_eq!(
            call("host", &["255.255.255.255/24", "254"]).unwrap(),
            "255.255.255.254"
        );
        assert_eq!(
            call("host", &["255.255.255.255/0", "1"]).unwrap(),
            "0.0.0.1"
        );
    }

    #[test]
    fn test_host_quoted_cidr() {
        // Arguments may come with quotes from the parser
        assert_eq!(
            call("host", &["\"10.0.18.0/24\"", "1"]).unwrap(),
            "10.0.18.1"
        );
    }

    #[test]
    fn test_host_wrong_arity() {
        assert_err_contains(call("host", &["10.0.0.0/24"]), "requires 2 arguments");
    }

    // ── CIDR parsing ──────────────────────────────────────

    #[test]
    fn test_cidr_missing_slash() {
        assert_err_contains(call("host", &["10.0.0.0", "1"]), "missing '/'");
    }

    #[test]
    fn test_cidr_bad_ip() {
        assert_err_contains(
            call("host", &["300.0.0.0/24", "1"]),
            "invalid IP '300.0.0.0'",
        );
    }

    // ── Unknown function ──────────────────────────────────

    #[test]
    fn test_unknown_function() {
        assert_err_contains(call("bogus", &["1", "2"]), "unknown function 'bogus'");
    }

    // ── IPv6 (issue #72) ──

    #[test]
    fn test_subnet_v6_basic() {
        assert_eq!(
            call("subnet", &["fd00::/48", "64", "1"]).unwrap(),
            "fd00:0:0:1::/64"
        );
        assert_eq!(
            call("subnet", &["2001:db8::/32", "48", "18"]).unwrap(),
            "2001:db8:12::/48"
        );
        assert_eq!(
            call("subnet", &["fd00::/48", "64", "0"]).unwrap(),
            "fd00::/64"
        );
    }

    #[test]
    fn test_subnet_v6_index_overflow() {
        assert_err_contains(
            call("subnet", &["fd00::/48", "64", "65536"]),
            "index 65536 exceeds max 65536",
        );
    }

    #[test]
    fn test_subnet_v6_prefix_out_of_range() {
        assert_err_contains(
            call("subnet", &["fd00::/48", "129", "0"]),
            "prefix length /129 exceeds 128",
        );
        assert_err_contains(
            call("subnet", &["fd00::/129", "64", "0"]),
            "invalid CIDR 'fd00::/129': prefix length /129 exceeds 128",
        );
    }

    #[test]
    fn test_subnet_v6_from_slash0() {
        assert_eq!(call("subnet", &["::/0", "1", "1"]).unwrap(), "8000::/1");
        assert_eq!(call("subnet", &["::/0", "128", "1"]).unwrap(), "::1/128");
    }

    #[test]
    fn test_subnet_v6_base_masked() {
        assert_eq!(
            call("subnet", &["fd00::1234/64", "80", "1"]).unwrap(),
            "fd00::1:0:0:0/80"
        );
    }

    #[test]
    fn test_host_v6_basic() {
        assert_eq!(
            call("host", &["fd00:0:0:1::/64", "1"]).unwrap(),
            "fd00:0:0:1::1"
        );
        assert_eq!(call("host", &["fd00::/64", "65535"]).unwrap(), "fd00::ffff");
        assert_err_contains(
            call("host", &["fd00::/64", "0"]),
            "host number 0 out of range",
        );
    }

    #[test]
    fn test_host_v6_slash127_and_128() {
        assert_eq!(call("host", &["fd00::/127", "1"]).unwrap(), "fd00::");
        assert_eq!(call("host", &["fd00::/127", "2"]).unwrap(), "fd00::1");
        assert_err_contains(
            call("host", &["fd00::/127", "3"]),
            "host number 3 out of range 1..=2",
        );
        assert_eq!(call("host", &["fd00::7/128", "1"]).unwrap(), "fd00::7");
        assert_err_contains(
            call("host", &["fd00::7/128", "2"]),
            "host number 2 out of range 1..=1",
        );
    }

    #[test]
    fn test_host_v6_base_masked() {
        assert_eq!(
            call("host", &["fd00::dead:beef/64", "1"]).unwrap(),
            "fd00::1"
        );
    }

    #[test]
    fn test_host_v6_slash0_saturates() {
        // 2^128 hosts do not fit u128: the count saturates instead of
        // overflowing, and small host numbers still work.
        assert_eq!(call("host", &["::/0", "1"]).unwrap(), "::1");
    }
}
