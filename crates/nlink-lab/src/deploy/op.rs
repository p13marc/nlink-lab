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
        Connection::<P>::new_in(self.spec())
    }

    /// Async-initialised connection (GENL families such as WireGuard).
    pub async fn connection_async<
        P: nlink::netlink::AsyncProtocolInit + nlink::netlink::construction::AsyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        Connection::<P>::new_in_async(self.spec()).await
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

    /// The namespace as an open nlink [`Netns`](nlink::netlink::namespace::Netns):
    /// run code in it ([`Netns::run`](nlink::netlink::namespace::Netns::run))
    /// or open a socket there, without a process in the node.
    pub fn netns(&self) -> std::result::Result<nlink::netlink::namespace::Netns, nlink::Error> {
        use nlink::netlink::namespace::Netns;
        match self {
            NsRef::Root => Netns::current(),
            NsRef::Named { name } => Netns::open(name),
            NsRef::Container { pid, .. } => Netns::from_pid(*pid),
        }
    }

    /// Path of the namespace file (`/proc/<pid>/ns/net` for containers).
    pub fn ns_path(&self) -> std::path::PathBuf {
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

    /// [`spawn_detached`](Self::spawn_detached) with a reaper that records
    /// the exit status in `<rc_prefix><pid>.rc` (see
    /// [`crate::ns_exec::spawn_detached_reaped`]).
    pub fn spawn_detached_reaped(
        &self,
        cmd: std::process::Command,
        rc_prefix: &std::path::Path,
    ) -> std::result::Result<u32, nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => crate::ns_exec::spawn_detached_reaped(name, cmd, rc_prefix),
            _ => crate::ns_exec::spawn_detached_path_reaped(&self.ns_path(), cmd, rc_prefix),
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
use crate::types::{EndpointRef, ExecConfig, IpvlanConfig, MacvlanConfig, WifiConfig};

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
    /// Declarative per-node stack: links, addresses, routes and traffic
    /// control (netem, `qdisc` blocks, rate limits, per-pair matrices,
    /// partitions), nftables, WireGuard.
    Stack,
    /// /etc/hosts and per-namespace /etc overlays.
    Dns,
    /// FRR daemons (zebra + ospfd/bgpd) — after DNS overlays, before user
    /// processes so services see converged routes (#65).
    RoutingDaemons,
    /// Background processes and healthchecks.
    Processes,
    /// hostapd / wpa_supplicant / mesh join.
    Wifi,
}

impl Stage {
    /// The stage's name in progress reports: `namespaces`, `mgmt-bridge`, …
    pub fn name(self) -> &'static str {
        match self {
            Stage::Namespaces => "namespaces",
            Stage::Hwsim => "hwsim",
            Stage::MgmtBridge => "mgmt-bridge",
            Stage::Networks => "networks",
            Stage::Links => "links",
            Stage::HostLinks => "host-links",
            Stage::LinksUp => "links-up",
            Stage::Sysctls => "sysctls",
            Stage::Stack => "stack",
            Stage::Dns => "dns",
            Stage::RoutingDaemons => "routing-daemons",
            Stage::Processes => "processes",
            Stage::Wifi => "wifi",
        }
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
#[derive(Debug, Clone, PartialEq)]
pub struct StackConfig {
    pub network: nlink::netlink::config::NetworkConfig,
    pub firewall: Option<crate::types::FirewallConfig>,
    pub nat: Option<crate::types::NatConfig>,
    #[cfg(feature = "wireguard")]
    pub wireguard: Option<nlink::netlink::genl::wireguard::WireguardConfig>,
}

impl StackConfig {
    /// The WireGuard interfaces the config declares.
    pub fn wireguard_ifnames(&self) -> Vec<String> {
        #[cfg(feature = "wireguard")]
        if let Some(wg) = &self.wireguard {
            return wg.devices().iter().map(|d| d.ifname.clone()).collect();
        }
        Vec::new()
    }
}

/// One unit of work in a deploy or apply plan.
///
/// Ops are pure data: building them touches nothing. `apply.rs` is
/// the only place that turns them into kernel/host mutations, and
/// every mutation records its inverse (`rollback::Undo`).
#[derive(Debug, Clone, PartialEq)]
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
        opts: Box<CreateOpts>,
    },
    /// A `mac80211_hwsim` radio for Wi-Fi interface `iface` of `node`
    /// (#176). Made over netlink in `maker`'s namespace — the same for
    /// every radio of the lab, so they share its netgroup and hear one
    /// another, and no other lab's — then moved to `node`, where its
    /// netdev is created as `iface`.
    CreateRadio {
        node: String,
        iface: String,
        /// The radio's (and its wiphy's) name, unique on the host.
        radio: String,
        mode: crate::types::WifiMode,
        maker: String,
    },
    CreateMgmtBridge {
        name: String,
        ip: std::net::IpAddr,
        prefix: u8,
    },
    CreateMgmtVeth {
        node: String,
        peer: String,
        bridge: String,
        node_ip: std::net::IpAddr,
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
        /// The network's MTU, on both ends (#161).
        mtu: Option<u32>,
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
        retries: Option<u32>,
    },
    WifiDaemon {
        node: String,
        wifi: WifiConfig,
    },
    /// Start FRR daemons for a router node (#65); the spec (configs)
    /// rides in the op so a config change restarts them.
    FrrDaemons {
        node: String,
        spec: Box<crate::frr::FrrNodeSpec>,
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
    /// A bridge network's bridge in the mgmt namespace (#85).
    DeleteBridge {
        ns: String,
        name: String,
    },
    KillNodeProcesses {
        node: String,
    },
    /// Stop the process a background `exec` block started (#84).
    KillExec {
        node: String,
        index: usize,
    },
    /// Stop a node's hostapd / wpa_supplicant (or leave a mesh) (#85).
    KillWifiDaemon {
        node: String,
        name: String,
        mode: crate::types::WifiMode,
    },
    /// Stop a node's FRR daemons and remove their runtime directories.
    KillFrrDaemons {
        node: String,
        pathspace: String,
        daemons: Vec<crate::frr::Daemon>,
    },
    RemoveDns {
        lab: String,
    },
    DeleteRadio {
        node: String,
        iface: String,
        radio: String,
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
            CreateRadio { .. } | DeleteRadio { .. } => Stage::Hwsim,
            CreateMgmtBridge { .. } | CreateMgmtVeth { .. } | DeleteHostLink { .. } => {
                Stage::MgmtBridge
            }
            CreateMgmtNamespace { .. }
            | CreateBridge { .. }
            | DeleteBridge { .. }
            | CreateNetworkVeth { .. } => Stage::Networks,
            CreateVeth { .. } | DeleteLink { .. } => Stage::Links,
            CreateMacvlan { .. } | CreateIpvlan { .. } => Stage::HostLinks,
            LinksUp { .. } => Stage::LinksUp,
            Sysctls { .. } => Stage::Sysctls,
            Stack { .. } => Stage::Stack,
            DnsInject { .. } | DnsNetnsEtc { .. } | RemoveDns { .. } => Stage::Dns,
            StartupDelay { .. }
            | Exec { .. }
            | Healthcheck { .. }
            | KillNodeProcesses { .. }
            | KillExec { .. } => Stage::Processes,
            FrrDaemons { .. } | KillFrrDaemons { .. } => Stage::RoutingDaemons,
            WifiDaemon { .. } | KillWifiDaemon { .. } => Stage::Wifi,
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
            CreateRadio { node, iface, .. } | DeleteRadio { node, iface, .. } => {
                format!("radio:{node}:{iface}")
            }
            CreateMgmtBridge { name, .. } => format!("hostlink:{name}"),
            CreateMgmtVeth { peer, .. } => format!("hostlink:{peer}"),
            DeleteHostLink { name } => format!("hostlink:{name}"),
            CreateMgmtNamespace { ns } => format!("ns:{ns}"),
            CreateBridge { ns, name, .. } | DeleteBridge { ns, name } => {
                format!("bridge:{ns}:{name}")
            }
            CreateNetworkVeth { node, iface, .. } | DeleteLink { node, iface } => {
                format!("link:{node}:{iface}")
            }
            CreateVeth { a, .. } => format!("link:{}:{}", a.node, a.iface),
            CreateMacvlan { node, cfg } => format!("link:{node}:{}", cfg.name),
            CreateIpvlan { node, cfg } => format!("link:{node}:{}", cfg.name),
            LinksUp { node, .. } => format!("linksup:{node}"),
            Sysctls { node, .. } => format!("sysctls:{node}"),
            Stack { node, .. } => format!("stack:{node}"),
            DnsInject { lab } | RemoveDns { lab } => format!("dns:{lab}"),
            DnsNetnsEtc { ns, .. } => format!("dns-etc:{ns}"),
            StartupDelay { node, .. } => format!("delay:{node}"),
            Exec { node, index, .. } | KillExec { node, index } => format!("exec:{node}:{index}"),
            Healthcheck { node, .. } => format!("healthcheck:{node}"),
            FrrDaemons { node, .. } | KillFrrDaemons { node, .. } => format!("frr:{node}"),
            WifiDaemon { node, wifi } => format!("wifi:{node}:{}", wifi.name),
            KillWifiDaemon { node, name, .. } => format!("wifi:{node}:{name}"),
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
            CreateBridge { ns, name, .. } => DeleteBridge {
                ns: ns.clone(),
                name: name.clone(),
            },
            FrrDaemons { node, spec } => KillFrrDaemons {
                node: node.clone(),
                pathspace: spec.pathspace.clone(),
                daemons: spec.daemons.clone(),
            },
            CreateRadio {
                node, iface, radio, ..
            } => DeleteRadio {
                node: node.clone(),
                iface: iface.clone(),
                radio: radio.clone(),
            },
            WifiDaemon { node, wifi } => KillWifiDaemon {
                node: node.clone(),
                name: wifi.name.clone(),
                mode: wifi.mode.clone(),
            },
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
            DnsInject { lab } => RemoveDns { lab: lab.clone() },
            Exec {
                node, index, exec, ..
            } if exec.background => KillExec {
                node: node.clone(),
                index: *index,
            },
            _ => return None,
        })
    }

    /// Whether the op creates something inside `node`'s network namespace
    /// (or a veth with an end there), or runs a process in it: what a new
    /// namespace for the node needs again.
    pub fn lives_in_node(&self, node: &str) -> bool {
        use Op::*;
        match self {
            CreateVeth { a, b, .. } => a.node == node || b.node == node,
            CreateNetworkVeth { node: n, .. }
            | CreateMgmtVeth { node: n, .. }
            | CreateMacvlan { node: n, .. }
            | CreateIpvlan { node: n, .. }
            | Sysctls { node: n, .. }
            | Exec { node: n, .. }
            | Healthcheck { node: n, .. }
            | WifiDaemon { node: n, .. } => n == node,
            _ => false,
        }
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
                | DeleteBridge { .. }
                | KillNodeProcesses { .. }
                | KillExec { .. }
                | KillWifiDaemon { .. }
                | KillFrrDaemons { .. }
                | RemoveDns { .. }
                | DeleteRadio { .. }
        )
    }

    /// One-line human description (for `deploy --dry-run` and logs).
    pub fn describe(&self) -> String {
        use Op::*;
        match self {
            CreateNamespace { node, ns } => format!("create namespace {ns} for {node}"),
            CreateContainer { node, image, .. } => format!("create container for {node} ({image})"),
            CreateRadio {
                node,
                iface,
                radio,
                mode,
                ..
            } => format!("radio {radio} → {node}:{iface} ({mode:?})"),
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
                format!("{node}: reconcile links/addresses/routes/tc, nftables, wireguard")
            }
            DnsInject { lab } => format!("inject /etc/hosts entries for {lab}"),
            DnsNetnsEtc { ns, .. } => format!("/etc/netns/{ns} overlay"),
            StartupDelay { node, delay } => format!("{node}: wait {delay}"),
            Exec { node, index, exec } => format!(
                "{node}: exec[{index}] {}{}",
                exec.cmd.join(" "),
                if exec.background { " (background)" } else { "" }
            ),
            Healthcheck { node, cmd, .. } => format!("{node}: healthcheck `{cmd}`"),
            FrrDaemons { node, spec } => format!(
                "{node}: frr {} (pathspace {})",
                spec.daemons
                    .iter()
                    .map(|d| d.name())
                    .collect::<Vec<_>>()
                    .join("+"),
                spec.pathspace
            ),
            KillFrrDaemons { node, .. } => format!("{node}: stop frr daemons"),
            WifiDaemon { node, wifi } => format!("{node}: wifi {} ({:?})", wifi.name, wifi.mode),
            DeleteNamespace { ns, .. } => format!("delete namespace {ns}"),
            RemoveContainer { node, .. } => format!("remove container of {node}"),
            DeleteLink { node, iface } => format!("delete link {node}:{iface}"),
            DeleteHostLink { name } => format!("delete host link {name}"),
            DeleteBridge { ns, name } => format!("delete bridge {name} in {ns}"),
            KillNodeProcesses { node } => format!("stop background processes of {node}"),
            KillExec { node, index } => format!("{node}: stop exec[{index}]"),
            KillWifiDaemon { node, name, mode } => format!("{node}: stop wifi {name} ({mode:?})"),
            RemoveDns { lab } => format!("remove /etc/hosts entries for {lab}"),
            DeleteRadio { node, radio, .. } => format!("delete radio {radio} of {node}"),
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

    /// The ops that do more than a diff always re-runs (each node's
    /// `Stack`, links up, the DNS overlays, which change nothing when
    /// nothing changed) and more than remove: creating or re-creating
    /// something (a container whose options changed), starting a process.
    /// A plan whose `work` and removals are empty has nothing to do.
    pub fn work(&self) -> impl Iterator<Item = &Op> {
        self.ops.iter().filter(|o| {
            !o.is_removal()
                && !matches!(
                    o,
                    Op::Stack { .. }
                        | Op::LinksUp { .. }
                        | Op::DnsInject { .. }
                        | Op::DnsNetnsEtc { .. }
                )
        })
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
        // A WireGuard interface is created by the node's Stack, and neither
        // its network layer (which purges only what it declares) nor its
        // WireGuard layer (which has no device removal) deletes one the
        // topology drops: it stayed up with its key and peers (#204).
        for (key, op) in &cur {
            if let (Op::Stack { node, cfg }, Some(Op::Stack { cfg: wanted, .. })) =
                (op, des.get(key))
            {
                let keep = wanted.wireguard_ifnames();
                for iface in cfg
                    .wireguard_ifnames()
                    .into_iter()
                    .filter(|i| !keep.contains(i))
                {
                    removals.push(Op::DeleteLink {
                        node: node.clone(),
                        iface,
                    });
                }
            }
        }
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
        // …nor a bridge whose namespace is going away.
        let dying_ns: std::collections::BTreeSet<String> = removals
            .iter()
            .filter_map(|o| match o {
                Op::DeleteNamespace { ns, .. } => Some(ns.clone()),
                _ => None,
            })
            .collect();
        // A namespace deletion kills nothing: a removed node's processes
        // (its `run` commands, wifi daemons, what `spawn` started) kept
        // running and kept the namespace alive (#204). Stop them all, and
        // drop their bookkeeping, before the node goes.
        for node in &dying {
            removals.push(Op::KillNodeProcesses { node: node.clone() });
        }
        removals.retain(|o| match o {
            Op::DeleteBridge { ns, .. } => !dying_ns.contains(ns.as_str()),
            // `KillNodeProcesses` (above) stops these for a dying node.
            Op::DeleteLink { node, .. }
            | Op::KillExec { node, .. }
            | Op::KillWifiDaemon { node, .. } => !dying.contains(node.as_str()),
            _ => true,
        });
        removals.sort_by_key(|o| std::cmp::Reverse(o.stage()));
        // dedupe (several background execs on one node → one kill op)
        let mut seen = std::collections::BTreeSet::new();
        removals.retain(|o| seen.insert(o.key() + &format!("{:?}", o.stage())));

        let mut changes: Vec<Op> = Vec::new();
        // nodes whose background exec was restarted → healthcheck again
        let mut restarted: std::collections::BTreeSet<String> = Default::default();
        // container nodes re-created: their namespace is new, see below
        let mut recreated: std::collections::BTreeSet<String> = Default::default();
        // (node, iface) of re-created radios: their netdev is new
        let mut radios: std::collections::BTreeSet<(String, String)> = Default::default();
        for op in &desired.ops {
            match cur.get(&op.key()) {
                None => changes.push(op.clone()),
                Some(existing) => match op {
                    // reconcile-style ops: always re-run (idempotent)
                    Op::Stack { .. }
                    | Op::LinksUp { .. }
                    | Op::DnsInject { .. }
                    | Op::DnsNetnsEtc { .. } => changes.push(op.clone()),
                    // an edited exec is stopped (background) and re-run;
                    // an unchanged one is left alone (#84)
                    Op::Exec { node, .. } => {
                        if *existing != op {
                            if let Some(inv) = existing.inverse() {
                                removals.push(inv);
                            }
                            changes.push(op.clone());
                            restarted.insert(node.clone());
                        }
                    }
                    Op::Healthcheck { node, .. } => {
                        if restarted.contains(node) || *existing != op {
                            changes.push(op.clone());
                        }
                    }
                    // one-shot: a pre-start delay only makes sense for a new node
                    Op::StartupDelay { .. } => {}
                    // TODO(#85): a changed bridge (mtu / vlan filtering) would
                    // need its member veths re-attached after the re-create;
                    // until then bridge edits are not applied.
                    Op::CreateBridge { .. } => {
                        if *existing != op {
                            tracing::warn!(
                                "apply: bridge {} changed but bridge edits are not applied yet",
                                op.key()
                            );
                        }
                    }
                    // A container's `extra_hosts` lists every address of a
                    // `dns hosts` lab: adding a node elsewhere changed it,
                    // and re-created every container (#204). It is not a
                    // reason to; the new names reach the container when it
                    // is next re-created.
                    Op::CreateContainer { node, .. } => {
                        if !same_container(existing, op) {
                            if let Some(inv) = existing.inverse() {
                                removals.push(inv);
                            }
                            changes.push(op.clone());
                            recreated.insert(node.clone());
                        }
                    }
                    // A radio is re-created when its `maker` changes (a Wi-Fi
                    // node added or removed first in name order moves every
                    // radio to a new netgroup); its daemon is restarted
                    // below.
                    Op::CreateRadio { node, iface, .. } => {
                        if *existing != op {
                            if let Some(inv) = existing.inverse() {
                                removals.push(inv);
                            }
                            changes.push(op.clone());
                            radios.insert((node.clone(), iface.clone()));
                        }
                    }
                    // everything else: re-create when the payload changed
                    _ => {
                        if *existing != op {
                            if let Some(inv) = existing.inverse() {
                                removals.push(inv);
                            }
                            changes.push(op.clone());
                        }
                    }
                },
            }
        }
        // A re-created container has a new namespace: every veth, port and
        // sysctl that lived in the old one died with it (both ends of a
        // veth), and its processes too. Unchanged, the diff would leave
        // them out, and the first `LinksUp` failed on a missing interface.
        for op in &desired.ops {
            if recreated.iter().any(|n| op.lives_in_node(n)) && !changes.contains(op) {
                changes.push(op.clone());
            }
        }
        // The hostapd / wpa_supplicant of a re-created radio was bound to
        // the netdev that went with the old one: unchanged, it was left
        // running on nothing (#204).
        for op in &desired.ops {
            if let Op::WifiDaemon { node, wifi } = op
                && radios.contains(&(node.clone(), wifi.name.clone()))
                && !changes.contains(op)
            {
                if let Some(inv) = cur.get(&op.key()).and_then(|o| o.inverse()) {
                    removals.push(inv);
                }
                changes.push(op.clone());
            }
        }
        // inverses added above go in stage order too: a daemon is stopped
        // before its radio is deleted
        removals.sort_by_key(|o| std::cmp::Reverse(o.stage()));
        changes.sort_by_key(|o| o.stage());
        let mut ops = removals;
        ops.extend(changes);
        Plan { ops }
    }
}

/// Two `CreateContainer` ops that differ at most in their `extra_hosts`.
fn same_container(a: &Op, b: &Op) -> bool {
    let without_hosts = |op: &Op| match op {
        Op::CreateContainer {
            node,
            name,
            image,
            pull,
            opts,
        } => {
            let mut opts = opts.clone();
            opts.extra_hosts.clear();
            Some((
                node.clone(),
                name.clone(),
                image.clone(),
                pull.clone(),
                opts,
            ))
        }
        _ => None,
    };
    without_hosts(a) == without_hosts(b)
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

    fn plan_of(nll: &str) -> Plan {
        let t = crate::parser::parse(nll).unwrap();
        crate::deploy::plan_for(&t).unwrap()
    }

    /// Traffic control is declared in the node's stack (#108): a changed
    /// impairment changes the `Stack` op of both endpoints' nodes, and
    /// nothing else; nlink then replaces the netem in place.
    #[test]
    fn a_changed_impairment_is_a_stack_change() {
        let lab = |delay: &str| {
            plan_of(&format!(
                r#"lab "q"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24  delay {delay} }}
"#
            ))
        };
        let stack = |p: &Plan, n: &str| {
            p.ops
                .iter()
                .find_map(|o| match o {
                    Op::Stack { node, cfg } if node == n => Some(cfg.clone()),
                    _ => None,
                })
                .expect("stack op")
        };
        let (cur, des) = (lab("5ms"), lab("40ms"));
        for n in ["a", "b"] {
            let (c, d) = (stack(&cur, n), stack(&des, n));
            assert_eq!(c.network.qdiscs().len(), 1, "{n}: one declared netem");
            assert_ne!(c, d, "{n}: the delay is in the stack");
        }
        let diff = Plan::diff(&cur, &des);
        assert!(
            diff.ops
                .iter()
                .all(|o| matches!(o, Op::Stack { .. } | Op::LinksUp { .. })),
            "{:?}",
            diff.ops
        );
    }

    fn container_lab(cmd: &str, extra: &str) -> Plan {
        plan_of(&format!(
            r#"lab "c" {{ dns hosts }}
profile router {{ forward ipv4 }}
node r : router image "alpine:3" cmd ["sleep", "{cmd}"]
node h
{extra}
link r:eth0 -- h:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        ))
    }

    /// Adding a node to a `dns hosts` lab changes every container's
    /// `extra_hosts`; that alone re-creates none (#204).
    #[test]
    fn a_new_hosts_entry_does_not_recreate_a_container() {
        let cur = container_lab("1000", "");
        let des = container_lab(
            "1000",
            "node x\nlink x:eth0 -- h:eth1 { 10.0.1.1/24 -- 10.0.1.2/24 }",
        );
        let diff = Plan::diff(&cur, &des);
        assert!(
            !diff
                .ops
                .iter()
                .any(|o| matches!(o, Op::CreateContainer { .. } | Op::RemoveContainer { .. })),
            "{:?}",
            diff.ops
        );
    }

    /// A container whose own options change is re-created, and what lived
    /// in its namespace (the veth, the forwarding sysctl) comes back with
    /// it: the diff used to leave them out as unchanged (#204).
    #[test]
    fn a_recreated_container_gets_its_links_and_sysctls_back() {
        let diff = Plan::diff(&container_lab("1000", ""), &container_lab("2000", ""));
        let pos = |f: &dyn Fn(&Op) -> bool| diff.ops.iter().position(f);
        let removed = pos(&|o| matches!(o, Op::RemoveContainer { node, .. } if node == "r"));
        let created = pos(&|o| matches!(o, Op::CreateContainer { node, .. } if node == "r"));
        let veth = pos(&|o| matches!(o, Op::CreateVeth { .. }));
        let sysctls = pos(&|o| matches!(o, Op::Sysctls { node, .. } if node == "r"));
        assert!(removed < created, "{:?}", diff.ops);
        assert!(created < veth && veth.is_some(), "{:?}", diff.ops);
        assert!(sysctls.is_some(), "{:?}", diff.ops);
    }

    /// A removed node's processes are stopped before its namespace goes:
    /// deleting the namespace killed nothing, and they kept it alive
    /// (#204).
    #[test]
    fn a_removed_node_has_its_processes_stopped_first() {
        let lab = |extra: &str| {
            plan_of(&format!(
                r#"lab "k"
node a
{extra}
"#
            ))
        };
        let cur = lab("node b { run background [\"sleep\", \"100\"] }");
        let diff = Plan::diff(&cur, &lab(""));
        let kill = diff
            .ops
            .iter()
            .position(|o| matches!(o, Op::KillNodeProcesses { node } if node == "b"));
        let delete = diff
            .ops
            .iter()
            .position(|o| matches!(o, Op::DeleteNamespace { node, .. } if node == "b"));
        assert!(kill.is_some() && kill < delete, "{:?}", diff.ops);
        assert!(
            !diff.ops.iter().any(|o| matches!(o, Op::KillExec { .. })),
            "the node-wide kill covers its execs: {:?}",
            diff.ops
        );
    }

    /// A WireGuard interface the topology drops is deleted: the node's
    /// Stack creates one but never removes it (#204).
    #[cfg(feature = "wireguard")]
    #[test]
    fn a_dropped_wireguard_interface_is_deleted() {
        let lab = |wg: bool| {
            let tunnel = |addr: &str, peer: &str| {
                if wg {
                    format!(
                        "{{ wireguard wg0 {{ key auto listen 51820 address {addr} peers [{peer}] }} }}"
                    )
                } else {
                    String::new()
                }
            };
            plan_of(&format!(
                "lab \"w\"\nnode a {}\nnode b {}\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/30 -- 10.0.0.2/30 }}\n",
                tunnel("192.168.255.1/32", "b"),
                tunnel("192.168.255.2/32", "a"),
            ))
        };
        let diff = Plan::diff(&lab(true), &lab(false));
        for n in ["a", "b"] {
            assert!(
                diff.ops.iter().any(
                    |o| matches!(o, Op::DeleteLink { node, iface } if node == n && iface == "wg0")
                ),
                "{n}: {:?}",
                diff.ops
            );
        }
        assert!(
            !Plan::diff(&lab(true), &lab(true))
                .ops
                .iter()
                .any(|o| o.is_removal()),
            "an unchanged tunnel stays"
        );
    }

    /// Radios are made in the first Wi-Fi node's namespace: a Wi-Fi node
    /// added before it re-creates every radio, and the daemons bound to
    /// the old netdevs are restarted on the new ones, each stopped before
    /// its radio is deleted (#204).
    #[test]
    fn a_recreated_radio_restarts_its_daemon() {
        let lab = |extra: &str| {
            plan_of(&format!(
                r#"lab "r"
{extra}
node m {{ wifi wlan0 mode ap {{ ssid "x" channel 6 10.0.0.1/24 }} }}
node s {{ wifi wlan0 mode station {{ ssid "x" 10.0.0.2/24 }} }}
"#
            ))
        };
        let diff = Plan::diff(
            &lab(""),
            &lab(r#"node a { wifi wlan0 mode station { ssid "x" 10.0.0.3/24 } }"#),
        );
        let pos = |f: &dyn Fn(&Op) -> bool| diff.ops.iter().position(f);
        for n in ["m", "s"] {
            let kill = pos(&|o| matches!(o, Op::KillWifiDaemon { node, .. } if node == n));
            let delete = pos(&|o| matches!(o, Op::DeleteRadio { node, .. } if node == n));
            let create = pos(&|o| matches!(o, Op::CreateRadio { node, .. } if node == n));
            let start = pos(&|o| matches!(o, Op::WifiDaemon { node, .. } if node == n));
            assert!(
                kill.is_some() && kill < delete && delete < create && create < start,
                "{n}: {:?}",
                diff.ops
            );
        }
    }
}
