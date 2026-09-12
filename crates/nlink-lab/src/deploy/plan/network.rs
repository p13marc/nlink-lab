//! Pure planner: a node's links/interfaces/addresses/routes → `NetworkConfig` (Plan 158e/159a).

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
                cfg = cfg.link(iface_name, move |mut b| {
                    b = b.bond().up();
                    if let Some(m) = mtu {
                        b = b.mtu(m);
                    }
                    b
                });
                // Enslave each member (Plan 158e Slice 2 folds in
                // what was step 10b). The member link itself must
                // exist already (veth — created in step 5).
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
                let local = if let Some(l) = &iface_config.local {
                    Some(l.parse::<std::net::Ipv4Addr>().map_err(|e| {
                        Error::invalid_topology(format!(
                            "bad vxlan local address '{l}' on \
                             '{node_name}:{iface_name}': {e}"
                        ))
                    })?)
                } else {
                    None
                };
                let remote = if let Some(r) = &iface_config.remote {
                    Some(r.parse::<std::net::Ipv4Addr>().map_err(|e| {
                        Error::invalid_topology(format!(
                            "bad vxlan remote address '{r}' on \
                             '{node_name}:{iface_name}': {e}"
                        ))
                    })?)
                } else {
                    None
                };
                let port = iface_config.port;
                let underlay = iface_config.underlay.clone();
                let mtu = iface_config.mtu;
                cfg = cfg.link(iface_name, move |mut b| {
                    b = b.vxlan(vni).up();
                    if let Some(l) = local {
                        b = b.vxlan_local(std::net::IpAddr::V4(l));
                    }
                    if let Some(r) = remote {
                        b = b.vxlan_remote(std::net::IpAddr::V4(r));
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
        cfg = cfg.link(iface_name, move |mut b| {
            b = b.vlan(&parent, vid).up();
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

    // Pass 4 — VRF routes, declared into the VRF's table (was the
    // imperative step 12b / `add_route_with_table`).
    for vrf_config in node.vrfs.values() {
        for (dest, route_config) in &vrf_config.routes {
            cfg = push_route(cfg, node_name, dest, route_config, Some(vrf_config.table))?;
        }
    }

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
    let is_v6 = route_config
        .via
        .as_deref()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.is_ipv6())
        .unwrap_or(false)
        || (dest != "default" && dest.contains(':'));

    let dst_cidr = if dest == "default" {
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
    };

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

/// Add a single route in a namespace.
/// Auto-generate static routes from the topology graph.
///
/// For stub nodes (single neighbor): adds a default route.
/// For transit nodes: runs BFS to find shortest paths to all remote subnets.
/// Manual routes are preserved — auto routes only fill gaps.
pub(crate) fn auto_generate_routes(
    topology: &Topology,
) -> BTreeMap<String, BTreeMap<String, crate::types::RouteConfig>> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    // 1. Build adjacency: node_name → Vec<(neighbor_name, gateway_ip)>
    let mut adjacency: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    // Also collect subnets per node: node_name → Vec<CIDR>
    let mut node_subnets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    // From point-to-point links
    for link in &topology.links {
        if let Some(addrs) = &link.addresses
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

    // From network (bridge) memberships
    for network in topology.networks.values() {
        let mut net_members: Vec<(String, String)> = Vec::new(); // (node, ip)
        for (ep_str, port) in &network.ports {
            if let Some(ep) = EndpointRef::parse(ep_str)
                && let Some(addr) = port.addresses.first()
            {
                let ip = addr.split('/').next().unwrap_or(addr);
                net_members.push((ep.node.clone(), ip.to_string()));
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

    for node_name in &all_node_names {
        let neighbors = adjacency.get(node_name).cloned().unwrap_or_default();
        let existing_routes = &topology.nodes[node_name].routes;

        if neighbors.is_empty() {
            continue;
        }

        // Stub node: single neighbor → default route
        if neighbors.len() == 1 || neighbors.iter().all(|(n, _)| n == &neighbors[0].0) {
            if !existing_routes.contains_key("default") {
                auto_routes.entry(node_name.clone()).or_default().insert(
                    "default".to_string(),
                    crate::types::RouteConfig {
                        via: Some(neighbors[0].1.clone()),
                        dev: None,
                        metric: None,
                    },
                );
            }
            continue;
        }

        // Transit node: BFS to find next-hop for remote subnets
        // Only if this node has ip_forward enabled (is a router)
        let is_router = topology.nodes[node_name]
            .sysctls
            .get("net.ipv4.ip_forward")
            .is_some_and(|v| v == "1");

        if !is_router {
            // Non-router with multiple neighbors: just add default via first
            if !existing_routes.contains_key("default") {
                auto_routes.entry(node_name.clone()).or_default().insert(
                    "default".to_string(),
                    crate::types::RouteConfig {
                        via: Some(neighbors[0].1.clone()),
                        dev: None,
                        metric: None,
                    },
                );
            }
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

    // 3. Convert to BTreeMap and return
    auto_routes
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect()
}
