//! The only module that turns a [`Plan`] into kernel / host mutations.
//!
//! `execute` walks the ops in order; each successful mutation records
//! its inverse in the [`Journal`], so a failure part-way unwinds exactly
//! what was created. The same function serves the initial deploy and
//! `apply` (where the plan is `Plan::diff(current, desired)` and the
//! declarative layers run with purge).

use std::collections::BTreeMap;

use nlink::netlink::bridge_vlan::BridgeVlanBuilder;
use nlink::netlink::namespace;
use nlink::{Connection, Route};

use super::op::{Op, Plan, Stage};
use super::rollback::{Journal, Undo};
use super::{NsRef, guard_namespace_absent};
use crate::container::Runtime;
use crate::error::{Error, Result};
use crate::running::RunningLab;
use crate::state::{self, ContainerState};
use crate::types::Topology;

/// Mutable context threaded through an `execute` run: what exists so
/// far (for later ops to reference) and what the state file records.
pub(super) struct ApplyEnv {
    pub lab: String,
    pub topology: Topology,
    pub runtime: Option<Runtime>,
    pub ns: BTreeMap<String, NsRef>,
    pub namespace_names: BTreeMap<String, String>,
    pub containers: BTreeMap<String, ContainerState>,
    pub pids: Vec<(String, u32)>,
    pub starttimes: BTreeMap<u32, u64>,
    /// `"<node>:<index>"` → pid of a background `exec` block (#84).
    pub exec_pids: BTreeMap<String, u32>,
    pub process_logs: BTreeMap<u32, (String, String)>,
    /// Tag of each background process on a container node (#215).
    pub process_tags: BTreeMap<u32, String>,
    pub mgmt_peers: BTreeMap<String, String>,
    pub dns_injected: bool,
    pub wifi_loaded: bool,
    /// Reconcile the declarative layers with purge (apply mode).
    pub purge: bool,
    /// The VRF tables each node declared routes into before this apply,
    /// purged even when the node no longer declares into them.
    pub previous_vrf_tables: BTreeMap<String, Vec<u32>>,
    /// Each node's network config as the running lab has it: what it
    /// declared and the desired one no longer does is removed (#159).
    pub previous_network: BTreeMap<String, std::sync::Arc<nlink::netlink::config::NetworkConfig>>,
    /// Resolved FRR daemon binaries (filled by the preflight when the
    /// plan starts routing daemons, #65).
    pub frr_bins: BTreeMap<crate::frr::Daemon, std::path::PathBuf>,
    /// The operation's progress and cancellation (#236).
    pub ctx: crate::ops::OpCtx,
}

impl ApplyEnv {
    /// Empty environment for a fresh deploy.
    pub fn for_deploy(topology: &Topology) -> Result<Self> {
        let runtime = if topology.nodes.values().any(|n| n.image.is_some()) {
            let rt_config = topology.lab.runtime.clone().unwrap_or_default();
            Some(Runtime::new(&rt_config)?)
        } else {
            None
        };
        Ok(Self {
            lab: topology.lab.name.clone(),
            topology: topology.clone(),
            runtime,
            ns: BTreeMap::new(),
            namespace_names: BTreeMap::new(),
            containers: BTreeMap::new(),
            pids: Vec::new(),
            starttimes: BTreeMap::new(),
            exec_pids: BTreeMap::new(),
            process_logs: BTreeMap::new(),
            process_tags: BTreeMap::new(),
            mgmt_peers: BTreeMap::new(),
            dns_injected: false,
            wifi_loaded: false,
            purge: false,
            previous_vrf_tables: BTreeMap::new(),
            previous_network: BTreeMap::new(),
            frr_bins: BTreeMap::new(),
            ctx: crate::ops::OpCtx::default(),
        })
    }

    /// Environment seeded from a running lab (apply mode).
    /// `current` is the running lab's plan (runtime impairments and
    /// partitions overlaid): its stacks are what each node declared, so
    /// what the desired ones no longer declare is removed.
    pub fn from_running(running: &RunningLab, desired: &Topology, current: &Plan) -> Result<Self> {
        let mut env = Self::for_deploy(desired)?;
        env.lab = running.name().to_string();
        if env.runtime.is_none()
            && let Some(bin) = running.runtime_binary()
        {
            env.runtime = Some(Runtime::with_binary(bin));
        }
        env.namespace_names = running.namespace_names().clone();
        env.containers = running.containers().clone();
        for (node, ns) in &env.namespace_names {
            env.ns
                .insert(node.clone(), NsRef::Named { name: ns.clone() });
        }
        for (node, c) in &env.containers {
            env.ns.insert(
                node.clone(),
                NsRef::Container {
                    id: c.id.clone(),
                    pid: c.pid,
                },
            );
        }
        env.pids = running.pids().to_vec();
        env.starttimes = running.starttimes().clone();
        env.exec_pids = running.exec_pids().clone();
        env.process_logs = running.process_logs_map().clone();
        env.process_tags = running.process_tags_map().clone();
        env.mgmt_peers = running.mgmt_peers().clone();
        env.dns_injected = running.dns_injected();
        env.wifi_loaded = running.wifi_loaded();
        env.purge = true;
        env.previous_vrf_tables = running
            .topology()
            .nodes
            .iter()
            .map(|(name, node)| (name.clone(), node.vrfs.values().map(|v| v.table).collect()))
            .collect();
        env.previous_network = super::stack_networks(current);
        Ok(env)
    }

    fn handle(&self, node: &str) -> Result<&NsRef> {
        self.ns.get(node).ok_or_else(|| Error::NodeNotFound {
            name: node.to_string(),
        })
    }

    fn route(&self, node: &str) -> Result<Connection<Route>> {
        self.handle(node)?
            .connection()
            .map_err(|e| Error::nlink(format!("connection for '{node}'"), e))
    }

    fn root() -> Result<Connection<Route>> {
        Connection::<Route>::new().map_err(|e| Error::nlink("root connection", e))
    }
}

/// Blocking work — a container runtime's CLI, a command run in a node, a
/// wait on a process — on tokio's blocking pool, so the plan's future does
/// not stall the runtime awaiting it (the CLI's progress and Ctrl-C, the
/// daemon's other work on a lab's thread). A panic resumes here; work the
/// runtime dropped (it is shutting down) is an error, so what runs after
/// it, a rollback included, still runs.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(out) => Ok(out),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(Error::deploy_failed(format!(
            "blocking plan work did not finish: {e}"
        ))),
    }
}

/// Execute every op of `plan` in order, journaling inverses.
pub(super) async fn execute(plan: &Plan, env: &mut ApplyEnv, journal: &mut Journal) -> Result<()> {
    let mut last_stage: Option<Stage> = None;
    // Preflight: every FRR daemon the plan needs must exist before the
    // first kernel mutation, so a missing `frr` package fails cleanly.
    let frr_needed: std::collections::BTreeSet<crate::frr::Daemon> = plan
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::FrrDaemons { spec, .. } => Some(spec.daemons.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    if !frr_needed.is_empty() {
        env.frr_bins = crate::frr::require_daemons(&frr_needed)?;
    }
    // the stages in order, with their op counts, for the progress (#236)
    let mut stages: Vec<(Stage, usize)> = Vec::new();
    for op in &plan.ops {
        match stages.last_mut() {
            Some((s, n)) if *s == op.stage() => *n += 1,
            _ => stages.push((op.stage(), 1)),
        }
    }
    let total = plan.ops.len();
    let mut stage_index = 0;
    for (done, op) in plan.ops.iter().enumerate() {
        // cancelled: stop here, and the caller rolls back what was done
        env.ctx.check()?;
        let stage = op.stage();
        if last_stage != Some(stage) {
            tracing::info!("stage {stage:?}");
            last_stage = Some(stage);
            let ops = stages.get(stage_index).map_or(0, |(_, n)| *n);
            stage_index += 1;
            env.ctx.emit(nlink_lab_shared::api::OpPhase::Stage {
                stage: stage.name().to_string(),
                index: stage_index,
                of: stages.len(),
                ops,
            });
        }
        tracing::debug!("op: {}", op.describe());
        execute_one(op, env, journal).await.map_err(|e| {
            tracing::debug!("op failed: {}: {e}", op.describe());
            e
        })?;
        env.ctx.step(done + 1, total, || op.describe());
    }
    if plan.ops.iter().any(|o| matches!(o, Op::WifiDaemon { .. })) {
        tracing::info!("waiting for WiFi association...");
        let settle = std::time::Duration::from_secs(2);
        env.ctx.waiting(
            nlink_lab_shared::api::WaitKind::WifiAssociation,
            None,
            Some(settle),
        );
        env.ctx.sleep(settle).await?;
    }
    wait_frr_converged(plan, env).await?;
    Ok(())
}

/// Wait (bounded) for the OSPF adjacencies / BGP sessions the plan's
/// FRR nodes expect, so `validate { reach … }` and user processes see
/// converged routes. `NLINK_LAB_FRR_WAIT=<secs>` overrides the 30 s
/// budget (`0` disables); without `vtysh` a fixed 5 s settle is used.
async fn wait_frr_converged(plan: &Plan, env: &ApplyEnv) -> Result<()> {
    let nodes: Vec<(&String, &crate::frr::FrrNodeSpec)> = plan
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::FrrDaemons { node, spec } => Some((node, spec.as_ref())),
            _ => None,
        })
        .collect();
    if nodes.is_empty() {
        return Ok(());
    }
    let budget = std::env::var("NLINK_LAB_FRR_WAIT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(30);
    if budget == 0 {
        return Ok(());
    }
    use nlink_lab_shared::api::WaitKind;
    let Some(vtysh) = crate::frr::locate_vtysh() else {
        tracing::info!("waiting 5s for FRR to converge (no vtysh to ask)");
        let settle = std::time::Duration::from_secs(5);
        env.ctx.waiting(
            WaitKind::FrrConvergence,
            Some("no vtysh to ask: a fixed settle".into()),
            Some(settle),
        );
        return env.ctx.sleep(settle).await;
    };
    tracing::info!("waiting for FRR adjacencies (up to {budget}s)...");
    let expected: usize = nodes
        .iter()
        .map(|(_, s)| s.expected_ospf_neighbors + s.expected_bgp_peers)
        .sum();
    env.ctx.waiting(
        WaitKind::FrrConvergence,
        Some(format!("{expected} adjacencies and sessions")),
        Some(std::time::Duration::from_secs(budget)),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget);
    loop {
        let mut all_ok = true;
        let mut up = 0usize;
        for (node, spec) in &nodes {
            let Ok(handle) = env.handle(node) else {
                continue;
            };
            let count = |cmd: &'static str, needle: &'static str| {
                let (handle, vtysh, pathspace) =
                    (handle.clone(), vtysh.clone(), spec.pathspace.clone());
                blocking(move || {
                    let mut c = std::process::Command::new(&vtysh);
                    c.args(["-N", &pathspace, "-c", cmd]);
                    match handle.spawn_output(c) {
                        Ok(out) => String::from_utf8_lossy(&out.stdout).matches(needle).count(),
                        Err(_) => 0,
                    }
                })
            };
            if spec.expected_ospf_neighbors > 0 {
                let full = count("show ip ospf neighbor json", "\"Full/").await?;
                up += full.min(spec.expected_ospf_neighbors);
                all_ok &= full >= spec.expected_ospf_neighbors;
            }
            if spec.expected_bgp_peers > 0 {
                let established =
                    count("show bgp summary json", "\"state\":\"Established\"").await?;
                up += established.min(spec.expected_bgp_peers);
                all_ok &= established >= spec.expected_bgp_peers;
            }
        }
        env.ctx
            .wait_progress(WaitKind::FrrConvergence, up, expected, None);
        if all_ok {
            tracing::info!("FRR converged");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!("FRR did not converge within {budget}s; continuing");
            env.ctx.log(
                "warn",
                format!("FRR did not converge within {budget}s ({up}/{expected}); continuing"),
            );
            return Ok(());
        }
        env.ctx.sleep(std::time::Duration::from_millis(500)).await?;
    }
}

/// Create `dir` (0750) owned by the `frr` user so the daemons, which
/// drop privileges themselves, can write there.
fn frr_dir(dir: &std::path::Path, ids: (u32, u32)) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::deploy_failed(format!("failed to create {}: {e}", dir.display())))?;
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o750))
        .map_err(|e| Error::deploy_failed(format!("chmod {}: {e}", dir.display())))?;
    std::os::unix::fs::chown(dir, Some(ids.0), Some(ids.1))
        .map_err(|e| Error::deploy_failed(format!("chown {}: {e}", dir.display())))?;
    Ok(())
}

fn frr_file(path: &std::path::Path, content: &str, ids: (u32, u32)) -> Result<()> {
    std::fs::write(path, content)
        .map_err(|e| Error::deploy_failed(format!("failed to write {}: {e}", path.display())))?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o640))
        .map_err(|e| Error::deploy_failed(format!("chmod {}: {e}", path.display())))?;
    std::os::unix::fs::chown(path, Some(ids.0), Some(ids.1))
        .map_err(|e| Error::deploy_failed(format!("chown {}: {e}", path.display())))?;
    Ok(())
}

async fn execute_one(op: &Op, env: &mut ApplyEnv, journal: &mut Journal) -> Result<()> {
    match op {
        Op::CreateNamespace { node, ns } => {
            guard_namespace_absent(ns)?;
            namespace::create(ns).map_err(|e| Error::Namespace {
                op: "create",
                ns: ns.clone(),
                source: e,
            })?;
            journal.record(Undo::DeleteNamespace { ns: ns.clone() });
            crate::netns_tag::tag(ns, &env.lab)?;
            env.namespace_names.insert(node.clone(), ns.clone());
            env.ns
                .insert(node.clone(), NsRef::Named { name: ns.clone() });
        }
        Op::CreateContainer {
            node,
            name,
            image,
            pull,
            opts,
        } => {
            let rt = env
                .runtime
                .clone()
                .ok_or_else(|| Error::deploy_failed("no container runtime available"))?;
            // the runtime is its CLI: every call runs off the async worker
            let has_image = || {
                let (rt, image) = (rt.clone(), image.clone());
                blocking(move || rt.has_image(&image))
            };
            match pull.as_deref() {
                // the runtime's `run` pulls a missing image itself, so
                // `never` pulled anyway (#217)
                Some("never") if !has_image().await? => {
                    return Err(Error::deploy_failed(format!(
                        "node '{node}': image '{image}' is not present locally and pull is `never`"
                    )));
                }
                Some("never") => {}
                Some("always") => {
                    env.ctx.waiting(
                        nlink_lab_shared::api::WaitKind::ImagePull {
                            image: image.clone(),
                        },
                        Some(node.clone()),
                        None,
                    );
                    let (rt, image) = (rt.clone(), image.clone());
                    blocking(move || rt.pull_image(&image)).await??
                }
                _ => {
                    if env.ctx.reporting() && !has_image().await? {
                        env.ctx.waiting(
                            nlink_lab_shared::api::WaitKind::ImagePull {
                                image: image.clone(),
                            },
                            Some(node.clone()),
                            None,
                        );
                    }
                    let (rt, image) = (rt.clone(), image.clone());
                    blocking(move || rt.ensure_image(&image)).await??
                }
            }
            let info = {
                let (rt, name, image, opts) =
                    (rt.clone(), name.clone(), image.clone(), opts.clone());
                blocking(move || rt.create(&name, &image, &opts)).await??
            };
            journal.record(Undo::RemoveContainer {
                binary: rt.binary().to_string(),
                id: info.id.clone(),
            });
            env.containers.insert(
                node.clone(),
                ContainerState {
                    id: info.id.clone(),
                    name: info.name.clone(),
                    image: image.clone(),
                    pid: info.pid,
                    starttime: crate::running::host_starttime(info.pid),
                },
            );
            env.ns.insert(
                node.clone(),
                NsRef::Container {
                    id: info.id,
                    pid: info.pid,
                },
            );
        }
        Op::CreateRadio {
            node,
            iface,
            radio,
            mode,
            maker,
        } => {
            crate::wifi::ensure_hwsim()?;
            let maker = env.handle(maker)?.clone();
            let target = env.handle(node)?.clone();
            create_radio(&maker, &target, node, iface, radio, mode, journal).await?;
            env.wifi_loaded = true;
        }
        Op::CreateMgmtBridge { name, ip, prefix } => {
            let root = ApplyEnv::root()?;
            root.add_link(nlink::netlink::link::BridgeLink::new(name))
                .await
                .map_err(|e| Error::nlink(format!("failed to create mgmt bridge '{name}'"), e))?;
            journal.record(Undo::DeleteHostLink { name: name.clone() });
            root.set_link_up(name.as_str())
                .await
                .map_err(|e| Error::nlink(format!("failed to bring up mgmt bridge '{name}'"), e))?;
            add_addr_nodad(&root, name, *ip, *prefix)
                .await
                .map_err(|e| Error::nlink("failed to assign IP to mgmt bridge", e))?;
        }
        Op::CreateMgmtVeth {
            node,
            peer,
            bridge,
            node_ip,
            prefix,
        } => {
            let root = ApplyEnv::root()?;
            let fd = env
                .handle(node)?
                .open_fd()
                .map_err(|e| Error::nlink(format!("open ns fd for '{node}'"), e))?;
            let veth = nlink::netlink::link::VethLink::new(peer, "mgmt0").peer_netns_fd(&fd);
            root.add_link(veth).await.map_err(|e| {
                Error::nlink(format!("failed to create mgmt veth for node '{node}'"), e)
            })?;
            journal.record(Undo::DeleteHostLink { name: peer.clone() });
            env.mgmt_peers.insert(node.clone(), peer.clone());
            root.set_link_master(peer.as_str(), bridge.as_str())
                .await
                .map_err(|e| {
                    Error::nlink(format!("failed to attach '{peer}' to mgmt bridge"), e)
                })?;
            root.set_link_up(peer.as_str())
                .await
                .map_err(|e| Error::nlink(format!("failed to bring up '{peer}'"), e))?;
            let conn = env.route(node)?;
            add_addr_nodad(&conn, "mgmt0", *node_ip, *prefix)
                .await
                .map_err(|e| Error::nlink(format!("failed to assign mgmt IP to '{node}'"), e))?;
            conn.set_link_up("mgmt0")
                .await
                .map_err(|e| Error::nlink(format!("failed to bring up mgmt0 on '{node}'"), e))?;
        }
        Op::CreateMgmtNamespace { ns } => {
            namespace::create(ns).map_err(|e| Error::Namespace {
                op: "create",
                ns: ns.clone(),
                source: e,
            })?;
            journal.record(Undo::DeleteNamespace { ns: ns.clone() });
            crate::netns_tag::tag(ns, &env.lab)?;
        }
        Op::CreateBridge {
            ns,
            network,
            name,
            vlan_filtering,
            mtu,
        } => {
            let conn: Connection<Route> = Connection::new_in(ns.as_str())
                .map_err(|e| Error::nlink(format!("connection for '{ns}'"), e))?;
            let mut bridge = nlink::netlink::link::BridgeLink::new(name);
            if *vlan_filtering {
                bridge = bridge.vlan_filtering(true);
            }
            if let Some(mtu) = mtu {
                bridge = bridge.mtu(*mtu);
            }
            conn.add_link(bridge).await.map_err(|e| {
                Error::nlink(
                    format!("failed to create bridge '{name}' for network '{network}'"),
                    e,
                )
            })?;
            journal.record(Undo::DeleteLink {
                ns: NsRef::Named { name: ns.clone() },
                iface: name.clone(),
            });
            conn.set_link_up(name.as_str())
                .await
                .map_err(|e| Error::nlink(format!("failed to bring up bridge '{name}'"), e))?;
        }
        Op::CreateNetworkVeth {
            node,
            iface,
            peer,
            mgmt_ns,
            bridge,
            vlans,
            mtu,
        } => {
            let mgmt_fd = namespace::open(mgmt_ns)
                .map_err(|e| Error::nlink("failed to open mgmt namespace", e))?;
            let mgmt_conn: Connection<Route> = Connection::new_in(mgmt_ns.as_str())
                .map_err(|e| Error::nlink(format!("connection for '{mgmt_ns}'"), e))?;
            let node_conn = env.route(node)?;
            let mut veth = nlink::netlink::link::VethLink::new(iface, peer).peer_netns_fd(&mgmt_fd);
            // The kernel moves a bridge to its smallest port's MTU as each
            // port joins: without the network's MTU on its ports, the
            // bridge fell back to 1500 (#161).
            if let Some(mtu) = mtu {
                veth = veth.mtu(*mtu).peer_mtu(*mtu);
            }
            node_conn.add_link(veth).await.map_err(|e| {
                Error::nlink(format!("failed to create veth for bridge '{bridge}' member '{node}:{iface}' (mgmt peer '{peer}')"), e)
            })?;
            journal.record(Undo::DeleteLink {
                ns: env.handle(node)?.clone(),
                iface: iface.clone(),
            });
            mgmt_conn
                .set_link_master(peer.as_str(), bridge.as_str())
                .await
                .map_err(|e| {
                    Error::nlink(format!("failed to attach '{peer}' to bridge '{bridge}'"), e)
                })?;
            mgmt_conn
                .set_link_up(peer.as_str())
                .await
                .map_err(|e| Error::nlink(format!("failed to bring up bridge port '{peer}'"), e))?;
            if let Some(pv) = vlans {
                for &vid in &pv.vlans {
                    let mut v = BridgeVlanBuilder::new(vid).dev(peer);
                    if pv.untagged {
                        v = v.untagged();
                    }
                    if Some(vid) == pv.pvid {
                        v = v.pvid().untagged();
                    }
                    mgmt_conn.add_bridge_vlan(v).await.map_err(|e| {
                        Error::nlink(
                            format!(
                                "failed to add VLAN {vid} to port '{peer}' on bridge '{bridge}'"
                            ),
                            e,
                        )
                    })?;
                }
                if let Some(pvid) = pv.pvid
                    && !pv.vlans.contains(&pvid)
                {
                    let v = BridgeVlanBuilder::new(pvid).dev(peer).pvid().untagged();
                    mgmt_conn.add_bridge_vlan(v).await.map_err(|e| {
                        Error::nlink(
                            format!(
                                "failed to add PVID {pvid} to port '{peer}' on bridge '{bridge}'"
                            ),
                            e,
                        )
                    })?;
                }
            }
        }
        Op::CreateVeth { a, b, mtu } => {
            let fd_b = env.handle(&b.node)?.open_fd().map_err(|e| {
                Error::deploy_failed(format!("failed to open namespace for '{}': {e}", b.node))
            })?;
            let conn_a = env.route(&a.node)?;
            let mut veth =
                nlink::netlink::link::VethLink::new(&a.iface, &b.iface).peer_netns_fd(&fd_b);
            if let Some(mtu) = mtu {
                veth = veth.mtu(*mtu);
            }
            conn_a.add_link(veth).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create veth pair {}:{} <-> {}:{}: {e}",
                    a.node, a.iface, b.node, b.iface
                ))
            })?;
            journal.record(Undo::DeleteLink {
                ns: env.handle(&a.node)?.clone(),
                iface: a.iface.clone(),
            });
        }
        Op::CreateMacvlan { node, cfg } => {
            use nlink::netlink::link::{MacvlanLink, MacvlanMode as M};
            let mode = match cfg.mode {
                crate::types::MacvlanMode::Bridge => M::Bridge,
                crate::types::MacvlanMode::Private => M::Private,
                crate::types::MacvlanMode::Vepa => M::Vepa,
                crate::types::MacvlanMode::Passthru => M::Passthru,
            };
            let host = ApplyEnv::root()?;
            host.add_link(MacvlanLink::new(&cfg.name, &cfg.parent).mode(mode))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create macvlan '{}' on node '{node}': {e}",
                        cfg.name
                    ))
                })?;
            journal.record(Undo::DeleteHostLink {
                name: cfg.name.clone(),
            });
            let fd = env
                .handle(node)?
                .open_fd()
                .map_err(|e| Error::nlink(format!("open ns fd for '{node}'"), e))?;
            host.set_link_netns_fd(cfg.name.as_str(), &fd)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to move macvlan '{}' to namespace '{node}': {e}",
                        cfg.name
                    ))
                })?;
        }
        Op::CreateIpvlan { node, cfg } => {
            use nlink::netlink::link::{IpvlanLink, IpvlanMode as M};
            let mode = match cfg.mode {
                crate::types::IpvlanMode::L2 => M::L2,
                crate::types::IpvlanMode::L3 => M::L3,
                crate::types::IpvlanMode::L3S => M::L3S,
            };
            let host = ApplyEnv::root()?;
            host.add_link(IpvlanLink::new(&cfg.name, &cfg.parent).mode(mode))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create ipvlan '{}' on node '{node}': {e}",
                        cfg.name
                    ))
                })?;
            journal.record(Undo::DeleteHostLink {
                name: cfg.name.clone(),
            });
            let fd = env
                .handle(node)?
                .open_fd()
                .map_err(|e| Error::nlink(format!("open ns fd for '{node}'"), e))?;
            host.set_link_netns_fd(cfg.name.as_str(), &fd)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to move ipvlan '{}' to namespace '{node}': {e}",
                        cfg.name
                    ))
                })?;
        }
        Op::LinksUp { node, ifaces } => {
            let conn = env.route(node)?;
            for iface in ifaces {
                conn.set_link_up(iface.as_str()).await.map_err(|e| {
                    Error::nlink(
                        format!("failed to bring up interface '{iface}' in '{node}'"),
                        e,
                    )
                })?;
            }
        }
        Op::Sysctls { node, entries } => {
            let entries: Vec<(&str, &str)> = entries
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            env.handle(node)?.set_sysctls(&entries).map_err(|e| {
                Error::nlink(format!("failed to apply sysctls for node '{node}'"), e)
            })?;
        }
        Op::Stack { node, cfg } => {
            let handle = env.handle(node)?.clone();
            let plan = super::network_plan_options(
                env.purge,
                env.previous_vrf_tables.get(node).map_or(&[], Vec::as_slice),
                env.previous_network.get(node),
            );
            super::apply_stack_for_node(&handle, node, cfg, plan).await?;
        }
        Op::DnsInject { lab } => {
            let entries = crate::dns::generate_hosts_entries(&env.topology);
            crate::dns::inject_hosts(lab, &entries)?;
            // A section the running lab already had is not this run's to
            // remove: a failed apply puts the old one back (#204).
            if !env.dns_injected {
                journal.record(Undo::RemoveHosts { lab: lab.clone() });
            }
            env.dns_injected = true;
        }
        Op::DnsNetnsEtc { node: _, ns } => {
            let entries = crate::dns::generate_hosts_entries(&env.topology);
            let existed = crate::dns::netns_hosts_path(ns).exists();
            crate::dns::create_netns_etc(ns, &entries)?;
            if !existed {
                journal.record(Undo::RemoveNetnsEtc { ns: ns.clone() });
            }
        }
        Op::StartupDelay { node, delay } => {
            let d = crate::helpers::parse_duration(delay).map_err(|e| {
                Error::deploy_failed(format!(
                    "node '{node}': invalid startup-delay '{delay}': {e}"
                ))
            })?;
            tracing::debug!("startup-delay {delay} for node '{node}'");
            env.ctx.sleep(d).await?;
        }
        Op::Exec { node, index, exec } => exec_op(node, *index, exec, env, journal).await?,
        Op::Healthcheck {
            node,
            cmd,
            interval,
            timeout,
            retries,
        } => {
            let parse = |what: &str,
                         v: &Option<String>,
                         default: u64|
             -> Result<std::time::Duration> {
                match v {
                    None => Ok(std::time::Duration::from_secs(default)),
                    Some(s) => crate::helpers::parse_duration(s).map_err(|e| {
                        Error::deploy_failed(format!("node '{node}': invalid {what} '{s}': {e}"))
                    }),
                }
            };
            let every = parse("healthcheck-interval", interval, 1)?;
            let limit = parse("healthcheck-timeout", timeout, 30)?;
            let handle = env.handle(node)?.clone();
            // On a container node the probe runs in the container, with its
            // binaries and files: through the node's handle it ran on the
            // host, in the container's network namespace only (#217).
            let container = handle.container_id().map(str::to_string);
            tracing::info!("waiting for healthcheck on '{node}': {cmd}");
            let what = || nlink_lab_shared::api::WaitKind::Healthcheck { node: node.clone() };
            env.ctx.waiting(what(), Some(cmd.clone()), Some(limit));
            let deadline = std::time::Instant::now() + limit;
            let mut failed = 0u32;
            let runtime = env.runtime.as_ref().map(|rt| rt.binary().to_string());
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let (handle, container, runtime) =
                    (handle.clone(), container.clone(), runtime.clone());
                let (script, at) = (cmd.clone(), node.clone());
                let passed = blocking(move || -> Result<bool> {
                    let child = match &container {
                        Some(id) => {
                            let rt = runtime.ok_or_else(|| {
                                Error::deploy_failed("no container runtime available")
                            })?;
                            let mut probe = std::process::Command::new(rt);
                            probe.args(["exec", id, "sh", "-c", &script]);
                            probe
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                                .map_err(|e| {
                                    Error::deploy_failed(format!("healthcheck on '{at}': {e}"))
                                })
                        }
                        None => {
                            let mut probe = std::process::Command::new("sh");
                            probe.args(["-c", &script]);
                            probe
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped());
                            handle
                                .spawn(probe)
                                .map_err(|e| Error::nlink(format!("healthcheck on '{at}'"), e))
                        }
                    }?;
                    // a probe that hangs is killed at the deadline: it held
                    // the deploy past it (#217)
                    Ok(crate::running::output_with_timeout(
                        child,
                        remaining.max(std::time::Duration::from_millis(100)),
                    )
                    .is_ok_and(|o| o.exit_code == 0))
                })
                .await??;
                if passed {
                    tracing::info!("healthcheck passed for '{node}'");
                    break;
                }
                failed += 1;
                if retries.is_some_and(|r| failed >= r) {
                    return Err(Error::deploy_failed(format!(
                        "healthcheck for node '{node}' failed {failed} time(s): {cmd}"
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(Error::deploy_failed(format!(
                        "healthcheck timeout for node '{node}': {cmd}"
                    )));
                }
                env.ctx.wait_progress(
                    what(),
                    failed as usize,
                    retries.map_or(0, |r| r as usize),
                    Some(format!("{failed} failed probe(s)")),
                );
                env.ctx.sleep(every).await?;
            }
        }
        Op::FrrDaemons { node, spec } => {
            use crate::frr;
            let ids = frr::frr_ids().ok_or_else(|| {
                Error::deploy_failed(format!(
                    "routing frr: user '{}' does not exist",
                    frr::FRR_USER
                ))
            })?;
            let handle = env.handle(node)?.clone();
            for dir in frr::runtime_dirs(&env.lab, node, &spec.pathspace) {
                let existed = dir.exists();
                frr_dir(&dir, ids)?;
                if !existed {
                    journal.record(Undo::RemoveDir { path: dir });
                }
            }
            for (d, text) in &spec.confs {
                frr_file(&frr::conf_path(&env.lab, node, *d), text, ids)?;
            }
            frr_file(
                &frr::node_dir(&env.lab, node).join("frr.conf"),
                &spec.integrated,
                ids,
            )?;
            for d in &spec.daemons {
                let bin = env
                    .frr_bins
                    .get(d)
                    .cloned()
                    .or_else(|| frr::locate_daemon(*d))
                    .ok_or_else(|| {
                        Error::deploy_failed(format!("routing frr: {} not found", d.name()))
                    })?;
                let pidfile = frr::pidfile(&env.lab, node, *d);
                let conf = frr::conf_path(&env.lab, node, *d);
                let log = frr::log_path(&env.lab, node, *d);
                let mut cmd = std::process::Command::new(bin);
                cmd.arg("-d")
                    .args(["-N", &spec.pathspace])
                    .arg("-f")
                    .arg(&conf)
                    .arg("-i")
                    .arg(&pidfile)
                    .arg(format!("--log=file:{}", log.display()))
                    .args(["--log-level", "informational"]);
                let pidfile_str = pidfile.to_string_lossy().to_string();
                let stderr_file =
                    frr::node_dir(&env.lab, node).join(format!("{}.stderr", d.name()));
                start_forking_daemon(
                    &handle,
                    cmd,
                    d.name(),
                    node,
                    &pidfile_str,
                    &stderr_file,
                    env,
                    journal,
                )
                .await?;
                if *d == frr::Daemon::Zebra {
                    // Protocol daemons retry the zapi socket anyway; waiting
                    // keeps their logs clean.
                    let sock = frr::run_dir(&spec.pathspace).join("zserv.api");
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while !sock.exists() && std::time::Instant::now() < deadline {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            }
        }
        Op::KillFrrDaemons {
            node,
            pathspace,
            daemons,
        } => {
            use crate::frr;
            for d in daemons.iter().rev() {
                let pidfile = frr::pidfile(&env.lab, node, *d);
                let pid = std::fs::read_to_string(&pidfile)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                match pid {
                    Some(pid) => {
                        let outcome =
                            crate::running::kill_tracked(pid, env.starttimes.get(&pid).copied());
                        tracing::info!("stop {} of '{node}' (pid {pid}): {outcome:?}", d.name());
                        env.starttimes.remove(&pid);
                        env.pids.retain(|(_, p)| *p != pid);
                    }
                    None => tracing::warn!(
                        "{} of '{node}': no pidfile at {}; left running",
                        d.name(),
                        pidfile.display()
                    ),
                }
            }
            for dir in frr::runtime_dirs(&env.lab, node, pathspace) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        Op::WifiDaemon { node, wifi } => {
            let handle = env.handle(node)?.clone();
            // The lab's other daemons' configs live there too (#204).
            let had_configs = crate::wifi::config_dir(&env.lab).exists();
            match wifi.mode {
                crate::types::WifiMode::Ap => {
                    // one file per interface: a second AP on the node
                    // replaced the first one's (#223)
                    let conf = crate::wifi::write_config(
                        &env.lab,
                        node,
                        &format!("{}-hostapd.conf", wifi.name),
                        &crate::wifi::generate_hostapd_conf(
                            wifi,
                            &crate::wifi::make_ctrl_dir(&env.lab, node)?,
                        ),
                    )?;
                    if !had_configs {
                        journal.record(Undo::CleanupWifiConfigs {
                            lab: env.lab.clone(),
                        });
                    }
                    let pidfile = wifi_pidfile(&env.lab, node, &wifi.name);
                    let mut cmd = std::process::Command::new("hostapd");
                    cmd.args(["-B", "-P", &pidfile, &conf]);
                    start_wifi_daemon(&handle, cmd, "hostapd", node, &pidfile, env, journal)
                        .await?;
                }
                crate::types::WifiMode::Station => {
                    let conf = crate::wifi::write_config(
                        &env.lab,
                        node,
                        &format!("{}-wpa.conf", wifi.name),
                        &crate::wifi::generate_wpa_conf(
                            wifi,
                            &crate::wifi::make_ctrl_dir(&env.lab, node)?,
                        ),
                    )?;
                    if !had_configs {
                        journal.record(Undo::CleanupWifiConfigs {
                            lab: env.lab.clone(),
                        });
                    }
                    let pidfile = wifi_pidfile(&env.lab, node, &wifi.name);
                    let mut cmd = std::process::Command::new("wpa_supplicant");
                    cmd.args(["-B", "-P", &pidfile, "-i", &wifi.name, "-c", &conf]);
                    start_wifi_daemon(&handle, cmd, "wpa_supplicant", node, &pidfile, env, journal)
                        .await?;
                }
                crate::types::WifiMode::Mesh => {
                    // `wifi-mesh-id` and `wifi-channel-range` refuse these
                    // before a deploy; a builder topology can still get here.
                    let mesh_id = wifi.mesh_id.as_deref().ok_or_else(|| {
                        Error::deploy_failed(format!(
                            "Wi-Fi '{node}:{}' is a mesh point without a mesh-id",
                            wifi.name
                        ))
                    })?;
                    let channel = wifi.channel.unwrap_or(1);
                    let freq = crate::wifi::freq_mhz(channel).ok_or_else(|| {
                        Error::deploy_failed(format!(
                            "Wi-Fi '{node}:{}': no frequency for channel {channel}",
                            wifi.name
                        ))
                    })?;
                    let nl: Connection<nlink::netlink::Nl80211> =
                        handle.connection_async().await.map_err(|e| {
                            Error::nlink(format!("nl80211 connection for '{node}'"), e)
                        })?;
                    nl.join_mesh(&wifi.name, mesh_id.as_bytes(), freq)
                        .await
                        .map_err(|e| {
                            Error::nlink(
                                format!(
                                    "failed to join mesh '{mesh_id}' on '{node}:{}'",
                                    wifi.name
                                ),
                                e,
                            )
                        })?;
                }
            }
        }

        // ── removals (apply diffs) ──
        Op::DeleteBridge { ns, name } => match Connection::new_in(ns.as_str()) {
            Ok(conn) => {
                let conn: Connection<Route> = conn;
                if let Err(e) = conn.del_link_if_exists(name.as_str()).await {
                    tracing::warn!("failed to delete bridge '{name}' in '{ns}': {e}");
                }
            }
            Err(e) => tracing::warn!("delete bridge '{name}': no connection to '{ns}': {e}"),
        },
        Op::KillWifiDaemon { node, name, mode } => {
            let pidfile = wifi_pidfile(&env.lab, node, name);
            match mode {
                crate::types::WifiMode::Mesh => {
                    if let Ok(handle) = env.handle(node) {
                        let left = match handle.connection_async::<nlink::netlink::Nl80211>().await
                        {
                            Ok(nl) => nl.leave_mesh(name).await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = left {
                            tracing::warn!("mesh leave on '{node}:{name}': {e}");
                        }
                    }
                }
                _ => {
                    let pid = std::fs::read_to_string(&pidfile)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    match pid {
                        Some(pid) => {
                            let outcome = crate::running::kill_tracked(
                                pid,
                                env.starttimes.get(&pid).copied(),
                            );
                            tracing::info!(
                                "stop wifi daemon of '{node}:{name}' (pid {pid}): {outcome:?}"
                            );
                            env.starttimes.remove(&pid);
                            env.pids.retain(|(_, p)| *p != pid);
                        }
                        None => tracing::warn!(
                            "wifi daemon of '{node}:{name}': no pidfile at {pidfile}; left running"
                        ),
                    }
                    let _ = std::fs::remove_file(&pidfile);
                }
            }
        }
        Op::DeleteRadio { node, radio, .. } => {
            if let Ok(ns) = env.handle(node)
                && let Err(e) = crate::wifi::delete_radio(ns, radio).await
            {
                tracing::warn!("{e}");
            }
        }
        Op::DeleteNamespace { node, ns } => {
            crate::dns::remove_netns_etc(ns);
            if namespace::exists(ns)
                && let Err(e) = namespace::delete(ns)
            {
                tracing::warn!("failed to delete namespace '{ns}': {e}");
            }
            crate::netns_tag::untag(ns);
            env.namespace_names.remove(node);
            env.ns.remove(node);
        }
        Op::RemoveContainer { node, id } => {
            if let Some(rt) = &env.runtime {
                let (binary, id) = (rt.binary().to_string(), id.clone());
                blocking(move || {
                    std::process::Command::new(binary)
                        .args(["rm", "-f", &id])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                })
                .await
                .ok();
            }
            env.containers.remove(node);
            env.ns.remove(node);
        }
        Op::DeleteLink { node, iface } => {
            if let Ok(conn) = env.route(node)
                && let Err(e) = conn.del_link_if_exists(iface.as_str()).await
            {
                tracing::warn!("failed to delete link '{iface}' in '{node}': {e}");
            }
        }
        Op::DeleteHostLink { name } => {
            let root = ApplyEnv::root()?;
            if let Err(e) = root.del_link_if_exists(name.as_str()).await {
                tracing::warn!("failed to delete host link '{name}': {e}");
            }
            env.mgmt_peers.retain(|_, p| p != name);
        }
        Op::KillExec { node, index } => {
            let key = format!("{node}:{index}");
            match env.exec_pids.remove(&key) {
                Some(pid) => {
                    let outcome =
                        crate::running::kill_tracked(pid, env.starttimes.get(&pid).copied());
                    tracing::info!("stop exec[{index}] of '{node}' (pid {pid}): {outcome:?}");
                    stop_in_container(env, node, pid);
                    env.starttimes.remove(&pid);
                    env.process_logs.remove(&pid);
                    env.pids.retain(|(_, p)| *p != pid);
                }
                None => tracing::warn!(
                    "exec[{index}] of '{node}': no tracked pid (started before 0.9); its \
                     process is left running"
                ),
            }
        }
        Op::KillNodeProcesses { node } => {
            let mine: Vec<u32> = env
                .pids
                .iter()
                .filter(|(n, _)| n == node)
                .map(|(_, p)| *p)
                .collect();
            for pid in mine {
                let _ = crate::running::kill_tracked(pid, env.starttimes.get(&pid).copied());
                stop_in_container(env, node, pid);
                env.starttimes.remove(&pid);
                env.process_logs.remove(&pid);
            }
            env.pids.retain(|(n, _)| n != node);
            env.exec_pids
                .retain(|k, _| k.split_once(':').map(|(n, _)| n) != Some(node));
        }
        Op::RemoveDns { lab } => {
            if let Err(e) = crate::dns::remove_hosts(lab) {
                tracing::warn!("failed to remove /etc/hosts entries: {e}");
            }
            for ns in env.namespace_names.values() {
                crate::dns::remove_netns_etc(ns);
            }
            env.dns_injected = false;
        }
    }
    Ok(())
}

/// `run` / `exec` blocks: foreground commands run to completion,
/// background ones are tracked (PID + start time + log files).
async fn exec_op(
    node: &str,
    index: usize,
    exec: &crate::types::ExecConfig,
    env: &mut ApplyEnv,
    journal: &mut Journal,
) -> Result<()> {
    let handle = env.handle(node)?.clone();
    let container_id = handle.container_id().map(str::to_string);

    // Foreground commands just run to completion.
    if !exec.background {
        if let Some(container_id) = &container_id {
            let rt = env
                .runtime
                .clone()
                .ok_or_else(|| Error::deploy_failed("no container runtime available"))?;
            let (id, argv) = (container_id.clone(), exec.cmd.clone());
            let output = blocking(move || {
                let cmd_strs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                rt.exec(&id, &cmd_strs)
            })
            .await?
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to exec in container '{node}' exec[{index}]: {e}"
                ))
            })?;
            if !output.status.success() {
                return Err(Error::deploy_failed(format!(
                    "exec[{index}] on container '{node}' failed (exit {}): {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            return Ok(());
        }
        let mut cmd = std::process::Command::new(&exec.cmd[0]);
        cmd.args(&exec.cmd[1..]);
        let output = {
            let handle = handle.clone();
            blocking(move || handle.spawn_output(cmd)).await?
        }
        .map_err(|e| {
            Error::nlink(
                format!("failed to run command on '{node}' exec[{index}]"),
                e,
            )
        })?;
        if !output.status.success() {
            return Err(Error::deploy_failed(format!(
                "exec[{index}] on node '{node}' failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        return Ok(());
    }

    // Background. Container nodes used to take a separate `exec -d` path that
    // discarded both streams, so nothing a service printed was recoverable --
    // `logs` only ever showed PID 1 (#112). Now both node kinds build a command
    // here and share the logging and tracking below; only the spawn differs.
    let tag = container_id
        .as_ref()
        .map(|_| crate::running::new_process_tag(&env.lab, node));
    let mut cmd = if let Some(container_id) = &container_id {
        let rt = env
            .runtime
            .as_ref()
            .ok_or_else(|| Error::deploy_failed("no container runtime available"))?;
        // No `-d`: we keep the pipes so the output lands in the lab's log dir,
        // exactly like a namespace node's background process.
        let mut cmd = std::process::Command::new(rt.binary());
        cmd.arg("exec");
        // what finds the command inside the container: stopping the
        // `exec` client alone left it running (#215)
        cmd.arg("-e").arg(format!(
            "{}={}",
            crate::running::TAG_VAR,
            tag.as_deref().unwrap_or_default()
        ));
        cmd.arg(container_id);
        cmd.args(&exec.cmd);
        cmd
    } else {
        let mut cmd = std::process::Command::new(&exec.cmd[0]);
        cmd.args(&exec.cmd[1..]);
        cmd
    };

    let log_dir = state::logs_dir(&env.lab);
    let fresh = !log_dir.exists();
    std::fs::create_dir_all(&log_dir)?;
    if fresh {
        journal.record(Undo::RemoveDir {
            path: log_dir.clone(),
        });
    }
    let basename = if exec.cmd.len() >= 3
        && (exec.cmd[0] == "sh" || exec.cmd[0] == "/bin/sh")
        && exec.cmd[1] == "-c"
    {
        exec.cmd[2]
            .split_whitespace()
            .next()
            .and_then(|s| std::path::Path::new(s).file_name()?.to_str())
            .unwrap_or("cmd")
    } else {
        std::path::Path::new(&exec.cmd[0])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("cmd")
    };
    let stdout_path = log_dir.join(format!("{node}-{basename}-{index}.stdout"));
    let stderr_path = log_dir.join(format!("{node}-{basename}-{index}.stderr"));
    cmd.stdout(std::fs::File::create(&stdout_path)?);
    cmd.stderr(std::fs::File::create(&stderr_path)?);
    // A reaper writes `<node>-<basename>-<pid>.rc` when the process exits,
    // next to its renamed `.stdout`, where `ps` and `ProcessExited` read
    // the exit code: as `spawn` does (#320).
    let rc_prefix = log_dir.join(format!("{node}-{basename}-"));
    let pid = if container_id.is_some() {
        // The container is already in its own namespaces; this runs the runtime
        // binary (`podman exec …`) on the host, detached like every other
        // background process: a plain `spawn` made it the deploying
        // process's child, and its zombie once the container went (#204).
        // The exit code is the `exec` client's, which is the command's.
        crate::ns_exec::spawn_detached_path_reaped(
            std::path::Path::new("/proc/self/ns/net"),
            cmd,
            &rc_prefix,
        )
        .map_err(|e| {
            Error::nlink(
                format!("failed to spawn background process in container '{node}' exec[{index}]"),
                e,
            )
        })?
    } else {
        handle.spawn_detached_reaped(cmd, &rc_prefix).map_err(|e| {
            Error::nlink(
                format!("failed to spawn background process on '{node}' exec[{index}]"),
                e,
            )
        })?
    };
    let started = crate::running::host_starttime(pid);
    journal.record(Undo::KillProcess {
        pid,
        starttime: started,
    });
    env.pids.push((node.to_string(), pid));
    if let Some(st) = started {
        env.starttimes.insert(pid, st);
    }
    env.exec_pids.insert(format!("{node}:{index}"), pid);
    // cgroup v2 limits for namespace nodes (#66); containers get theirs
    // from the runtime.
    let had_cgroups = crate::cgroup::lab_exists(&env.lab);
    if container_id.is_none()
        && let Some(n) = env.topology.nodes.get(node)
        && let Some(dir) =
            crate::cgroup::ensure_node(&env.lab, node, n.cpu.as_deref(), n.memory.as_deref())?
    {
        // Removing the lab's subtree kills every node's processes in it:
        // only a run that created it may (#204). The kill above takes
        // this process.
        if !had_cgroups {
            journal.record(Undo::RemoveCgroups {
                lab: env.lab.clone(),
            });
        }
        if let Err(e) = crate::cgroup::attach(&dir, pid) {
            tracing::warn!(
                "node '{node}': cannot move pid {pid} into {}: {e}",
                dir.display()
            );
        }
    }
    // Renamed to include the pid; where a rename fails, the log is recorded
    // where it is. The reaper's `.rc` is named from the prefix whatever
    // happens here, but `ps` looks for it next to the recorded `.stdout`.
    let rename = |from: &std::path::Path, to: std::path::PathBuf| match std::fs::rename(from, &to) {
        Ok(()) => to,
        Err(e) => {
            tracing::warn!(
                "keeping log {}: rename to {}: {e} (`ps` will not show its exit code)",
                from.display(),
                to.display()
            );
            from.to_path_buf()
        }
    };
    let final_stdout = rename(
        &stdout_path,
        log_dir.join(format!("{node}-{basename}-{pid}.stdout")),
    );
    let final_stderr = rename(
        &stderr_path,
        log_dir.join(format!("{node}-{basename}-{pid}.stderr")),
    );
    env.process_logs.insert(
        pid,
        (
            final_stdout.to_string_lossy().to_string(),
            final_stderr.to_string_lossy().to_string(),
        ),
    );
    if let Some(tag) = tag {
        env.process_tags.insert(pid, tag);
    }
    Ok(())
}

/// After its `exec` client is stopped, stop what it started inside a
/// container node: the client forwards no signal (#215).
fn stop_in_container(env: &mut ApplyEnv, node: &str, pid: u32) {
    if let Some(tag) = env.process_tags.remove(&pid)
        && let Some(c) = env.containers.get(node)
    {
        let n = crate::running::stop_tagged(c.pid, &tag);
        tracing::info!("stopped {n} process(es) tagged {tag} in container '{node}'");
    }
}

/// Assign a management address. IPv6 addresses are added with `nodad`
/// so the host-reachable bridge and `mgmt0` are usable immediately
/// instead of sitting in `tentative` for a second; the segment is
/// static and nlink-lab hands out every address, so DAD adds nothing.
async fn add_addr_nodad(
    conn: &nlink::Connection<nlink::netlink::Route>,
    iface: &str,
    ip: std::net::IpAddr,
    prefix: u8,
) -> nlink::Result<()> {
    match ip {
        std::net::IpAddr::V4(_) => conn.add_address_by_name(iface, ip, prefix).await,
        std::net::IpAddr::V6(v6) => {
            conn.add_address(nlink::netlink::addr::Ipv6Address::new(iface, v6, prefix).nodad())
                .await
        }
    }
}

fn wifi_pidfile(lab: &str, node: &str, iface: &str) -> String {
    crate::wifi::config_dir(lab)
        .join(format!("{node}-{iface}.pid"))
        .to_string_lossy()
        .into_owned()
}

/// Run a daemonising (`-B`) wifi daemon to completion of its foreground
/// parent, then track the daemon through the pidfile it wrote (#85): the
/// pid joins `env.pids` with its start time and an `Undo::KillProcess`,
/// so `destroy`, node removal and `KillWifiDaemon` can all stop it.
async fn start_wifi_daemon(
    handle: &NsRef,
    cmd: std::process::Command,
    what: &str,
    node: &str,
    pidfile: &str,
    env: &mut ApplyEnv,
    journal: &mut Journal,
) -> Result<()> {
    let _ = std::fs::remove_file(pidfile);
    let output = {
        let handle = handle.clone();
        blocking(move || handle.spawn_output(cmd)).await?
    }
    .map_err(|e| Error::nlink(format!("failed to start {what} on '{node}'"), e))?;
    if !output.status.success() {
        // hostapd and wpa_supplicant report on stdout.
        let said = [&output.stderr, &output.stdout]
            .map(|b| String::from_utf8_lossy(b).trim().to_string())
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::deploy_failed(format!(
            "{what} on '{node}' exited with {}: {said}",
            output.status,
        )));
    }
    track_pidfile(what, node, pidfile, env, journal).await;
    Ok(())
}

/// Start a daemon that forks itself into the background (FRR's `-d`)
/// *without* capturing its stdio through pipes: the daemonised child
/// inherits them and would keep a pipe open forever, so the launcher
/// would never see EOF. stdout goes to `/dev/null`, stderr to a file
/// that is read back only when the foreground parent exits non-zero.
#[allow(clippy::too_many_arguments)]
async fn start_forking_daemon(
    handle: &NsRef,
    mut cmd: std::process::Command,
    what: &str,
    node: &str,
    pidfile: &str,
    stderr_file: &std::path::Path,
    env: &mut ApplyEnv,
    journal: &mut Journal,
) -> Result<()> {
    let _ = std::fs::remove_file(pidfile);
    let stderr = std::fs::File::create(stderr_file).map_err(|e| {
        Error::deploy_failed(format!("failed to create {}: {e}", stderr_file.display()))
    })?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr));
    let mut child = handle
        .spawn(cmd)
        .map_err(|e| Error::nlink(format!("failed to start {what} on '{node}'"), e))?;
    let status = blocking(move || child.wait())
        .await?
        .map_err(|e| Error::deploy_failed(format!("failed to wait for {what} on '{node}': {e}")))?;
    if !status.success() {
        let msg = std::fs::read_to_string(stderr_file).unwrap_or_default();
        return Err(Error::deploy_failed(format!(
            "{what} on '{node}' exited with {status}: {}",
            msg.trim()
        )));
    }
    track_pidfile(what, node, pidfile, env, journal).await;
    Ok(())
}

/// Wait up to 2 s for `pidfile`, then journal + track the pid it names.
async fn track_pidfile(
    what: &str,
    node: &str,
    pidfile: &str,
    env: &mut ApplyEnv,
    journal: &mut Journal,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            break Some(pid);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    match pid {
        Some(pid) => {
            let started = crate::running::host_starttime(pid);
            journal.record(Undo::KillProcess {
                pid,
                starttime: started,
            });
            env.pids.push((node.to_string(), pid));
            if let Some(st) = started {
                env.starttimes.insert(pid, st);
            }
        }
        None => tracing::warn!("{what} on '{node}' wrote no pidfile at {pidfile}; not tracked"),
    }
}

/// [`Op::CreateRadio`]: a radio with no interface, made in `maker`'s
/// namespace (the lab's netgroup), its wiphy moved to `target`, and the
/// declared interface created on it there (#176). Deleting the radio
/// takes the interface with it.
async fn create_radio(
    maker: &NsRef,
    target: &NsRef,
    node: &str,
    iface: &str,
    radio: &str,
    mode: &crate::types::WifiMode,
    journal: &mut Journal,
) -> Result<()> {
    use nlink::netlink::genl::hwsim::NewRadio;
    use nlink::netlink::{Hwsim, Nl80211};

    let hwsim: Connection<Hwsim> = maker
        .connection_async()
        .await
        .map_err(|e| Error::nlink("hwsim connection", e))?;
    hwsim
        .new_radio(NewRadio::new().name(radio).no_vif())
        .await
        .map_err(|e| {
            Error::nlink(
                format!("failed to create radio '{radio}' for '{node}:{iface}'"),
                e,
            )
        })?;
    journal.record(Undo::DeleteRadio {
        ns: maker.clone(),
        radio: radio.to_string(),
    });

    let nl: Connection<Nl80211> = maker
        .connection_async()
        .await
        .map_err(|e| Error::nlink("nl80211 connection", e))?;
    let phy = nl
        .get_phy_by_name(radio)
        .await
        .map_err(|e| Error::nlink(format!("failed to find the wiphy of radio '{radio}'"), e))?
        .ok_or_else(|| Error::deploy_failed(format!("radio '{radio}' has no wiphy")))?;
    if target != maker {
        let fd = target
            .open_fd()
            .map_err(|e| Error::nlink(format!("open ns fd for '{node}'"), e))?;
        nl.set_wiphy_netns(phy.index, &fd)
            .await
            .map_err(|e| Error::nlink(format!("failed to move radio '{radio}' to '{node}'"), e))?;
        journal.record(Undo::DeleteRadio {
            ns: target.clone(),
            radio: radio.to_string(),
        });
    }

    let nl: Connection<Nl80211> = target
        .connection_async()
        .await
        .map_err(|e| Error::nlink(format!("nl80211 connection for '{node}'"), e))?;
    nl.new_interface(phy.index, iface, crate::wifi::interface_type(mode))
        .await
        .map_err(|e| {
            Error::nlink(
                format!("failed to create Wi-Fi interface '{node}:{iface}'"),
                e,
            )
        })?;
    Ok(())
}
