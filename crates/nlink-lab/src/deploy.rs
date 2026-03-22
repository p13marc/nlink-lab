//! Lab deployment engine.
//!
//! Takes a validated [`Topology`] and creates the actual network lab using
//! nlink APIs. Follows the deployment sequence from the design document.

use std::collections::HashMap;
use std::net::IpAddr;
use nlink::netlink::namespace;
use nlink::netlink::ratelimit::RateLimiter;
use nlink::netlink::tc::NetemConfig;
use nlink::{Connection, Route};

use crate::error::{Error, Result};
use crate::helpers::{parse_cidr, parse_duration, parse_percent, parse_rate_bps};
use crate::running::RunningLab;
use crate::state::{self, LabState};
use crate::types::{EndpointRef, Topology};

/// Deploy a topology, creating all namespaces, links, addresses, routes, etc.
///
/// Returns a [`RunningLab`] handle for interacting with the deployed lab.
pub async fn deploy(topology: &Topology) -> Result<RunningLab> {
    // Safety check: validate first
    topology.validate().bail()?;

    // Check if lab already exists
    if state::exists(&topology.lab.name) {
        return Err(Error::AlreadyExists {
            name: topology.lab.name.clone(),
        });
    }

    let mut cleanup = Cleanup::new();
    let mut namespace_names: HashMap<String, String> = HashMap::new();
    let mut pids: Vec<(String, u32)> = Vec::new();

    // ── Step 3: Create namespaces ──────────────────────────────────
    for node_name in topology.nodes.keys() {
        let ns_name = topology.namespace_name(node_name);
        if namespace::exists(&ns_name) {
            return Err(Error::AlreadyExists {
                name: format!("namespace '{ns_name}' already exists"),
            });
        }
        namespace::create(&ns_name).map_err(|e| {
            Error::deploy_failed(format!("failed to create namespace '{ns_name}': {e}"))
        })?;
        cleanup.add_namespace(ns_name.clone());
        namespace_names.insert(node_name.clone(), ns_name);
    }

    // ── Step 4: Create bridge networks ───────────────────────────────
    // Bridges live in a management namespace. For each network, create the bridge
    // in a dedicated namespace, then create veth pairs from member nodes.
    let mut bridge_ns_names: HashMap<String, String> = HashMap::new();
    if !topology.networks.is_empty() {
        let mgmt_ns = format!("{}-mgmt", topology.lab.prefix());
        namespace::create(&mgmt_ns).map_err(|e| {
            Error::deploy_failed(format!("failed to create management namespace '{mgmt_ns}': {e}"))
        })?;
        cleanup.add_namespace(mgmt_ns.clone());

        let mgmt_conn: Connection<Route> =
            namespace::connection_for(&mgmt_ns).map_err(|e| {
                Error::deploy_failed(format!("connection for '{mgmt_ns}': {e}"))
            })?;

        for (net_name, network) in &topology.networks {
            let bridge_name = format!("{}-{}", topology.lab.prefix(), net_name);
            // Truncate to 15 chars (Linux interface name limit)
            let bridge_name = if bridge_name.len() > 15 {
                bridge_name[..15].to_string()
            } else {
                bridge_name
            };

            let mut bridge = nlink::netlink::link::BridgeLink::new(&bridge_name);
            if let Some(true) = network.vlan_filtering {
                bridge = bridge.vlan_filtering(true);
            }
            if let Some(mtu) = network.mtu {
                bridge = bridge.mtu(mtu);
            }

            mgmt_conn.add_link(bridge).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create bridge '{bridge_name}' for network '{net_name}': {e}"
                ))
            })?;
            mgmt_conn.set_link_up(&bridge_name).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up bridge '{bridge_name}': {e}"
                ))
            })?;

            bridge_ns_names.insert(net_name.clone(), mgmt_ns.clone());

            // Create veth pairs for each member: one end in node ns, other in mgmt ns attached to bridge
            let mgmt_ns_fd = namespace::open(&mgmt_ns).map_err(|e| {
                Error::deploy_failed(format!("failed to open mgmt namespace: {e}"))
            })?;

            for (k, member) in network.members.iter().enumerate() {
                let ep = EndpointRef::parse(member).ok_or_else(|| Error::InvalidEndpoint {
                    endpoint: member.clone(),
                })?;
                let node_ns = namespace_names.get(&ep.node).ok_or_else(|| {
                    Error::NodeNotFound {
                        name: ep.node.clone(),
                    }
                })?;

                // The peer end in mgmt ns gets a generated name
                let peer_name = format!("br{}p{}", net_name.chars().take(4).collect::<String>(), k);
                let peer_name = if peer_name.len() > 15 {
                    peer_name[..15].to_string()
                } else {
                    peer_name
                };

                let node_conn: Connection<Route> =
                    namespace::connection_for(node_ns).map_err(|e| {
                        Error::deploy_failed(format!("connection for '{node_ns}': {e}"))
                    })?;

                let veth = nlink::netlink::link::VethLink::new(&ep.iface, &peer_name)
                    .peer_netns_fd(mgmt_ns_fd.as_raw_fd());

                node_conn.add_link(veth).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create veth for network '{net_name}' member '{member}': {e}"
                    ))
                })?;

                // Step 7: Attach the peer end to the bridge in mgmt ns
                mgmt_conn
                    .set_link_master(&peer_name, &bridge_name)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to attach '{peer_name}' to bridge '{bridge_name}': {e}"
                        ))
                    })?;
                mgmt_conn
                    .set_link_up(&peer_name)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to bring up bridge port '{peer_name}': {e}"
                        ))
                    })?;
            }
        }
    }

    // ── Step 5: Create veth pairs ──────────────────────────────────
    for (i, link) in topology.links.iter().enumerate() {
        let ep_a = EndpointRef::parse(&link.endpoints[0]).ok_or_else(|| {
            Error::InvalidEndpoint {
                endpoint: link.endpoints[0].clone(),
            }
        })?;
        let ep_b = EndpointRef::parse(&link.endpoints[1]).ok_or_else(|| {
            Error::InvalidEndpoint {
                endpoint: link.endpoints[1].clone(),
            }
        })?;

        let ns_a = namespace_names.get(&ep_a.node).ok_or_else(|| Error::NodeNotFound {
            name: ep_a.node.clone(),
        })?;
        let ns_b = namespace_names.get(&ep_b.node).ok_or_else(|| Error::NodeNotFound {
            name: ep_b.node.clone(),
        })?;

        // Open namespace fd for the peer end
        let ns_b_fd = namespace::open(ns_b).map_err(|e| {
            Error::deploy_failed(format!("failed to open namespace '{ns_b}': {e}"))
        })?;

        // Get connection for namespace A
        let conn_a: Connection<Route> = namespace::connection_for(ns_a).map_err(|e| {
            Error::deploy_failed(format!("failed to connect to namespace '{ns_a}': {e}"))
        })?;

        // Create veth pair
        let mut veth =
            nlink::netlink::link::VethLink::new(&ep_a.iface, &ep_b.iface)
                .peer_netns_fd(ns_b_fd.as_raw_fd());

        if let Some(mtu) = link.mtu {
            veth = veth.mtu(mtu);
        }

        conn_a.add_link(veth).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create veth pair for link[{i}] ({} <-> {}): {e}",
                link.endpoints[0], link.endpoints[1]
            ))
        })?;
    }

    // ── Step 6: Create additional interfaces (loopback addresses handled in step 9) ──
    for (node_name, node) in &topology.nodes {
        let ns_name = &namespace_names[node_name];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("failed to connect to namespace '{ns_name}': {e}"))
        })?;

        for (iface_name, iface_config) in &node.interfaces {
            match iface_config.kind.as_deref() {
                Some("dummy") => {
                    conn.add_link(nlink::netlink::link::DummyLink::new(iface_name))
                        .await
                        .map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to create dummy interface '{iface_name}' on node '{node_name}': {e}"
                            ))
                        })?;
                }
                Some("vxlan") => {
                    let vni = iface_config.vni.ok_or_else(|| {
                        Error::invalid_topology(format!(
                            "vxlan interface '{iface_name}' on node '{node_name}' missing vni"
                        ))
                    })?;
                    let mut vxlan = nlink::netlink::link::VxlanLink::new(iface_name, vni);
                    if let Some(local) = &iface_config.local {
                        let addr: std::net::Ipv4Addr = local.parse().map_err(|e| {
                            Error::invalid_topology(format!("bad vxlan local address '{local}': {e}"))
                        })?;
                        vxlan = vxlan.local(addr);
                    }
                    if let Some(remote) = &iface_config.remote {
                        let addr: std::net::Ipv4Addr = remote.parse().map_err(|e| {
                            Error::invalid_topology(format!(
                                "bad vxlan remote address '{remote}': {e}"
                            ))
                        })?;
                        vxlan = vxlan.remote(addr);
                    }
                    if let Some(port) = iface_config.port {
                        vxlan = vxlan.port(port);
                    }
                    conn.add_link(vxlan).await.map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to create vxlan '{iface_name}' on node '{node_name}': {e}"
                        ))
                    })?;
                }
                // loopback or no kind — skip creation (lo exists already, addresses set in step 9)
                None => {}
                Some(kind) => {
                    tracing::warn!(
                        "unsupported interface kind '{kind}' on node '{node_name}'.{iface_name} — skipping"
                    );
                }
            }

            // Set MTU if specified
            if let Some(mtu) = iface_config.mtu {
                if iface_config.kind.is_some() {
                    // Only set MTU on interfaces we created (not lo)
                    conn.set_link_mtu(iface_name, mtu).await.map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to set MTU on '{node_name}'.{iface_name}: {e}"
                        ))
                    })?;
                }
            }
        }
    }

    // ── Step 9: Set interface addresses ────────────────────────────
    // From links
    for (i, link) in topology.links.iter().enumerate() {
        if let Some(addresses) = &link.addresses {
            for (j, ep_str) in link.endpoints.iter().enumerate() {
                let ep = EndpointRef::parse(ep_str).unwrap();
                let ns_name = &namespace_names[&ep.node];
                let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
                    Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
                })?;
                let (ip, prefix) = parse_cidr(&addresses[j])?;
                let iface_ref = nlink::netlink::InterfaceRef::Name(ep.iface.clone());
                let idx = conn.resolve_interface(&iface_ref).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "cannot resolve interface '{}' in link[{i}]: {e}",
                        ep.iface
                    ))
                })?;
                conn.add_address_by_index(idx, ip, prefix).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to add address '{}'/{prefix} to '{}' on '{}': {e}",
                        ip, ep.iface, ep.node
                    ))
                })?;
            }
        }
    }

    // From explicit interfaces
    for (node_name, node) in &topology.nodes {
        let ns_name = &namespace_names[node_name];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;

        for (iface_name, iface_config) in &node.interfaces {
            for addr_str in &iface_config.addresses {
                let (ip, prefix) = parse_cidr(addr_str)?;
                // For loopback, use index 1; otherwise resolve by name
                let idx = if iface_name == "lo" {
                    1
                } else {
                    let iface_ref =
                        nlink::netlink::InterfaceRef::Name(iface_name.clone());
                    conn.resolve_interface(&iface_ref).await.map_err(|e| {
                        Error::deploy_failed(format!(
                            "cannot resolve interface '{iface_name}' on '{node_name}': {e}"
                        ))
                    })?
                };
                conn.add_address_by_index(idx, ip, prefix)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to add address '{ip}'/{prefix} to '{iface_name}' on '{node_name}': {e}"
                        ))
                    })?;
            }
        }
    }

    // ── Step 10: Bring interfaces up ───────────────────────────────
    for (node_name, _) in &topology.nodes {
        let ns_name = &namespace_names[node_name];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;
        let links = conn.get_links().await.map_err(|e| {
            Error::deploy_failed(format!("failed to list links in '{ns_name}': {e}"))
        })?;
        for link_msg in &links {
            conn.set_link_up_by_index(link_msg.ifindex()).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up interface idx {} in '{ns_name}': {e}",
                    link_msg.ifindex()
                ))
            })?;
        }
    }

    // ── Step 11: Apply sysctls ─────────────────────────────────────
    for (node_name, node) in &topology.nodes {
        let sysctls = topology.effective_sysctls(node);
        if !sysctls.is_empty() {
            let ns_name = &namespace_names[node_name];
            // Use execute_in to write sysctls via /proc/sys
            namespace::execute_in(ns_name, || {
                for (key, value) in &sysctls {
                    let path = format!("/proc/sys/{}", key.replace('.', "/"));
                    if let Err(e) = std::fs::write(&path, value) {
                        return Err(nlink::Error::InvalidMessage(format!(
                            "failed to set sysctl '{key}' = '{value}': {e}"
                        )));
                    }
                }
                Ok::<(), nlink::Error>(())
            })
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to apply sysctls for node '{node_name}': {e}"
                ))
            })?
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to apply sysctls for node '{node_name}': {e}"
                ))
            })?;
        }
    }

    // ── Step 12: Add routes ────────────────────────────────────────
    for (node_name, node) in &topology.nodes {
        if node.routes.is_empty() {
            continue;
        }
        let ns_name = &namespace_names[node_name];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;

        for (dest, route_config) in &node.routes {
            add_route(&conn, node_name, dest, route_config).await?;
        }
    }

    // ── Step 13: Apply nftables firewall rules ──────────────────────
    for (node_name, node) in &topology.nodes {
        if let Some(fw) = topology.effective_firewall(node) {
            let ns_name = &namespace_names[node_name];
            apply_firewall(ns_name, node_name, fw).await?;
        }
    }

    // ── Step 14: Apply netem impairments ───────────────────────────
    for (endpoint_str, impairment) in &topology.impairments {
        let ep = EndpointRef::parse(endpoint_str).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint_str.clone(),
        })?;
        let ns_name = &namespace_names[&ep.node];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;

        let netem = build_netem(impairment)?;
        conn.add_qdisc(&ep.iface, netem).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to apply netem on '{endpoint_str}': {e}"
            ))
        })?;
    }

    // ── Step 15: Apply rate limits ─────────────────────────────────
    for (endpoint_str, rate_limit) in &topology.rate_limits {
        // Skip if this endpoint also has an impairment (netem handles rate via .rate_bps)
        if topology.impairments.contains_key(endpoint_str) {
            tracing::warn!(
                "rate limit on '{endpoint_str}' skipped: netem impairment already configured (use impairment.rate instead)"
            );
            continue;
        }

        let ep = EndpointRef::parse(endpoint_str).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint_str.clone(),
        })?;
        let ns_name = &namespace_names[&ep.node];
        let conn: Connection<Route> = namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!("connection for '{ns_name}': {e}"))
        })?;

        let mut limiter = RateLimiter::new(&ep.iface);
        if let Some(egress) = &rate_limit.egress {
            limiter = limiter.egress(egress).map_err(|e| {
                Error::deploy_failed(format!("bad egress rate on '{endpoint_str}': {e}"))
            })?;
        }
        if let Some(ingress) = &rate_limit.ingress {
            limiter = limiter.ingress(ingress).map_err(|e| {
                Error::deploy_failed(format!("bad ingress rate on '{endpoint_str}': {e}"))
            })?;
        }
        limiter.apply(&conn).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to apply rate limit on '{endpoint_str}': {e}"
            ))
        })?;
    }

    // ── Step 16: Spawn background processes ────────────────────────
    for (node_name, node) in &topology.nodes {
        let ns_name = &namespace_names[node_name];

        for (i, exec_config) in node.exec.iter().enumerate() {
            if exec_config.cmd.is_empty() {
                continue;
            }

            let mut cmd = std::process::Command::new(&exec_config.cmd[0]);
            cmd.args(&exec_config.cmd[1..]);

            if exec_config.background {
                // Spawn in namespace using pre_exec + setns
                let ns_path = format!("/var/run/netns/{ns_name}");
                let child = spawn_in_namespace(&ns_path, cmd).map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to spawn background process on '{node_name}' exec[{i}]: {e}"
                    ))
                })?;
                pids.push((node_name.clone(), child.id()));
            } else {
                // Run and wait for completion
                let ns_path = format!("/var/run/netns/{ns_name}");
                let output = spawn_output_in_namespace(&ns_path, cmd).map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to run command on '{node_name}' exec[{i}]: {e}"
                    ))
                })?;
                if !output.status.success() {
                    return Err(Error::deploy_failed(format!(
                        "exec[{i}] on node '{node_name}' failed (exit {}): {}",
                        output.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
            }
        }
    }

    // ── Step 18: Write state file ──────────────────────────────────
    let lab_state = LabState {
        name: topology.lab.name.clone(),
        created_at: now_iso8601(),
        namespaces: namespace_names.clone(),
        pids: pids.clone(),
    };
    state::save(&lab_state, topology)?;

    // Disarm cleanup — deployment succeeded
    cleanup.disarm();

    Ok(RunningLab::new(
        topology.clone(),
        namespace_names,
        pids,
    ))
}

/// Apply nftables firewall rules for a node.
async fn apply_firewall(
    ns_name: &str,
    node_name: &str,
    fw: &crate::types::FirewallConfig,
) -> Result<()> {
    use nlink::netlink::nftables::types::{Chain, ChainType, Family, Hook, Policy, Priority, Rule};
    use nlink::netlink::Nftables;

    // nftables needs Connection<Nftables> (NETLINK_NETFILTER socket)
    let nft_conn: Connection<Nftables> =
        namespace::connection_for(ns_name).map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create nftables connection for '{node_name}': {e}"
            ))
        })?;

    let table_name = "nlink-lab";

    // Create table
    nft_conn.add_table(table_name, Family::Inet).await.map_err(|e| {
        Error::deploy_failed(format!("failed to create nftables table on '{node_name}': {e}"))
    })?;

    // Create input chain with policy
    let policy = match fw.policy.as_deref() {
        Some("drop") => Policy::Drop,
        _ => Policy::Accept,
    };
    let chain = Chain::new(table_name, "input")
        .family(Family::Inet)
        .hook(Hook::Input)
        .priority(Priority::Filter)
        .chain_type(ChainType::Filter)
        .policy(policy);
    nft_conn.add_chain(chain).await.map_err(|e| {
        Error::deploy_failed(format!(
            "failed to create nftables input chain on '{node_name}': {e}"
        ))
    })?;

    // Create forward chain with same policy
    let fwd_chain = Chain::new(table_name, "forward")
        .family(Family::Inet)
        .hook(Hook::Forward)
        .priority(Priority::Filter)
        .chain_type(ChainType::Filter)
        .policy(policy);
    nft_conn.add_chain(fwd_chain).await.map_err(|e| {
        Error::deploy_failed(format!(
            "failed to create nftables forward chain on '{node_name}': {e}"
        ))
    })?;

    // Add rules to input chain
    for fw_rule in &fw.rules {
        let action = fw_rule.action.as_deref().unwrap_or("accept");
        let match_expr = fw_rule.match_expr.as_deref().unwrap_or("");

        let mut rule = Rule::new(table_name, "input").family(Family::Inet);

        // Parse common match expressions
        rule = apply_match_expr(rule, match_expr);

        // Apply action
        rule = match action {
            "accept" => rule.accept(),
            "drop" => rule.drop(),
            _ => rule.accept(),
        };

        nft_conn.add_rule(rule).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to add nftables rule on '{node_name}': match='{match_expr}' action='{action}': {e}"
            ))
        })?;
    }

    Ok(())
}

/// Parse a match expression and apply it to an nftables rule.
fn apply_match_expr(
    rule: nlink::netlink::nftables::types::Rule,
    expr: &str,
) -> nlink::netlink::nftables::types::Rule {
    use nlink::netlink::nftables::types::CtState;

    let expr = expr.trim();

    if expr.starts_with("tcp dport ") {
        if let Ok(port) = expr.trim_start_matches("tcp dport ").trim().parse::<u16>() {
            return rule.match_tcp_dport(port);
        }
    }

    if expr.starts_with("udp dport ") {
        if let Ok(port) = expr.trim_start_matches("udp dport ").trim().parse::<u16>() {
            return rule.match_udp_dport(port);
        }
    }

    if expr.starts_with("ct state ") {
        let states = expr.trim_start_matches("ct state ").trim();
        let mut ct = CtState(0);
        for state in states.split(',') {
            match state.trim() {
                "established" => ct = ct | CtState::ESTABLISHED,
                "related" => ct = ct | CtState::RELATED,
                "new" => ct = ct | CtState::NEW,
                "invalid" => ct = ct | CtState::INVALID,
                _ => {}
            }
        }
        return rule.match_ct_state(ct);
    }

    // For unrecognized expressions, return the rule unchanged (best effort)
    tracing::warn!("unrecognized nftables match expression: '{expr}'");
    rule
}

/// Add a single route in a namespace.
async fn add_route(
    conn: &Connection<Route>,
    node_name: &str,
    dest: &str,
    route_config: &crate::types::RouteConfig,
) -> Result<()> {
    // Determine if this is IPv4 or IPv6 based on the gateway or destination
    let is_default = dest == "default";

    // Parse gateway to determine IP version
    let gw: Option<IpAddr> = if let Some(via) = &route_config.via {
        Some(via.parse().map_err(|e| {
            Error::invalid_topology(format!(
                "invalid gateway '{via}' for route '{dest}' on node '{node_name}': {e}"
            ))
        })?)
    } else {
        None
    };

    let is_v6 = gw.map_or(false, |ip| ip.is_ipv6())
        || (!is_default && dest.contains(':'));

    if is_v6 {
        let mut route = if is_default {
            nlink::netlink::route::Ipv6Route::new("::", 0)
        } else {
            let (addr, prefix) = parse_cidr(dest)?;
            match addr {
                IpAddr::V6(v6) => nlink::netlink::route::Ipv6Route::from_addr(v6, prefix),
                _ => {
                    return Err(Error::invalid_topology(format!(
                        "route '{dest}' on '{node_name}': expected IPv6 address"
                    )));
                }
            }
        };
        if let Some(IpAddr::V6(gw)) = gw {
            route = route.gateway(gw);
        }
        if let Some(dev) = &route_config.dev {
            route = route.dev(dev);
        }
        if let Some(metric) = route_config.metric {
            route = route.metric(metric);
        }
        conn.add_route(route).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to add route '{dest}' on node '{node_name}': {e}"
            ))
        })?;
    } else {
        let mut route = if is_default {
            nlink::netlink::route::Ipv4Route::new("0.0.0.0", 0)
        } else {
            let (addr, prefix) = parse_cidr(dest)?;
            match addr {
                IpAddr::V4(v4) => nlink::netlink::route::Ipv4Route::from_addr(v4, prefix),
                _ => {
                    return Err(Error::invalid_topology(format!(
                        "route '{dest}' on '{node_name}': expected IPv4 address"
                    )));
                }
            }
        };
        if let Some(IpAddr::V4(gw)) = gw {
            route = route.gateway(gw);
        }
        if let Some(dev) = &route_config.dev {
            route = route.dev(dev);
        }
        if let Some(metric) = route_config.metric {
            route = route.metric(metric);
        }
        conn.add_route(route).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to add route '{dest}' on node '{node_name}': {e}"
            ))
        })?;
    }

    Ok(())
}

/// Build a NetemConfig from an Impairment.
pub(crate) fn build_netem(impairment: &crate::types::Impairment) -> Result<NetemConfig> {
    let mut netem = NetemConfig::new();

    if let Some(delay) = &impairment.delay {
        netem = netem.delay(parse_duration(delay)?);
    }
    if let Some(jitter) = &impairment.jitter {
        netem = netem.jitter(parse_duration(jitter)?);
    }
    if let Some(loss) = &impairment.loss {
        netem = netem.loss(parse_percent(loss)?);
    }
    if let Some(rate) = &impairment.rate {
        netem = netem.rate_bps(parse_rate_bps(rate)?);
    }
    if let Some(corrupt) = &impairment.corrupt {
        netem = netem.corrupt(parse_percent(corrupt)?);
    }
    if let Some(reorder) = &impairment.reorder {
        netem = netem.reorder(parse_percent(reorder)?);
    }

    Ok(netem)
}

/// Spawn a process in a namespace using pre_exec + setns.
fn spawn_in_namespace(
    ns_path: &str,
    mut cmd: std::process::Command,
) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;

    let ns_path = ns_path.to_string();
    // SAFETY: pre_exec runs between fork and exec in the child process.
    // We open the namespace file and call setns to switch the child's network namespace.
    unsafe {
        cmd.pre_exec(move || {
            let file = std::fs::File::open(&ns_path)?;
            let ret = libc::setns(std::os::fd::AsRawFd::as_raw_fd(&file), libc::CLONE_NEWNET);
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn().map_err(|e| Error::deploy_failed(format!("spawn failed: {e}")))
}

/// Spawn a process in a namespace and wait for output.
fn spawn_output_in_namespace(
    ns_path: &str,
    mut cmd: std::process::Command,
) -> Result<std::process::Output> {
    use std::os::unix::process::CommandExt;

    let ns_path = ns_path.to_string();
    unsafe {
        cmd.pre_exec(move || {
            let file = std::fs::File::open(&ns_path)?;
            let ret = libc::setns(std::os::fd::AsRawFd::as_raw_fd(&file), libc::CLONE_NEWNET);
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.output().map_err(|e| Error::deploy_failed(format!("spawn failed: {e}")))
}

fn now_iso8601() -> String {
    // Simple UTC timestamp without external crate
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Rough conversion to ISO 8601 (good enough for state tracking)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to Y-M-D (simplified)
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Cleanup guard that removes namespaces on drop if deployment fails.
struct Cleanup {
    namespaces: Vec<String>,
    armed: bool,
}

impl Cleanup {
    fn new() -> Self {
        Self {
            namespaces: Vec::new(),
            armed: true,
        }
    }

    fn add_namespace(&mut self, name: String) {
        self.namespaces.push(name);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for ns in &self.namespaces {
            if namespace::exists(ns) {
                let _ = namespace::delete(ns);
            }
        }
    }
}
