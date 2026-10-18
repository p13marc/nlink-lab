//! Per-host subnet allocator for `subnet auto/N` placeholders in NLL.
//!
//! NLL syntax allows `subnet auto/24` (or `auto/30`, etc.) anywhere a
//! literal CIDR is accepted. At deploy time the placeholder is
//! replaced with a concrete /N drawn from the host-wide pool tracked
//! at `<state root>/subnet-pool.json` (see `state::root_dir`).
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

/// Next to the labs' state: it fell back to the world-writable
/// `/tmp/nlink-lab` without `HOME`, which #38 fixed for the state only
/// (#221).
fn pool_path() -> PathBuf {
    crate::state::root_dir().join("subnet-pool.json")
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
        crate::state::ensure_dir(parent)?;
    }
    // neither truncated nor followed: `File::create` emptied whatever a
    // planted symlink pointed at (#221)
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?
    };
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
    load_pool_at(&pool_path())
}

fn load_pool_at(path: &std::path::Path) -> Result<Pool> {
    let path = path.to_path_buf();
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
        crate::state::ensure_dir(parent)?;
    }
    let json = serde_json::to_string_pretty(pool)?;
    crate::state::atomic_write_mode(&path, &json, 0o640)
}

/// Take over the subnets lab `lab_name` holds in the pool of an older
/// per-user state root (#234), when its state moves into the state root.
/// A subnet another lab holds here already stays that lab's, with a
/// warning: two labs had been handed it from separate pools.
pub fn adopt_from(from_root: &std::path::Path, lab_name: &str) -> Result<Vec<String>> {
    let theirs = load_pool_at(&from_root.join("subnet-pool.json"))?;
    let _lock = lock()?;
    let mut pool = load_pool()?;
    let mut adopted = Vec::new();
    for (cidr, owner) in theirs.in_use.iter().filter(|(_, o)| *o == lab_name) {
        match pool.in_use.get(cidr) {
            Some(other) if other != owner => {
                tracing::warn!("subnet {cidr} of lab '{lab_name}' is also lab '{other}''s");
            }
            _ => {
                pool.in_use.insert(cidr.clone(), owner.clone());
                adopted.push(cidr.clone());
            }
        }
    }
    if !adopted.is_empty() {
        save_pool(&pool)?;
    }
    Ok(adopted)
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
            set_subnet(net, cidr)?;
        }
    }

    Ok(allocated)
}

/// The running lab's subnet for each of `desired`'s `auto/N` networks:
/// what an `apply` of the file the lab was deployed from needs. It left
/// the placeholder in, so the members had no addresses and the apply
/// would have removed theirs (#210). A network the running lab does not
/// have is an error: subnets are allocated at deploy.
pub fn resolve_auto_subnets(
    running: &crate::types::Topology,
    desired: &mut crate::types::Topology,
) -> Result<()> {
    for (name, net) in desired.networks.iter_mut() {
        let Some(s) = &net.subnet else { continue };
        if parse_auto_placeholder(s).is_none() {
            continue;
        }
        let have = running
            .networks
            .get(name)
            .and_then(|n| n.subnet.as_ref())
            .filter(|s| parse_auto_placeholder(s).is_none());
        let Some(have) = have else {
            return Err(Error::invalid_topology(format!(
                "network '{name}': `subnet {s}` is allocated when the lab is deployed; \
                 give a network an apply adds a subnet of its own"
            )));
        };
        set_subnet(net, have.clone())?;
    }
    Ok(())
}

/// Give `net` its concrete subnet, and its members the addresses the
/// parser's auto-assignment gives (it skipped a placeholder): the
/// substituted subnet used to leave every member unaddressed (#210).
fn set_subnet(net: &mut crate::types::Network, cidr: String) -> Result<()> {
    let explicit: std::collections::BTreeMap<String, Vec<String>> = net
        .ports
        .iter()
        .filter(|(_, p)| !p.addresses.is_empty())
        .map(|(k, p)| (k.clone(), p.addresses.clone()))
        .collect();
    let assigned = crate::parser::nll::lower::subnet_auto_assign(&cidr, &net.members, &explicit)
        .map_err(Error::invalid_topology)?;
    for (member, addr) in assigned.into_iter().flatten() {
        net.ports.entry(member).or_default().addresses.push(addr);
    }
    net.subnet = Some(cidr);
    Ok(())
}

/// `"auto/24"` → `Some(24)`. `"auto"` → `Some(24)` (default prefix).
/// Anything else → `None`.
pub(crate) fn parse_auto_placeholder(s: &str) -> Option<u8> {
    if s == "auto" {
        return Some(24);
    }
    let rest = s.strip_prefix("auto/")?;
    rest.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `auto` network's members get addresses from the subnet it is
    /// given, and an apply reuses the running lab's (#210).
    #[test]
    fn auto_networks_get_member_addresses_and_keep_their_subnet() {
        let nll =
            "lab \"a\"\nnode a\nnode b\nnetwork lan { members [a:eth0, b:eth0] subnet auto/24 }\n";
        let mut deployed = crate::parser::parse(nll).unwrap();
        substitute_auto_subnets(&mut deployed, |_| Ok("10.7.3.0/24".into())).unwrap();
        let lan = &deployed.networks["lan"];
        assert_eq!(lan.ports["a:eth0"].addresses, vec!["10.7.3.1/24"]);
        assert_eq!(lan.ports["b:eth0"].addresses, vec!["10.7.3.2/24"]);
        let mut desired = crate::parser::parse(nll).unwrap();
        resolve_auto_subnets(&deployed, &mut desired).unwrap();
        assert_eq!(
            serde_json::to_value(&desired.networks).unwrap(),
            serde_json::to_value(&deployed.networks).unwrap()
        );
        let mut added = crate::parser::parse(
            "lab \"a\"\nnode a\nnode b\nnetwork wan { members [a:eth0, b:eth0] subnet auto }\n",
        )
        .unwrap();
        assert!(resolve_auto_subnets(&deployed, &mut added).is_err());
    }

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
