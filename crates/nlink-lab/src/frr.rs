//! FRR routing daemons for router nodes (issue #65).
//!
//! `routing frr { ospf }` (lab-wide default) and per-node `frr { ospf …
//! bgp … }` blocks run `zebra` plus `ospfd`/`bgpd` inside the node's
//! namespace. Everything here is pure (config text, paths, topology
//! introspection); `deploy/apply.rs` starts and stops the processes.
//!
//! Run model (verified against FRR 10.3 on Debian 13):
//!
//! - each daemon gets its own config file via `-f` (no `vtysh`, no
//!   `mgmtd` needed), a pidfile via `-i`, a log file via `--log file:`;
//! - `-N <pathspace>` keeps a lab's daemons apart: their sockets live in
//!   `/var/run/frr/<pathspace>/` (`zserv.api`, `*.vty`);
//! - daemons drop to the packaged `frr` user themselves (running them as
//!   root fails the `frrvty` group check), so every file and directory
//!   they touch is owned by `frr`: configs, pidfiles and logs under
//!   [`node_dir`] (`/run/nlink-lab/frr/<lab>/<node>/`), the socket dir,
//!   and `/var/lib/frr/<pathspace>/` for the daemon state save;
//! - non-router nodes keep the `auto` static default routes, so
//!   `routing frr { ospf }` is a drop-in for `routing auto` on
//!   router-only topologies.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::{BgpConfig, BgpNeighbor, FrrConfig, Node, OspfConfig, Topology};

/// Where Debian/Fedora/local builds put the daemons (they are not on
/// `$PATH`; only `vtysh` is).
pub const DAEMON_DIRS: &[&str] = &["/usr/lib/frr", "/usr/libexec/frr", "/usr/local/sbin"];

/// FRR's runtime root: `-N <ps>` puts the sockets in `RUN_ROOT/<ps>/`.
pub const RUN_ROOT: &str = "/var/run/frr";

/// Daemon state save root (`ospfd.json`, …), also pathspace-suffixed.
pub const LIB_ROOT: &str = "/var/lib/frr";

/// The user the daemons drop to (compiled into the Debian package).
pub const FRR_USER: &str = "frr";

/// A daemon we run. Order = start order (zebra first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Daemon {
    Zebra,
    Ospfd,
    Bgpd,
}

impl Daemon {
    pub fn name(self) -> &'static str {
        match self {
            Daemon::Zebra => "zebra",
            Daemon::Ospfd => "ospfd",
            Daemon::Bgpd => "bgpd",
        }
    }
}

/// One node's fully resolved FRR material; lives inside `Op::FrrDaemons`,
/// so a changed config changes the op and `apply` restarts the daemons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrrNodeSpec {
    /// `-N` value, unique per lab + node.
    pub pathspace: String,
    /// zebra + whichever protocol daemons the config needs.
    pub daemons: Vec<Daemon>,
    /// Per-daemon config text (what `-f` reads).
    pub confs: BTreeMap<Daemon, String>,
    /// Integrated `frr.conf` for humans / `vtysh -N`.
    pub integrated: String,
    /// OSPF neighbours this node is expected to bring to `Full`.
    pub expected_ospf_neighbors: usize,
    /// BGP peers this node is expected to bring to `Established`.
    pub expected_bgp_peers: usize,
}

/// `-N` pathspace for a namespace: `nl-<ns>` with `.` replaced (FRR
/// rejects dots and slashes), capped so the longest socket path
/// (`/var/run/frr/<ps>/mgmtd_be.sock`) stays under `sun_path`'s 108 bytes.
pub fn pathspace(namespace_name: &str) -> String {
    let cleaned: String = namespace_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let full = format!("nl-{cleaned}");
    if full.len() <= 60 {
        return full;
    }
    // djb2 over the full name keeps distinct long names distinct.
    let mut h: u32 = 5381;
    for b in namespace_name.bytes() {
        h = h.wrapping_mul(33) ^ u32::from(b);
    }
    let head: String = cleaned.chars().take(40).collect();
    format!("nl-{h:08x}-{head}")
}

/// `/var/run/frr/<pathspace>` — the daemons' socket directory.
pub fn run_dir(pathspace: &str) -> PathBuf {
    Path::new(RUN_ROOT).join(pathspace)
}

/// `/var/lib/frr/<pathspace>` — the daemons' state-save directory.
pub fn lib_dir(pathspace: &str) -> PathBuf {
    Path::new(LIB_ROOT).join(pathspace)
}

/// `/run/nlink-lab/frr/<lab>` — configs, pidfiles and logs of every node.
pub fn lab_dir(lab: &str) -> PathBuf {
    Path::new("/run/nlink-lab/frr").join(lab)
}

/// `/run/nlink-lab/frr/<lab>/<node>`.
pub fn node_dir(lab: &str, node: &str) -> PathBuf {
    lab_dir(lab).join(node)
}

pub fn pidfile(lab: &str, node: &str, daemon: Daemon) -> PathBuf {
    node_dir(lab, node).join(format!("{}.pid", daemon.name()))
}

pub fn conf_path(lab: &str, node: &str, daemon: Daemon) -> PathBuf {
    node_dir(lab, node).join(format!("{}.conf", daemon.name()))
}

pub fn log_path(lab: &str, node: &str, daemon: Daemon) -> PathBuf {
    node_dir(lab, node).join(format!("{}.log", daemon.name()))
}

/// Find a daemon binary: `$PATH`, then [`DAEMON_DIRS`].
pub fn locate_daemon(daemon: Daemon) -> Option<PathBuf> {
    locate(daemon.name())
}

/// `vtysh` (used for the convergence wait; optional).
pub fn locate_vtysh() -> Option<PathBuf> {
    locate("vtysh")
}

fn locate(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(DAEMON_DIRS.iter().map(PathBuf::from))
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// uid/gid of the `frr` user (the daemons refuse to run as root).
pub fn frr_ids() -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut f = line.split(':');
        let name = f.next()?;
        if name != FRR_USER {
            return None;
        }
        f.next()?; // password
        let uid = f.next()?.parse().ok()?;
        let gid = f.next()?.parse().ok()?;
        Some((uid, gid))
    })
}

/// Every binary the specs need, or one clear error naming what is
/// missing and where it was looked for.
pub fn require_daemons(needed: &BTreeSet<Daemon>) -> Result<BTreeMap<Daemon, PathBuf>> {
    let mut found = BTreeMap::new();
    let mut missing = Vec::new();
    for d in needed {
        match locate_daemon(*d) {
            Some(p) => {
                found.insert(*d, p);
            }
            None => missing.push(d.name()),
        }
    }
    if !missing.is_empty() {
        return Err(Error::deploy_failed(format!(
            "routing frr: daemon(s) not found: {} (looked in $PATH and {}) — install the `frr` package",
            missing.join(", "),
            DAEMON_DIRS.join(", ")
        )));
    }
    if frr_ids().is_none() {
        return Err(Error::deploy_failed(format!(
            "routing frr: user '{FRR_USER}' does not exist; the daemons run as it (install the `frr` package)"
        )));
    }
    Ok(found)
}

/// One L2 segment a node is attached to, from `node`'s point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceAddrs {
    pub name: String,
    /// IPv4 addresses (CIDR) on the interface.
    pub addrs: Vec<String>,
    /// Other nodes on the same segment.
    pub peers: Vec<String>,
    /// Point-to-point `link` (two nodes) rather than a bridge.
    pub p2p: bool,
}

/// The node's interfaces with their IPv4 addresses and segment peers,
/// from `link`s and `network` ports.
pub fn node_interfaces(topology: &Topology, node_name: &str) -> Vec<IfaceAddrs> {
    use crate::types::EndpointRef;
    let mut out: Vec<IfaceAddrs> = Vec::new();
    for link in &topology.links {
        let Some(addrs) = &link.addresses else {
            continue;
        };
        let eps: Vec<Option<EndpointRef>> = link
            .endpoints
            .iter()
            .map(|e| EndpointRef::parse(e))
            .collect();
        let (Some(a), Some(b)) = (&eps[0], &eps[1]) else {
            continue;
        };
        for (me, other, addr) in [(a, b, &addrs[0]), (b, a, &addrs[1])] {
            if me.node == node_name && is_v4(addr) {
                out.push(IfaceAddrs {
                    name: me.iface.clone(),
                    addrs: vec![addr.clone()],
                    peers: vec![other.node.clone()],
                    p2p: true,
                });
            }
        }
    }
    for network in topology.networks.values() {
        let members: Vec<EndpointRef> = network
            .ports
            .keys()
            .filter_map(|k| EndpointRef::parse(k))
            .collect();
        for (ep_str, port) in &network.ports {
            let Some(ep) = EndpointRef::parse(ep_str) else {
                continue;
            };
            if ep.node != node_name {
                continue;
            }
            let addrs: Vec<String> = port
                .addresses
                .iter()
                .filter(|a| is_v4(a))
                .cloned()
                .collect();
            if addrs.is_empty() {
                continue;
            }
            let peers: Vec<String> = members
                .iter()
                .filter(|m| m.node != node_name)
                .map(|m| m.node.clone())
                .collect();
            out.push(IfaceAddrs {
                name: ep.iface.clone(),
                addrs,
                peers,
                p2p: false,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn is_v4(cidr: &str) -> bool {
    cidr.split('/')
        .next()
        .and_then(|ip| ip.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_ipv4())
}

/// The nodes that run FRR under this topology.
pub fn frr_nodes(topology: &Topology) -> BTreeSet<String> {
    topology
        .nodes
        .iter()
        .filter(|(_, n)| topology.effective_frr(n).is_some())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Resolve `neighbor NODE [as N] [remote IP]` into (address, remote-as).
pub fn resolve_bgp_neighbor(
    topology: &Topology,
    node_name: &str,
    nb: &BgpNeighbor,
) -> Result<(IpAddr, u32)> {
    let peer = topology.nodes.get(&nb.node).ok_or_else(|| {
        Error::invalid_topology(format!(
            "bgp neighbor '{}' of '{node_name}' is not a node",
            nb.node
        ))
    })?;
    if nb.node == node_name {
        return Err(Error::invalid_topology(format!(
            "bgp neighbor of '{node_name}' cannot be the node itself"
        )));
    }
    let remote_as = match nb.remote_as {
        Some(asn) => asn,
        None => topology
            .effective_frr(peer)
            .and_then(|f| f.bgp.map(|b| b.asn))
            .ok_or_else(|| {
                Error::invalid_topology(format!(
                    "bgp neighbor '{}' of '{node_name}' has no `bgp {{ as … }}`; give `as N` explicitly",
                    nb.node
                ))
            })?,
    };
    let addr: IpAddr = match &nb.remote {
        Some(ip) => ip.parse().map_err(|e| {
            Error::invalid_topology(format!(
                "bgp neighbor '{}' of '{node_name}': bad remote address '{ip}': {e}",
                nb.node
            ))
        })?,
        None => {
            // The peer's address on a segment shared with this node.
            let mine = node_interfaces(topology, node_name);
            let theirs = node_interfaces(topology, &nb.node);
            let mut candidates: Vec<IpAddr> = Vec::new();
            for m in &mine {
                if !m.peers.iter().any(|p| p == &nb.node) {
                    continue;
                }
                for t in &theirs {
                    if !t.peers.iter().any(|p| p == node_name) {
                        continue;
                    }
                    if same_segment(m, t) {
                        for a in &t.addrs {
                            if let Some(ip) = a.split('/').next().and_then(|s| s.parse().ok()) {
                                candidates.push(ip);
                            }
                        }
                    }
                }
            }
            candidates.dedup();
            match candidates.as_slice() {
                [one] => *one,
                [] => {
                    return Err(Error::invalid_topology(format!(
                        "bgp neighbor '{}' of '{node_name}' shares no addressed segment; give `remote IP`",
                        nb.node
                    )));
                }
                _ => {
                    return Err(Error::invalid_topology(format!(
                        "bgp neighbor '{}' of '{node_name}' is reachable on several segments; give `remote IP`",
                        nb.node
                    )));
                }
            }
        }
    };
    Ok((addr, remote_as))
}

/// Two interface records describe the same segment when an address of
/// one lies in a prefix of the other.
fn same_segment(a: &IfaceAddrs, b: &IfaceAddrs) -> bool {
    for x in &a.addrs {
        let Ok((net, prefix)) = crate::helpers::parse_cidr(x) else {
            continue;
        };
        for y in &b.addrs {
            if let Some(ip) = y.split('/').next().and_then(|s| s.parse::<IpAddr>().ok())
                && crate::helpers::ip_in_subnet(ip, net, prefix)
            {
                return true;
            }
        }
    }
    false
}

/// Router ID: the first `lo` address, else the lowest IPv4 of the node.
fn default_router_id(node: &Node, ifaces: &[IfaceAddrs]) -> Option<String> {
    if let Some(lo) = node.interfaces.get("lo") {
        for a in &lo.addresses {
            if is_v4(a) {
                return a.split('/').next().map(str::to_string);
            }
        }
    }
    let mut ips: Vec<std::net::Ipv4Addr> = ifaces
        .iter()
        .flat_map(|i| i.addrs.iter())
        .filter_map(|a| a.split('/').next()?.parse().ok())
        .collect();
    ips.sort();
    ips.first().map(|ip| ip.to_string())
}

/// Build the daemon configs for `node`.
pub fn build_spec(
    topology: &Topology,
    node_name: &str,
    node: &Node,
    cfg: &FrrConfig,
) -> Result<FrrNodeSpec> {
    let ns = topology.namespace_name(node_name);
    let ps = pathspace(&ns);
    let ifaces = node_interfaces(topology, node_name);
    let routers = frr_nodes(topology);
    let router_id_default = default_router_id(node, &ifaces);

    let mut daemons = vec![Daemon::Zebra];
    let mut confs = BTreeMap::new();
    confs.insert(Daemon::Zebra, generate_zebra_conf(node_name));
    let mut expected_ospf_neighbors = 0;
    let mut expected_bgp_peers = 0;

    if let Some(ospf) = &cfg.ospf {
        daemons.push(Daemon::Ospfd);
        let router_id = ospf.router_id.clone().or_else(|| router_id_default.clone());
        // An interface is passive when no FRR router sits on its segment.
        let mut iface_lines = Vec::new();
        for i in &ifaces {
            let has_router_peer = i.peers.iter().any(|p| routers.contains(p));
            let passive = ospf.passive.iter().any(|p| p == &i.name) || !has_router_peer;
            if !passive {
                expected_ospf_neighbors += i.peers.iter().filter(|p| routers.contains(*p)).count();
            }
            iface_lines.push((i.name.clone(), passive, i.p2p));
        }
        confs.insert(
            Daemon::Ospfd,
            generate_ospfd_conf(node_name, &iface_lines, ospf, router_id.as_deref()),
        );
    }

    if let Some(bgp) = &cfg.bgp {
        daemons.push(Daemon::Bgpd);
        let router_id = bgp.router_id.clone().or_else(|| router_id_default.clone());
        let mut neighbors = Vec::new();
        for nb in &bgp.neighbors {
            neighbors.push(resolve_bgp_neighbor(topology, node_name, nb)?);
        }
        expected_bgp_peers = neighbors.len();
        confs.insert(
            Daemon::Bgpd,
            generate_bgpd_conf(node_name, bgp, &neighbors, router_id.as_deref()),
        );
    }

    let integrated = generate_integrated(node_name, &confs);
    Ok(FrrNodeSpec {
        pathspace: ps,
        daemons,
        confs,
        integrated,
        expected_ospf_neighbors,
        expected_bgp_peers,
    })
}

fn header(hostname: &str) -> String {
    format!("frr defaults traditional\nhostname {hostname}\n!\n")
}

pub fn generate_zebra_conf(hostname: &str) -> String {
    format!("{}line vty\n", header(hostname))
}

/// `iface_lines`: (name, passive, point-to-point).
pub fn generate_ospfd_conf(
    hostname: &str,
    iface_lines: &[(String, bool, bool)],
    ospf: &OspfConfig,
    router_id: Option<&str>,
) -> String {
    let area = ospf.area.as_deref().unwrap_or("0.0.0.0");
    let mut s = header(hostname);
    for (name, passive, p2p) in iface_lines {
        s.push_str(&format!("interface {name}\n ip ospf area {area}\n"));
        if *p2p {
            s.push_str(" ip ospf network point-to-point\n");
        }
        if *passive {
            s.push_str(" ip ospf passive\n");
        }
        if let Some(h) = ospf.hello {
            s.push_str(&format!(" ip ospf hello-interval {h}\n"));
        }
        if let Some(d) = ospf.dead {
            s.push_str(&format!(" ip ospf dead-interval {d}\n"));
        }
        s.push_str("!\n");
    }
    s.push_str("router ospf\n");
    if let Some(id) = router_id {
        s.push_str(&format!(" ospf router-id {id}\n"));
    }
    for r in &ospf.redistribute {
        s.push_str(&format!(" redistribute {r}\n"));
    }
    s.push_str("!\nline vty\n");
    s
}

pub fn generate_bgpd_conf(
    hostname: &str,
    bgp: &BgpConfig,
    neighbors: &[(IpAddr, u32)],
    router_id: Option<&str>,
) -> String {
    let mut s = header(hostname);
    s.push_str(&format!("router bgp {}\n", bgp.asn));
    if let Some(id) = router_id {
        s.push_str(&format!(" bgp router-id {id}\n"));
    }
    // FRR >= 7.4 drops eBGP routes without policy; labs want them.
    s.push_str(" no bgp ebgp-requires-policy\n");
    if !bgp.networks.is_empty() {
        s.push_str(" no bgp network import-check\n");
    }
    for (addr, asn) in neighbors {
        s.push_str(&format!(" neighbor {addr} remote-as {asn}\n"));
    }
    s.push_str(" !\n address-family ipv4 unicast\n");
    for n in &bgp.networks {
        s.push_str(&format!("  network {n}\n"));
    }
    for r in &bgp.redistribute {
        s.push_str(&format!("  redistribute {r}\n"));
    }
    for (addr, _) in neighbors {
        s.push_str(&format!("  neighbor {addr} activate\n"));
    }
    s.push_str(" exit-address-family\n!\nline vty\n");
    s
}

/// One file with every daemon's section (for humans and `vtysh -N`).
pub fn generate_integrated(hostname: &str, confs: &BTreeMap<Daemon, String>) -> String {
    let mut s = header(hostname);
    for (d, text) in confs {
        s.push_str(&format!("! ---- {}\n", d.name()));
        for line in text.lines() {
            if line.starts_with("frr defaults")
                || line.starts_with("hostname ")
                || line == "line vty"
            {
                continue;
            }
            s.push_str(line);
            s.push('\n');
        }
    }
    s.push_str("line vty\n");
    s
}

/// Directories the daemons of `lab`/`node` need, created and chowned to
/// `frr` by the applier; removed on kill/destroy.
pub fn runtime_dirs(lab: &str, node: &str, pathspace: &str) -> Vec<PathBuf> {
    vec![node_dir(lab, node), run_dir(pathspace), lib_dir(pathspace)]
}

/// Remove every FRR runtime directory of a lab (destroy / orphan reap).
pub fn cleanup(topology: &Topology) {
    for (node_name, node) in &topology.nodes {
        if topology.effective_frr(node).is_none() {
            continue;
        }
        let ps = pathspace(&topology.namespace_name(node_name));
        for d in [run_dir(&ps), lib_dir(&ps)] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
    let _ = std::fs::remove_dir_all(lab_dir(&topology.lab.name));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topo(src: &str) -> Topology {
        crate::parser::parse(src).unwrap()
    }

    const TRIANGLE: &str = r#"lab "t" { routing frr { ospf } }
profile router { forward ipv4 }
node r1 : router { lo 1.1.1.1/32 }
node r2 : router { lo 2.2.2.2/32 }
node r3 : router
node h1
link r1:eth0 -- r2:eth0 { 10.0.12.1/30 -- 10.0.12.2/30 }
link r2:eth1 -- r3:eth0 { 10.0.23.1/30 -- 10.0.23.2/30 }
link r3:eth1 -- r1:eth1 { 10.0.31.1/30 -- 10.0.31.2/30 }
link r1:eth2 -- h1:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 }
"#;

    #[test]
    fn pathspace_is_sanitised_and_bounded() {
        assert_eq!(pathspace("lab-r1"), "nl-lab-r1");
        assert_eq!(pathspace("my.lab-r.1"), "nl-my_lab-r_1");
        let long = "x".repeat(120);
        let ps = pathspace(&long);
        assert!(ps.len() <= 60, "{ps}");
        assert!(ps.starts_with("nl-"));
        assert_ne!(ps, pathspace(&"y".repeat(120)));
    }

    #[test]
    fn frr_nodes_are_forwarding_namespace_nodes_under_lab_default() {
        let t = topo(TRIANGLE);
        let nodes = frr_nodes(&t);
        assert_eq!(
            nodes.into_iter().collect::<Vec<_>>(),
            vec!["r1", "r2", "r3"]
        );
        let mut manual = t.clone();
        manual.lab.routing = crate::types::RoutingMode::Auto;
        assert!(frr_nodes(&manual).is_empty());
    }

    #[test]
    fn ospf_golden_config() {
        let t = topo(TRIANGLE);
        let cfg = t.effective_frr(&t.nodes["r1"]).unwrap();
        let spec = build_spec(&t, "r1", &t.nodes["r1"], &cfg).unwrap();
        assert_eq!(spec.daemons, vec![Daemon::Zebra, Daemon::Ospfd]);
        assert_eq!(spec.expected_ospf_neighbors, 2);
        let ospfd = &spec.confs[&Daemon::Ospfd];
        assert_eq!(
            ospfd,
            "frr defaults traditional\nhostname r1\n!\n\
interface eth0\n ip ospf area 0.0.0.0\n ip ospf network point-to-point\n!\n\
interface eth1\n ip ospf area 0.0.0.0\n ip ospf network point-to-point\n!\n\
interface eth2\n ip ospf area 0.0.0.0\n ip ospf network point-to-point\n ip ospf passive\n!\n\
router ospf\n ospf router-id 1.1.1.1\n!\nline vty\n"
        );
        // r3 has no loopback → lowest IPv4 is the router-id
        let spec3 = build_spec(&t, "r3", &t.nodes["r3"], &cfg).unwrap();
        assert!(spec3.confs[&Daemon::Ospfd].contains("ospf router-id 10.0.23.2"));
        assert!(spec.integrated.contains("! ---- ospfd"));
    }

    #[test]
    fn bgp_neighbour_resolution_and_golden_config() {
        let t = topo(
            r#"lab "b" { routing frr }
profile router { forward ipv4 }
node r1 : router {
  lo 1.1.1.1/32
  frr { bgp { as 65001 router-id 1.1.1.1 neighbor r2 network 10.10.0.0/24 redistribute [connected] } }
}
node r2 : router { frr { bgp { as 65002 neighbor r1 neighbor r3 as 65003 remote 192.0.2.9 } } }
node r3 : router
link r1:eth0 -- r2:eth0 { 192.0.2.1/30 -- 192.0.2.2/30 }
link r2:eth1 -- r3:eth0 { 192.0.2.5/30 -- 192.0.2.6/30 }
"#,
        );
        let cfg = t.effective_frr(&t.nodes["r1"]).unwrap();
        let spec = build_spec(&t, "r1", &t.nodes["r1"], &cfg).unwrap();
        assert_eq!(spec.daemons, vec![Daemon::Zebra, Daemon::Bgpd]);
        assert_eq!(spec.expected_bgp_peers, 1);
        assert_eq!(
            spec.confs[&Daemon::Bgpd],
            concat!(
                "frr defaults traditional\nhostname r1\n!\n",
                "router bgp 65001\n bgp router-id 1.1.1.1\n no bgp ebgp-requires-policy\n no bgp network import-check\n",
                " neighbor 192.0.2.2 remote-as 65002\n !\n address-family ipv4 unicast\n  network 10.10.0.0/24\n",
                "  redistribute connected\n  neighbor 192.0.2.2 activate\n exit-address-family\n!\nline vty\n"
            )
        );
        // r2: r1 resolved from the shared link, r3 explicit
        let cfg2 = t.effective_frr(&t.nodes["r2"]).unwrap();
        let spec2 = build_spec(&t, "r2", &t.nodes["r2"], &cfg2).unwrap();
        let bgpd = &spec2.confs[&Daemon::Bgpd];
        assert!(
            bgpd.contains("neighbor 192.0.2.1 remote-as 65001"),
            "{bgpd}"
        );
        assert!(
            bgpd.contains("neighbor 192.0.2.9 remote-as 65003"),
            "{bgpd}"
        );
        // r3 has no bgp config and no `as` → error when someone peers with it implicitly
        let bad = BgpNeighbor {
            node: "r3".into(),
            remote_as: None,
            remote: None,
        };
        let err = resolve_bgp_neighbor(&t, "r2", &bad)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no `bgp { as … }`"), "{err}");
    }

    #[test]
    fn node_interfaces_covers_links_and_networks() {
        let t = topo(
            r#"lab "n"
node a
node b
node c
link a:eth0 -- b:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
network lan { members [a:eth1, b:eth1, c:eth0] port a:eth1 { 10.9.0.1/24 } port b:eth1 { 10.9.0.2/24 } port c:eth0 { 10.9.0.3/24 } }
"#,
        );
        let a = node_interfaces(&t, "a");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "eth0");
        assert!(a[0].p2p && a[0].peers == vec!["b".to_string()]);
        assert_eq!(a[1].name, "eth1");
        assert!(!a[1].p2p);
        assert_eq!(a[1].peers, vec!["b".to_string(), "c".to_string()]);
    }
}
