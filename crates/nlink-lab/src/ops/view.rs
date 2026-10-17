//! What a lab looks like from outside: the views `ls`, `status`, `ps`,
//! `stats` and the daemon's read calls share (#233), so the CLI's JSON and
//! the API's say the same thing the same way.

use std::collections::BTreeMap;

use nlink_lab_shared::api::{
    EndpointImpairment, ImpairmentSpec, LabSummary, ProcessDto, SnapshotDto,
};
use sha2::Digest as _;

use crate::error::Result;
use crate::running::{ProcessInfo, RunningLab};
use crate::state::{self, LabSource, LabState};
use crate::types::Impairment;

/// A node of the lab.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct NodeView {
    pub node: String,
    /// `namespace` or `container`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The container's id and init pid, for a container node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerView>,
    /// Interface → addresses, `mgmt0` included.
    pub addresses: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ContainerView {
    pub id: String,
    pub pid: u32,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct LinkView {
    pub endpoints: [String; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addresses: Option<[String; 2]>,
}

/// An endpoint's impairment as it is: a runtime one (`impair`) replaces
/// the declared one.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ImpairmentView {
    pub endpoint: String,
    pub impairment: Impairment,
    /// Set at runtime, not declared.
    pub runtime: bool,
}

/// A named firewall counter (`counter name …`), read live.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct CounterView {
    pub node: String,
    pub name: String,
    pub packets: u64,
    pub bytes: u64,
}

/// One lab, all of it: what `status` prints and `get_lab` returns.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct LabStatus {
    pub lab: String,
    /// RFC 3339.
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<LabSource>,
    pub nodes: Vec<NodeView>,
    pub links: Vec<LinkView>,
    pub networks: Vec<String>,
    pub impairments: Vec<ImpairmentView>,
    pub partitions: Vec<String>,
    pub processes: Vec<ProcessInfo>,
    /// Named firewall counters, read live (root only: absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters: Option<Vec<CounterView>>,
    /// The mgmt bridge's name and every declared subnet, to spot
    /// collisions between labs.
    pub host_resources: serde_json::Value,
}

/// The lab's status. With `counters`, the named firewall counters are
/// read from the kernel (root), on a thread of their own: a blocking call.
pub fn lab_status(running: &RunningLab, state: &LabState, counters: bool) -> LabStatus {
    let topo = running.topology();
    let containers = running.containers();
    let nodes = topo
        .nodes
        .iter()
        .map(|(name, node)| NodeView {
            node: name.clone(),
            kind: if node.image.is_some() {
                "container"
            } else {
                "namespace"
            },
            image: node.image.clone(),
            container: containers.get(name).map(|c| ContainerView {
                id: c.id.clone(),
                pid: c.pid,
            }),
            addresses: running.node_addresses(name).unwrap_or_default(),
        })
        .collect();
    let links = topo
        .links
        .iter()
        .map(|l| LinkView {
            endpoints: l.endpoints.clone(),
            addresses: l.addresses.clone(),
        })
        .collect();
    let counters = counters.then(|| {
        let mut rows = Vec::new();
        for (name, node) in &topo.nodes {
            let counts = topo
                .effective_firewall(node)
                .is_some_and(|fw| fw.all_rules().any(|r| r.counter_name.is_some()));
            if !counts {
                continue;
            }
            match running.firewall_counters_blocking(name) {
                Ok(counters) => rows.extend(counters.into_iter().map(|c| CounterView {
                    node: name.clone(),
                    name: c.name,
                    packets: c.packets,
                    bytes: c.bytes,
                })),
                Err(e) => tracing::warn!("firewall counters of '{name}': {e}"),
            }
        }
        rows
    });
    LabStatus {
        lab: running.name().to_string(),
        created_at: state.created_at.clone(),
        source: state.source.clone(),
        nodes,
        links,
        networks: topo.networks.keys().cloned().collect(),
        impairments: impairments(running),
        partitions: running.partitions().keys().cloned().collect(),
        processes: running.process_status(),
        counters,
        host_resources: host_resources(running),
    }
}

/// Impairments as they are: what `impair` set at runtime replaces the
/// declaration, and a cleared one is gone (#223). Sorted by endpoint.
pub fn impairments(running: &RunningLab) -> Vec<ImpairmentView> {
    let topo = running.topology();
    let live = running.live_impairments();
    let mut rows: Vec<ImpairmentView> = topo
        .impairments
        .iter()
        .filter(|(ep, _)| !live.contains_key(*ep))
        .map(|(ep, imp)| ImpairmentView {
            endpoint: ep.clone(),
            impairment: imp.clone(),
            runtime: false,
        })
        .chain(
            live.iter()
                .filter(|(_, imp)| **imp != Impairment::default())
                .map(|(ep, imp)| ImpairmentView {
                    endpoint: ep.clone(),
                    impairment: imp.clone(),
                    runtime: true,
                }),
        )
        .collect();
    rows.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    rows
}

/// Every endpoint with an impairment or a partition: what it declares,
/// what `impair` set, whether it is partitioned (`get_impairments`).
pub fn endpoint_impairments(running: &RunningLab) -> Vec<EndpointImpairment> {
    let topo = running.topology();
    let live = running.live_impairments();
    let partitions = running.partitions();
    let mut endpoints: Vec<&String> = topo
        .impairments
        .keys()
        .chain(live.keys())
        .chain(partitions.keys())
        .collect();
    endpoints.sort();
    endpoints.dedup();
    endpoints
        .into_iter()
        .map(|ep| EndpointImpairment {
            endpoint: ep.clone(),
            declared: topo.impairments.get(ep).map(ImpairmentSpec::from),
            live: live.get(ep).map(ImpairmentSpec::from),
            partitioned: partitions.contains_key(ep),
        })
        .collect()
}

/// The mgmt bridge's name and every declared subnet (links and
/// networks), to detect collisions between labs without netlink.
pub fn host_resources(running: &RunningLab) -> serde_json::Value {
    let topo = running.topology();
    let mut subnets = std::collections::BTreeSet::new();
    for link in &topo.links {
        for a in link.addresses.iter().flatten() {
            if let Some((_, prefix)) = a.split_once('/')
                && let Some(network) = subnet_of(a)
            {
                subnets.insert(format!("{network}/{prefix}"));
            }
        }
    }
    for network in topo.networks.values() {
        if let Some(s) = &network.subnet {
            subnets.insert(s.clone());
        }
    }
    serde_json::json!({
        "mgmt_bridge": crate::types::mgmt_bridge_name_for(running.name()),
        "subnets": subnets.into_iter().collect::<Vec<_>>(),
    })
}

/// The network address of an IPv4 CIDR (`10.0.0.5/24` → `10.0.0.0`).
fn subnet_of(cidr: &str) -> Option<String> {
    let (ip, prefix) = cidr.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let prefix: u32 = prefix.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    Some(std::net::Ipv4Addr::from(u32::from(ip) & mask).to_string())
}

/// The lab's topology revision: the start of the sha256 of its saved
/// topology. An `apply`, a `restore` or a `node add` changes it.
pub fn revision(lab: &str) -> String {
    match std::fs::read(state::state_dir(lab).join("topology.toml")) {
        Ok(bytes) => {
            let digest = sha2::Sha256::digest(&bytes);
            digest[..8].iter().map(|b| format!("{b:02x}")).collect()
        }
        Err(_) => String::new(),
    }
}

/// One lab as a list shows it.
pub fn summary(running: &RunningLab, state: &LabState) -> LabSummary {
    let topo = running.topology();
    let missing = state
        .namespaces
        .values()
        .any(|ns| !std::path::Path::new("/run/netns").join(ns).exists());
    LabSummary {
        lab: running.name().to_string(),
        nodes: topo.nodes.len(),
        containers: running.containers().len(),
        links: topo.links.len(),
        created_at: state.created_at.clone(),
        health: if missing { "stale" } else { "running" }.to_string(),
        impaired: impairments(running).len(),
        partitioned: running.partitions().len(),
        processes: running.process_status().iter().filter(|p| p.alive).count(),
        revision: revision(running.name()),
    }
}

/// [`summary`] of a lab by name.
pub fn lab_summary(lab: &str) -> Result<LabSummary> {
    let running = RunningLab::load(lab)?;
    let (state, _) = state::load(lab)?;
    Ok(summary(&running, &state))
}

/// Every lab of the state root, summarized; a lab whose state does not
/// load is left out (it is being deployed or destroyed).
pub fn lab_summaries() -> Result<Vec<LabSummary>> {
    Ok(state::list()?
        .into_iter()
        .filter_map(|info| lab_summary(&info.name).ok())
        .collect())
}

/// The lab's tracked processes as the API carries them.
pub fn processes(running: &RunningLab) -> Vec<ProcessDto> {
    running
        .process_status()
        .into_iter()
        .map(|p| ProcessDto {
            node: p.node,
            pid: p.pid,
            alive: p.alive,
            exit_code: p.exit_code,
            stdout: p.stdout_log,
            stderr: p.stderr_log,
        })
        .collect()
}

/// The lab's snapshots as the API carries them.
pub fn snapshots(lab: &str) -> Result<Vec<SnapshotDto>> {
    Ok(state::snapshot_list(lab)?
        .into_iter()
        .map(|s| SnapshotDto {
            name: s.name,
            created_at: s.created_at,
            description: s.description,
            nodes: s.node_count,
            links: s.link_count,
        })
        .collect())
}

/// One node's cgroup usage.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct NodeStats {
    pub node: String,
    /// `namespace` or `container`.
    pub kind: &'static str,
    #[serde(flatten)]
    pub usage: crate::cgroup::Usage,
}

/// What the nodes with a cgroup use (containers and cpu/memory-limited
/// namespace nodes), of every node or of `only`. Sorted by node.
pub fn node_stats(running: &RunningLab, only: Option<&str>) -> Vec<NodeStats> {
    let lab = running.name();
    let wanted = |name: &str| only.is_none_or(|n| n == name);
    let mut rows: Vec<NodeStats> = running
        .topology()
        .nodes
        .iter()
        .filter(|(name, n)| {
            wanted(name) && n.image.is_none() && (n.cpu.is_some() || n.memory.is_some())
        })
        .filter_map(|(name, _)| {
            crate::cgroup::usage(lab, name).map(|usage| NodeStats {
                node: name.clone(),
                kind: "namespace",
                usage,
            })
        })
        .collect();
    for (name, c) in running.containers() {
        if !wanted(name) {
            continue;
        }
        match crate::cgroup::process_dir(c.pid).and_then(|dir| crate::cgroup::usage_of(&dir)) {
            Some(usage) => rows.push(NodeStats {
                node: name.clone(),
                kind: "container",
                usage,
            }),
            None => tracing::warn!("cannot read the cgroup of container node '{name}'"),
        }
    }
    rows.sort_by(|a, b| a.node.cmp(&b.node));
    rows
}

/// Lifecycle events of `lab`, of these `kinds` (every kind when empty),
/// and the cursor to read on from (a byte offset into `events.ndjson`).
///
/// Without a cursor, the latest `limit` events (all of them, the rotated
/// generation included, without a limit); with one an earlier read
/// returned, the first `limit` events after it. A cursor past the end of
/// the file (it was rotated, or the lab deployed again) starts over at
/// the new file.
pub fn read_events(
    lab: &str,
    since: Option<u64>,
    kinds: &[String],
    limit: Option<usize>,
) -> Result<(Vec<serde_json::Value>, u64)> {
    let path = crate::events::events_path(lab);
    let text = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let limit = limit.unwrap_or(usize::MAX);
    let mut events = Vec::new();
    let mut start = since.unwrap_or(0) as usize;
    if start > text.len() {
        start = 0;
    }
    if since.is_none()
        && let Ok(old) = std::fs::read_to_string(path.with_file_name("events.1.ndjson"))
    {
        events.extend(old.lines().filter_map(|l| event_line(l, kinds)));
    }
    let mut next = start;
    // only whole lines: a writer may be mid-line at the end
    for line in text[start..].split_inclusive(|&b| b == b'\n') {
        if !line.ends_with(b"\n") || (since.is_some() && events.len() >= limit) {
            break;
        }
        next += line.len();
        if let Ok(line) = std::str::from_utf8(line)
            && let Some(ev) = event_line(line, kinds)
        {
            events.push(ev);
        }
    }
    if events.len() > limit {
        events.drain(..events.len() - limit);
    }
    Ok((events, next as u64))
}

/// A line of the log as JSON, if it parses and is of one of `kinds`.
fn event_line(line: &str, kinds: &[String]) -> Option<serde_json::Value> {
    let ev: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let kind = ev.get("event")?.as_str()?;
    (kinds.is_empty() || kinds.iter().any(|k| k == kind)).then_some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets_are_network_addresses() {
        assert_eq!(subnet_of("10.0.0.5/24").as_deref(), Some("10.0.0.0"));
        assert_eq!(subnet_of("10.1.2.3/0").as_deref(), Some("0.0.0.0"));
        assert_eq!(subnet_of("10.1.2.3/32").as_deref(), Some("10.1.2.3"));
        assert_eq!(subnet_of("10.1.2.3/33"), None);
        assert_eq!(subnet_of("fd00::1/64"), None);
    }

    #[test]
    fn events_read_on_from_a_cursor() {
        let _guard = state::xdg_state_lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(state::STATE_DIR_ENV, dir.path()) };
        std::fs::create_dir_all(state::state_dir("cur")).unwrap();
        use crate::events::{LifecycleKind, record};
        record("cur", LifecycleKind::Deployed { nodes: 2, links: 1 });
        record("cur", LifecycleKind::SnapshotTaken { name: "a".into() });

        let (all, next) = read_events("cur", None, &[], None).unwrap();
        assert_eq!(all.len(), 2);
        let (none, same) = read_events("cur", Some(next), &[], None).unwrap();
        assert!(none.is_empty());
        assert_eq!(same, next);

        record("cur", LifecycleKind::Destroyed);
        let (new, _) = read_events("cur", Some(next), &[], None).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0]["event"], "destroyed");

        // kinds filter, and a limit keeps the cursor after the last one read
        let (snaps, _) = read_events("cur", None, &["snapshot_taken".into()], None).unwrap();
        assert_eq!(snaps.len(), 1);
        let (first, after_first) = read_events("cur", Some(0), &[], Some(1)).unwrap();
        assert_eq!(first[0]["event"], "deployed");
        let (rest, _) = read_events("cur", Some(after_first), &[], None).unwrap();
        assert_eq!(rest.len(), 2);
        // without a cursor, the latest
        let (latest, end) = read_events("cur", None, &[], Some(1)).unwrap();
        assert_eq!(latest[0]["event"], "destroyed");
        assert_eq!(
            end,
            std::fs::metadata(crate::events::events_path("cur"))
                .unwrap()
                .len()
        );

        // a half-written line waits for its end
        use std::io::Write as _;
        let path = crate::events::events_path("cur");
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"ts\":\"x\",\"lab\":\"cur\",\"ev")
            .unwrap();
        let (torn, at) = read_events("cur", Some(len), &[], None).unwrap();
        assert!(torn.is_empty());
        assert_eq!(at, len);

        // a cursor past the end starts over
        let (again, _) = read_events("cur", Some(len * 10), &[], None).unwrap();
        assert_eq!(again.len(), 3);
        unsafe { std::env::remove_var(state::STATE_DIR_ENV) };
    }
}
