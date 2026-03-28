//! Lab deployment engine.
//!
//! Takes a validated [`Topology`] and creates the actual network lab using
//! nlink APIs. Follows the deployment sequence from the design document.

use std::collections::HashMap;
use std::net::IpAddr;
use nlink::netlink::bridge_vlan::BridgeVlanBuilder;
use nlink::netlink::namespace;
use nlink::netlink::ratelimit::RateLimiter;
use nlink::netlink::tc::NetemConfig;
use nlink::{Connection, Route, Wireguard};

use nlink::netlink::namespace::NamespaceFd;

use crate::container::{CreateOpts, Runtime};
use crate::error::{Error, Result};
use crate::helpers::{parse_cidr, parse_duration, parse_percent, parse_rate_bps};
use crate::running::RunningLab;
use crate::state::{self, ContainerState, LabState};
use crate::types::{EndpointRef, InterfaceKind, Topology};

/// Abstraction over bare namespace vs container node.
enum NodeHandle {
    Namespace { ns_name: String },
    Container {
        id: String,
        pid: u32,
        ns_path: String,
    },
}

impl NodeHandle {
    fn connection<P: nlink::netlink::ProtocolState + Default>(&self) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::connection_for(ns_name),
            NodeHandle::Container { pid, .. } => namespace::connection_for_pid(*pid),
        }
    }

    fn open_ns_fd(&self) -> std::result::Result<NamespaceFd, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::open(ns_name),
            NodeHandle::Container { ns_path, .. } => namespace::open_path(ns_path),
        }
    }

    fn set_sysctls(&self, entries: &[(&str, &str)]) -> std::result::Result<(), nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::set_sysctls(ns_name, entries),
            NodeHandle::Container { ns_path, .. } => namespace::set_sysctls_path(ns_path, entries),
        }
    }

    fn spawn_output(&self, cmd: std::process::Command) -> std::result::Result<std::process::Output, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::spawn_output(ns_name, cmd),
            NodeHandle::Container { ns_path, .. } => namespace::spawn_output_path(ns_path, cmd),
        }
    }

    fn spawn(&self, cmd: std::process::Command) -> std::result::Result<std::process::Child, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::spawn(ns_name, cmd),
            NodeHandle::Container { ns_path, .. } => namespace::spawn_path(ns_path, cmd),
        }
    }

    fn enter(&self) -> std::result::Result<nlink::netlink::namespace::NamespaceGuard, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::enter(ns_name),
            NodeHandle::Container { ns_path, .. } => namespace::enter_path(ns_path),
        }
    }

    fn container_id(&self) -> Option<&str> {
        match self {
            NodeHandle::Container { id, .. } => Some(id),
            NodeHandle::Namespace { .. } => None,
        }
    }
}

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
    let mut node_handles: HashMap<String, NodeHandle> = HashMap::new();
    let mut namespace_names: HashMap<String, String> = HashMap::new();
    let mut container_states: HashMap<String, ContainerState> = HashMap::new();
    let mut pids: Vec<(String, u32)> = Vec::new();

    // Detect container runtime if any node uses an image
    let has_container_nodes = topology.nodes.values().any(|n| n.image.is_some());
    let container_runtime = if has_container_nodes {
        let rt_config = topology.lab.runtime.as_ref().cloned().unwrap_or_default();
        let rt = Runtime::new(&rt_config)?;
        cleanup.set_runtime(rt.binary());
        Some(rt)
    } else {
        None
    };

    // ── Step 3: Create namespaces / containers ─────────────────────
    for (node_name, node) in &topology.nodes {
        if let Some(image) = &node.image {
            // Container node
            let rt = container_runtime.as_ref().unwrap();
            rt.ensure_image(image)?;
            let container_name = format!("{}-{}", topology.lab.prefix(), node_name);
            let opts = CreateOpts {
                cmd: node.cmd.clone(),
                env: node.env.clone().unwrap_or_default(),
                volumes: node.volumes.clone().unwrap_or_default(),
            };
            let info = rt.create(&container_name, image, &opts)?;
            cleanup.add_container(info.id.clone());
            container_states.insert(
                node_name.clone(),
                ContainerState {
                    id: info.id.clone(),
                    name: info.name.clone(),
                    image: image.clone(),
                    pid: info.pid,
                },
            );
            node_handles.insert(
                node_name.clone(),
                NodeHandle::Container {
                    id: info.id,
                    pid: info.pid,
                    ns_path: format!("/proc/{}/ns/net", info.pid),
                },
            );
        } else {
            // Bare namespace node
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
            namespace_names.insert(node_name.clone(), ns_name.clone());
            node_handles.insert(
                node_name.clone(),
                NodeHandle::Namespace { ns_name },
            );
        }
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
                let node_handle = node_handles.get(&ep.node).ok_or_else(|| {
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
                    node_handle.connection().map_err(|e| {
                        Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
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

                // Apply VLAN configuration for this port if defined
                if let Some(port_config) = network.ports.get(&ep.node) {
                    // Apply tagged VLANs
                    for &vid in &port_config.vlans {
                        let mut vlan = BridgeVlanBuilder::new(vid).dev(&peer_name);
                        if port_config.untagged == Some(true) {
                            vlan = vlan.untagged();
                        }
                        if Some(vid) == port_config.pvid {
                            vlan = vlan.pvid().untagged();
                        }
                        mgmt_conn.add_bridge_vlan(vlan).await.map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to add VLAN {vid} to port '{peer_name}' on bridge '{bridge_name}': {e}"
                            ))
                        })?;
                    }
                    // Apply PVID if not already covered by vlans list
                    if let Some(pvid) = port_config.pvid {
                        if !port_config.vlans.contains(&pvid) {
                            let vlan = BridgeVlanBuilder::new(pvid).dev(&peer_name).pvid().untagged();
                            mgmt_conn.add_bridge_vlan(vlan).await.map_err(|e| {
                                Error::deploy_failed(format!(
                                    "failed to add PVID {pvid} to port '{peer_name}' on bridge '{bridge_name}': {e}"
                                ))
                            })?;
                        }
                    }
                }
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

        let handle_a = node_handles.get(&ep_a.node).ok_or_else(|| Error::NodeNotFound {
            name: ep_a.node.clone(),
        })?;
        let handle_b = node_handles.get(&ep_b.node).ok_or_else(|| Error::NodeNotFound {
            name: ep_b.node.clone(),
        })?;

        // Open namespace fd for the peer end
        let ns_b_fd = handle_b.open_ns_fd().map_err(|e| {
            Error::deploy_failed(format!("failed to open namespace for '{}': {e}", ep_b.node))
        })?;

        // Get connection for namespace A
        let conn_a: Connection<Route> = handle_a.connection().map_err(|e| {
            Error::deploy_failed(format!("failed to connect to '{}': {e}", ep_a.node))
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
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (iface_name, iface_config) in &node.interfaces {
            match &iface_config.kind {
                Some(InterfaceKind::Dummy) => {
                    conn.add_link(nlink::netlink::link::DummyLink::new(iface_name))
                        .await
                        .map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to create dummy interface '{iface_name}' on node '{node_name}': {e}"
                            ))
                        })?;
                }
                Some(InterfaceKind::Vxlan) => {
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
                Some(InterfaceKind::Bond) => {
                    conn.add_link(nlink::netlink::link::BondLink::new(iface_name))
                        .await
                        .map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to create bond interface '{iface_name}' on node '{node_name}': {e}"
                            ))
                        })?;
                }
                Some(InterfaceKind::Vlan) => {
                    let parent = iface_config.parent.as_deref().ok_or_else(|| {
                        Error::invalid_topology(format!(
                            "vlan interface '{iface_name}' on node '{node_name}' missing parent"
                        ))
                    })?;
                    let vid = iface_config.vni.ok_or_else(|| {
                        Error::invalid_topology(format!(
                            "vlan interface '{iface_name}' on node '{node_name}' missing vni (VLAN ID)"
                        ))
                    })? as u16;
                    conn.add_link(nlink::netlink::link::VlanLink::new(iface_name, parent, vid))
                        .await
                        .map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to create vlan '{iface_name}' on node '{node_name}': {e}"
                            ))
                        })?;
                }
                // loopback or no kind — skip creation (lo exists already, addresses set in step 9)
                None => {}
                Some(InterfaceKind::Loopback) => {
                    // loopback exists already, addresses set in step 9
                }
            }

            // Set MTU if specified
            if let Some(mtu) = iface_config.mtu {
                if iface_config.kind.is_some() && iface_config.kind != Some(InterfaceKind::Loopback) {
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

    // ── Step 6b: Create VRF interfaces ─────────────────────────────
    for (node_name, node) in &topology.nodes {
        if node.vrfs.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (vrf_name, vrf_config) in &node.vrfs {
            conn.add_link(nlink::netlink::link::VrfLink::new(vrf_name, vrf_config.table))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create VRF '{vrf_name}' on node '{node_name}': {e}"
                    ))
                })?;
            conn.set_link_up(vrf_name).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up VRF '{vrf_name}' on node '{node_name}': {e}"
                ))
            })?;
        }
    }

    // ── Step 6c: Create WireGuard interfaces ─────────────────────
    // Phase 1: Create the netlink interfaces (configuration happens after Step 10)
    for (node_name, node) in &topology.nodes {
        if node.wireguard.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for wg_name in node.wireguard.keys() {
            conn.add_link(nlink::netlink::link::WireguardLink::new(wg_name))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create WireGuard interface '{wg_name}' on node '{node_name}': {e}"
                    ))
                })?;
        }
    }

    // ── Step 9: Set interface addresses ────────────────────────────
    // From links
    for (i, link) in topology.links.iter().enumerate() {
        if let Some(addresses) = &link.addresses {
            for (j, ep_str) in link.endpoints.iter().enumerate() {
                let ep = EndpointRef::parse(ep_str).unwrap();
                let ep_handle = &node_handles[&ep.node];
                let conn: Connection<Route> = ep_handle.connection().map_err(|e| {
                    Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
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
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
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

    // From WireGuard interfaces
    for (node_name, node) in &topology.nodes {
        if node.wireguard.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (wg_name, wg_config) in &node.wireguard {
            let iface_ref = nlink::netlink::InterfaceRef::Name(wg_name.clone());
            let idx = conn.resolve_interface(&iface_ref).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "cannot resolve WireGuard interface '{wg_name}' on '{node_name}': {e}"
                ))
            })?;
            for addr_str in &wg_config.addresses {
                let (ip, prefix) = parse_cidr(addr_str)?;
                conn.add_address_by_index(idx, ip, prefix)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to add address '{ip}'/{prefix} to WireGuard '{wg_name}' on '{node_name}': {e}"
                        ))
                    })?;
            }
        }
    }

    // ── Step 10: Bring interfaces up ───────────────────────────────
    for (node_name, _) in &topology.nodes {
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;
        let links = conn.get_links().await.map_err(|e| {
            Error::deploy_failed(format!("failed to list links in '{node_name}': {e}"))
        })?;
        for link_msg in &links {
            conn.set_link_up_by_index(link_msg.ifindex()).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up interface idx {} in '{node_name}': {e}",
                    link_msg.ifindex()
                ))
            })?;
        }
    }

    // ── Step 10b: Enslave bond members ─────────────────────────────
    for (node_name, node) in &topology.nodes {
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (iface_name, iface_config) in &node.interfaces {
            if iface_config.kind != Some(InterfaceKind::Bond) || iface_config.members.is_empty() {
                continue;
            }
            for member in &iface_config.members {
                // Members must be down to be enslaved to a bond
                conn.set_link_down(member).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to bring down '{member}' for bond enslavement on '{node_name}': {e}"
                    ))
                })?;
                conn.set_link_master(member, iface_name).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to enslave '{member}' to bond '{iface_name}' on '{node_name}': {e}"
                    ))
                })?;
                conn.set_link_up(member).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to bring up '{member}' after bond enslavement on '{node_name}': {e}"
                    ))
                })?;
            }
        }
    }

    // ── Step 10c: Enslave interfaces to VRFs ─────────────────────
    for (node_name, node) in &topology.nodes {
        if node.vrfs.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (vrf_name, vrf_config) in &node.vrfs {
            for iface in &vrf_config.interfaces {
                conn.set_link_master(iface, vrf_name).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to enslave '{iface}' to VRF '{vrf_name}' on '{node_name}': {e}"
                    ))
                })?;
            }
        }
    }

    // ── Step 10d: Configure WireGuard devices ────────────────────
    // Phase 2: Generate keys and configure peers.
    // We collect all generated public keys first, then configure peers.
    let mut wg_public_keys: HashMap<String, HashMap<String, [u8; 32]>> = HashMap::new();

    // First pass: set private keys and listen ports, collect public keys
    for (node_name, node) in &topology.nodes {
        if node.wireguard.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let _guard = node_handle.enter().map_err(|e| {
            Error::deploy_failed(format!("failed to enter namespace for '{node_name}': {e}"))
        })?;
        let wg_conn = Connection::<Wireguard>::new_async().await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create WireGuard connection for '{node_name}': {e}"
            ))
        })?;

        let mut node_keys = HashMap::new();
        for (wg_name, wg_config) in &node.wireguard {
            let private_key = match wg_config.private_key.as_deref() {
                Some("auto") | None => generate_wg_private_key()?,
                Some(key_str) => decode_wg_key(key_str).map_err(|e| {
                    Error::invalid_topology(format!(
                        "invalid WireGuard private key for '{wg_name}' on '{node_name}': {e}"
                    ))
                })?,
            };

            let public_key = derive_wg_public_key(&private_key);
            node_keys.insert(wg_name.clone(), public_key);

            wg_conn
                .set_device(wg_name.as_str(), |dev| {
                    let mut dev = dev.private_key(private_key);
                    if let Some(port) = wg_config.listen_port {
                        dev = dev.listen_port(port);
                    }
                    dev
                })
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to configure WireGuard '{wg_name}' on '{node_name}': {e}"
                    ))
                })?;
        }
        wg_public_keys.insert(node_name.clone(), node_keys);
    }

    // Second pass: configure peers
    for (node_name, node) in &topology.nodes {
        if node.wireguard.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let _guard = node_handle.enter().map_err(|e| {
            Error::deploy_failed(format!("failed to enter namespace for '{node_name}': {e}"))
        })?;
        let wg_conn = Connection::<Wireguard>::new_async().await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create WireGuard connection for '{node_name}': {e}"
            ))
        })?;

        for (wg_name, wg_config) in &node.wireguard {
            if wg_config.peers.is_empty() {
                continue;
            }

            for peer_node_name in &wg_config.peers {
                // Find the peer's WireGuard public key and endpoint
                let peer_keys = wg_public_keys.get(peer_node_name).ok_or_else(|| {
                    Error::invalid_topology(format!(
                        "WireGuard peer '{peer_node_name}' referenced by '{node_name}'.{wg_name} has no WireGuard interfaces"
                    ))
                })?;

                // Find the matching WG interface on the peer (first one that lists us as a peer)
                let peer_node = &topology.nodes[peer_node_name];
                for (peer_wg_name, peer_wg_config) in &peer_node.wireguard {
                    if !peer_wg_config.peers.contains(node_name) {
                        continue;
                    }
                    let peer_pubkey = peer_keys.get(peer_wg_name).ok_or_else(|| {
                        Error::deploy_failed(format!(
                            "missing public key for '{peer_node_name}'.{peer_wg_name}"
                        ))
                    })?;

                    let mut peer_builder = nlink::netlink::genl::wireguard::WgPeerBuilder::new(*peer_pubkey);

                    // Set endpoint if peer has a listen port and an address we can reach
                    if let Some(port) = peer_wg_config.listen_port {
                        // Try to find a reachable address for the peer from link addresses
                        if let Some(addr) = find_peer_endpoint(topology, peer_node_name) {
                            let endpoint = std::net::SocketAddr::new(addr, port);
                            peer_builder = peer_builder.endpoint(endpoint);
                        }
                    }

                    // Add allowed IPs from the peer's WireGuard addresses
                    for addr_str in &peer_wg_config.addresses {
                        if let Ok((ip, prefix)) = parse_cidr(addr_str) {
                            let allowed_ip = match ip {
                                IpAddr::V4(v4) => nlink::netlink::genl::wireguard::AllowedIp::v4(v4, prefix),
                                IpAddr::V6(v6) => nlink::netlink::genl::wireguard::AllowedIp::v6(v6, prefix),
                            };
                            peer_builder = peer_builder.allowed_ip(allowed_ip);
                        }
                    }

                    wg_conn
                        .set_device(wg_name.as_str(), |dev| dev.peer(peer_builder))
                        .await
                        .map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to add peer '{peer_node_name}' to WireGuard '{wg_name}' on '{node_name}': {e}"
                            ))
                        })?;
                }
            }
        }
    }

    // ── Step 11: Apply sysctls ─────────────────────────────────────
    for (node_name, node) in &topology.nodes {
        let sysctls = topology.effective_sysctls(node);
        if !sysctls.is_empty() {
            let node_handle = &node_handles[node_name];
            let entries: Vec<(&str, &str)> = sysctls
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            node_handle.set_sysctls(&entries).map_err(|e| {
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
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (dest, route_config) in &node.routes {
            add_route(&conn, node_name, dest, route_config).await?;
        }
    }

    // ── Step 12b: Add VRF routes ───────────────────────────────────
    for (node_name, node) in &topology.nodes {
        if node.vrfs.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{node_name}': {e}"))
        })?;

        for (vrf_name, vrf_config) in &node.vrfs {
            for (dest, route_config) in &vrf_config.routes {
                add_route_with_table(&conn, node_name, dest, route_config, vrf_config.table, vrf_name)
                    .await?;
            }
        }
    }

    // ── Step 13: Apply nftables firewall rules ──────────────────────
    for (node_name, node) in &topology.nodes {
        if let Some(fw) = topology.effective_firewall(node) {
            let node_handle = &node_handles[node_name];
            apply_firewall(node_handle, node_name, fw).await?;
        }
    }

    // ── Step 14: Apply netem impairments ───────────────────────────
    for (endpoint_str, impairment) in &topology.impairments {
        let ep = EndpointRef::parse(endpoint_str).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint_str.clone(),
        })?;
        let ep_handle = &node_handles[&ep.node];
        let conn: Connection<Route> = ep_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
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
        let ep_handle = &node_handles[&ep.node];
        let conn: Connection<Route> = ep_handle.connection().map_err(|e| {
            Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
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
        let node_handle = &node_handles[node_name];

        for (i, exec_config) in node.exec.iter().enumerate() {
            if exec_config.cmd.is_empty() {
                continue;
            }

            // For container nodes, use docker/podman exec so commands see the container FS
            if node.is_container() {
                if let Some(rt) = &container_runtime {
                    let container_id = node_handle.container_id().unwrap();
                    let cmd_strs: Vec<&str> = exec_config.cmd.iter().map(|s| s.as_str()).collect();
                    if exec_config.background {
                        // Use -d flag for background exec in container
                        let mut args = vec!["exec", "-d", container_id];
                        args.extend(&cmd_strs);
                        let output = std::process::Command::new(rt.binary())
                            .args(&args)
                            .output()
                            .map_err(|e| {
                                Error::deploy_failed(format!(
                                    "failed to exec in container '{node_name}' exec[{i}]: {e}"
                                ))
                            })?;
                        if !output.status.success() {
                            return Err(Error::deploy_failed(format!(
                                "exec[{i}] on container '{node_name}' failed: {}",
                                String::from_utf8_lossy(&output.stderr)
                            )));
                        }
                    } else {
                        let output = rt.exec(container_id, &cmd_strs).map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to exec in container '{node_name}' exec[{i}]: {e}"
                            ))
                        })?;
                        if !output.status.success() {
                            return Err(Error::deploy_failed(format!(
                                "exec[{i}] on container '{node_name}' failed (exit {}): {}",
                                output.status.code().unwrap_or(-1),
                                String::from_utf8_lossy(&output.stderr)
                            )));
                        }
                    }
                }
            } else {
                let mut cmd = std::process::Command::new(&exec_config.cmd[0]);
                cmd.args(&exec_config.cmd[1..]);

                if exec_config.background {
                    let child = node_handle.spawn(cmd).map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to spawn background process on '{node_name}' exec[{i}]: {e}"
                        ))
                    })?;
                    pids.push((node_name.clone(), child.id()));
                } else {
                    let output = node_handle.spawn_output(cmd).map_err(|e| {
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
    }

    // ── Step 18: Write state file ──────────────────────────────────
    // Encode WG public keys as base64 for state persistence
    let wg_public_keys_b64 = {
        use base64::Engine;
        let mut map = HashMap::new();
        for (node, keys) in &wg_public_keys {
            let mut node_map = HashMap::new();
            for (iface, pubkey) in keys {
                node_map.insert(
                    iface.clone(),
                    base64::engine::general_purpose::STANDARD.encode(pubkey),
                );
            }
            map.insert(node.clone(), node_map);
        }
        map
    };

    let lab_state = LabState {
        name: topology.lab.name.clone(),
        created_at: now_iso8601(),
        namespaces: namespace_names.clone(),
        pids: pids.clone(),
        wg_public_keys: wg_public_keys_b64,
        containers: container_states.clone(),
        runtime: container_runtime.as_ref().map(|rt| rt.binary().to_string()),
    };
    state::save(&lab_state, topology)?;

    // Disarm cleanup — deployment succeeded
    cleanup.disarm();

    Ok(RunningLab::new(
        topology.clone(),
        namespace_names,
        container_states,
        container_runtime.as_ref().map(|rt| rt.binary().to_string()),
        pids,
    ))
}

/// Apply nftables firewall rules for a node.
async fn apply_firewall(
    node_handle: &NodeHandle,
    node_name: &str,
    fw: &crate::types::FirewallConfig,
) -> Result<()> {
    use nlink::netlink::nftables::types::{Chain, ChainType, Family, Hook, Policy, Priority, Rule};
    use nlink::netlink::Nftables;

    // nftables needs Connection<Nftables> (NETLINK_NETFILTER socket)
    let nft_conn: Connection<Nftables> =
        node_handle.connection().map_err(|e| {
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
        if !match_expr.is_empty() {
            rule = apply_match_expr(rule, match_expr)?;
        }

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
) -> Result<nlink::netlink::nftables::types::Rule> {
    use nlink::netlink::nftables::types::CtState;

    let expr = expr.trim();

    if expr.starts_with("tcp dport ") {
        if let Ok(port) = expr.trim_start_matches("tcp dport ").trim().parse::<u16>() {
            return Ok(rule.match_tcp_dport(port));
        }
    }

    if expr.starts_with("udp dport ") {
        if let Ok(port) = expr.trim_start_matches("udp dport ").trim().parse::<u16>() {
            return Ok(rule.match_udp_dport(port));
        }
    }

    if expr.starts_with("ct state ") {
        let states = expr.trim_start_matches("ct state ").trim();
        let mut ct = CtState::empty();
        for state in states.split(',') {
            match state.trim() {
                "established" => ct = ct | CtState::ESTABLISHED,
                "related" => ct = ct | CtState::RELATED,
                "new" => ct = ct | CtState::NEW,
                "invalid" => ct = ct | CtState::INVALID,
                _ => {}
            }
        }
        return Ok(rule.match_ct_state(ct));
    }

    Err(Error::deploy_failed(format!(
        "unsupported firewall match expression: '{expr}'. \
         Supported: 'ct state ...', 'tcp dport N', 'udp dport N'"
    )))
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

/// Add a single route in a VRF routing table.
async fn add_route_with_table(
    conn: &Connection<Route>,
    node_name: &str,
    dest: &str,
    route_config: &crate::types::RouteConfig,
    table: u32,
    vrf_name: &str,
) -> Result<()> {
    let is_default = dest == "default";

    let gw: Option<IpAddr> = if let Some(via) = &route_config.via {
        Some(via.parse().map_err(|e| {
            Error::invalid_topology(format!(
                "invalid gateway '{via}' for VRF route '{dest}' on '{node_name}'.{vrf_name}: {e}"
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
                        "VRF route '{dest}' on '{node_name}': expected IPv6 address"
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
        route = route.table(table);
        conn.add_route(route).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to add VRF route '{dest}' in '{vrf_name}' on '{node_name}': {e}"
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
                        "VRF route '{dest}' on '{node_name}': expected IPv4 address"
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
        route = route.table(table);
        conn.add_route(route).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to add VRF route '{dest}' in '{vrf_name}' on '{node_name}': {e}"
            ))
        })?;
    }

    Ok(())
}

/// Generate a random WireGuard private key.
fn generate_wg_private_key() -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| {
        Error::deploy_failed(format!("failed to generate WireGuard key: {e}"))
    })?;
    // Clamp per Curve25519 convention
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
    Ok(key)
}

/// Derive a WireGuard public key from a private key.
fn derive_wg_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let secret = x25519_dalek::StaticSecret::from(*private_key);
    let public = x25519_dalek::PublicKey::from(&secret);
    public.to_bytes()
}

/// Decode a base64-encoded WireGuard key.
fn decode_wg_key(s: &str) -> std::result::Result<[u8; 32], String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64 decode: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Find a reachable IP address for a peer node (from link or interface addresses).
fn find_peer_endpoint(topology: &crate::types::Topology, peer_name: &str) -> Option<IpAddr> {
    // Check link addresses first
    for link in &topology.links {
        if let Some(addresses) = &link.addresses {
            for (i, ep_str) in link.endpoints.iter().enumerate() {
                if let Some(ep) = EndpointRef::parse(ep_str) {
                    if ep.node == peer_name {
                        if let Ok((ip, _)) = parse_cidr(&addresses[i]) {
                            return Some(ip);
                        }
                    }
                }
            }
        }
    }
    // Check explicit interface addresses
    if let Some(node) = topology.nodes.get(peer_name) {
        for iface_config in node.interfaces.values() {
            for addr_str in &iface_config.addresses {
                if let Ok((ip, _)) = parse_cidr(addr_str) {
                    return Some(ip);
                }
            }
        }
    }
    None
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

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Cleanup guard that removes namespaces on drop if deployment fails.
struct Cleanup {
    namespaces: Vec<String>,
    containers: Vec<String>,
    runtime_binary: Option<String>,
    armed: bool,
}

impl Cleanup {
    fn new() -> Self {
        Self {
            namespaces: Vec::new(),
            containers: Vec::new(),
            runtime_binary: None,
            armed: true,
        }
    }

    fn add_namespace(&mut self, name: String) {
        self.namespaces.push(name);
    }

    fn add_container(&mut self, id: String) {
        self.containers.push(id);
    }

    fn set_runtime(&mut self, binary: &str) {
        self.runtime_binary = Some(binary.to_string());
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
        if let Some(binary) = &self.runtime_binary {
            for id in &self.containers {
                let _ = std::process::Command::new(binary)
                    .args(["rm", "-f", id])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}
