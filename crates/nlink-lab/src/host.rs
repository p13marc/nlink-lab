//! Host-side resource scan: orphaned mgmt bridges / veths / namespaces
//! (no state file), stale labs (state file, missing namespaces), and
//! the best-effort cleanups built on them. The cleanups report what they
//! did as [`Cleanup`]s; front ends print or return them.

/// A kind of host resource a cleanup touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Lab,
    Namespace,
    Veth,
    MgmtBridge,
}

impl std::fmt::Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Resource::Lab => "lab",
            Resource::Namespace => "namespace",
            Resource::Veth => "veth",
            Resource::MgmtBridge => "mgmt bridge",
        })
    }
}

/// One thing a cleanup did, or did not do. Displayed as the line the CLI
/// prints for it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "cleanup", rename_all = "snake_case")]
pub enum Cleanup {
    /// The journal of an interrupted deploy or apply was unwound.
    Unwound {
        lab: String,
        entries: usize,
    },
    Deleted {
        kind: Resource,
        name: String,
    },
    /// Lock files of labs that no longer exist.
    RemovedLocks {
        count: usize,
    },
    Skipped {
        kind: Resource,
        name: String,
        reason: String,
    },
    /// `action` failed (`delete namespace 'x'`): a warning, the cleanup
    /// goes on.
    Failed {
        action: String,
        error: String,
    },
}

impl Cleanup {
    /// Whether this is a failure, which the CLI prints as a warning.
    pub fn is_failure(&self) -> bool {
        matches!(self, Cleanup::Failed { .. })
    }

    fn failed(action: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Cleanup::Failed {
            action: action.into(),
            error: error.to_string(),
        }
    }
}

impl std::fmt::Display for Cleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cleanup::Unwound { lab, entries } => {
                write!(
                    f,
                    "unwound {entries} journal entries of interrupted lab '{lab}'"
                )
            }
            Cleanup::Deleted { kind, name } => write!(f, "deleted {kind} '{name}'"),
            Cleanup::RemovedLocks { count } => write!(f, "removed {count} stale lock file(s)"),
            Cleanup::Skipped { kind, name, reason } => {
                write!(f, "skipped {kind} '{name}': {reason}")
            }
            Cleanup::Failed { action, error } => write!(f, "failed to {action}: {error}"),
        }
    }
}

/// Best-effort cleanup when state is missing: delete this lab's namespaces
/// and its root-namespace mgmt bridge/veth peers.
///
/// Namespaces are matched by the `{prefix}-` prefix (the topology's
/// `prefix` when the caller has it, else the lab name) *and* must not
/// carry a `netns_tag` for a different lab — `destroy simple --force` must
/// not take "simple-2-router" down with it. Untagged prefix matches are
/// still reaped here (unlike `--orphans`) because the caller named the lab
/// explicitly and deploys that predate tagging left nothing else to go on.
pub async fn force_cleanup(name: &str, prefix: &str) -> Vec<Cleanup> {
    let mut done = Vec::new();
    let prefix = format!("{prefix}-");
    match nlink::netlink::namespace::list() {
        Ok(all) => {
            let mut mine = Vec::new();
            for ns in all.iter().filter(|ns| ns.starts_with(&prefix)) {
                if let Some(owner) = crate::netns_tag::lab_of(ns)
                    && owner != name
                {
                    done.push(Cleanup::Skipped {
                        kind: Resource::Namespace,
                        name: ns.clone(),
                        reason: format!("owned by lab '{owner}'"),
                    });
                    continue;
                }
                mine.push(ns.clone());
            }
            stop_namespace_processes(&mine).await;
            for ns in &mine {
                match nlink::netlink::namespace::delete(ns) {
                    Ok(()) => {
                        crate::netns_tag::untag(ns);
                        done.push(Cleanup::Deleted {
                            kind: Resource::Namespace,
                            name: ns.clone(),
                        });
                    }
                    Err(e) => done.push(Cleanup::failed(format!("delete namespace '{ns}'"), e)),
                }
            }
        }
        Err(e) => done.push(Cleanup::failed("list namespaces", e)),
    }

    // Clean up root-namespace mgmt veth peers first (may be orphaned if bridge
    // was already deleted or namespaces were deleted before the bridge).
    // Veth peers are named nm{hash8}{idx} — same hash as the bridge
    // (`nl{hash8}`), so strip the "nl" prefix.
    let bridge_name = crate::mgmt_bridge_name_for(name);
    let veth_prefix = format!("nm{}", &bridge_name[2..]);
    match nlink::Connection::<nlink::Route>::new() {
        Ok(conn) => {
            for ifname in list_ip_links_with(&conn).await {
                if ifname.starts_with(veth_prefix.as_str()) {
                    let _ = conn.del_link_if_exists(ifname.as_str()).await;
                }
            }
            if let Ok(true) = conn.del_link_if_exists(bridge_name.as_str()).await {
                done.push(Cleanup::Deleted {
                    kind: Resource::MgmtBridge,
                    name: bridge_name,
                });
            }
        }
        Err(e) => done.push(Cleanup::failed("open a netlink socket", e)),
    }

    // Also clean up state directory, and the lock file with it (#103),
    // and what it claimed in the host namespace (#332).
    let _ = crate::host_registry::release(name);
    let _ = crate::state::remove(name);
    crate::state::remove_lock_if_unheld(name);
    done
}

/// Resources on the host that look like lab-owned state but have no matching
/// `state.json` — usually left behind by a crashed deploy.
#[derive(Debug, Default, serde::Serialize, schemars::JsonSchema)]
pub struct Orphans {
    /// Root-namespace mgmt bridges (`nl{hash8}`).
    pub bridges: Vec<String>,
    /// Root-namespace mgmt veth peers (`nm{hash8}{idx}`).
    pub veths: Vec<String>,
    /// Named network namespaces tagged by nlink-lab (`netns_tag`) whose
    /// owning lab is not registered, or is registered but no longer
    /// claims them in `state.json`.
    pub netns: Vec<String>,
    /// Namespaces on the host without an nlink-lab ownership tag. Never
    /// listed and never touched (#29 — they belong to libvirt, podman,
    /// CNI, ...); surfaced only as a count for `status --scan -v`.
    #[serde(skip_serializing_if = "is_zero")]
    pub untagged_ignored: usize,
    /// Labs whose state file claims namespaces that no longer exist on the
    /// host — the mirror case of the above (state with no resources). Most
    /// commonly caused by a reboot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<StaleLab>,
}

/// A state-backed lab with one or more namespaces missing from the host.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct StaleLab {
    /// Lab name.
    pub name: String,
    /// Namespaces claimed by `state.json` that are absent from `ip netns list`.
    pub missing_namespaces: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Orphans {
    /// True when nothing is reportable. `untagged_ignored` is
    /// informational only and does not count.
    pub fn is_empty(&self) -> bool {
        self.bridges.is_empty()
            && self.veths.is_empty()
            && self.netns.is_empty()
            && self.stale.is_empty()
    }
}

/// Scan the host for lab-owned resources without a matching state file, and
/// state-backed labs whose resources are gone from the host.
///
/// Detection rules for *orphans* (resource with no state):
/// - Interfaces matching `^nl[0-9a-f]{8}$` are mgmt bridges; orphan if the
///   hash doesn't match any known lab's `mgmt_bridge_name_for`.
/// - Interfaces starting with `nm` + 8 hex + digits are mgmt veth peers;
///   orphan if the hash portion doesn't match any known lab.
/// - Named netns are orphans only when they carry an nlink-lab ownership
///   tag (`netns_tag`) *and* the tagged lab is not registered, or is
///   registered but its `state.json` no longer lists that namespace.
///   Untagged namespaces are never reported (#29) — name heuristics
///   cannot tell a crashed lab from libvirt/podman/CNI.
///
/// Detection rule for *stale* (state with no resources): for each known lab,
/// compare the namespaces it claims in `state.json` against the host's
/// current namespace list. Any missing namespace marks the lab stale.
pub async fn find_orphans(known: &[crate::state::LabInfo]) -> Orphans {
    let ifnames: Vec<String> = list_ip_links().await;
    let netns: Vec<String> = list_netns();
    let tagged: Vec<(String, Option<String>)> =
        netns.iter().map(|ns| (ns.clone(), tag_of(ns))).collect();

    let lab_namespaces: Vec<(String, Vec<String>)> = known
        .iter()
        .filter_map(|info| {
            crate::state::load_namespace_names(&info.name)
                .ok()
                .map(|ns| (info.name.clone(), ns))
        })
        .collect();
    // Labs that own resources but have no readable state are not
    // necessarily gone: classify them as registered labs whose state is
    // unreadable, which keeps everything they own.
    let candidates = tagged
        .iter()
        .filter_map(|(_, owner)| owner.clone())
        .chain(crate::state::labs_with_pending_journal());
    let mut known_plus = known.to_vec();
    let protected = protected_labs(known, candidates, crate::state::exists, lab_busy);
    // A lab still in a per-user root of an older nlink-lab is live, not
    // debris: before one state root, `sudo destroy --orphans` reaped the
    // labs another identity had deployed (#234).
    let legacy = crate::state::legacy_labs()
        .into_iter()
        .map(|(_, name)| name)
        .filter(|name| !known.iter().any(|k| &k.name == name));
    known_plus.extend(
        protected
            .into_iter()
            .chain(legacy)
            .map(|name| crate::state::LabInfo {
                name,
                node_count: 0,
                created_at: String::new(),
            }),
    );
    let mut orphans = classify_orphans(&ifnames, &tagged, &known_plus, &lab_namespaces);
    orphans.stale = classify_stale(&lab_namespaces, &netns);
    orphans
}

/// The labs among `candidates` that own host resources but are missing
/// from `known` (no parseable `state.json`) and are still not orphans:
///
/// - one whose lock is held: a deploy, apply or destroy is running, and a
///   deploy writes `state.json` only at the end, after its namespaces and
///   journal exist;
/// - one whose `state.json` exists but does not parse: `state::list` leaves
///   it out, but it is a lab, not debris.
///
/// `has_state` and `busy` are [`crate::state::exists`] and [`lab_busy`]
/// outside tests.
fn protected_labs(
    known: &[crate::state::LabInfo],
    candidates: impl Iterator<Item = String>,
    has_state: impl Fn(&str) -> bool,
    busy: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = candidates
        .filter(|name| !name.is_empty() && !known.iter().any(|k| &k.name == name))
        .collect();
    out.sort();
    out.dedup();
    out.retain(|name| has_state(name) || busy(name));
    out
}

/// Whether another process holds the lab's lock. Taking it to find out
/// removes a lock file nobody holds, which is all such a file is.
fn lab_busy(name: &str) -> bool {
    !crate::state::remove_lock_if_unheld(name)
}

/// Ownership tag of a namespace as the classifier sees it: `None` for an
/// untagged namespace, `Some(lab)` otherwise. A tag file that exists but
/// is empty (crash mid-write) still counts as tagged — by a lab nobody
/// knows — so it stays reapable.
fn tag_of(ns: &str) -> Option<String> {
    crate::netns_tag::is_tagged(ns).then(|| crate::netns_tag::lab_of(ns).unwrap_or_default())
}

/// Pure stale-lab classifier.
///
/// For each `(lab_name, claimed_namespaces)` pair, return a [`StaleLab`] if
/// any claimed namespace is absent from `netns_present`. Labs with all
/// namespaces present are omitted. Claimed namespaces are deduplicated and
/// sorted in the output so results are stable for tests.
fn classify_stale(labs: &[(String, Vec<String>)], netns_present: &[String]) -> Vec<StaleLab> {
    let present: std::collections::HashSet<&str> =
        netns_present.iter().map(|s| s.as_str()).collect();
    let mut out = Vec::new();
    for (name, claimed) in labs {
        let mut missing: Vec<String> = claimed
            .iter()
            .filter(|ns| !present.contains(ns.as_str()))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            out.push(StaleLab {
                name: name.clone(),
                missing_namespaces: missing,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Interface names in the root namespace (RTM_GETLINK dump).
async fn list_ip_links() -> Vec<String> {
    match nlink::Connection::<nlink::Route>::new() {
        Ok(conn) => list_ip_links_with(&conn).await,
        Err(e) => {
            tracing::warn!("cannot open netlink socket: {e}");
            Vec::new()
        }
    }
}

async fn list_ip_links_with(conn: &nlink::Connection<nlink::Route>) -> Vec<String> {
    match conn.get_links().await {
        Ok(links) => links
            .iter()
            .filter_map(|l| l.name().map(str::to_string))
            .collect(),
        Err(e) => {
            tracing::warn!("failed to list links: {e}");
            Vec::new()
        }
    }
}

/// Stop every process whose network namespace is one of `namespaces`
/// (named namespaces): SIGTERM, then SIGKILL for whatever still runs a
/// second later. What a lab's commands leave behind — a daemon an `exec`
/// started (`iperf3 -D`), a shell's `&` — is not a tracked process, and
/// it kept a deleted namespace alive, interfaces included, for as long as
/// it ran. Returns how many processes there were.
pub async fn stop_namespace_processes(namespaces: &[String]) -> usize {
    let dir = std::path::Path::new(nlink::netlink::namespace::NETNS_RUN_DIR);
    let ids: Vec<(u64, u64)> = namespaces
        .iter()
        .filter_map(|ns| ns_identity(dir.join(ns)))
        .collect();
    let first = pids_in(&ids);
    if first.is_empty() {
        return 0;
    }
    signal_all(&first, libc::SIGTERM);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let left = pids_in(&ids);
        if left.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                "{} process(es) in the lab's namespaces outlived SIGTERM: killed",
                left.len()
            );
            signal_all(&left, libc::SIGKILL);
            break;
        }
    }
    first.len()
}

/// `(dev, ino)` of a namespace file: equal for two processes in one
/// namespace (what `ip netns pids` compares).
fn ns_identity(path: impl AsRef<std::path::Path>) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path).ok()?;
    Some((md.dev(), md.ino()))
}

/// Every process but this one whose network namespace is one of `ids`.
/// A zombie has none any more, and a process gone mid-walk is skipped.
fn pids_in(ids: &[(u64, u64)]) -> Vec<u32> {
    if ids.is_empty() {
        return Vec::new();
    }
    let me = std::process::id();
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    proc_dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me)
        .filter(|pid| {
            ns_identity(format!("/proc/{pid}/ns/net")).is_some_and(|id| ids.contains(&id))
        })
        .collect()
}

fn signal_all(pids: &[u32], signal: libc::c_int) {
    for &pid in pids {
        // SAFETY: kill(2) on a process just seen in a lab namespace.
        unsafe { libc::kill(pid as i32, signal) };
    }
}

/// Named network namespaces on the host (`/var/run/netns`).
fn list_netns() -> Vec<String> {
    nlink::netlink::namespace::list().unwrap_or_else(|e| {
        tracing::warn!("failed to list namespaces: {e}");
        Vec::new()
    })
}

/// Pure classification — given host state and known labs, emit orphans.
///
/// `netns` pairs every namespace name with its `netns_tag` owner (`None`
/// = untagged). `claimed` lists, per registered lab, the namespaces its
/// `state.json` currently records; a registered lab missing from
/// `claimed` (state unreadable) is treated as still owning everything it
/// tagged, so nothing is reaped on a corrupt state file.
fn classify_orphans(
    ifnames: &[String],
    netns: &[(String, Option<String>)],
    known: &[crate::state::LabInfo],
    claimed: &[(String, Vec<String>)],
) -> Orphans {
    use std::collections::{HashMap, HashSet};

    let mut known_bridges: HashSet<String> = HashSet::new();
    let mut known_hashes: HashSet<String> = HashSet::new();
    let mut known_names: HashSet<&str> = HashSet::new();
    for info in known {
        let bridge = crate::mgmt_bridge_name_for(&info.name);
        if bridge.len() > 2 {
            known_hashes.insert(bridge[2..].to_string());
        }
        known_bridges.insert(bridge);
        known_names.insert(info.name.as_str());
    }
    let claimed_by: HashMap<&str, HashSet<&str>> = claimed
        .iter()
        .map(|(lab, nss)| (lab.as_str(), nss.iter().map(String::as_str).collect()))
        .collect();

    let mut orphans = Orphans::default();
    for ifname in ifnames {
        // Mgmt bridge: `nl` + 8 hex chars, total 10.
        if ifname.len() == 10
            && ifname.starts_with("nl")
            && ifname[2..].chars().all(|c| c.is_ascii_hexdigit())
            && !known_bridges.contains(ifname)
        {
            orphans.bridges.push(ifname.clone());
            continue;
        }
        // Mgmt veth peer: `nm` + 8 hex + 1+ digits.
        if ifname.len() >= 11
            && ifname.starts_with("nm")
            && ifname[2..10].chars().all(|c| c.is_ascii_hexdigit())
            && ifname[10..].chars().all(|c| c.is_ascii_digit())
            && !known_hashes.contains(&ifname[2..10])
        {
            orphans.veths.push(ifname.clone());
        }
    }

    for (ns, tag) in netns {
        let Some(owner) = tag else {
            orphans.untagged_ignored += 1;
            continue;
        };
        let orphaned = match claimed_by.get(owner.as_str()) {
            // Registered lab with readable state: orphan iff it no
            // longer claims this namespace.
            Some(claims) => !claims.contains(ns.as_str()),
            // Registered but state unreadable: leave it alone.
            None if known_names.contains(owner.as_str()) => false,
            // Tagged by a lab that is not registered at all.
            None => true,
        };
        if orphaned {
            orphans.netns.push(ns.clone());
        }
    }

    orphans
}

/// Best-effort cleanup of orphan resources found by [`find_orphans`].
/// Only tagged namespaces ever reach here (see `classify_orphans`).
/// Nothing but [`Cleanup::RemovedLocks`] in the result means there was
/// nothing to reap.
pub async fn reap_orphans(known: &[crate::state::LabInfo]) -> Vec<Cleanup> {
    let mut done = Vec::new();
    // Journals left by interrupted deploys/applies know exactly what
    // was created; unwind them first, then fall back to the tag scan.
    let pending: Vec<String> = crate::state::labs_with_pending_journal()
        .into_iter()
        .filter(|lab| !crate::state::exists(lab))
        .collect();
    for lab in &pending {
        // A deploy persists its journal as it goes and its state only at
        // the end: a journal without state is just as much a deploy that
        // is still running. Its lock says which; hold it while unwinding.
        let Ok(guard) = crate::state::lock_for(lab, "reap") else {
            done.push(Cleanup::Skipped {
                kind: Resource::Lab,
                name: lab.clone(),
                reason: "its lock is held (a deploy may be running)".into(),
            });
            continue;
        };
        if let Some(mut journal) = crate::deploy::rollback::Journal::load_pending(lab) {
            let entries = journal.entries().len();
            journal.unwind().await;
            done.push(Cleanup::Unwound {
                lab: lab.clone(),
                entries,
            });
            let _ = crate::state::remove(lab);
        }
        guard.remove_file();
    }
    // Lock files of labs that no longer exist (#103). Independent of the
    // host resources below, and done before their early returns so it
    // always runs.
    match crate::state::sweep_locks() {
        0 => {}
        count => done.push(Cleanup::RemovedLocks { count }),
    }
    // Host-namespace claims of labs that are gone (#332). A deploy claims
    // under its lab's lock before it writes state: skip a held one.
    for lab in crate::host_registry::stale().unwrap_or_default() {
        if let Ok(guard) = crate::state::lock_for(&lab, "reap")
            && !crate::state::exists(&lab)
        {
            let _ = crate::host_registry::release(&lab);
            done.push(Cleanup::Deleted {
                kind: Resource::Lab,
                name: format!("{lab} (host claims)"),
            });
            guard.remove_file();
        }
    }
    let orphans = find_orphans(known).await;
    if orphans.is_empty() {
        return done;
    }
    // Netns first: deleting a namespace reaps the veths inside it. Each
    // owner's lock is held while its namespaces go, so a deploy that
    // starts meanwhile waits instead of losing them.
    let mut by_owner: std::collections::BTreeMap<String, Vec<&String>> = Default::default();
    for ns in &orphans.netns {
        by_owner
            .entry(tag_of(ns).unwrap_or_default())
            .or_default()
            .push(ns);
    }
    for (owner, namespaces) in by_owner {
        let guard = if owner.is_empty() {
            None
        } else {
            match crate::state::lock_for(&owner, "reap") {
                Ok(guard) => Some(guard),
                Err(_) => {
                    done.push(Cleanup::Skipped {
                        kind: Resource::Lab,
                        name: owner,
                        reason: "its lock is held; its namespaces are kept".into(),
                    });
                    continue;
                }
            }
        };
        let names: Vec<String> = namespaces.iter().map(|ns| ns.to_string()).collect();
        stop_namespace_processes(&names).await;
        for ns in namespaces {
            match nlink::netlink::namespace::delete(ns) {
                Ok(()) => {
                    crate::netns_tag::untag(ns);
                    done.push(Cleanup::Deleted {
                        kind: Resource::Namespace,
                        name: ns.clone(),
                    });
                }
                Err(e) => done.push(Cleanup::failed(format!("delete namespace '{ns}'"), e)),
            }
        }
        if let Some(guard) = guard
            && !crate::state::exists(&owner)
        {
            guard.remove_file();
        }
    }
    if orphans.veths.is_empty() && orphans.bridges.is_empty() {
        return done;
    }
    let conn = match nlink::Connection::<nlink::Route>::new() {
        Ok(c) => c,
        Err(e) => {
            done.push(Cleanup::failed("open a netlink socket", e));
            return done;
        }
    };
    for (kind, names) in [
        (Resource::Veth, &orphans.veths),
        (Resource::MgmtBridge, &orphans.bridges),
    ] {
        for name in names {
            match conn.del_link_if_exists(name.as_str()).await {
                Ok(true) => done.push(Cleanup::Deleted {
                    kind,
                    name: name.clone(),
                }),
                // Already gone — a veth's peer went with a namespace above.
                Ok(false) => {}
                Err(e) => done.push(Cleanup::failed(format!("delete {kind} '{name}'"), e)),
            }
        }
    }
    done
}

/// Number of veth attachments a node has in the topology: point-to-point
/// links, bridge network memberships, and the mgmt veth when the lab has a
/// management network. All are moved into the node's namespace at deploy
/// time and vanish when a container is restarted (#31).
pub fn node_link_count(topo: &crate::Topology, node: &str) -> usize {
    let is_node = |endpoint: &str| {
        endpoint == node || crate::EndpointRef::parse(endpoint).is_some_and(|r| r.node == node)
    };
    let links = topo
        .links
        .iter()
        .filter(|l| l.endpoints.iter().any(|e| is_node(e)))
        .count();
    let members = topo
        .networks
        .values()
        .flat_map(|n| n.members.iter())
        .filter(|m| is_node(m))
        .count();
    let mgmt = usize::from(topo.lab.mgmt_subnet.is_some());
    links + members + mgmt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LabInfo;

    fn info(name: &str) -> LabInfo {
        LabInfo {
            name: name.to_string(),
            node_count: 0,
            created_at: String::new(),
        }
    }

    #[test]
    fn classify_orphans_reports_unknown_mgmt_bridge() {
        let known = vec![info("keep")];
        let keep_bridge = crate::mgmt_bridge_name_for("keep");
        let orphan_bridge = crate::mgmt_bridge_name_for("gone");
        let ifnames = vec![keep_bridge.clone(), orphan_bridge.clone(), "eth0".into()];
        let orphans = classify_orphans(&ifnames, &[], &known, &[]);
        assert_eq!(orphans.bridges, vec![orphan_bridge]);
        assert!(orphans.veths.is_empty());
    }

    #[test]
    fn classify_orphans_skips_known_mgmt_veths() {
        let known = vec![info("keep")];
        let keep_hash = &crate::mgmt_bridge_name_for("keep")[2..];
        let gone_hash = &crate::mgmt_bridge_name_for("gone")[2..];
        let ifnames = vec![
            format!("nm{keep_hash}0"),
            format!("nm{gone_hash}0"),
            format!("nm{gone_hash}42"),
            "lo".into(),
            "eth0".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &known, &[]);
        assert_eq!(orphans.veths.len(), 2);
        assert!(orphans.veths.iter().all(|v| v.contains(gone_hash)));
    }

    fn untagged(ns: &str) -> (String, Option<String>) {
        (ns.to_string(), None)
    }

    fn tagged(ns: &str, lab: &str) -> (String, Option<String>) {
        (ns.to_string(), Some(lab.to_string()))
    }

    #[test]
    fn classify_orphans_ignores_system_netns() {
        // Untagged system netns are never flagged, only counted.
        let netns = vec![untagged("default"), untagged("init")];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty());
        assert_eq!(orphans.untagged_ignored, 2);
        assert!(
            orphans.is_empty(),
            "untagged count must not make it non-empty"
        );
    }

    #[test]
    fn classify_orphans_reports_tagged_unregistered_netns() {
        // Tagged by a lab with no state file at all → orphan (#29).
        let known = vec![info("keep")];
        let claimed = vec![(
            "keep".to_string(),
            vec!["keep-router".to_string(), "keep-mgmt".to_string()],
        )];
        let netns = vec![
            tagged("keep-router", "keep"),
            tagged("keep-mgmt", "keep"),
            tagged("stale-mgmt", "stale"),
            tagged("stale-node1", "stale"),
        ];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert_eq!(orphans.netns.len(), 2);
        assert!(orphans.netns.contains(&"stale-mgmt".to_string()));
        assert!(orphans.netns.contains(&"stale-node1".to_string()));
        assert_eq!(orphans.untagged_ignored, 0);
    }

    #[test]
    fn classify_orphans_keeps_tagged_registered_netns() {
        // Tagged and claimed by a registered lab → not an orphan, even
        // though the name would have matched nothing by prefix.
        let known = vec![info("keep")];
        let claimed = vec![("keep".to_string(), vec!["r1".to_string()])];
        let netns = vec![tagged("r1", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
    }

    #[test]
    fn classify_orphans_reports_tagged_but_unclaimed_netns() {
        // Registered lab whose state.json no longer lists the namespace
        // (e.g. an `apply` removed the node but the delete failed).
        let known = vec![info("keep")];
        let claimed = vec![("keep".to_string(), vec!["keep-a".to_string()])];
        let netns = vec![tagged("keep-a", "keep"), tagged("keep-b", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert_eq!(orphans.netns, vec!["keep-b".to_string()]);
    }

    #[test]
    fn classify_orphans_keeps_tagged_netns_when_state_unreadable() {
        // Registered lab absent from `claimed` (state.json unreadable):
        // be conservative and leave its namespaces alone.
        let known = vec![info("keep")];
        let netns = vec![tagged("keep-a", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
    }

    /// A lab with no readable state is protected while its lock is held
    /// (a deploy writes `state.json` last) or while a `state.json` exists
    /// that does not parse; otherwise its resources are an orphan's.
    #[test]
    fn labs_without_readable_state_are_protected_while_busy_or_corrupt() {
        let known = vec![info("live")];
        let candidates = ["live", "deploying", "corrupt", "gone", "", "deploying"]
            .into_iter()
            .map(String::from);
        let protected = protected_labs(
            &known,
            candidates,
            |name| name == "corrupt",
            |name| name == "deploying",
        );
        assert_eq!(protected, ["corrupt", "deploying"]);

        // Classified as registered labs with unreadable state, their
        // namespaces stay; the plain orphan's go.
        let mut with = known.clone();
        with.extend(protected.iter().map(|n| info(n)));
        let netns = vec![
            tagged("deploying-a", "deploying"),
            tagged("corrupt-a", "corrupt"),
            tagged("gone-a", "gone"),
        ];
        let orphans = classify_orphans(&[], &netns, &with, &[]);
        assert_eq!(orphans.netns, vec!["gone-a".to_string()]);
        // …and so does a deploying lab's mgmt bridge.
        let bridge = crate::mgmt_bridge_name_for("deploying");
        let orphans = classify_orphans(&[bridge], &[], &with, &[]);
        assert!(orphans.bridges.is_empty(), "{orphans:?}");
    }

    #[test]
    fn classify_orphans_never_touches_untagged_dashed_names() {
        // The exact #29 failure: lab-shaped names with hyphens that
        // nlink-lab did not create. No registered labs at all.
        let netns = vec![
            untagged("my-lab-router"),
            untagged("stale-mgmt"),
            untagged("ns-with-dashes"),
        ];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
        assert_eq!(orphans.untagged_ignored, 3);
    }

    #[test]
    fn classify_orphans_ignores_libvirt_and_cni_style_netns() {
        // Names other tools create on a shared host; none are tagged.
        let netns = vec![
            untagged("qemu-1-vm1"),
            untagged("cni-2f3a1b7c-9d4e-4a1b-8c2d-0e1f2a3b4c5d"),
            untagged("netns-podman-abcdef"),
            untagged("mininet-h1"),
            untagged("vrf-blue"),
        ];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
        assert_eq!(orphans.untagged_ignored, 5);
    }

    #[test]
    fn classify_orphans_empty_tag_counts_as_tagged_by_nobody() {
        // A tag file that exists but is empty (crash mid-write) maps to
        // `Some("")`, which no registered lab matches → reapable.
        let netns = vec![tagged("half-written", "")];
        let orphans = classify_orphans(&[], &netns, &[info("keep")], &[]);
        assert_eq!(orphans.netns, vec!["half-written".to_string()]);
    }

    #[test]
    fn node_link_count_counts_links_members_and_mgmt() {
        use crate::{Link, Network, Topology};
        let mut topo = Topology::default();
        topo.links.push(Link {
            endpoints: ["r1:eth0".into(), "h1:eth0".into()],
            addresses: None,
            mtu: None,
        });
        topo.links.push(Link {
            endpoints: ["r1:eth1".into(), "h2:eth0".into()],
            addresses: None,
            mtu: None,
        });
        topo.networks.insert(
            "lan".into(),
            Network {
                members: vec!["h1:eth1".into(), "h3:eth0".into()],
                ..Default::default()
            },
        );
        assert_eq!(node_link_count(&topo, "r1"), 2);
        assert_eq!(node_link_count(&topo, "h1"), 2);
        assert_eq!(node_link_count(&topo, "h3"), 1);
        assert_eq!(node_link_count(&topo, "lonely"), 0);

        // A management network adds one veth to every node.
        topo.lab.mgmt_subnet = Some("10.99.0.0/24".into());
        assert_eq!(node_link_count(&topo, "lonely"), 1);
        assert_eq!(node_link_count(&topo, "r1"), 3);
    }

    #[test]
    fn classify_stale_flags_lab_with_missing_namespace() {
        let labs = vec![(
            "des-3m".into(),
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ],
        )];
        // Host sees nothing — classic WSL-restart case.
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "des-3m");
        assert_eq!(
            stale[0].missing_namespaces,
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ]
        );
    }

    #[test]
    fn classify_stale_ignores_healthy_labs() {
        let labs = vec![(
            "healthy".into(),
            vec!["healthy-a".to_string(), "healthy-b".to_string()],
        )];
        let present = vec!["healthy-a".into(), "healthy-b".into()];
        assert!(classify_stale(&labs, &present).is_empty());
    }

    #[test]
    fn classify_stale_reports_partial_loss() {
        let labs = vec![(
            "partial".into(),
            vec!["partial-a".to_string(), "partial-b".to_string()],
        )];
        let present = vec!["partial-a".into()];
        let stale = classify_stale(&labs, &present);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].missing_namespaces, vec!["partial-b".to_string()]);
    }

    #[test]
    fn classify_stale_orders_deterministically() {
        // Two stale labs provided out of alphabetical order — output should
        // be sorted so test assertions and `--json` output are stable.
        let labs = vec![
            ("zebra".into(), vec!["zebra-a".to_string()]),
            ("alpha".into(), vec!["alpha-a".to_string()]),
        ];
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].name, "alpha");
        assert_eq!(stale[1].name, "zebra");
    }

    #[test]
    fn classify_orphans_ignores_non_lab_interfaces() {
        // Random interface names should not trigger detection.
        let ifnames = vec![
            "eth0".into(),
            "docker0".into(),
            "br-abc".into(),
            "wlp3s0".into(),
            // nl-prefixed but wrong length / non-hex — not a mgmt bridge.
            "nlmonitor".into(),
            "nl1234".into(),
            // nm-prefixed but no trailing digits.
            "nmabcdef01".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &[], &[]);
        assert!(
            orphans.bridges.is_empty() && orphans.veths.is_empty(),
            "false positives: {orphans:?}"
        );
    }
}
