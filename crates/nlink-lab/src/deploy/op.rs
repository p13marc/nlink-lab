//! Deploy-plan vocabulary: where a node lives (`NsRef`) and, in later
//! steps, the typed operations a plan is made of.

use nlink::Connection;
use nlink::netlink::namespace::{self, NamespaceFd, NamespaceSpec};
use serde::{Deserialize, Serialize};

/// Where a node's network namespace lives.
///
/// One value type for every consumer (deployer, apply, `watch`,
/// `RunningLab`), replacing the deployer's `NodeHandle` and the watch
/// loop's `NsResolver`. It is a thin owner over
/// [`nlink::netlink::namespace::NamespaceSpec`] — call [`spec`](Self::spec)
/// to get the borrowed nlink view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NsRef {
    /// The root (host) network namespace — the mgmt bridge lives here.
    Root,
    /// A named namespace under `/var/run/netns/`.
    Named { name: String },
    /// A container node: its network namespace is `/proc/<pid>/ns/net`.
    Container { id: String, pid: u32 },
}

impl NsRef {
    /// Borrowed nlink view of this namespace.
    pub fn spec(&self) -> NamespaceSpec<'_> {
        match self {
            NsRef::Root => NamespaceSpec::Default,
            NsRef::Named { name } => NamespaceSpec::Named(name),
            NsRef::Container { pid, .. } => NamespaceSpec::Pid(*pid),
        }
    }

    /// `/var/run/netns/<name>` name for a bare namespace node.
    pub fn name(&self) -> Option<&str> {
        match self {
            NsRef::Named { name } => Some(name),
            _ => None,
        }
    }

    /// Container id for a container node.
    pub fn container_id(&self) -> Option<&str> {
        match self {
            NsRef::Container { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Sync-constructible netlink connection (Route, Nftables, …) into
    /// this namespace.
    pub fn connection<
        P: nlink::netlink::ProtocolState + Default + nlink::netlink::construction::SyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        self.spec().connection()
    }

    /// Async-initialised connection (GENL families such as WireGuard).
    pub async fn connection_async<
        P: nlink::netlink::AsyncProtocolInit + nlink::netlink::construction::AsyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        self.spec().connection_async().await
    }

    /// Open the namespace as a file descriptor (for `peer_netns_fd` /
    /// `set_link_netns_fd`).
    pub fn open_fd(&self) -> std::result::Result<NamespaceFd, nlink::netlink::Error> {
        match self {
            NsRef::Root => namespace::open_path("/proc/self/ns/net"),
            NsRef::Named { name } => namespace::open(name),
            NsRef::Container { pid, .. } => namespace::open_pid(*pid),
        }
    }

    /// Path of the namespace file (`/proc/<pid>/ns/net` for containers).
    fn ns_path(&self) -> std::path::PathBuf {
        match self {
            NsRef::Root => "/proc/self/ns/net".into(),
            NsRef::Named { name } => std::path::Path::new(namespace::NETNS_RUN_DIR).join(name),
            NsRef::Container { pid, .. } => format!("/proc/{pid}/ns/net").into(),
        }
    }

    /// Apply sysctls inside the namespace.
    pub fn set_sysctls(
        &self,
        entries: &[(&str, &str)],
    ) -> std::result::Result<(), nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => namespace::set_sysctls(name, entries),
            _ => namespace::set_sysctls_path(self.ns_path(), entries),
        }
    }

    /// Spawn a process inside the namespace, with the `/etc/netns/<ns>`
    /// overlay when one exists (best effort — see [`crate::ns_exec`]).
    pub fn spawn(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Child, nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => crate::ns_exec::spawn(name, cmd),
            _ => self.spec().spawn(cmd),
        }
    }

    /// Spawn a detached background process inside the namespace and
    /// return its pid (see [`crate::ns_exec::spawn_detached`]).
    pub fn spawn_detached(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<u32, nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => crate::ns_exec::spawn_detached(name, cmd),
            _ => crate::ns_exec::spawn_detached_path(&self.ns_path(), cmd),
        }
    }

    /// Run a process to completion inside the namespace.
    pub fn spawn_output(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Output, nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => crate::ns_exec::spawn_output(name, cmd),
            _ => self.spec().spawn_output(cmd),
        }
    }
}

// ─── Plan vocabulary ──────────────────────────────────────

use std::collections::BTreeMap;

use crate::container::CreateOpts;
use crate::types::{
    EndpointRef, ExecConfig, Impairment, IpvlanConfig, MacvlanConfig, RateLimit, WifiConfig,
};

/// Deploy stages, in execution order. A plan is sorted by stage; a
/// rollback / removal walks stages in reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Namespaces and containers.
    Namespaces,
    /// mac80211_hwsim load + PHY moves.
    Hwsim,
    /// Host-reachable mgmt bridge + per-node veth peers.
    MgmtBridge,
    /// Bridge networks (mgmt namespace, bridges, member veths, VLANs).
    Networks,
    /// Point-to-point veth pairs.
    Links,
    /// Host-side macvlan/ipvlan moved into namespaces.
    HostLinks,
    /// Bring nlink-lab-created interfaces up.
    LinksUp,
    /// Per-node sysctls.
    Sysctls,
    /// Declarative per-node stack (links/addresses/routes, nftables, WireGuard).
    Stack,
    /// Routes in non-main tables (VRF), owned explicitly because nlink's
    /// purge only converges the main table (#83).
    Routes,
    /// Traffic control: netem, per-pair impairments, rate limits.
    Tc,
    /// /etc/hosts and per-namespace /etc overlays.
    Dns,
    /// Background processes and healthchecks.
    Processes,
    /// hostapd / wpa_supplicant / mesh join.
    Wifi,
}

/// One route in a non-main table, fully resolved so the delete can
/// replay exactly what the add declared (kernel route identity is
/// destination + table + metric; gateway/dev disambiguate multipath).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSpec {
    pub dest: std::net::IpAddr,
    pub prefix: u8,
    pub table: u32,
    pub via: Option<std::net::IpAddr>,
    pub dev: Option<String>,
    pub metric: Option<u32>,
}

impl std::fmt::Display for RouteSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{} table {}", self.dest, self.prefix, self.table)?;
        if let Some(gw) = &self.via {
            write!(f, " via {gw}")?;
        }
        if let Some(d) = &self.dev {
            write!(f, " dev {d}")?;
        }
        if let Some(m) = self.metric {
            write!(f, " metric {m}")?;
        }
        Ok(())
    }
}

/// Bridge-port VLAN settings for one network member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortVlans {
    pub vlans: Vec<u16>,
    pub pvid: Option<u16>,
    pub untagged: bool,
}

/// Per-node declarative stack: what step 11c applies.
#[derive(Debug, Clone)]
pub struct StackConfig {
    pub network: nlink::netlink::config::NetworkConfig,
    pub firewall: Option<crate::types::FirewallConfig>,
    pub nat: Option<crate::types::NatConfig>,
    #[cfg(feature = "wireguard")]
    pub wireguard: Option<nlink::netlink::genl::wireguard::WireguardConfig>,
}

/// One unit of work in a deploy or apply plan.
///
/// Ops are pure data: building them touches nothing. `apply.rs` is
/// the only place that turns them into kernel/host mutations, and
/// every mutation records its inverse (`rollback::Undo`).
#[derive(Debug, Clone)]
pub enum Op {
    CreateNamespace {
        node: String,
        ns: String,
    },
    CreateContainer {
        node: String,
        name: String,
        image: String,
        pull: Option<String>,
        opts: CreateOpts,
    },
    LoadHwsim {
        radios: u32,
        order: Vec<(String, String)>,
    },
    CreateMgmtBridge {
        name: String,
        ip: std::net::Ipv4Addr,
        prefix: u8,
    },
    CreateMgmtVeth {
        node: String,
        peer: String,
        bridge: String,
        node_ip: std::net::Ipv4Addr,
        prefix: u8,
    },
    CreateMgmtNamespace {
        ns: String,
    },
    CreateBridge {
        ns: String,
        network: String,
        name: String,
        vlan_filtering: bool,
        mtu: Option<u32>,
    },
    CreateNetworkVeth {
        node: String,
        iface: String,
        peer: String,
        mgmt_ns: String,
        bridge: String,
        vlans: Option<PortVlans>,
    },
    CreateVeth {
        a: EndpointRef,
        b: EndpointRef,
        mtu: Option<u32>,
    },
    CreateMacvlan {
        node: String,
        cfg: MacvlanConfig,
    },
    CreateIpvlan {
        node: String,
        cfg: IpvlanConfig,
    },
    LinksUp {
        node: String,
        ifaces: Vec<String>,
    },
    Sysctls {
        node: String,
        entries: BTreeMap<String, String>,
    },
    Stack {
        node: String,
        cfg: Box<StackConfig>,
    },
    /// A route in a non-main table (VRF); see [`Stage::Routes`].
    Route {
        node: String,
        route: RouteSpec,
    },
    Netem {
        node: String,
        iface: String,
        impairment: Impairment,
    },
    /// All per-pair `impair a -- b` matrices of every `network` block.
    NetworkImpairments,
    RateLimit {
        node: String,
        iface: String,
        limit: RateLimit,
    },
    DnsInject {
        lab: String,
    },
    DnsNetnsEtc {
        node: String,
        ns: String,
    },
    StartupDelay {
        node: String,
        delay: String,
    },
    Exec {
        node: String,
        index: usize,
        exec: ExecConfig,
    },
    Healthcheck {
        node: String,
        cmd: String,
        interval: Option<String>,
        timeout: Option<String>,
    },
    WifiDaemon {
        node: String,
        wifi: WifiConfig,
    },

    // ── diff-only (apply): inverses of the ops above ──
    DeleteNamespace {
        node: String,
        ns: String,
    },
    RemoveContainer {
        node: String,
        id: String,
    },
    DeleteLink {
        node: String,
        iface: String,
    },
    DeleteHostLink {
        name: String,
    },
    DelRoute {
        node: String,
        route: RouteSpec,
    },
    ClearQdisc {
        node: String,
        iface: String,
    },
    RemoveRateLimit {
        node: String,
        iface: String,
    },
    KillNodeProcesses {
        node: String,
    },
    RemoveDns {
        lab: String,
    },
}

impl Op {
    /// Stage the op executes in.
    pub fn stage(&self) -> Stage {
        use Op::*;
        match self {
            CreateNamespace { .. }
            | CreateContainer { .. }
            | DeleteNamespace { .. }
            | RemoveContainer { .. } => Stage::Namespaces,
            LoadHwsim { .. } => Stage::Hwsim,
            CreateMgmtBridge { .. } | CreateMgmtVeth { .. } | DeleteHostLink { .. } => {
                Stage::MgmtBridge
            }
            CreateMgmtNamespace { .. } | CreateBridge { .. } | CreateNetworkVeth { .. } => {
                Stage::Networks
            }
            CreateVeth { .. } | DeleteLink { .. } => Stage::Links,
            CreateMacvlan { .. } | CreateIpvlan { .. } => Stage::HostLinks,
            LinksUp { .. } => Stage::LinksUp,
            Sysctls { .. } => Stage::Sysctls,
            Stack { .. } => Stage::Stack,
            Route { .. } | DelRoute { .. } => Stage::Routes,
            Netem { .. }
            | NetworkImpairments
            | RateLimit { .. }
            | ClearQdisc { .. }
            | RemoveRateLimit { .. } => Stage::Tc,
            DnsInject { .. } | DnsNetnsEtc { .. } | RemoveDns { .. } => Stage::Dns,
            StartupDelay { .. } | Exec { .. } | Healthcheck { .. } | KillNodeProcesses { .. } => {
                Stage::Processes
            }
            WifiDaemon { .. } => Stage::Wifi,
        }
    }

    /// Stable identity used to diff two plans. Two ops with the same
    /// key describe the same resource.
    pub fn key(&self) -> String {
        use Op::*;
        match self {
            CreateNamespace { ns, .. } | DeleteNamespace { ns, .. } => format!("ns:{ns}"),
            CreateContainer { node, .. } | RemoveContainer { node, .. } => {
                format!("container:{node}")
            }
            LoadHwsim { .. } => "hwsim".into(),
            CreateMgmtBridge { name, .. } => format!("hostlink:{name}"),
            CreateMgmtVeth { peer, .. } => format!("hostlink:{peer}"),
            DeleteHostLink { name } => format!("hostlink:{name}"),
            CreateMgmtNamespace { ns } => format!("ns:{ns}"),
            CreateBridge { ns, name, .. } => format!("bridge:{ns}:{name}"),
            CreateNetworkVeth { node, iface, .. } | DeleteLink { node, iface } => {
                format!("link:{node}:{iface}")
            }
            CreateVeth { a, .. } => format!("link:{}:{}", a.node, a.iface),
            CreateMacvlan { node, cfg } => format!("link:{node}:{}", cfg.name),
            CreateIpvlan { node, cfg } => format!("link:{node}:{}", cfg.name),
            LinksUp { node, .. } => format!("linksup:{node}"),
            Sysctls { node, .. } => format!("sysctls:{node}"),
            Stack { node, .. } => format!("stack:{node}"),
            Route { node, route } | DelRoute { node, route } => format!(
                "route:{node}:{}:{}/{}",
                route.table, route.dest, route.prefix
            ),
            Netem { node, iface, .. } | ClearQdisc { node, iface } => {
                format!("qdisc:{node}:{iface}")
            }
            NetworkImpairments => "network-impairments".into(),
            RateLimit { node, iface, .. } | RemoveRateLimit { node, iface } => {
                format!("ratelimit:{node}:{iface}")
            }
            DnsInject { lab } | RemoveDns { lab } => format!("dns:{lab}"),
            DnsNetnsEtc { ns, .. } => format!("dns-etc:{ns}"),
            StartupDelay { node, .. } => format!("delay:{node}"),
            Exec { node, index, .. } => format!("exec:{node}:{index}"),
            Healthcheck { node, .. } => format!("healthcheck:{node}"),
            WifiDaemon { node, wifi } => format!("wifi:{node}:{}", wifi.name),
            KillNodeProcesses { node } => format!("procs:{node}"),
        }
    }

    /// The op that undoes this one at apply time, when the resource
    /// disappears from the desired topology. `None` for ops whose
    /// removal is implied by another inverse (a namespace deletion
    /// takes its links, qdiscs and sysctls with it) or that have no
    /// meaningful inverse (a delay, a one-shot exec).
    pub fn inverse(&self) -> Option<Op> {
        use Op::*;
        Some(match self {
            CreateNamespace { node, ns } => DeleteNamespace {
                node: node.clone(),
                ns: ns.clone(),
            },
            CreateMgmtNamespace { ns } => DeleteNamespace {
                node: String::new(),
                ns: ns.clone(),
            },
            CreateContainer { node, name, .. } => RemoveContainer {
                node: node.clone(),
                id: name.clone(),
            },
            CreateMgmtBridge { name, .. } => DeleteHostLink { name: name.clone() },
            CreateMgmtVeth { peer, .. } => DeleteHostLink { name: peer.clone() },
            CreateNetworkVeth { node, iface, .. } => DeleteLink {
                node: node.clone(),
                iface: iface.clone(),
            },
            CreateVeth { a, .. } => DeleteLink {
                node: a.node.clone(),
                iface: a.iface.clone(),
            },
            CreateMacvlan { node, cfg } => DeleteLink {
                node: node.clone(),
                iface: cfg.name.clone(),
            },
            CreateIpvlan { node, cfg } => DeleteLink {
                node: node.clone(),
                iface: cfg.name.clone(),
            },
            Route { node, route } => DelRoute {
                node: node.clone(),
                route: route.clone(),
            },
            Netem { node, iface, .. } => ClearQdisc {
                node: node.clone(),
                iface: iface.clone(),
            },
            RateLimit { node, iface, .. } => RemoveRateLimit {
                node: node.clone(),
                iface: iface.clone(),
            },
            DnsInject { lab } => RemoveDns { lab: lab.clone() },
            Exec { node, exec, .. } if exec.background => KillNodeProcesses { node: node.clone() },
            _ => return None,
        })
    }

    /// True for the diff-only removal ops.
    pub fn is_removal(&self) -> bool {
        use Op::*;
        matches!(
            self,
            DeleteNamespace { .. }
                | RemoveContainer { .. }
                | DeleteLink { .. }
                | DeleteHostLink { .. }
                | DelRoute { .. }
                | ClearQdisc { .. }
                | RemoveRateLimit { .. }
                | KillNodeProcesses { .. }
                | RemoveDns { .. }
        )
    }

    /// One-line human description (for `deploy --dry-run` and logs).
    pub fn describe(&self) -> String {
        use Op::*;
        match self {
            CreateNamespace { node, ns } => format!("create namespace {ns} for {node}"),
            CreateContainer { node, image, .. } => format!("create container for {node} ({image})"),
            LoadHwsim { radios, .. } => format!("load mac80211_hwsim radios={radios}"),
            CreateMgmtBridge { name, ip, prefix } => {
                format!("create mgmt bridge {name} {ip}/{prefix}")
            }
            CreateMgmtVeth {
                node,
                peer,
                node_ip,
                ..
            } => format!("mgmt veth {peer} ↔ {node}:mgmt0 {node_ip}"),
            CreateMgmtNamespace { ns } => format!("create bridge namespace {ns}"),
            CreateBridge { network, name, .. } => {
                format!("create bridge {name} for network {network}")
            }
            CreateNetworkVeth {
                node,
                iface,
                bridge,
                ..
            } => format!("attach {node}:{iface} to {bridge}"),
            CreateVeth { a, b, .. } => {
                format!("veth {}:{} ↔ {}:{}", a.node, a.iface, b.node, b.iface)
            }
            CreateMacvlan { node, cfg } => {
                format!("macvlan {} (parent {}) → {node}", cfg.name, cfg.parent)
            }
            CreateIpvlan { node, cfg } => {
                format!("ipvlan {} (parent {}) → {node}", cfg.name, cfg.parent)
            }
            LinksUp { node, ifaces } => format!("{node}: up {}", ifaces.join(",")),
            Sysctls { node, entries } => format!("{node}: {} sysctl(s)", entries.len()),
            Stack { node, .. } => {
                format!("{node}: reconcile links/addresses/routes, nftables, wireguard")
            }
            Route { node, route } => format!("{node}: route {route}"),
            Netem { node, iface, .. } => format!("netem on {node}:{iface}"),
            NetworkImpairments => "per-pair network impairments".into(),
            RateLimit { node, iface, .. } => format!("rate limit on {node}:{iface}"),
            DnsInject { lab } => format!("inject /etc/hosts entries for {lab}"),
            DnsNetnsEtc { ns, .. } => format!("/etc/netns/{ns} overlay"),
            StartupDelay { node, delay } => format!("{node}: wait {delay}"),
            Exec { node, index, exec } => format!(
                "{node}: exec[{index}] {}{}",
                exec.cmd.join(" "),
                if exec.background { " (background)" } else { "" }
            ),
            Healthcheck { node, cmd, .. } => format!("{node}: healthcheck `{cmd}`"),
            WifiDaemon { node, wifi } => format!("{node}: wifi {} ({:?})", wifi.name, wifi.mode),
            DeleteNamespace { ns, .. } => format!("delete namespace {ns}"),
            RemoveContainer { node, .. } => format!("remove container of {node}"),
            DeleteLink { node, iface } => format!("delete link {node}:{iface}"),
            DeleteHostLink { name } => format!("delete host link {name}"),
            DelRoute { node, route } => format!("{node}: delete route {route}"),
            ClearQdisc { node, iface } => format!("clear qdisc on {node}:{iface}"),
            RemoveRateLimit { node, iface } => format!("remove rate limit on {node}:{iface}"),
            KillNodeProcesses { node } => format!("stop background processes of {node}"),
            RemoveDns { lab } => format!("remove /etc/hosts entries for {lab}"),
        }
    }
}

/// An ordered list of ops: the whole deploy (or the difference between
/// two deploys) as data.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub ops: Vec<Op>,
}

impl Plan {
    /// Sort by stage (stable: planner insertion order within a stage).
    pub fn sorted(mut self) -> Self {
        self.ops.sort_by_key(|op| op.stage());
        self
    }

    /// Ops of one stage, in order.
    pub fn stage(&self, stage: Stage) -> impl Iterator<Item = &Op> {
        self.ops.iter().filter(move |op| op.stage() == stage)
    }

    /// `desired − current` as an executable plan: removals first
    /// (inverse ops, reverse stage order), then every op that is new
    /// or whose payload changed, in stage order. `Stack` ops of nodes
    /// present on both sides are always included — the declarative
    /// layers reconcile idempotently, so an unchanged node costs zero
    /// kernel calls.
    pub fn diff(current: &Plan, desired: &Plan) -> Plan {
        let cur: BTreeMap<String, &Op> = current.ops.iter().map(|o| (o.key(), o)).collect();
        let des: BTreeMap<String, &Op> = desired.ops.iter().map(|o| (o.key(), o)).collect();

        let mut removals: Vec<Op> = current
            .ops
            .iter()
            .filter(|o| !des.contains_key(&o.key()))
            .filter_map(|o| o.inverse())
            .collect();
        // Do not delete links/qdiscs of a namespace that is going away.
        let dying: std::collections::BTreeSet<String> = removals
            .iter()
            .filter_map(|o| match o {
                Op::DeleteNamespace { node, .. } | Op::RemoveContainer { node, .. } => {
                    Some(node.clone())
                }
                _ => None,
            })
            .collect();
        removals.retain(|o| match o {
            Op::DeleteLink { node, .. }
            | Op::DelRoute { node, .. }
            | Op::ClearQdisc { node, .. }
            | Op::RemoveRateLimit { node, .. }
            | Op::KillNodeProcesses { node } => !dying.contains(node.as_str()),
            _ => true,
        });
        removals.sort_by_key(|o| std::cmp::Reverse(o.stage()));
        // dedupe (several background execs on one node → one kill op)
        let mut seen = std::collections::BTreeSet::new();
        removals.retain(|o| seen.insert(o.key() + &format!("{:?}", o.stage())));

        let mut changes: Vec<Op> = Vec::new();
        for op in &desired.ops {
            match cur.get(&op.key()) {
                None => changes.push(op.clone()),
                Some(existing) => match op {
                    // reconcile-style ops: always re-run (idempotent)
                    Op::Stack { .. }
                    | Op::LinksUp { .. }
                    | Op::NetworkImpairments
                    | Op::DnsInject { .. }
                    | Op::DnsNetnsEtc { .. } => changes.push(op.clone()),
                    // one-shot process ops only run for new nodes
                    Op::Exec { .. }
                    | Op::Healthcheck { .. }
                    | Op::StartupDelay { .. }
                    | Op::WifiDaemon { .. } => {}
                    // everything else: re-create when the payload changed
                    _ => {
                        if format!("{existing:?}") != format!("{op:?}") {
                            if let Some(inv) = existing.inverse() {
                                removals.push(inv);
                            }
                            changes.push(op.clone());
                        }
                    }
                },
            }
        }
        changes.sort_by_key(|o| o.stage());
        let mut ops = removals;
        ops.extend(changes);
        Plan { ops }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every inverse must address the same resource as the op it undoes,
    /// otherwise `Plan::diff` cannot pair them.
    #[test]
    fn inverse_keeps_the_key() {
        let t = crate::parser::parse(
            r#"lab "k"
profile router { forward ipv4 }
node r : router {
  vrf red table 10 { interfaces [eth0] route default via 10.0.0.2 }
  run ["sleep", "1"] background
}
node h { route default via 10.0.0.1 }
link r:eth0 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms  rate 1mbit }
network lan { subnet 10.9.0.0/24  members [r:eth1, h:eth1] }
"#,
        )
        .unwrap();
        let p = crate::deploy::plan_for(&t).unwrap();
        let mut checked = 0;
        for op in &p.ops {
            if let Some(inv) = op.inverse() {
                assert!(inv.is_removal(), "{inv:?} must be a removal op");
                if !matches!(op, Op::Exec { .. }) {
                    assert_eq!(op.key(), inv.key(), "inverse of {op:?} changes the key");
                }
                checked += 1;
            }
        }
        assert!(
            checked >= 8,
            "only {checked} inverses exercised: {:?}",
            p.ops
        );
    }
}
