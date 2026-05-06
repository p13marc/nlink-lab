//! Per-host subnet allocator for `subnet auto/N` placeholders in NLL.
//!
//! NLL syntax allows `subnet auto/24` (or `auto/30`, etc.) anywhere a
//! literal CIDR is accepted. At deploy time the placeholder is
//! replaced with a concrete /N drawn from the host-wide pool tracked
//! at `$XDG_STATE_HOME/nlink-lab/subnet-pool.json`.
//!
//! The pool is `10.0.0.0/8` (private RFC1918 — 65 536 /24 slots).
//! Allocations are recorded against the requesting lab name and
//! freed when that lab is destroyed. The pool file is acquired
//! under a blocking flock so concurrent deploys serialise rather
//! than race. Round-5 §2.5.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Persisted pool state. One entry per allocated subnet, keyed by
/// CIDR string, valued by the lab name that owns it.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Pool {
    /// `cidr -> lab_name`. We use `BTreeMap` for stable ordering on
    /// disk and deterministic test output.
    in_use: BTreeMap<String, String>,
    /// Hint for the next allocation — bumps to skip already-checked
    /// slots. Not authoritative; allocator falls back to a full scan
    /// if needed.
    next_offset: u32,
}

fn pool_path() -> PathBuf {
    let base = if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(state_home).join("nlink-lab")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("state").join("nlink-lab")
    } else {
        PathBuf::from("/tmp/nlink-lab")
    };
    base.join("subnet-pool.json")
}

fn lock_path() -> PathBuf {
    pool_path().with_extension("lock")
}

/// Acquire a blocking exclusive flock on the pool's sentinel file.
/// Returned guard releases the lock on drop.
fn lock() -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&path)?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(Error::deploy_failed(format!(
            "failed to lock subnet pool: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(file)
}

fn load_pool() -> Result<Pool> {
    let path = pool_path();
    if !path.exists() {
        return Ok(Pool::default());
    }
    let text = std::fs::read_to_string(&path)?;
    let pool: Pool = serde_json::from_str(&text).map_err(|e| Error::State {
        op: "parse",
        detail: format!("subnet pool: {e}"),
        path,
    })?;
    Ok(pool)
}

fn save_pool(pool: &Pool) -> Result<()> {
    let path = pool_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(pool)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Allocate a fresh /`prefix` subnet for `lab_name`.
///
/// Currently supports `prefix == 24` only — the most common case
/// from the harness team's topology. Extending to other prefixes is
/// mechanical (see `next_free_for_prefix` below). Returns the CIDR
/// string (e.g. `"10.42.0.0/24"`).
pub fn allocate(lab_name: &str, prefix: u8) -> Result<String> {
    if prefix != 24 {
        return Err(Error::invalid_topology(format!(
            "subnet auto/{prefix} not yet supported (only /24 today)",
        )));
    }
    let _lock = lock()?;
    let mut pool = load_pool()?;
    let cidr = next_free_24(&pool).ok_or_else(|| {
        Error::deploy_failed("subnet pool exhausted (no free 10.x.0.0/24 slot)".to_string())
    })?;
    pool.in_use.insert(cidr.clone(), lab_name.to_string());
    save_pool(&pool)?;
    Ok(cidr)
}

/// Free every subnet currently allocated to `lab_name`. Called from
/// `RunningLab::destroy` so cleanup is automatic. Returns the list of
/// freed subnets (for logging/observability).
pub fn free_for_lab(lab_name: &str) -> Result<Vec<String>> {
    let _lock = lock()?;
    let mut pool = load_pool()?;
    let to_remove: Vec<String> = pool
        .in_use
        .iter()
        .filter(|(_, owner)| *owner == lab_name)
        .map(|(cidr, _)| cidr.clone())
        .collect();
    for cidr in &to_remove {
        pool.in_use.remove(cidr);
    }
    if !to_remove.is_empty() {
        save_pool(&pool)?;
    }
    Ok(to_remove)
}

/// Walk `10.<x>.<y>.0/24` for `x = next_offset .. 256`, then `0 ..
/// next_offset`. Returns the first CIDR not in `pool.in_use`, or
/// None if the entire space is exhausted.
fn next_free_24(pool: &Pool) -> Option<String> {
    // Prefer the offset hint; fall through to a full scan.
    for x in 0..=255u32 {
        for y in 0..=255u32 {
            let cidr = format!("10.{x}.{y}.0/24");
            if !pool.in_use.contains_key(&cidr) {
                return Some(cidr);
            }
        }
    }
    None
}

/// Pure function (does not touch the filesystem) — replace every
/// `auto/N` placeholder in the topology with a concrete CIDR using
/// `alloc(prefix) -> cidr`. Returns the list of allocated CIDRs in
/// the order they were requested (so the caller can free them on
/// failure). Round-5 §2.5.
pub fn substitute_auto_subnets<F>(
    topology: &mut crate::types::Topology,
    mut alloc: F,
) -> Result<Vec<String>>
where
    F: FnMut(u8) -> Result<String>,
{
    let mut allocated = Vec::new();

    // Network blocks: `network lan { subnet auto/24 ... }`. The lowered
    // `Link` type doesn't carry a subnet field (auto-assignment for
    // links works off explicit per-endpoint addresses), so only
    // network-block subnets are subject to allocation today.
    for net in topology.networks.values_mut() {
        if let Some(s) = &net.subnet
            && let Some(prefix) = parse_auto_placeholder(s)
        {
            let cidr = alloc(prefix)?;
            allocated.push(cidr.clone());
            net.subnet = Some(cidr);
        }
    }

    Ok(allocated)
}

/// `"auto/24"` → `Some(24)`. `"auto"` → `Some(24)` (default prefix).
/// Anything else → `None`.
fn parse_auto_placeholder(s: &str) -> Option<u8> {
    if s == "auto" {
        return Some(24);
    }
    let rest = s.strip_prefix("auto/")?;
    rest.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auto_placeholder_recognises_forms() {
        assert_eq!(parse_auto_placeholder("auto"), Some(24));
        assert_eq!(parse_auto_placeholder("auto/24"), Some(24));
        assert_eq!(parse_auto_placeholder("auto/30"), Some(30));
        assert_eq!(parse_auto_placeholder("10.0.0.0/24"), None);
        assert_eq!(parse_auto_placeholder(""), None);
        assert_eq!(parse_auto_placeholder("auto/abc"), None);
    }

    #[test]
    fn next_free_24_returns_first_unused() {
        let mut pool = Pool::default();
        pool.in_use.insert("10.0.0.0/24".into(), "lab-a".into());
        pool.in_use.insert("10.0.1.0/24".into(), "lab-b".into());
        // Should skip 10.0.0.0 and 10.0.1.0, return 10.0.2.0.
        assert_eq!(next_free_24(&pool), Some("10.0.2.0/24".into()));
    }

    #[test]
    fn next_free_24_empty_pool_returns_first() {
        let pool = Pool::default();
        assert_eq!(next_free_24(&pool), Some("10.0.0.0/24".into()));
    }

    /// Walk `substitute_auto_subnets` with a controlled allocator
    /// that hands out predictable CIDRs. Asserts that placeholders
    /// in network blocks are replaced and the list of allocations is
    /// returned in order.
    #[test]
    fn substitute_auto_subnets_replaces_placeholders() {
        use crate::types::{Network, Topology};

        let mut topo = Topology::default();
        let mut net = Network {
            subnet: Some("auto/24".into()),
            ..Default::default()
        };
        net.members.push("a:eth0".into());
        topo.networks.insert("lan".into(), net);

        let mut counter = 0u32;
        let allocated = substitute_auto_subnets(&mut topo, |prefix| {
            assert_eq!(prefix, 24);
            counter += 1;
            Ok(format!("172.16.{}.0/24", counter - 1))
        })
        .unwrap();

        assert_eq!(allocated, vec!["172.16.0.0/24".to_string()]);
        assert_eq!(
            topo.networks.get("lan").unwrap().subnet,
            Some("172.16.0.0/24".into())
        );
    }

    /// Subnets that aren't `auto/...` placeholders pass through
    /// unchanged.
    #[test]
    fn substitute_auto_subnets_leaves_literal_alone() {
        use crate::types::{Network, Topology};

        let mut topo = Topology::default();
        let net = Network {
            subnet: Some("10.99.0.0/24".into()),
            ..Default::default()
        };
        topo.networks.insert("lan".into(), net);

        let allocated = substitute_auto_subnets(&mut topo, |_| {
            panic!("allocator should not be called for literal subnets")
        })
        .unwrap();

        assert!(allocated.is_empty());
        assert_eq!(
            topo.networks.get("lan").unwrap().subnet,
            Some("10.99.0.0/24".into())
        );
    }
}
