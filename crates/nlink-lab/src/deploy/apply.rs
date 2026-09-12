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
use nlink::netlink::ratelimit::RateLimiter;
use nlink::{Connection, Route};

use super::op::{Op, Plan, RouteSpec, Stage};
use super::rollback::{Journal, Undo};
use super::{NsRef, apply_network_impairments, apply_stack_for_node, guard_namespace_absent};
use crate::container::Runtime;
use crate::error::{Error, Result};
use crate::helpers::parse_rate_bps;
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
    pub mgmt_peers: BTreeMap<String, String>,
    pub dns_injected: bool,
    pub wifi_loaded: bool,
    /// Reconcile the declarative layers with purge (apply mode).
    pub purge: bool,
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
            mgmt_peers: BTreeMap::new(),
            dns_injected: false,
            wifi_loaded: false,
            purge: false,
        })
    }

    /// Environment seeded from a running lab (apply mode).
    pub fn from_running(running: &RunningLab, desired: &Topology) -> Result<Self> {
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
        env.mgmt_peers = running.mgmt_peers().clone();
        env.dns_injected = running.dns_injected();
        env.wifi_loaded = running.wifi_loaded();
        env.purge = true;
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
            .map_err(|e| Error::deploy_failed(format!("connection for '{node}': {e}")))
    }

    fn root() -> Result<Connection<Route>> {
        Connection::<Route>::new()
            .map_err(|e| Error::deploy_failed(format!("root connection: {e}")))
    }
}

/// Execute every op of `plan` in order, journaling inverses.
pub(super) async fn execute(plan: &Plan, env: &mut ApplyEnv, journal: &mut Journal) -> Result<()> {
    let mut last_stage: Option<Stage> = None;
    for op in &plan.ops {
        let stage = op.stage();
        if last_stage != Some(stage) {
            tracing::info!("stage {stage:?}");
            last_stage = Some(stage);
        }
        tracing::debug!("op: {}", op.describe());
        execute_one(op, env, journal).await.map_err(|e| {
            tracing::debug!("op failed: {}: {e}", op.describe());
            e
        })?;
    }
    if plan.ops.iter().any(|o| matches!(o, Op::WifiDaemon { .. })) {
        tracing::info!("waiting for WiFi association...");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
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
                .as_ref()
                .ok_or_else(|| Error::deploy_failed("no container runtime available"))?;
            match pull.as_deref() {
                Some("never") => {}
                Some("always") => rt.pull_image(image)?,
                _ => rt.ensure_image(image)?,
            }
            let info = rt.create(name, image, opts)?;
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
        Op::LoadHwsim { radios, order } => {
            let _ = crate::wifi::load_hwsim_for(&env.lab, *radios)?;
            journal.record(Undo::ReleaseHwsim {
                lab: env.lab.clone(),
            });
            env.wifi_loaded = true;
            use nlink::netlink::Nl80211;
            let nl = nlink::Connection::<Nl80211>::new_async()
                .await
                .map_err(|e| Error::deploy_failed(format!("nl80211 connection: {e}")))?;
            let phys = nl
                .get_phys()
                .await
                .map_err(|e| Error::deploy_failed(format!("failed to list PHYs: {e}")))?;
            if phys.len() < order.len() {
                return Err(Error::deploy_failed(format!(
                    "expected {} hwsim PHYs but found {}",
                    order.len(),
                    phys.len()
                )));
            }
            for (i, (node, _iface)) in order.iter().enumerate() {
                let phy = &phys[i];
                let fd = env
                    .handle(node)?
                    .open_fd()
                    .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node}': {e}")))?;
                nl.set_wiphy_netns(phy.index, fd.as_raw_fd())
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to move phy{} to namespace '{node}': {e}",
                            phy.index
                        ))
                    })?;
            }
        }
        Op::CreateMgmtBridge { name, ip, prefix } => {
            let root = ApplyEnv::root()?;
            root.add_link(nlink::netlink::link::BridgeLink::new(name))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to create mgmt bridge '{name}': {e}"))
                })?;
            journal.record(Undo::DeleteHostLink { name: name.clone() });
            root.set_link_up(name.as_str()).await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up mgmt bridge '{name}': {e}"))
            })?;
            root.add_address_by_name(name, std::net::IpAddr::V4(*ip), *prefix)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to assign IP to mgmt bridge: {e}"))
                })?;
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
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node}': {e}")))?;
            let veth =
                nlink::netlink::link::VethLink::new(peer, "mgmt0").peer_netns_fd(fd.as_raw_fd());
            root.add_link(veth).await.map_err(|e| {
                Error::deploy_failed(format!("failed to create mgmt veth for node '{node}': {e}"))
            })?;
            journal.record(Undo::DeleteHostLink { name: peer.clone() });
            env.mgmt_peers.insert(node.clone(), peer.clone());
            root.set_link_master(peer.as_str(), bridge.as_str())
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to attach '{peer}' to mgmt bridge: {e}"))
                })?;
            root.set_link_up(peer.as_str())
                .await
                .map_err(|e| Error::deploy_failed(format!("failed to bring up '{peer}': {e}")))?;
            let conn = env.route(node)?;
            conn.add_address_by_name("mgmt0", std::net::IpAddr::V4(*node_ip), *prefix)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to assign mgmt IP to '{node}': {e}"))
                })?;
            conn.set_link_up("mgmt0").await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up mgmt0 on '{node}': {e}"))
            })?;
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
            let conn: Connection<Route> = namespace::connection_for(ns)
                .map_err(|e| Error::deploy_failed(format!("connection for '{ns}': {e}")))?;
            let mut bridge = nlink::netlink::link::BridgeLink::new(name);
            if *vlan_filtering {
                bridge = bridge.vlan_filtering(true);
            }
            if let Some(mtu) = mtu {
                bridge = bridge.mtu(*mtu);
            }
            conn.add_link(bridge).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create bridge '{name}' for network '{network}': {e}"
                ))
            })?;
            conn.set_link_up(name.as_str()).await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up bridge '{name}': {e}"))
            })?;
        }
        Op::CreateNetworkVeth {
            node,
            iface,
            peer,
            mgmt_ns,
            bridge,
            vlans,
        } => {
            let mgmt_fd = namespace::open(mgmt_ns)
                .map_err(|e| Error::deploy_failed(format!("failed to open mgmt namespace: {e}")))?;
            let mgmt_conn: Connection<Route> = namespace::connection_for(mgmt_ns)
                .map_err(|e| Error::deploy_failed(format!("connection for '{mgmt_ns}': {e}")))?;
            let node_conn = env.route(node)?;
            let veth =
                nlink::netlink::link::VethLink::new(iface, peer).peer_netns_fd(mgmt_fd.as_raw_fd());
            node_conn.add_link(veth).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create veth for bridge '{bridge}' member '{node}:{iface}' (mgmt peer '{peer}'): {e}"
                ))
            })?;
            journal.record(Undo::DeleteLink {
                ns: env.handle(node)?.clone(),
                iface: iface.clone(),
            });
            mgmt_conn
                .set_link_master(peer.as_str(), bridge.as_str())
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to attach '{peer}' to bridge '{bridge}': {e}"
                    ))
                })?;
            mgmt_conn.set_link_up(peer.as_str()).await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up bridge port '{peer}': {e}"))
            })?;
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
                        Error::deploy_failed(format!(
                            "failed to add VLAN {vid} to port '{peer}' on bridge '{bridge}': {e}"
                        ))
                    })?;
                }
                if let Some(pvid) = pv.pvid
                    && !pv.vlans.contains(&pvid)
                {
                    let v = BridgeVlanBuilder::new(pvid).dev(peer).pvid().untagged();
                    mgmt_conn.add_bridge_vlan(v).await.map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to add PVID {pvid} to port '{peer}' on bridge '{bridge}': {e}"
                        ))
                    })?;
                }
            }
        }
        Op::CreateVeth { a, b, mtu } => {
            let fd_b = env.handle(&b.node)?.open_fd().map_err(|e| {
                Error::deploy_failed(format!("failed to open namespace for '{}': {e}", b.node))
            })?;
            let conn_a = env.route(&a.node)?;
            let mut veth = nlink::netlink::link::VethLink::new(&a.iface, &b.iface)
                .peer_netns_fd(fd_b.as_raw_fd());
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
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node}': {e}")))?;
            host.set_link_netns_fd(cfg.name.as_str(), fd.as_raw_fd())
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
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node}': {e}")))?;
            host.set_link_netns_fd(cfg.name.as_str(), fd.as_raw_fd())
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
                    Error::deploy_failed(format!(
                        "failed to bring up interface '{iface}' in '{node}': {e}"
                    ))
                })?;
            }
        }
        Op::Sysctls { node, entries } => {
            let entries: Vec<(&str, &str)> = entries
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            env.handle(node)?.set_sysctls(&entries).map_err(|e| {
                Error::deploy_failed(format!("failed to apply sysctls for node '{node}': {e}"))
            })?;
        }
        Op::Stack { node, cfg } => {
            let handle = env.handle(node)?.clone();
            #[cfg(feature = "wireguard")]
            let wg = cfg.wireguard.clone();
            #[cfg(not(feature = "wireguard"))]
            let wg: Option<()> = None;
            apply_stack_for_node(
                &handle,
                node,
                cfg.network.clone(),
                cfg.firewall.as_ref(),
                cfg.nat.as_ref(),
                wg,
                env.purge,
            )
            .await?;
        }
        Op::Netem {
            node,
            iface,
            impairment,
        } => {
            let conn = env.route(node)?;
            let netem = super::build_netem(impairment)?;
            conn.replace_qdisc(iface.as_str(), netem)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to apply netem on '{node}:{iface}': {e}"))
                })?;
            journal.record(Undo::ClearQdisc {
                ns: env.handle(node)?.clone(),
                iface: iface.clone(),
            });
        }
        Op::NetworkImpairments => {
            apply_network_impairments(&env.topology, &env.ns).await?;
        }
        Op::RateLimit { node, iface, limit } => {
            let conn = env.route(node)?;
            let mut limiter = RateLimiter::new(iface);
            if let Some(egress) = &limit.egress {
                let bits = parse_rate_bps(egress).map_err(|e| {
                    Error::deploy_failed(format!("bad egress rate on '{node}:{iface}': {e}"))
                })?;
                limiter = limiter.egress(nlink::util::Rate::bits_per_sec(bits));
            }
            if let Some(ingress) = &limit.ingress {
                let bits = parse_rate_bps(ingress).map_err(|e| {
                    Error::deploy_failed(format!("bad ingress rate on '{node}:{iface}': {e}"))
                })?;
                limiter = limiter.ingress(nlink::util::Rate::bits_per_sec(bits));
            }
            let report = limiter.reconcile(&conn).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to reconcile rate limit on '{node}:{iface}': {e}"
                ))
            })?;
            tracing::debug!(endpoint = %format!("{node}:{iface}"), changes = report.changes_made, "rate limit reconcile complete");
            journal.record(Undo::ClearQdisc {
                ns: env.handle(node)?.clone(),
                iface: iface.clone(),
            });
        }
        Op::DnsInject { lab } => {
            let entries = crate::dns::generate_hosts_entries(&env.topology);
            crate::dns::inject_hosts(lab, &entries)?;
            journal.record(Undo::RemoveHosts { lab: lab.clone() });
            env.dns_injected = true;
        }
        Op::DnsNetnsEtc { node: _, ns } => {
            let entries = crate::dns::generate_hosts_entries(&env.topology);
            crate::dns::create_netns_etc(ns, &entries)?;
            journal.record(Undo::RemoveNetnsEtc { ns: ns.clone() });
        }
        Op::StartupDelay { node, delay } => {
            let d = crate::helpers::parse_duration(delay).map_err(|e| {
                Error::deploy_failed(format!(
                    "node '{node}': invalid startup-delay '{delay}': {e}"
                ))
            })?;
            tracing::debug!("startup-delay {delay} for node '{node}'");
            tokio::time::sleep(d).await;
        }
        Op::Exec { node, index, exec } => exec_op(node, *index, exec, env, journal)?,
        Op::Healthcheck {
            node,
            cmd,
            interval,
            timeout,
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
            tracing::info!("waiting for healthcheck on '{node}': {cmd}");
            let deadline = std::time::Instant::now() + limit;
            loop {
                let mut probe = std::process::Command::new("sh");
                probe.args(["-c", cmd]);
                if handle.spawn_output(probe).is_ok_and(|o| o.status.success()) {
                    tracing::info!("healthcheck passed for '{node}'");
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(Error::deploy_failed(format!(
                        "healthcheck timeout for node '{node}': {cmd}"
                    )));
                }
                tokio::time::sleep(every).await;
            }
        }
        Op::WifiDaemon { node, wifi } => {
            let handle = env.handle(node)?.clone();
            match wifi.mode {
                crate::types::WifiMode::Ap => {
                    let conf = crate::wifi::write_config(
                        &env.lab,
                        node,
                        "hostapd.conf",
                        &crate::wifi::generate_hostapd_conf(wifi),
                    )?;
                    journal.record(Undo::CleanupWifiConfigs {
                        lab: env.lab.clone(),
                    });
                    let mut cmd = std::process::Command::new("hostapd");
                    cmd.args(["-B", &conf]);
                    handle.spawn(cmd).map_err(|e| {
                        Error::deploy_failed(format!("failed to start hostapd on '{node}': {e}"))
                    })?;
                }
                crate::types::WifiMode::Station => {
                    let conf = crate::wifi::write_config(
                        &env.lab,
                        node,
                        "wpa.conf",
                        &crate::wifi::generate_wpa_conf(wifi),
                    )?;
                    journal.record(Undo::CleanupWifiConfigs {
                        lab: env.lab.clone(),
                    });
                    let mut cmd = std::process::Command::new("wpa_supplicant");
                    cmd.args(["-B", "-i", &wifi.name, "-c", &conf]);
                    handle.spawn(cmd).map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to start wpa_supplicant on '{node}': {e}"
                        ))
                    })?;
                }
                crate::types::WifiMode::Mesh => {
                    if let Some(mesh_id) = &wifi.mesh_id {
                        let mut cmd = std::process::Command::new("iw");
                        cmd.args([
                            "dev",
                            &wifi.name,
                            "mesh",
                            "join",
                            mesh_id,
                            "freq",
                            &super::freq_from_channel(wifi.channel.unwrap_or(1)),
                        ]);
                        let output = handle.spawn_output(cmd).map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to join mesh '{mesh_id}' on '{node}': {e}"
                            ))
                        })?;
                        if !output.status.success() {
                            tracing::warn!(
                                "mesh join failed on '{node}': {}",
                                String::from_utf8_lossy(&output.stderr)
                            );
                        }
                    }
                }
            }
        }

        // ── removals (apply diffs) ──
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
                let _ = std::process::Command::new(rt.binary())
                    .args(["rm", "-f", id])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            env.containers.remove(node);
            env.ns.remove(node);
        }
        Op::Route { node, route } => {
            let conn = env.route(node)?;
            replace_route_spec(&conn, route).await.map_err(|e| {
                Error::deploy_failed(format!("failed to add route {route} on '{node}': {e}"))
            })?;
            journal.record(Undo::DelRoute {
                ns: env.handle(node)?.clone(),
                route: route.clone(),
            });
        }
        Op::DelRoute { node, route } => {
            if let Ok(conn) = env.route(node)
                && let Err(e) = del_route_lenient(&conn, route).await
            {
                tracing::warn!("failed to delete route {route} on '{node}': {e}");
            }
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
        Op::ClearQdisc { node, iface } | Op::RemoveRateLimit { node, iface } => {
            if let Ok(conn) = env.route(node) {
                conn.del_qdisc_if_exists(iface.as_str(), nlink::TcHandle::ROOT)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to clear qdisc on '{node}:{iface}': {e}"
                        ))
                    })?;
            }
        }
        Op::KillExec { node, index } => {
            let key = format!("{node}:{index}");
            match env.exec_pids.remove(&key) {
                Some(pid) => {
                    let outcome =
                        crate::running::kill_tracked(pid, env.starttimes.get(&pid).copied());
                    tracing::info!("stop exec[{index}] of '{node}' (pid {pid}): {outcome:?}");
                    env.starttimes.remove(&pid);
                    env.process_logs.remove(&pid);
                    env.pids.retain(|(_, p)| *p != pid);
                }
                None => tracing::warn!(
                    "exec[{index}] of '{node}': no tracked pid (started before 0.9, or a \
                     container exec); its process is left running"
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
fn exec_op(
    node: &str,
    index: usize,
    exec: &crate::types::ExecConfig,
    env: &mut ApplyEnv,
    journal: &mut Journal,
) -> Result<()> {
    let handle = env.handle(node)?.clone();
    if let Some(container_id) = handle.container_id() {
        let rt = env
            .runtime
            .as_ref()
            .ok_or_else(|| Error::deploy_failed("no container runtime available"))?;
        let cmd_strs: Vec<&str> = exec.cmd.iter().map(|s| s.as_str()).collect();
        if exec.background {
            let mut args = vec!["exec", "-d", container_id];
            args.extend(&cmd_strs);
            let output = std::process::Command::new(rt.binary())
                .args(&args)
                .output()
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to exec in container '{node}' exec[{index}]: {e}"
                    ))
                })?;
            if !output.status.success() {
                return Err(Error::deploy_failed(format!(
                    "exec[{index}] on container '{node}' failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        } else {
            let output = rt.exec(container_id, &cmd_strs).map_err(|e| {
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
        }
        return Ok(());
    }

    let mut cmd = std::process::Command::new(&exec.cmd[0]);
    cmd.args(&exec.cmd[1..]);
    if !exec.background {
        let output = handle.spawn_output(cmd).map_err(|e| {
            Error::deploy_failed(format!(
                "failed to run command on '{node}' exec[{index}]: {e}"
            ))
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
    let pid = handle.spawn_detached(cmd).map_err(|e| {
        Error::deploy_failed(format!(
            "failed to spawn background process on '{node}' exec[{index}]: {e}"
        ))
    })?;
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
    let final_stdout = log_dir.join(format!("{node}-{basename}-{pid}.stdout"));
    let final_stderr = log_dir.join(format!("{node}-{basename}-{pid}.stderr"));
    let _ = std::fs::rename(&stdout_path, &final_stdout);
    let _ = std::fs::rename(&stderr_path, &final_stderr);
    env.process_logs.insert(
        pid,
        (
            final_stdout.to_string_lossy().to_string(),
            final_stderr.to_string_lossy().to_string(),
        ),
    );
    Ok(())
}

/// `ip route replace` for a [`RouteSpec`] (idempotent).
async fn replace_route_spec(
    conn: &Connection<Route>,
    spec: &RouteSpec,
) -> std::result::Result<(), nlink::netlink::Error> {
    match spec.dest {
        std::net::IpAddr::V4(dst) => conn.replace_route(route_v4(spec, dst)).await,
        std::net::IpAddr::V6(dst) => conn.replace_route(route_v6(spec, dst)).await,
    }
}

/// `ip route del` for a [`RouteSpec`]; an already-absent route is not
/// an error (ESRCH / not found), like the `del_*_if_exists` family.
pub(super) async fn del_route_lenient(
    conn: &Connection<Route>,
    spec: &RouteSpec,
) -> std::result::Result<(), nlink::netlink::Error> {
    let res = match spec.dest {
        std::net::IpAddr::V4(dst) => conn.del_route(route_v4(spec, dst)).await,
        std::net::IpAddr::V6(dst) => conn.del_route(route_v6(spec, dst)).await,
    };
    match res {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() || e.errno() == Some(libc::ESRCH) => Ok(()),
        Err(e) => Err(e),
    }
}

fn route_v4(spec: &RouteSpec, dst: std::net::Ipv4Addr) -> nlink::netlink::route::Ipv4Route {
    let mut r = nlink::netlink::route::Ipv4Route::from_addr(dst, spec.prefix).table(spec.table);
    if let Some(std::net::IpAddr::V4(gw)) = spec.via {
        r = r.gateway(gw);
    }
    if let Some(d) = &spec.dev {
        r = r.dev(d.clone());
    }
    if let Some(m) = spec.metric {
        r = r.metric(m);
    }
    r
}

fn route_v6(spec: &RouteSpec, dst: std::net::Ipv6Addr) -> nlink::netlink::route::Ipv6Route {
    let mut r = nlink::netlink::route::Ipv6Route::from_addr(dst, spec.prefix).table(spec.table);
    if let Some(std::net::IpAddr::V6(gw)) = spec.via {
        r = r.gateway(gw);
    }
    if let Some(d) = &spec.dev {
        r = r.dev(d.clone());
    }
    if let Some(m) = spec.metric {
        r = r.metric(m);
    }
    r
}
