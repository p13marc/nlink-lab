//! Pure planner: a node's links/interfaces/addresses/routes → `NetworkConfig` (Plan 158e/159a).

use crate::deploy::op::{Op, RouteSpec};
use crate::error::{Error, Result};
use crate::types::{EndpointRef, Topology};
use std::collections::BTreeMap;

/// Build a per-namespace [`NetworkConfig`] covering every IP
/// address and route nlink-lab wants on `node`. Plan 158e
/// Slice 1.
///
/// Each declared address/route carries enough identity for
/// `NetworkConfig::diff` to detect "modify" vs "add" — no
/// USERDATA-style keying like nftables has (RTNETLINK
/// resources are already keyed by their own attributes:
/// `(dev, address, prefix_len)` for addresses,
/// `(destination, prefix_len, table)` for routes).
///
/// **Scope** (Slice 1 — addresses + routes only): collects
/// every address attached to the node from the five existing
/// nlink-lab sources (per-link endpoints, per-node
/// `interfaces[...]`, network port configs, WireGuard
/// addresses, macvlan/ipvlan addresses) plus every route from
/// `node.routes` + auto-generated routes. **Veth creation,
/// interface ifup, bond/VRF/WG enslave, and exotic link kinds
/// stay imperative** (their bodies still live in the deploy
/// steps preceding this one).
pub(crate) fn topology_to_network_config(
    node_name: &str,
    node: &crate::types::Node,
    topology: &Topology,
    auto_routes: Option<&BTreeMap<String, crate::types::RouteConfig>>,
) -> Result<nlink::netlink::config::NetworkConfig> {
    use nlink::netlink::config::NetworkConfig;

    let mut cfg = NetworkConfig::new();

    // ── Links — single-namespace kinds only (Plan 158e Slice 2+3) ──
    //
    // Veth pairs stay imperative (peer_netns_fd is cross-namespace);
    // macvlan/ipvlan/VRF/WG/Wi-Fi stay imperative (upstream
    // LinkBuilder doesn't cover them or has thin coverage). Dummies,
    // bonds, and VLANs are clean single-namespace resources — fold
    // them into the declarative path with `.up()` so re-deploys are
    // idempotent here too.
    //
    // **Order matters for VLAN parents.** `node.interfaces` is a
    // `BTreeMap`, so iteration order is non-deterministic. nlink's
    // `NetworkConfig::apply` iterates `links_to_add` in declaration
    // order; a VLAN whose parent is also a declarative link (e.g. a
    // Dummy declared on the same node) must be declared *after* its
    // parent or the kernel returns `ENODEV` for the VLAN create
    // (Plan 158e polish — bug caught during the polish audit).
    //
    // Two-pass: declare Dummy + Bond + bond-member-master ops in
    // pass 1, declare VLANs in pass 2. VLANs whose parents are
    // imperative links (veths created in step 5, macvlans created
    // in step 6a, etc.) work either way; the two-pass shape only
    // matters when the parent is also declarative.
    use crate::types::InterfaceKind;

    // Pass 1 — parentless single-namespace kinds (Dummy, Bond,
    // VRF, Vxlan). The 0.19 upstream topo-sort handles dependency
    // ordering across `links_to_add`, but declaring parents
    // before children inside the config is still the cleanest
    // shape and matches what apply iterates.
    for (iface_name, iface_config) in &node.interfaces {
        match iface_config.kind {
            Some(InterfaceKind::Dummy) => {
                let mtu = iface_config.mtu;
                cfg = cfg.link(iface_name, move |mut b| {
                    b = b.dummy().up();
                    if let Some(m) = mtu {
                        b = b.mtu(m);
                    }
                    b
                });
            }
            Some(InterfaceKind::Bond) => {
                let mtu = iface_config.mtu;
                let bond = iface_config.bond.clone();
                cfg = cfg.link(iface_name, move |mut b| {
                    b = b.bond().up();
                    if let Some(m) = mtu {
                        b = b.mtu(m);
                    }
                    // Bonding options (#75): nlink 0.26 `LinkBuilder::bond_*`.
                    if let Some(o) = &bond {
                        use crate::types::{BondMode as M, LacpRate as L};
                        use nlink::netlink::config::{BondLacpRate, BondMode};
                        if let Some(mode) = o.mode {
                            b = b.bond_mode(match mode {
                                M::BalanceRr => BondMode::BalanceRr,
                                M::ActiveBackup => BondMode::ActiveBackup,
                                M::BalanceXor => BondMode::BalanceXor,
                                M::Broadcast => BondMode::Broadcast,
                                M::Lacp => BondMode::Ieee802_3ad,
                                M::BalanceTlb => BondMode::BalanceTlb,
                                M::BalanceAlb => BondMode::BalanceAlb,
                            });
                        }
                        if let Some(ms) = o.miimon {
                            b = b.miimon(ms);
                        }
                        if let Some(rate) = o.lacp_rate {
                            b = b.bond_lacp_rate(match rate {
                                L::Slow => BondLacpRate::Slow,
                                L::Fast => BondLacpRate::Fast,
                            });
                        }
                        if let Some(policy) = o.xmit_hash {
                            b = b.xmit_hash_policy(policy.kernel_value());
                        }
                        if let Some(n) = o.min_links {
                            b = b.min_links(n);
                        }
                        if let Some(ms) = o.updelay {
                            b = b.bond_updelay(ms);
                        }
                        if let Some(ms) = o.downdelay {
                            b = b.bond_downdelay(ms);
                        }
                    }
                    b
                });
                // Enslave each member (Plan 158e Slice 2 folds in
                // what was step 10b). The member link itself must
                // exist already (veth — created in step 5) and is left
                // *down* by the LinksUp stage: the kernel refuses to
                // enslave an up device, and nlink would `set_link_up`
                // before `set_link_master`. The bonding driver opens the
                // slave itself once enslaved, so the state stays
                // `Unchanged` here.
                for member in &iface_config.members {
                    let bond_name = iface_name.clone();
                    cfg = cfg.link(member, move |b| b.master(&bond_name));
                }
            }
            Some(InterfaceKind::Vxlan) => {
                // Plan 159a Slice 4 — declarative VXLAN via 0.19's
                // `vxlan_local` + `vxlan_remote` + `vxlan_port` +
                // `vxlan_underlay_dev` setters (upstream Plan 190
                // §2.1). NLL surfaces all four via the `local`,
                // `remote`, `port`, `underlay` keywords inside the
                // `vxlan` block.
                let vni = iface_config.vni.ok_or_else(|| {
                    Error::invalid_topology(format!(
                        "vxlan interface '{iface_name}' on node \
                         '{node_name}' missing vni"
                    ))
                })?;
                // nlink 0.26 only plumbs IPv4 underlay addresses
                // (`IFLA_VXLAN_LOCAL6`/`GROUP6` are dropped silently), so
                // an IPv6 underlay is refused here rather than deployed
                // as a tunnel without endpoints. The validator reports
                // it first (`vxlan-underlay-address`).
                let parse_underlay = |what: &str, v: &str| -> Result<std::net::IpAddr> {
                    let ip: std::net::IpAddr = v.parse().map_err(|e| {
                        Error::invalid_topology(format!(
                            "bad vxlan {what} address '{v}' on \
                             '{node_name}:{iface_name}': {e}"
                        ))
                    })?;
                    if ip.is_ipv6() {
                        return Err(Error::invalid_topology(format!(
                            "vxlan {what} address '{v}' on '{node_name}:{iface_name}' is IPv6: \
                             an IPv6 underlay is not applied by nlink 0.26"
                        )));
                    }
                    Ok(ip)
                };
                let local = iface_config
                    .local
                    .as_deref()
                    .map(|l| parse_underlay("local", l))
                    .transpose()?;
                let remote = iface_config
                    .remote
                    .as_deref()
                    .map(|r| parse_underlay("remote", r))
                    .transpose()?;
                let port = iface_config.port;
                let underlay = iface_config.underlay.clone();
                let mtu = iface_config.mtu;
                cfg = cfg.link(iface_name, move |mut b| {
                    b = b.vxlan(vni).up();
                    if let Some(l) = local {
                        b = b.vxlan_local(l);
                    }
                    if let Some(r) = remote {
                        b = b.vxlan_remote(r);
                    }
                    if let Some(p) = port {
                        b = b.vxlan_port(p);
                    }
                    if let Some(u) = underlay {
                        b = b.vxlan_underlay_dev(u);
                    }
                    if let Some(m) = mtu {
                        b = b.mtu(m);
                    }
                    b
                });
            }
            // Pass 2 below handles Vlan. VRF declares below (parent-
            // less but separate iteration on `node.vrfs`). Loopback
            // / None stay implicit.
            _ => {}
        }
    }

    // Pass 1.x — VRF link declarations (Plan 159a Slice 4 — closes
    // the VRF half of the 158e Slice 4 gap; 0.19 ships
    // `LinkBuilder::vrf(table)` per upstream Plan 190 §2.3).
    for (vrf_name, vrf_config) in &node.vrfs {
        let table = vrf_config.table;
        cfg = cfg.link(vrf_name, move |b| b.vrf(table).up());
    }

    // Pass 2 — VLAN sub-interfaces. Declared AFTER their potential
    // parent siblings so `NetworkConfig::apply` creates the parent
    // first within its links_to_add iteration.
    for (iface_name, iface_config) in &node.interfaces {
        let Some(InterfaceKind::Vlan) = iface_config.kind else {
            continue;
        };
        let parent = match iface_config.parent.as_deref() {
            Some(p) => p.to_string(),
            None => {
                return Err(Error::invalid_topology(format!(
                    "vlan interface '{iface_name}' on node \
                     '{node_name}' missing parent"
                )));
            }
        };
        let vid = match iface_config.vni {
            Some(v) => v as u16,
            None => {
                return Err(Error::invalid_topology(format!(
                    "vlan interface '{iface_name}' on node \
                     '{node_name}' missing vni (VLAN ID)"
                )));
            }
        };
        let mtu = iface_config.mtu;
        let protocol = iface_config.vlan_protocol;
        cfg = cfg.link(iface_name, move |mut b| {
            b = b.vlan(&parent, vid).up();
            // 802.1ad outer tag for Q-in-Q (#75).
            if let Some(p) = protocol {
                b = b.vlan_protocol(match p {
                    crate::types::VlanProtocol::Dot1q => nlink::netlink::link::VlanProtocol::Dot1q,
                    crate::types::VlanProtocol::Dot1ad => {
                        nlink::netlink::link::VlanProtocol::Dot1ad
                    }
                });
            }
            if let Some(m) = mtu {
                b = b.mtu(m);
            }
            b
        });
    }

    // Pass 3 — VRF enslave. Declared AFTER VRF link + VLAN children
    // so an iface enslaved to a VRF — including a declarative VLAN
    // — has both endpoints declared before the master ref. Plan
    // 159a Slice 4 (folds in what was step 10c).
    for (vrf_name, vrf_config) in &node.vrfs {
        for iface in &vrf_config.interfaces {
            let master = vrf_name.clone();
            cfg = cfg.link(iface, move |b| b.master(&master));
        }
    }

    // VRF-table routes are NOT declared here: nlink's purge converges
    // the main table only, so they are explicit `Op::Route` ops
    // (`vrf_route_ops`). `with_vrf_routes` adds them back for the
    // diff-only view.

    // ── Addresses, in the same order step 9 used to apply them ──
    // 1. From per-link endpoint addresses.
    for link in &topology.links {
        let Some(addresses) = &link.addresses else {
            continue;
        };
        for (j, ep_str) in link.endpoints.iter().enumerate() {
            let Some(ep) = EndpointRef::parse(ep_str) else {
                continue;
            };
            if ep.node != node_name {
                continue;
            }
            cfg = cfg.address(&ep.iface, &addresses[j]).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{}' on '{}:{}': {e}",
                    addresses[j], ep.node, ep.iface
                ))
            })?;
        }
    }

    // 2. From explicit per-node interfaces.
    for (iface_name, iface_config) in &node.interfaces {
        for addr_str in &iface_config.addresses {
            cfg = cfg.address(iface_name, addr_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{addr_str}' on '{node_name}:{iface_name}': {e}"
                ))
            })?;
        }
    }

    // 3. From network (bridge) port configs.
    for network in topology.networks.values() {
        for (key, port) in &network.ports {
            if port.addresses.is_empty() {
                continue;
            }
            let (port_node, port_iface) = match EndpointRef::parse(key) {
                Some(ep) => (ep.node, ep.iface),
                None => match port.interface.as_deref() {
                    Some(iface) => (key.clone(), iface.to_string()),
                    None => {
                        tracing::warn!(
                            "network port '{key}' has addresses but no resolvable iface; skipping"
                        );
                        continue;
                    }
                },
            };
            if port_node != node_name {
                continue;
            }
            for addr_str in &port.addresses {
                cfg = cfg.address(&port_iface, addr_str).map_err(|e| {
                    Error::deploy_failed(format!(
                        "invalid address '{addr_str}' on '{port_node}:{port_iface}': {e}"
                    ))
                })?;
            }
        }
    }

    // 4. WireGuard interface addresses.
    for (wg_name, wg_config) in &node.wireguard {
        for addr_str in &wg_config.addresses {
            cfg = cfg.address(wg_name, addr_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{addr_str}' on WireGuard '{node_name}:{wg_name}': {e}"
                ))
            })?;
        }
    }

    // 5. WiFi addresses.
    for w in &node.wifi {
        for addr_str in &w.addresses {
            cfg = cfg.address(&w.name, addr_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{addr_str}' on WiFi '{node_name}:{}': {e}",
                    w.name
                ))
            })?;
        }
    }

    // 6. macvlan + ipvlan addresses.
    for mv in &node.macvlans {
        for addr_str in &mv.addresses {
            cfg = cfg.address(&mv.name, addr_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{addr_str}' on macvlan '{node_name}:{}': {e}",
                    mv.name
                ))
            })?;
        }
    }
    for iv in &node.ipvlans {
        for addr_str in &iv.addresses {
            cfg = cfg.address(&iv.name, addr_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "invalid address '{addr_str}' on ipvlan '{node_name}:{}': {e}",
                    iv.name
                ))
            })?;
        }
    }

    // ── Routes (main + auto-generated) ──
    // Manual routes win on conflict; auto-routes only fill gaps.
    let mut route_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (dest, route_config) in &node.routes {
        cfg = push_route(cfg, node_name, dest, route_config, None)?;
        route_keys.insert(dest.clone());
    }
    if let Some(autos) = auto_routes {
        for (dest, route_config) in autos {
            if route_keys.contains(dest) {
                continue;
            }
            cfg = push_route(cfg, node_name, dest, route_config, None)?;
        }
    }

    Ok(cfg)
}

/// Add a single route to the in-progress `NetworkConfig`.
/// Handles the "default" → "0.0.0.0/0" / "::/0" translation
/// (nlink's `RouteBuilder` only accepts proper CIDR
/// destinations).
pub(crate) fn push_route(
    cfg: nlink::netlink::config::NetworkConfig,
    node_name: &str,
    dest: &str,
    route_config: &crate::types::RouteConfig,
    table: Option<u32>,
) -> Result<nlink::netlink::config::NetworkConfig> {
    let dst_cidr = normalize_dest(dest, route_config);

    let via = route_config.via.clone();
    let dev = route_config.dev.clone();
    let metric = route_config.metric;

    let cfg = cfg
        .route(&dst_cidr, move |mut r| {
            if let Some(gw) = &via {
                r = r.via(gw);
            }
            if let Some(d) = &dev {
                r = r.dev(d);
            }
            if let Some(m) = metric {
                r = r.metric(m);
            }
            if let Some(t) = table {
                // VRF routes (what used to be the imperative step 12b):
                // nlink 0.26's DeclaredRouteBuilder::table
                r = r.table(t);
            }
            r
        })
        .map_err(|e| {
            Error::deploy_failed(format!("invalid route '{dest}' on node '{node_name}': {e}"))
        })?;
    Ok(cfg)
}

/// `default` → `0.0.0.0/0` / `::/0`, bare IP → host route; family is
/// taken from the destination, else from `via`.
fn normalize_dest(dest: &str, route_config: &crate::types::RouteConfig) -> String {
    let is_v6 = route_config
        .via
        .as_deref()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.is_ipv6())
        .unwrap_or(false)
        || (dest != "default" && dest.contains(':'));
    if dest == "default" {
        if is_v6 { "::/0" } else { "0.0.0.0/0" }.to_string()
    } else if !dest.contains('/') {
        // Bare IP without prefix — assume host route.
        if is_v6 {
            format!("{dest}/128")
        } else {
            format!("{dest}/32")
        }
    } else {
        dest.to_string()
    }
}

/// Every `vrf { route … }` of `node` as an explicit, fully resolved
/// [`RouteSpec`] op (`Stage::Routes`).
pub(crate) fn vrf_route_ops(node_name: &str, node: &crate::types::Node) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    for (vrf_name, vrf_config) in &node.vrfs {
        for (dest, route_config) in &vrf_config.routes {
            let route = route_spec(node_name, vrf_name, dest, route_config, vrf_config.table)?;
            ops.push(Op::Route {
                node: node_name.to_string(),
                route,
            });
        }
    }
    Ok(ops)
}

fn route_spec(
    node_name: &str,
    vrf_name: &str,
    dest: &str,
    route_config: &crate::types::RouteConfig,
    table: u32,
) -> Result<RouteSpec> {
    let cidr = normalize_dest(dest, route_config);
    let bad = |why: String| {
        Error::invalid_topology(format!(
            "vrf '{vrf_name}' on node '{node_name}': route '{dest}': {why}"
        ))
    };
    let (addr, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| bad("expected CIDR".into()))?;
    let dest_ip: std::net::IpAddr = addr
        .parse()
        .map_err(|e| bad(format!("invalid destination: {e}")))?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|e| bad(format!("invalid prefix: {e}")))?;
    let max = if dest_ip.is_ipv6() { 128 } else { 32 };
    if prefix > max {
        return Err(bad(format!("prefix /{prefix} exceeds /{max}")));
    }
    let via = match &route_config.via {
        Some(gw) => {
            let gw: std::net::IpAddr = gw
                .parse()
                .map_err(|e| bad(format!("invalid gateway '{gw}': {e}")))?;
            if gw.is_ipv6() != dest_ip.is_ipv6() {
                return Err(bad(format!(
                    "gateway {gw} and destination {dest_ip} are different address families"
                )));
            }
            Some(gw)
        }
        None => None,
    };
    Ok(RouteSpec {
        dest: dest_ip,
        prefix,
        table,
        via,
        dev: route_config.dev.clone(),
        metric: route_config.metric,
    })
}

/// Diff-only view: the node's `NetworkConfig` *with* its VRF-table
/// routes declared, so `apply --check` / `compute_layered_diff` still
/// report VRF route additions and changes (removals are reported by
/// the plan diff's `DelRoute` ops instead).
pub(crate) fn with_vrf_routes(
    mut cfg: nlink::netlink::config::NetworkConfig,
    node_name: &str,
    node: &crate::types::Node,
) -> Result<nlink::netlink::config::NetworkConfig> {
    for vrf_config in node.vrfs.values() {
        for (dest, route_config) in &vrf_config.routes {
            cfg = push_route(cfg, node_name, dest, route_config, Some(vrf_config.table))?;
        }
    }
    Ok(cfg)
}

/// Static routes the planner adds per node for the lab's routing mode:
/// none for `manual`, the BFS result for `auto`, and for `frr` the same
/// result minus the FRR routers (they learn routes from their daemons;
/// hosts and stubs keep their static defaults).
pub(crate) fn auto_routes_for(
    topology: &Topology,
) -> BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>> {
    match topology.lab.routing {
        crate::types::RoutingMode::Manual => BTreeMap::new(),
        crate::types::RoutingMode::Auto => auto_generate_routes(topology),
        crate::types::RoutingMode::Frr => {
            let routers = crate::frr::frr_nodes(topology);
            let mut routes = auto_generate_routes(topology);
            routes.retain(|n, _| !routers.contains(n));
            routes
        }
    }
}

/// Auto-generate static routes from the topology graph, per address
/// family.
///
/// For stub nodes (single neighbor): adds a default route.
/// For transit nodes: runs BFS to find shortest paths to all remote subnets.
/// Manual routes are preserved — auto routes only fill gaps.
///
/// IPv4 and IPv6 are computed independently (issue #72): a dual-stack
/// stub gets both `default` and `::/0`, a router forwards a family only
/// when its sysctl says so (`net.ipv4.ip_forward` /
/// `net.ipv6.conf.all.forwarding`), and next hops never cross families.
pub(crate) fn auto_generate_routes(
    topology: &Topology,
) -> BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>> {
    let mut routes = auto_routes_for_family(topology, false);
    for (node, v6) in auto_routes_for_family(topology, true) {
        routes.entry(node).or_default().extend(v6);
    }
    routes
}

/// Does `existing` already carry a default route of this family?
/// `default` is the v4 default unless its gateway is IPv6; `::/0` and
/// `0.0.0.0/0` are explicit.
fn has_default(existing: &BTreeMap<String, crate::types::RouteConfig>, v6: bool) -> bool {
    existing.iter().any(|(dest, cfg)| {
        let via_v6 = cfg
            .via
            .as_deref()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.is_ipv6());
        match dest.as_str() {
            "::/0" => v6,
            "0.0.0.0/0" => !v6,
            "default" => via_v6.map_or(!v6, |is_v6| is_v6 == v6),
            _ => false,
        }
    })
}

fn is_family(addr: &str, v6: bool) -> bool {
    addr.split('/')
        .next()
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_ipv6() == v6)
}

fn auto_routes_for_family(
    topology: &Topology,
    v6: bool,
) -> BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    let default_key = if v6 { "::/0" } else { "default" };
    let forward_sysctl = if v6 {
        "net.ipv6.conf.all.forwarding"
    } else {
        "net.ipv4.ip_forward"
    };

    // 1. Build adjacency: node_name → Vec<(neighbor_name, gateway_ip)>
    let mut adjacency: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    // Also collect subnets per node: node_name → Vec<CIDR>
    let mut node_subnets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    // From point-to-point links
    for link in &topology.links {
        if let Some(addrs) = &link.addresses
            && is_family(&addrs[0], v6)
            && is_family(&addrs[1], v6)
            && let (Some(ep_a), Some(ep_b)) = (
                EndpointRef::parse(&link.endpoints[0]),
                EndpointRef::parse(&link.endpoints[1]),
            )
        {
            let ip_a = addrs[0].split('/').next().unwrap_or(&addrs[0]);
            let ip_b = addrs[1].split('/').next().unwrap_or(&addrs[1]);
            adjacency
                .entry(ep_a.node.clone())
                .or_default()
                .push((ep_b.node.clone(), ip_b.to_string()));
            adjacency
                .entry(ep_b.node.clone())
                .or_default()
                .push((ep_a.node.clone(), ip_a.to_string()));
            node_subnets
                .entry(ep_a.node.clone())
                .or_default()
                .insert(addrs[0].clone());
            node_subnets
                .entry(ep_b.node.clone())
                .or_default()
                .insert(addrs[1].clone());
        }
    }

    // From network (bridge) memberships — every address of the family
    // on the port counts (a port may carry several).
    for network in topology.networks.values() {
        let mut net_members: Vec<(String, String)> = Vec::new(); // (node, ip)
        for (ep_str, port) in &network.ports {
            let Some(ep) = EndpointRef::parse(ep_str) else {
                continue;
            };
            let mut first = true;
            for addr in port.addresses.iter().filter(|a| is_family(a, v6)) {
                let ip = addr.split('/').next().unwrap_or(addr);
                if first {
                    net_members.push((ep.node.clone(), ip.to_string()));
                    first = false;
                }
                node_subnets
                    .entry(ep.node.clone())
                    .or_default()
                    .insert(addr.clone());
            }
        }
        // All members of the same network are adjacent to each other
        for i in 0..net_members.len() {
            for j in 0..net_members.len() {
                if i != j {
                    adjacency
                        .entry(net_members[i].0.clone())
                        .or_default()
                        .push((net_members[j].0.clone(), net_members[j].1.clone()));
                }
            }
        }
    }

    // Ensure all nodes are in adjacency (even isolated ones)
    for node_name in topology.nodes.keys() {
        adjacency.entry(node_name.clone()).or_default();
    }

    // 2. For each node, compute routes
    let all_node_names: Vec<String> = topology.nodes.keys().cloned().collect();
    let mut auto_routes: BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>> =
        BTreeMap::new();

    let forwards = |name: &str| {
        topology
            .nodes
            .get(name)
            .is_some_and(|n| n.sysctls.get(forward_sysctl).is_some_and(|v| v == "1"))
    };

    for node_name in &all_node_names {
        let neighbors = adjacency.get(node_name).cloned().unwrap_or_default();
        let existing_routes = &topology.nodes[node_name].routes;

        if neighbors.is_empty() {
            continue;
        }

        // Default gateway for a non-router: prefer a neighbour that
        // forwards this family (on a shared segment the first neighbour
        // in name order is often another host), else the first one.
        let gateway = neighbors
            .iter()
            .find(|(n, _)| forwards(n))
            .unwrap_or(&neighbors[0])
            .1
            .clone();

        let default_via =
            |auto_routes: &mut BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>>,
             gw: &str| {
                if !has_default(existing_routes, v6) {
                    auto_routes.entry(node_name.clone()).or_default().insert(
                        default_key.to_string(),
                        crate::types::RouteConfig {
                            via: Some(gw.to_string()),
                            dev: None,
                            metric: None,
                        },
                    );
                }
            };

        // Stub node: single neighbor → default route
        if neighbors.len() == 1 || neighbors.iter().all(|(n, _)| n == &neighbors[0].0) {
            default_via(&mut auto_routes, &neighbors[0].1);
            continue;
        }

        // Transit node: BFS to find next-hop for remote subnets
        // Only if this node forwards this family (is a router)
        if !forwards(node_name) {
            // Non-router with multiple neighbors: default via the router
            // among them (or the first neighbour).
            default_via(&mut auto_routes, &gateway);
            continue;
        }

        // Router: BFS to find all reachable nodes and their next-hops
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<(String, String)> = VecDeque::new(); // (node, next_hop_ip)
        visited.insert(node_name.clone());

        // Seed with direct neighbors
        for (neighbor, gateway_ip) in &neighbors {
            if !visited.contains(neighbor) {
                visited.insert(neighbor.clone());
                queue.push_back((neighbor.clone(), gateway_ip.clone()));
            }
        }

        while let Some((current, next_hop_ip)) = queue.pop_front() {
            // Add routes for current node's subnets via next_hop_ip
            if let Some(subnets) = node_subnets.get(&current) {
                for subnet in subnets {
                    // Skip if directly connected
                    let my_subnets = node_subnets.get(node_name);
                    let is_direct = my_subnets.is_some_and(|s| s.contains(subnet));
                    if is_direct {
                        continue;
                    }
                    // Skip if manual route exists
                    if existing_routes.contains_key(subnet) {
                        continue;
                    }
                    // Derive the network CIDR from the address
                    if let Ok((ip, prefix)) = crate::helpers::parse_cidr(subnet) {
                        let net_addr = crate::helpers::network_address(ip, prefix);
                        let net_cidr = format!("{net_addr}/{prefix}");
                        if !existing_routes.contains_key(&net_cidr) {
                            auto_routes
                                .entry(node_name.clone())
                                .or_default()
                                .entry(net_cidr)
                                .or_insert(crate::types::RouteConfig {
                                    via: Some(next_hop_ip.clone()),
                                    dev: None,
                                    metric: None,
                                });
                        }
                    }
                }
            }

            // Continue BFS
            if let Some(next_neighbors) = adjacency.get(&current) {
                for (next, _) in next_neighbors {
                    if !visited.contains(next) {
                        visited.insert(next.clone());
                        queue.push_back((next.clone(), next_hop_ip.clone()));
                    }
                }
            }
        }
    }

    auto_routes
}
