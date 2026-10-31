//! What labs claim in the host's own network namespace (#332): the mgmt
//! subnet and bridge of a `mgmt … host-reachable` lab, which add a bridge,
//! an address and a connected route to the host itself. Two labs on
//! overlapping subnets both did, and the host routed one lab's traffic to
//! the other.
//!
//! The registry is `<state root>/host/allocations.toml`, under its own
//! lock (`allocations.lock`, never removed). A deploy claims before its
//! first op and fails on a conflict with another lab's claim; destroy, a
//! failed deploy's rollback and the orphan reaper release; `doctor`
//! reports claims of labs that are gone, and `doctor --fix` rebuilds the
//! registry from the running labs. The lock order is always a lab's lock,
//! then this one.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::Topology;

/// What one lab holds in the host namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The host-reachable mgmt subnet (IPv4 or IPv6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mgmt: Option<String>,
    /// Root-namespace bridge names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<String>,
}

impl Claims {
    /// What `topology` claims when deployed: a host-reachable mgmt
    /// network's subnet and bridge, and its foreign taps' bridges (#344).
    /// `None` when it takes nothing in the host's namespace.
    pub fn of(topology: &Topology) -> Option<Self> {
        let mgmt = topology
            .lab
            .mgmt_subnet
            .clone()
            .filter(|_| topology.lab.mgmt_host_reachable);
        let mut bridges = Vec::new();
        if mgmt.is_some() {
            bridges.push(topology.lab.mgmt_bridge_name());
        }
        bridges.extend(crate::foreign::tap_bridges(topology));
        (mgmt.is_some() || !bridges.is_empty()).then_some(Self { mgmt, bridges })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    labs: BTreeMap<String, Claims>,
}

fn dir() -> PathBuf {
    crate::state::root_dir().join("host")
}

fn path() -> PathBuf {
    dir().join("allocations.toml")
}

/// The registry's own lock, held for one read-modify-write.
struct Guard(#[allow(dead_code)] std::fs::File);

fn lock() -> Result<Guard> {
    let dir = dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", dir.display())))?;
    let path = dir.join("allocations.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", path.display())))?;
    // SAFETY: flock on a file this process owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(Error::deploy_failed(format!(
            "lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(Guard(file))
}

fn load() -> Result<Registry> {
    match std::fs::read_to_string(path()) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| Error::deploy_failed(format!("{}: {e}", path().display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(Error::deploy_failed(format!("{}: {e}", path().display()))),
    }
}

fn save(registry: &Registry) -> Result<()> {
    let text = toml::to_string(registry)
        .map_err(|e| Error::deploy_failed(format!("host registry: {e}")))?;
    let tmp = path().with_extension(format!("tmp.{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    };
    write()
        .and_then(|()| std::fs::rename(&tmp, path()))
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", path().display())))
}

/// Whether two CIDRs share an address (one contains the other).
fn overlaps(a: &str, b: &str) -> bool {
    let (Ok((ia, pa)), Ok((ib, pb))) =
        (crate::helpers::parse_cidr(a), crate::helpers::parse_cidr(b))
    else {
        return false;
    };
    if ia.is_ipv4() != ib.is_ipv4() {
        return false;
    }
    let shorter = pa.min(pb);
    let na: IpAddr = crate::helpers::network_address(ia, shorter);
    crate::helpers::ip_in_subnet(ib, na, shorter)
}

/// The first claim of another lab that `claims` conflicts with, as the
/// rule's message.
fn conflict(registry: &Registry, lab: &str, claims: &Claims) -> Option<String> {
    for (other, theirs) in registry.labs.iter().filter(|(name, _)| *name != lab) {
        if let (Some(mine), Some(their)) = (&claims.mgmt, &theirs.mgmt)
            && overlaps(mine, their)
        {
            return Some(format!(
                "mgmt-subnet-overlap: lab '{lab}' wants host-reachable mgmt {mine}, which \
                 overlaps lab '{other}''s {their}: the host would route one lab's traffic to \
                 the other; give one of them another subnet"
            ));
        }
        if let Some(bridge) = claims.bridges.iter().find(|b| theirs.bridges.contains(b)) {
            return Some(format!(
                "mgmt-subnet-overlap: lab '{lab}' wants host bridge {bridge}, which lab \
                 '{other}' has"
            ));
        }
    }
    None
}

/// Claim `claims` for `lab`, or fail with rule `mgmt-subnet-overlap`
/// naming the lab that has them. A lab's own earlier claim is replaced.
pub fn claim(lab: &str, claims: &Claims) -> Result<()> {
    let _guard = lock()?;
    let mut registry = load()?;
    if let Some(message) = conflict(&registry, lab, claims) {
        return Err(Error::invalid_topology(message));
    }
    registry.labs.insert(lab.to_string(), claims.clone());
    save(&registry)
}

/// Let go of everything `lab` claimed. Nothing to release is fine.
pub fn release(lab: &str) -> Result<()> {
    let _guard = lock()?;
    let mut registry = load()?;
    if registry.labs.remove(lab).is_some() {
        save(&registry)?;
    }
    Ok(())
}

/// Every lab's claims.
pub fn all() -> Result<BTreeMap<String, Claims>> {
    let _guard = lock()?;
    Ok(load()?.labs)
}

/// The labs with claims but no state: gone without releasing (a crash).
pub fn stale() -> Result<Vec<String>> {
    Ok(all()?
        .into_keys()
        .filter(|lab| !crate::state::exists(lab))
        .collect())
}

/// Rewrite the registry from the running labs' topologies (`doctor
/// --fix`). Returns how many labs it lists.
pub fn rebuild(labs: impl IntoIterator<Item = (String, Topology)>) -> Result<usize> {
    let _guard = lock()?;
    let registry = Registry {
        labs: labs
            .into_iter()
            .filter_map(|(name, t)| Claims::of(&t).map(|c| (name, c)))
            .collect(),
    };
    save(&registry)?;
    Ok(registry.labs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(mgmt: &str, bridge: &str) -> Claims {
        Claims {
            mgmt: Some(mgmt.into()),
            bridges: vec![bridge.into()],
        }
    }

    #[test]
    fn overlapping_subnets_are_found_either_way() {
        assert!(overlaps("172.20.0.0/24", "172.20.0.0/24"));
        assert!(overlaps("172.20.0.0/16", "172.20.5.0/24"));
        assert!(overlaps("172.20.5.0/24", "172.20.0.0/16"));
        assert!(!overlaps("172.20.0.0/24", "172.20.1.0/24"));
        assert!(overlaps("fd00:1::/64", "fd00:1::/48"));
        assert!(!overlaps("fd00:1::/64", "172.20.0.0/24"));
    }

    #[test]
    fn a_claim_conflicts_with_another_labs_and_not_its_own() {
        let mut r = Registry::default();
        r.labs
            .insert("a".into(), claims("172.20.0.0/24", "nlaaaaaaaa"));
        let msg = conflict(&r, "b", &claims("172.20.0.0/16", "nlbbbbbbbb")).unwrap();
        assert!(msg.starts_with("mgmt-subnet-overlap:"), "{msg}");
        assert!(msg.contains("lab 'a'"), "{msg}");
        assert!(conflict(&r, "b", &claims("172.21.0.0/24", "nlbbbbbbbb")).is_none());
        assert!(conflict(&r, "a", &claims("172.20.0.0/24", "nlaaaaaaaa")).is_none());
        let msg = conflict(&r, "b", &claims("10.9.0.0/24", "nlaaaaaaaa")).unwrap();
        assert!(msg.contains("nlaaaaaaaa"), "{msg}");
    }

    #[test]
    fn claims_are_saved_released_and_rebuilt() {
        let _guard = crate::state::xdg_state_lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(crate::state::STATE_DIR_ENV, dir.path()) };
        claim("a", &claims("172.20.0.0/24", "nlaaaaaaaa")).unwrap();
        let err = claim("b", &claims("172.20.0.0/25", "nlbbbbbbbb")).unwrap_err();
        assert!(err.to_string().contains("mgmt-subnet-overlap"), "{err}");
        claim("b", &claims("172.21.0.0/24", "nlbbbbbbbb")).unwrap();
        assert_eq!(all().unwrap().len(), 2);
        // neither has a state file: both are stale
        assert_eq!(stale().unwrap(), ["a", "b"]);
        release("a").unwrap();
        release("a").unwrap();
        assert_eq!(all().unwrap().keys().collect::<Vec<_>>(), ["b"]);
        assert_eq!(rebuild(std::iter::empty()).unwrap(), 0);
        assert!(all().unwrap().is_empty());
    }
}
