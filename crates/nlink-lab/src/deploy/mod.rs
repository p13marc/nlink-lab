//! Lab deployment engine.
//!
//! Takes a validated [`Topology`] and creates the actual network lab using
//! nlink APIs. Follows the deployment sequence from the design document.

#[cfg(feature = "wireguard")]
use nlink::Wireguard;
use nlink::netlink::bridge_vlan::BridgeVlanBuilder;
use nlink::netlink::namespace;
use nlink::netlink::ratelimit::RateLimiter;
use nlink::{Connection, Route};
use std::collections::BTreeMap;
use std::net::IpAddr;

use nlink::netlink::namespace::NamespaceFd;

use crate::container::Runtime;
use crate::error::{Error, Result};
use crate::helpers::{parse_cidr, parse_rate_bps};
use crate::running::RunningLab;
use crate::state::{self, ContainerState, LabState};
use crate::types::{DnsMode, EndpointRef, Topology};

mod plan;
pub(crate) use plan::network::*;
pub(crate) use plan::nftables::*;
pub(crate) use plan::process::*;
pub(crate) use plan::qdisc::*;
pub(crate) use plan::wireguard::*;

/// Abstraction over bare namespace vs container node.
enum NodeHandle {
    Namespace {
        ns_name: String,
    },
    Container {
        id: String,
        pid: u32,
        ns_path: String,
    },
}

impl NodeHandle {
    fn connection<
        P: nlink::netlink::ProtocolState + Default + nlink::netlink::construction::SyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::connection_for(ns_name),
            NodeHandle::Container { pid, .. } => namespace::connection_for_pid(*pid),
        }
    }

    #[cfg(feature = "wireguard")]
    async fn wireguard_connection(
        &self,
    ) -> std::result::Result<Connection<Wireguard>, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::connection_for_async(ns_name).await,
            NodeHandle::Container { pid, .. } => namespace::connection_for_pid_async(*pid).await,
        }
    }

    fn open_ns_fd(&self) -> std::result::Result<NamespaceFd, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::open(ns_name),
            NodeHandle::Container { ns_path, .. } => namespace::open_path(ns_path),
        }
    }

    fn set_sysctls(
        &self,
        entries: &[(&str, &str)],
    ) -> std::result::Result<(), nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::set_sysctls(ns_name, entries),
            NodeHandle::Container { ns_path, .. } => namespace::set_sysctls_path(ns_path, entries),
        }
    }

    fn spawn_output(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Output, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::spawn_output_with_etc(ns_name, cmd),
            NodeHandle::Container { ns_path, .. } => namespace::spawn_output_path(ns_path, cmd),
        }
    }

    fn spawn(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Child, nlink::netlink::Error> {
        match self {
            NodeHandle::Namespace { ns_name } => namespace::spawn_with_etc(ns_name, cmd),
            NodeHandle::Container { ns_path, .. } => namespace::spawn_path(ns_path, cmd),
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

    // Acquire exclusive lock
    let _lock = state::lock(&topology.lab.name)?;

    // Check if lab already exists
    if state::exists(&topology.lab.name) {
        return Err(Error::AlreadyExists {
            name: topology.lab.name.clone(),
        });
    }

    // Resolve `auto/N` subnet placeholders against the host-wide pool
    // before any kernel state is created. The pool acquires its own
    // flock; allocations are recorded against the lab name so destroy
    // can free them. (Round-5 §2.5.) Clone the topology since the
    // `&Topology` we received is borrowed; substitution mutates.
    let mut owned_topology = topology.clone();
    let lab_name = owned_topology.lab.name.clone();
    let allocated_subnets =
        crate::subnet_pool::substitute_auto_subnets(&mut owned_topology, |prefix| {
            crate::subnet_pool::allocate(&lab_name, prefix)
        })?;
    let topology = &owned_topology;
    let mut cleanup = Cleanup::new(lab_name.clone());
    if !allocated_subnets.is_empty() {
        cleanup.set_subnet_pool_lab(lab_name.clone());
    }

    // Every kernel/host mutation happens in `deploy_inner`; on any error
    // the journal of what was created so far is unwound (async, so
    // root-namespace links can be deleted through netlink) before the
    // error propagates. `Cleanup`'s `Drop` remains as a synchronous
    // last resort for panics.
    match deploy_inner(topology, &mut cleanup).await {
        Ok(running) => Ok(running),
        Err(e) => {
            tracing::warn!("deploy of '{lab_name}' failed: {e}; rolling back");
            cleanup.rollback().await;
            Err(e)
        }
    }
}

async fn deploy_inner(topology: &Topology, cleanup: &mut Cleanup) -> Result<RunningLab> {
    let mut node_handles: BTreeMap<String, NodeHandle> = BTreeMap::new();
    let mut namespace_names: BTreeMap<String, String> = BTreeMap::new();
    let mut container_states: BTreeMap<String, ContainerState> = BTreeMap::new();
    let mut pids: Vec<(String, u32)> = Vec::new();
    let mut starttimes: BTreeMap<u32, u64> = BTreeMap::new();
    let mut mgmt_peers: std::collections::BTreeMap<String, String> = Default::default();
    let mut process_logs: BTreeMap<u32, (String, String)> = BTreeMap::new();

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

    // Pre-compute DNS hosts entries for container --add-host flags.
    // IPs are known at parse time from link addresses, so this is safe before step 3.
    let dns_extra_hosts: Vec<String> = if topology.lab.dns == DnsMode::Hosts {
        crate::dns::generate_hosts_entries(topology)
            .iter()
            .flat_map(|entry| {
                entry
                    .names
                    .iter()
                    .map(|name| format!("{name}:{}", entry.ip))
            })
            .collect()
    } else {
        Vec::new()
    };

    // ── Step 3: Create namespaces / containers ─────────────────────
    tracing::info!("step 3/18: creating namespaces");
    for (node_name, node) in &topology.nodes {
        if let Some(image) = &node.image {
            // Container node
            let rt = container_runtime.as_ref().unwrap();
            // Pull policy: "always" forces pull, "never" skips, "missing" (default) pulls if needed
            match node.pull.as_deref() {
                Some("never") => {}
                Some("always") => {
                    rt.pull_image(image)?;
                }
                _ => {
                    rt.ensure_image(image)?;
                }
            }
            let container_name = format!("{}-{}", topology.lab.prefix(), node_name);
            let opts = build_create_opts(node, &dns_extra_hosts);
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
            guard_namespace_absent(&ns_name)?;
            namespace::create(&ns_name).map_err(|e| Error::Namespace {
                op: "create",
                ns: ns_name.clone(),
                source: e,
            })?;
            cleanup.add_namespace(ns_name.clone());
            // Ownership tag: the only thing `destroy --orphans` trusts.
            crate::netns_tag::tag(&ns_name, &topology.lab.name)?;
            namespace_names.insert(node_name.clone(), ns_name.clone());
            node_handles.insert(node_name.clone(), NodeHandle::Namespace { ns_name });
        }
    }

    // ── Step 3b: Load mac80211_hwsim and move PHYs ──────────────────
    let wifi_radio_count = crate::wifi::count_wifi_nodes(topology);
    let mut wifi_loaded = false;
    if wifi_radio_count > 0 {
        tracing::info!("step 3b: loading mac80211_hwsim with {wifi_radio_count} radios");
        let _ = crate::wifi::load_hwsim_for(&topology.lab.name, wifi_radio_count)?;
        wifi_loaded = true;
        cleanup.wifi_loaded = true;

        // Use nlink's nl80211 to enumerate PHYs and move them to namespaces
        use nlink::netlink::Nl80211;
        let nl_conn = nlink::Connection::<Nl80211>::new_async()
            .await
            .map_err(|e| Error::deploy_failed(format!("nl80211 connection: {e}")))?;

        let phys = nl_conn
            .get_phys()
            .await
            .map_err(|e| Error::deploy_failed(format!("failed to list PHYs: {e}")))?;

        // Collect WiFi nodes in deterministic order, map each to a PHY
        let mut wifi_nodes: Vec<(&str, &crate::types::WifiConfig)> = Vec::new();
        for (node_name, node) in &topology.nodes {
            for wifi in &node.wifi {
                wifi_nodes.push((node_name, wifi));
            }
        }

        if phys.len() < wifi_nodes.len() {
            return Err(Error::deploy_failed(format!(
                "expected {} hwsim PHYs but found {}",
                wifi_nodes.len(),
                phys.len()
            )));
        }

        tracing::info!("step 3c: moving PHYs to namespaces");
        for (i, (node_name, _wifi)) in wifi_nodes.iter().enumerate() {
            let phy = &phys[i];
            let node_handle = &node_handles[*node_name];
            let ns_fd = node_handle
                .open_ns_fd()
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node_name}': {e}")))?;

            nl_conn
                .set_wiphy_netns(phy.index, ns_fd.as_raw_fd())
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to move phy{} to namespace '{node_name}': {e}",
                        phy.index
                    ))
                })?;
        }
    }

    // ── Step 3d: Create host-reachable management bridge ──────────────
    if topology.lab.mgmt_host_reachable
        && let Some(ref mgmt_subnet) = topology.lab.mgmt_subnet
    {
        tracing::info!("step 3d: creating host-reachable management bridge");
        let (base_ip, prefix) = parse_cidr(mgmt_subnet)?;
        let std::net::IpAddr::V4(base_v4) = base_ip else {
            return Err(Error::deploy_failed("mgmt subnet must be IPv4"));
        };
        let base_u32 = u32::from(base_v4);
        // The bridge takes .1 and every node .2, .3, … — make sure the
        // subnet actually holds them instead of silently spilling into
        // the next subnet (issue #40).
        let usable_hosts = (1u64 << (32 - prefix.min(32) as u32)).saturating_sub(2);
        if node_handles.len() as u64 + 1 > usable_hosts {
            return Err(Error::deploy_failed(format!(
                "mgmt subnet {mgmt_subnet} has {usable_hosts} usable host address(es) but the bridge plus {} nodes need {}",
                node_handles.len(),
                node_handles.len() + 1
            )));
        }

        let bridge_name = topology.lab.mgmt_bridge_name();

        // Create bridge in root namespace
        let root_conn: Connection<Route> = Connection::<Route>::new()
            .map_err(|e| Error::deploy_failed(format!("root connection: {e}")))?;

        let bridge = nlink::netlink::link::BridgeLink::new(&bridge_name);
        root_conn.add_link(bridge).await.map_err(|e| {
            Error::deploy_failed(format!("failed to create mgmt bridge '{bridge_name}': {e}"))
        })?;
        cleanup.add_host_link(bridge_name.clone());
        root_conn.set_link_up(&bridge_name).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to bring up mgmt bridge '{bridge_name}': {e}"
            ))
        })?;

        // Assign .1 to the bridge
        let bridge_ip = std::net::IpAddr::V4(std::net::Ipv4Addr::from(base_u32 + 1));
        root_conn
            .add_address_by_name(&bridge_name, bridge_ip, prefix)
            .await
            .map_err(|e| {
                Error::deploy_failed(format!("failed to assign IP to mgmt bridge: {e}"))
            })?;

        // For each node (sorted by name for deterministic IP assignment), create veth pair
        let mut sorted_nodes: Vec<&str> = node_handles.keys().map(|s| s.as_str()).collect();
        sorted_nodes.sort();

        for (idx, node_name) in sorted_nodes.iter().enumerate() {
            let node_handle = &node_handles[*node_name];
            let node_ns_fd = node_handle
                .open_ns_fd()
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node_name}': {e}")))?;

            let mgmt_iface = "mgmt0";
            let peer_name = topology.lab.mgmt_peer_name(idx);

            // Create veth pair in root ns, peer goes to node ns
            let veth = nlink::netlink::link::VethLink::new(&peer_name, mgmt_iface)
                .peer_netns_fd(node_ns_fd.as_raw_fd());

            root_conn.add_link(veth).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create mgmt veth for node '{node_name}': {e}"
                ))
            })?;
            cleanup.add_host_link(peer_name.clone());
            mgmt_peers.insert(node_name.to_string(), peer_name.clone());

            // Attach our end (peer_name) to the bridge
            root_conn
                .set_link_master(&peer_name, &bridge_name)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to attach '{peer_name}' to mgmt bridge: {e}"
                    ))
                })?;
            root_conn.set_link_up(&peer_name).await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up '{peer_name}': {e}"))
            })?;

            // Assign IP to mgmt0 in node ns: .2, .3, .4, ...
            let node_ip = std::net::IpAddr::V4(std::net::Ipv4Addr::from(base_u32 + 2 + idx as u32));
            let node_conn: Connection<Route> = node_handle
                .connection()
                .map_err(|e| Error::deploy_failed(format!("connection for '{node_name}': {e}")))?;
            node_conn
                .add_address_by_name(mgmt_iface, node_ip, prefix)
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!("failed to assign mgmt IP to '{node_name}': {e}"))
                })?;
            node_conn.set_link_up(mgmt_iface).await.map_err(|e| {
                Error::deploy_failed(format!("failed to bring up mgmt0 on '{node_name}': {e}"))
            })?;
        }
    }

    // ── Step 4: Create bridge networks ───────────────────────────────
    // Bridges live in a management namespace. For each network, create the bridge
    // in a dedicated namespace, then create veth pairs from member nodes.
    let mut bridge_ns_names: BTreeMap<String, String> = BTreeMap::new();
    if !topology.networks.is_empty() {
        let mgmt_ns = format!("{}-mgmt", topology.lab.prefix());
        namespace::create(&mgmt_ns).map_err(|e| Error::Namespace {
            op: "create",
            ns: mgmt_ns.clone(),
            source: e,
        })?;
        cleanup.add_namespace(mgmt_ns.clone());
        crate::netns_tag::tag(&mgmt_ns, &topology.lab.name)?;

        let mgmt_conn: Connection<Route> = namespace::connection_for(&mgmt_ns)
            .map_err(|e| Error::deploy_failed(format!("connection for '{mgmt_ns}': {e}")))?;

        for (net_name, network) in &topology.networks {
            // Hash-based bridge name: `nb{hash8}` (10 chars). Always
            // fits the 15-char Linux IFNAMSIZ budget, never collides
            // for distinct net_names (DJB2 hash collisions are
            // statistically negligible at the few-networks-per-lab
            // scale we care about). See network_bridge_name_for() for
            // the full rationale and the regression-test reference.
            let bridge_name = crate::types::network_bridge_name_for(net_name);

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
                Error::deploy_failed(format!("failed to bring up bridge '{bridge_name}': {e}"))
            })?;

            bridge_ns_names.insert(net_name.clone(), mgmt_ns.clone());

            // Create veth pairs for each member: one end in node ns, other in mgmt ns attached to bridge
            let mgmt_ns_fd = namespace::open(&mgmt_ns)
                .map_err(|e| Error::deploy_failed(format!("failed to open mgmt namespace: {e}")))?;

            for (k, member) in network.members.iter().enumerate() {
                let ep = EndpointRef::parse(member).ok_or_else(|| Error::InvalidEndpoint {
                    endpoint: member.clone(),
                })?;
                let node_handle =
                    node_handles
                        .get(&ep.node)
                        .ok_or_else(|| Error::NodeNotFound {
                            name: ep.node.clone(),
                        })?;

                // The peer end in mgmt ns gets a generated name.
                // Uses a hash of `net_name` so networks sharing a prefix
                // (e.g. `lan_a`/`lan_b`) don't collide in the mgmt ns.
                let peer_name = crate::types::network_peer_name_for(net_name, k);

                let node_conn: Connection<Route> = node_handle.connection().map_err(|e| {
                    Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
                })?;

                let veth = nlink::netlink::link::VethLink::new(&ep.iface, &peer_name)
                    .peer_netns_fd(mgmt_ns_fd.as_raw_fd());

                node_conn.add_link(veth).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create veth for network '{net_name}' member '{member}' \
                         (node iface '{node_iface}' in ns '{node_ns}', mgmt peer '{peer_name}'): {e}",
                        node_iface = ep.iface,
                        node_ns = ep.node,
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
                mgmt_conn.set_link_up(&peer_name).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to bring up bridge port '{peer_name}': {e}"
                    ))
                })?;

                // Apply VLAN configuration for this port if defined. Port
                // keys are `node:iface` since the port-key normalisation;
                // fall back to the bare node for topologies persisted by
                // older releases.
                if let Some(port_config) = network
                    .ports
                    .get(&format!("{}:{}", ep.node, ep.iface))
                    .or_else(|| network.ports.get(&ep.node))
                {
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
                    if let Some(pvid) = port_config.pvid
                        && !port_config.vlans.contains(&pvid)
                    {
                        let vlan = BridgeVlanBuilder::new(pvid)
                            .dev(&peer_name)
                            .pvid()
                            .untagged();
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

    // ── Step 5: Create veth pairs ──────────────────────────────────
    tracing::info!("step 5/18: creating veth pairs");
    for (i, link) in topology.links.iter().enumerate() {
        let ep_a =
            EndpointRef::parse(&link.endpoints[0]).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: link.endpoints[0].clone(),
            })?;
        let ep_b =
            EndpointRef::parse(&link.endpoints[1]).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: link.endpoints[1].clone(),
            })?;

        let handle_a = node_handles
            .get(&ep_a.node)
            .ok_or_else(|| Error::NodeNotFound {
                name: ep_a.node.clone(),
            })?;
        let handle_b = node_handles
            .get(&ep_b.node)
            .ok_or_else(|| Error::NodeNotFound {
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
        let mut veth = nlink::netlink::link::VethLink::new(&ep_a.iface, &ep_b.iface)
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
    //
    // After Plan 159a Slice 4, every `InterfaceKind` (Dummy,
    // Bond, Vlan, Vxlan) creates declaratively in step 11c:
    // - 158e Slice 2 — Dummy + Bond (+ member enslave that was 10b)
    // - 158e Slice 3 — Vlan sub-interfaces
    // - 159a Slice 4 — Vxlan (incl. `vxlan_local` / `_remote` / `_port`)
    // Loopback exists already; addresses for every kind get
    // handled by step 11c's address-application pass. This step
    // is now an empty marker for the step-numbering audit trail.

    // ── Step 6a: Create macvlan/ipvlan interfaces ───────────────────
    // These are created on the host and moved into namespaces because the
    // parent interface (e.g., enp3s0) lives on the host, not inside the NS.
    {
        let host_conn: Connection<Route> = Connection::<Route>::new()
            .map_err(|e| Error::deploy_failed(format!("host connection: {e}")))?;

        for (node_name, node) in &topology.nodes {
            let node_handle = &node_handles[node_name];
            let ns_fd = node_handle
                .open_ns_fd()
                .map_err(|e| Error::deploy_failed(format!("open ns fd for '{node_name}': {e}")))?;

            for mv in &node.macvlans {
                use nlink::netlink::link::{MacvlanLink, MacvlanMode as NlinkMacvlanMode};
                let mode = match mv.mode {
                    crate::types::MacvlanMode::Bridge => NlinkMacvlanMode::Bridge,
                    crate::types::MacvlanMode::Private => NlinkMacvlanMode::Private,
                    crate::types::MacvlanMode::Vepa => NlinkMacvlanMode::Vepa,
                    crate::types::MacvlanMode::Passthru => NlinkMacvlanMode::Passthru,
                };
                let macvlan = MacvlanLink::new(&mv.name, &mv.parent).mode(mode);
                host_conn.add_link(macvlan).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create macvlan '{}' on node '{node_name}': {e}",
                        mv.name
                    ))
                })?;
                // Still on the host until the move below succeeds; a
                // rollback deletes it from the host if it is still there.
                cleanup.add_host_link(mv.name.clone());
                host_conn
                    .set_link_netns_fd(&mv.name, ns_fd.as_raw_fd())
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to move macvlan '{}' to namespace '{node_name}': {e}",
                            mv.name
                        ))
                    })?;
            }

            for iv in &node.ipvlans {
                use nlink::netlink::link::{IpvlanLink, IpvlanMode as NlinkIpvlanMode};
                let mode = match iv.mode {
                    crate::types::IpvlanMode::L2 => NlinkIpvlanMode::L2,
                    crate::types::IpvlanMode::L3 => NlinkIpvlanMode::L3,
                    crate::types::IpvlanMode::L3S => NlinkIpvlanMode::L3S,
                };
                let ipvlan = IpvlanLink::new(&iv.name, &iv.parent).mode(mode);
                host_conn.add_link(ipvlan).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to create ipvlan '{}' on node '{node_name}': {e}",
                        iv.name
                    ))
                })?;
                cleanup.add_host_link(iv.name.clone());
                host_conn
                    .set_link_netns_fd(&iv.name, ns_fd.as_raw_fd())
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to move ipvlan '{}' to namespace '{node_name}': {e}",
                            iv.name
                        ))
                    })?;
            }
        }
    }

    // ── Step 6b: Create VRF interfaces ─────────────────────────────
    //
    // Plan 159a Slice 4 — VRF creation + bring-up absorbed into
    // the declarative NetworkConfig path (step 11c). Uses
    // `LinkBuilder::vrf(table)` from nlink 0.19 (upstream Plan
    // 190 §2.3). Empty marker kept for the step-numbering audit
    // trail.

    // ── Step 6c: Create WireGuard interfaces ─────────────────────
    //
    // Plan 160 (nlink 0.25) — the imperative
    // `add_link(WireguardLink::new(...))` loop is gone. The WG link
    // is now bootstrapped idempotently inside the declarative WG
    // apply (step 10d) via `WireguardConfig::ensure_devices`
    // (nlink 0.24, #169) — no separate pre-create pass. Empty
    // marker kept for the step-numbering audit trail.

    // ── Step 9: Set interface addresses ────────────────────────────
    //
    // Plan 158e Slice 1 moves the per-link / per-interface / network
    // port / WireGuard / macvlan / ipvlan / WiFi address application
    // into a single per-namespace `NetworkConfig::diff().apply()`
    // call (step 11c below). This step is now a no-op marker kept
    // for the step-numbering audit trail.

    // ── Step 10: Bring interfaces up ───────────────────────────────
    // Only the interfaces nlink-lab created imperatively (veth ends,
    // bridge-network members, mgmt0, macvlan/ipvlan, wifi) plus `lo`.
    // Everything declared in step 11c carries its own `.up()`. Bringing
    // up *every* link by ifindex used to fail on hosts whose kernel
    // populates each namespace with gre0/gretap0/sit0/tunl0 —
    // `gretap0` answers EADDRNOTAVAIL — and took the deploy down with it.
    tracing::info!("step 10/18: bringing interfaces up");
    for (node_name, node) in &topology.nodes {
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{node_name}': {e}")))?;
        let mut ifaces: Vec<String> = vec!["lo".to_string()];
        for link in &topology.links {
            for ep_str in &link.endpoints {
                if let Some(ep) = EndpointRef::parse(ep_str)
                    && &ep.node == node_name
                {
                    ifaces.push(ep.iface);
                }
            }
        }
        for network in topology.networks.values() {
            for member in &network.members {
                if let Some(ep) = EndpointRef::parse(member)
                    && &ep.node == node_name
                {
                    ifaces.push(ep.iface);
                }
            }
        }
        if topology.lab.mgmt_host_reachable && topology.lab.mgmt_subnet.is_some() {
            ifaces.push("mgmt0".to_string());
        }
        ifaces.extend(node.macvlans.iter().map(|m| m.name.clone()));
        ifaces.extend(node.ipvlans.iter().map(|i| i.name.clone()));
        ifaces.extend(node.wifi.iter().map(|w| w.name.clone()));
        ifaces.sort();
        ifaces.dedup();
        for iface in &ifaces {
            conn.set_link_up(iface.as_str()).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up interface '{iface}' in '{node_name}': {e}"
                ))
            })?;
        }
    }

    // ── Step 10b: Enslave bond members ─────────────────────────────
    //
    // Plan 158e Slice 2 — absorbed into the declarative
    // NetworkConfig path in step 11c (`.link(member, |b|
    // b.master(bond))`). Empty marker kept for the step-numbering
    // audit trail; the imperative body is gone.

    // ── Step 10c: Enslave interfaces to VRFs ─────────────────────
    //
    // Plan 159a Slice 4 — VRF enslave absorbed into the declarative
    // NetworkConfig path (step 11c, `LinkBuilder::master(vrf)`).
    // Empty marker kept for the step-numbering audit trail.

    // ── Step 10d: Configure WireGuard devices (declarative) ───────
    //
    // Plan 159a Phase 2 — replace the two-pass imperative
    // `wg_conn.set_device(...)` loops with a per-node
    // `WireguardConfig::apply_reconcile()` call (upstream Plan 196).
    // Key generation (sync, no kernel touch) still happens in a
    // pre-pass so peer cross-references resolve. Plan 160 (nlink
    // 0.25) — the WG link itself is now bootstrapped here too, via
    // `WireguardConfig::ensure_devices` (nlink 0.24, #169), which
    // creates any missing declared WG interface idempotently
    // through a same-namespace Route connection before the GENL
    // apply. This retired the imperative step-6c pre-create loop.

    #[cfg(not(feature = "wireguard"))]
    {
        let has_wg = topology.nodes.values().any(|n| !n.wireguard.is_empty());
        if has_wg {
            return Err(Error::deploy_failed(
                "topology uses WireGuard but the 'wireguard' feature is not enabled. \
                 Rebuild with: cargo build --features wireguard",
            ));
        }
    }

    #[cfg(feature = "wireguard")]
    let wg_public_keys = build_wg_public_key_map(topology)?;

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

    // ── Step 11b: Auto-generate routes from topology graph ──────────
    let auto_routes = if topology.lab.routing == crate::types::RoutingMode::Auto {
        tracing::info!("step 11b: auto-generating routes from topology");
        auto_generate_routes(topology)
    } else {
        BTreeMap::new()
    };

    // ── Step 11c + 10d + 13: Per-node Stack-pattern apply ──────────
    //
    // Plan 159c — collapse the three previously-separate per-node
    // loops (network/step 11c, WireGuard/step 10d, nftables/step 13)
    // into one orchestrated pass via `apply_stack_for_node`. Each
    // node sees: build configs → apply network → apply nftables →
    // apply WireGuard, with one aggregated `tracing::info!` per
    // node. Mirrors upstream `facade::Stack` shape but routes
    // through `NodeHandle::connection<P>()` so container namespaces
    // (`connection_for_pid`) work alongside bare namespaces
    // (`connection_for(name)`) — upstream `Stack::apply_in_namespace`
    // only accepts a name.
    tracing::info!("step 11c+10d+13: applying network + nftables + WireGuard per node");
    for (node_name, node) in &topology.nodes {
        let node_handle = &node_handles[node_name];
        let net =
            topology_to_network_config(node_name, node, topology, auto_routes.get(node_name))?;
        let fw = topology.effective_firewall(node);
        let nat = node.nat.as_ref();
        #[cfg(feature = "wireguard")]
        let wg = if node.wireguard.is_empty() {
            None
        } else {
            Some(topology_to_wireguard_config(
                node_name,
                node,
                topology,
                &wg_public_keys,
            )?)
        };
        #[cfg(not(feature = "wireguard"))]
        let wg: Option<()> = None;
        apply_stack_for_node(node_handle, node_name, net, fw, nat, wg).await?;
    }

    // ── Step 12b: Add VRF routes ───────────────────────────────────
    for (node_name, node) in &topology.nodes {
        if node.vrfs.is_empty() {
            continue;
        }
        let node_handle = &node_handles[node_name];
        let conn: Connection<Route> = node_handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{node_name}': {e}")))?;

        for (vrf_name, vrf_config) in &node.vrfs {
            for (dest, route_config) in &vrf_config.routes {
                add_route_with_table(
                    &conn,
                    node_name,
                    dest,
                    route_config,
                    vrf_config.table,
                    vrf_name,
                )
                .await?;
            }
        }
    }

    // ── Step 14: Apply netem impairments ───────────────────────────
    tracing::info!("step 14/18: applying impairments");
    for (endpoint_str, impairment) in &topology.impairments {
        let ep = EndpointRef::parse(endpoint_str).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint_str.clone(),
        })?;
        let ep_handle = &node_handles[&ep.node];
        let conn: Connection<Route> = ep_handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;

        let netem = build_netem(impairment)?;
        conn.add_qdisc(&ep.iface, netem).await.map_err(|e| {
            Error::deploy_failed(format!("failed to apply netem on '{endpoint_str}': {e}"))
        })?;
    }

    // ── Step 14b: Apply per-pair network impairments ───────────────
    apply_network_impairments(topology, &node_handles).await?;

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
        let conn: Connection<Route> = ep_handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;

        let mut limiter = RateLimiter::new(&ep.iface);
        if let Some(egress) = &rate_limit.egress {
            let bits = parse_rate_bps(egress).map_err(|e| {
                Error::deploy_failed(format!("bad egress rate on '{endpoint_str}': {e}"))
            })?;
            limiter = limiter.egress(nlink::util::Rate::bits_per_sec(bits));
        }
        if let Some(ingress) = &rate_limit.ingress {
            let bits = parse_rate_bps(ingress).map_err(|e| {
                Error::deploy_failed(format!("bad ingress rate on '{endpoint_str}': {e}"))
            })?;
            limiter = limiter.ingress(nlink::util::Rate::bits_per_sec(bits));
        }
        // Plan 160 (nlink 0.24) — `reconcile` converges idempotently
        // instead of `apply`'s delete-root-qdisc-then-rebuild, so an
        // unchanged rate limit makes zero kernel calls on re-deploy
        // and there's no packet-drop window.
        let report = limiter.reconcile(&conn).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to reconcile rate limit on '{endpoint_str}': {e}"
            ))
        })?;
        tracing::debug!(
            endpoint = %endpoint_str,
            changes = report.changes_made,
            "rate limit reconcile complete"
        );
    }

    // ── Step 15b: Inject DNS hosts entries ──────────────────────────
    let mut dns_injected = false;
    if topology.lab.dns == DnsMode::Hosts {
        tracing::info!("step 15b: injecting /etc/hosts entries");
        let entries = crate::dns::generate_hosts_entries(topology);
        if !entries.is_empty() {
            crate::dns::inject_hosts(&topology.lab.name, &entries)?;
            dns_injected = true;
            cleanup.set_dns_lab(topology.lab.name.clone());

            // ── Step 15c: Create per-namespace /etc/netns/ files ──────
            tracing::info!("step 15c: creating per-namespace DNS files");
            for (node_name, node) in &topology.nodes {
                if node.image.is_some() {
                    continue; // containers use --add-host
                }
                let ns_name = &namespace_names[node_name];
                crate::dns::create_netns_etc(ns_name, &entries)?;
            }
        }
    }

    // ── Step 16: Spawn background processes (dependency-ordered) ───
    tracing::info!("step 16/18: spawning background processes");

    // Topologically sort nodes by depends_on for ordered startup
    let spawn_order = topo_sort_nodes(&topology.nodes);

    for node_name in &spawn_order {
        let node = &topology.nodes[node_name.as_str()];
        let node_handle = &node_handles[node_name];

        // Apply startup_delay before spawning. A malformed value is an
        // error, not a silently skipped delay (issue #40).
        if let Some(ref delay_str) = node.startup_delay {
            let delay = crate::helpers::parse_duration(delay_str).map_err(|e| {
                Error::deploy_failed(format!(
                    "node '{node_name}': invalid startup-delay '{delay_str}': {e}"
                ))
            })?;
            tracing::debug!("startup-delay {delay_str} for node '{node_name}'");
            tokio::time::sleep(delay).await;
        }

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
                    // Capture stdout/stderr to log files
                    let log_dir = state::logs_dir(&topology.lab.name);
                    std::fs::create_dir_all(&log_dir)?;
                    // For shell-wrapped commands (sh -c "actual cmd"), extract
                    // the actual command name for readable log filenames.
                    let cmd_basename = if exec_config.cmd.len() >= 3
                        && (exec_config.cmd[0] == "sh" || exec_config.cmd[0] == "/bin/sh")
                        && exec_config.cmd[1] == "-c"
                    {
                        exec_config.cmd[2]
                            .split_whitespace()
                            .next()
                            .and_then(|s| std::path::Path::new(s).file_name()?.to_str())
                            .unwrap_or("cmd")
                    } else {
                        std::path::Path::new(&exec_config.cmd[0])
                            .file_name()
                            .and_then(|f| f.to_str())
                            .unwrap_or("cmd")
                    };
                    let stdout_path =
                        log_dir.join(format!("{node_name}-{cmd_basename}-{i}.stdout"));
                    let stderr_path =
                        log_dir.join(format!("{node_name}-{cmd_basename}-{i}.stderr"));
                    let stdout_file = std::fs::File::create(&stdout_path)?;
                    let stderr_file = std::fs::File::create(&stderr_path)?;
                    cmd.stdout(stdout_file);
                    cmd.stderr(stderr_file);

                    let child = node_handle.spawn(cmd).map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to spawn background process on '{node_name}' exec[{i}]: {e}"
                        ))
                    })?;
                    let pid = child.id();
                    pids.push((node_name.clone(), pid));
                    let started = crate::running::host_starttime(pid);
                    if let Some(st) = started {
                        starttimes.insert(pid, st);
                    }
                    cleanup.add_pid(pid, started);
                    cleanup.set_logs_dir(log_dir.clone());

                    // Rename log files to include actual PID
                    let final_stdout =
                        log_dir.join(format!("{node_name}-{cmd_basename}-{pid}.stdout"));
                    let final_stderr =
                        log_dir.join(format!("{node_name}-{cmd_basename}-{pid}.stderr"));
                    let _ = std::fs::rename(&stdout_path, &final_stdout);
                    let _ = std::fs::rename(&stderr_path, &final_stderr);
                    process_logs.insert(
                        pid,
                        (
                            final_stdout.to_string_lossy().to_string(),
                            final_stderr.to_string_lossy().to_string(),
                        ),
                    );
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

        // Poll healthcheck until healthy (or timeout)
        if let Some(ref hc_cmd) = node.healthcheck {
            let parse_dur =
                |what: &str, v: Option<&str>, default: u64| -> Result<std::time::Duration> {
                    match v {
                        None => Ok(std::time::Duration::from_secs(default)),
                        Some(s) => crate::helpers::parse_duration(s).map_err(|e| {
                            Error::deploy_failed(format!(
                                "node '{node_name}': invalid {what} '{s}': {e}"
                            ))
                        }),
                    }
                };
            let hc_interval = parse_dur(
                "healthcheck-interval",
                node.healthcheck_interval.as_deref(),
                1,
            )?;
            let hc_timeout = parse_dur(
                "healthcheck-timeout",
                node.healthcheck_timeout.as_deref(),
                30,
            )?;

            tracing::info!("waiting for healthcheck on '{node_name}': {hc_cmd}");
            let deadline = std::time::Instant::now() + hc_timeout;
            loop {
                let mut probe = std::process::Command::new("sh");
                probe.args(["-c", hc_cmd]);
                let result = node_handle.spawn_output(probe);
                if result.is_ok_and(|o| o.status.success()) {
                    tracing::info!("healthcheck passed for '{node_name}'");
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(Error::deploy_failed(format!(
                        "healthcheck timeout for node '{node_name}': {hc_cmd}"
                    )));
                }
                tokio::time::sleep(hc_interval).await;
            }
        }
    }

    // ── Step 16b: Start WiFi daemons ────────────────────────────────
    if wifi_radio_count > 0 {
        tracing::info!("step 16b: starting WiFi daemons");
        for (node_name, node) in &topology.nodes {
            let node_handle = &node_handles[node_name];
            for wifi in &node.wifi {
                match wifi.mode {
                    crate::types::WifiMode::Ap => {
                        let conf_content = crate::wifi::generate_hostapd_conf(wifi);
                        let conf_path = crate::wifi::write_config(
                            &topology.lab.name,
                            node_name,
                            "hostapd.conf",
                            &conf_content,
                        )?;
                        let mut cmd = std::process::Command::new("hostapd");
                        cmd.args(["-B", &conf_path]);
                        node_handle.spawn(cmd).map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to start hostapd on '{node_name}': {e}"
                            ))
                        })?;
                    }
                    crate::types::WifiMode::Station => {
                        let conf_content = crate::wifi::generate_wpa_conf(wifi);
                        let conf_path = crate::wifi::write_config(
                            &topology.lab.name,
                            node_name,
                            "wpa.conf",
                            &conf_content,
                        )?;
                        let mut cmd = std::process::Command::new("wpa_supplicant");
                        cmd.args(["-B", "-i", &wifi.name, "-c", &conf_path]);
                        node_handle.spawn(cmd).map_err(|e| {
                            Error::deploy_failed(format!(
                                "failed to start wpa_supplicant on '{node_name}': {e}"
                            ))
                        })?;
                    }
                    crate::types::WifiMode::Mesh => {
                        // 802.11s mesh: use iw to join mesh
                        if let Some(mesh_id) = &wifi.mesh_id {
                            let mut cmd = std::process::Command::new("iw");
                            cmd.args([
                                "dev",
                                &wifi.name,
                                "mesh",
                                "join",
                                mesh_id,
                                "freq",
                                &freq_from_channel(wifi.channel.unwrap_or(1)),
                            ]);
                            let output = node_handle.spawn_output(cmd).map_err(|e| {
                                Error::deploy_failed(format!(
                                    "failed to join mesh '{mesh_id}' on '{node_name}': {e}"
                                ))
                            })?;
                            if !output.status.success() {
                                tracing::warn!(
                                    "mesh join failed on '{node_name}': {}",
                                    String::from_utf8_lossy(&output.stderr)
                                );
                            }
                        }
                    }
                }
            }
        }

        // Brief pause for WiFi association
        tracing::info!("waiting for WiFi association...");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // ── Step 18: Write state file ──────────────────────────────────
    tracing::info!("step 18/18: writing state file");
    // Encode WG public keys as base64 for state persistence.
    // Plan 159a Phase 2 — `wg_public_keys` now stores
    // `(private_key, public_key)` tuples per WG iface; the state
    // file only persists the public half.
    let wg_public_keys_b64 = {
        #[cfg(feature = "wireguard")]
        {
            use base64::Engine;
            let mut map = BTreeMap::new();
            for (node, keys) in &wg_public_keys {
                let mut node_map = BTreeMap::new();
                for (iface, (_priv, pubkey)) in keys {
                    node_map.insert(
                        iface.clone(),
                        base64::engine::general_purpose::STANDARD.encode(pubkey),
                    );
                }
                map.insert(node.clone(), node_map);
            }
            map
        }
        #[cfg(not(feature = "wireguard"))]
        {
            BTreeMap::new()
        }
    };

    let mut lab_state = LabState::new(topology.lab.name.clone(), now_iso8601());
    lab_state.namespaces = namespace_names.clone();
    lab_state.pids = pids.clone();
    lab_state.starttimes = starttimes.clone();
    lab_state.mgmt_peers = mgmt_peers.clone();
    lab_state.wg_public_keys = wg_public_keys_b64;
    lab_state.containers = container_states.clone();
    lab_state.runtime = container_runtime.as_ref().map(|rt| rt.binary().to_string());
    lab_state.dns_injected = dns_injected;
    lab_state.wifi_loaded = wifi_loaded;
    lab_state.process_logs = process_logs.clone();
    state::save(&lab_state, topology)?;

    // Disarm cleanup — deployment succeeded
    cleanup.disarm();

    let mut running = RunningLab::new(
        topology.clone(),
        namespace_names,
        container_states,
        container_runtime.as_ref().map(|rt| rt.binary().to_string()),
        pids,
        dns_injected,
        wifi_loaded,
    );
    running.set_starttimes(starttimes);
    running.set_mgmt_peers(mgmt_peers);

    // ── Step 19: Run validate assertions ─────────────────────────
    // Never fails the deploy; the structured results ride on the
    // returned lab so callers (`deploy --strict`) can decide.
    if !topology.assertions.is_empty() {
        tracing::info!("step 19: running validate assertions");
        let results = run_assertions(&running, topology);
        running.set_assertion_results(results);
    }

    Ok(running)
}

/// Apply the unified `nlink-lab` nftables table for a node.
/// Plan 158a.
///
/// Builds an [`NftablesConfig`] covering firewall + NAT
/// chains and rules from the desired state and commits it via
/// `NftablesDiff::apply_reconcile`. Idempotent re-apply makes
/// zero kernel calls; in-place edits replace rule bodies
/// atomically without rebuilding the chain.
async fn apply_nftables_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
) -> Result<()> {
    use nlink::netlink::Nftables;
    use nlink::netlink::nftables::config::ReconcileOptions;

    // Phase 0: validate user-supplied literals so the
    // declarative closures can `.expect()` cleanly.
    if let Some(nat) = nat {
        validate_nat_rule_literals(nat)?;
    }

    let cfg = topology_to_nftables_config(fw, nat)?;

    let nft_conn: Connection<Nftables> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!(
            "failed to create nftables connection for '{node_name}': {e}"
        ))
    })?;

    // Nothing declared at all: an empty `NftablesConfig` declares no
    // table, so its diff has nothing to reconcile against and a table
    // left from an earlier deploy/apply would survive. Delete it
    // outright (idempotent) so `apply` after removing a node's last
    // firewall/NAT block really clears the rules.
    if fw.is_none() && nat.is_none() {
        let removed = nft_conn
            .del_table_if_exists(NLINK_LAB_TABLE, nlink::netlink::nftables::Family::Inet)
            .await
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to remove nftables table on '{node_name}': {e}"
                ))
            })?;
        if removed {
            tracing::info!(node = %node_name, "nftables: removed table {NLINK_LAB_TABLE}");
        }
        return Ok(());
    }

    let diff = cfg.diff(&nft_conn).await.map_err(|e| {
        Error::deploy_failed(format!(
            "failed to diff nftables config on '{node_name}': {e}"
        ))
    })?;

    let report = diff
        .apply_reconcile(&nft_conn, ReconcileOptions::default())
        .await
        .map_err(|e| {
            Error::deploy_failed(format!(
                "failed to apply nftables config on '{node_name}': {e}"
            ))
        })?;

    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count,
        "nftables reconcile"
    );
    Ok(())
}

/// Apply per-pair network impairments using `PerPeerImpairer`.
///
/// For each network with impairments, group rules by source node and
/// install one HTB+netem+flower tree per source interface. We use
/// `reconcile()` so re-deploying an unchanged topology makes zero
/// kernel calls.
async fn apply_network_impairments(
    topology: &Topology,
    node_handles: &BTreeMap<String, NodeHandle>,
) -> Result<()> {
    use nlink::netlink::impair::{PeerImpairment, PerPeerImpairer};
    use nlink::util::Rate;

    let networks_with_impair: Vec<_> = topology
        .networks
        .iter()
        .filter(|(_, n)| !n.impairments.is_empty())
        .collect();

    if networks_with_impair.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "step 14b: applying per-pair network impairments ({} network(s))",
        networks_with_impair.len()
    );

    for (net_name, network) in networks_with_impair {
        // Map node name → its interface in this network (taken from
        // the first member entry that names the node).
        let mut node_ifaces: BTreeMap<String, String> = BTreeMap::new();
        // Map node name → its IP on this network (first address from
        // the auto-assigned subnet).
        let mut node_ips: BTreeMap<String, IpAddr> = BTreeMap::new();

        for member in &network.members {
            let Some(ep) = EndpointRef::parse(member) else {
                continue;
            };
            node_ifaces
                .entry(ep.node.clone())
                .or_insert_with(|| ep.iface.clone());

            if let Some(port) = network.ports.get(member)
                && let Some(addr_with_prefix) = port.addresses.first()
                && let Some((addr_str, _)) = addr_with_prefix.split_once('/')
                && let Ok(ip) = addr_str.parse::<IpAddr>()
            {
                node_ips.entry(ep.node.clone()).or_insert(ip);
            }
        }

        // Group impairments by source node.
        let mut by_source: BTreeMap<&str, Vec<&crate::types::NetworkImpairment>> = BTreeMap::new();
        for imp in &network.impairments {
            by_source.entry(&imp.src[..]).or_default().push(imp);
        }

        for (src_node, rules) in by_source {
            let Some(src_iface) = node_ifaces.get(src_node) else {
                return Err(Error::deploy_failed(format!(
                    "network '{net_name}': src node '{src_node}' has no interface in this network"
                )));
            };
            let Some(src_handle) = node_handles.get(src_node) else {
                return Err(Error::deploy_failed(format!(
                    "network '{net_name}': src node '{src_node}' has no namespace handle"
                )));
            };

            let mut impairer = PerPeerImpairer::new(src_iface.as_str());

            for rule in rules {
                let Some(dst_ip) = node_ips.get(&rule.dst) else {
                    return Err(Error::deploy_failed(format!(
                        "network '{net_name}' impair {} -- {}: cannot resolve IP for dst node \
                         '{}' (network needs a subnet, or the dst must have an explicit address)",
                        rule.src, rule.dst, rule.dst
                    )));
                };

                let netem = build_netem(&rule.impairment)?;
                let mut peer = PeerImpairment::new(netem);
                if let Some(rc) = &rule.rate_cap {
                    let bits = parse_rate_bps(rc).map_err(|e| {
                        Error::deploy_failed(format!(
                            "network '{net_name}' impair {} -- {}: bad rate-cap '{rc}': {e}",
                            rule.src, rule.dst
                        ))
                    })?;
                    peer = peer.rate_cap(Rate::bits_per_sec(bits));
                }

                impairer = impairer.impair_dst_ip(*dst_ip, peer);
            }

            let conn: Connection<Route> = src_handle.connection().map_err(|e| {
                Error::deploy_failed(format!(
                    "network '{net_name}': connection for '{src_node}': {e}"
                ))
            })?;

            impairer.apply(&conn).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "network '{net_name}': failed to apply per-pair impairment on \
                     '{src_node}:{src_iface}': {e}"
                ))
            })?;
        }
    }

    Ok(())
}

// NOTE: the imperative `apply_nat(...)` body (Plan 152 era)
// is deleted by Plan 158a. All NAT rule application now goes
// through `apply_nftables_for_node` which builds a single
// `NftablesConfig` covering firewall + NAT and commits it
// via `NftablesDiff::apply_reconcile`.

/// Apply the declarative [`NetworkConfig`] for one node.
/// Plan 158e Slice 1.
///
/// Wraps `cfg.diff(&conn).await?.apply(&conn, …).await?`.
/// Idempotent re-apply makes zero kernel calls for the
/// address + route layer.
async fn apply_network_config_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    cfg: nlink::netlink::config::NetworkConfig,
) -> Result<()> {
    // Skip the round-trip when the declared config is empty (no
    // addresses, no routes, no links, no qdiscs).
    if cfg.links().is_empty()
        && cfg.addresses().is_empty()
        && cfg.routes().is_empty()
        && cfg.qdiscs().is_empty()
    {
        return Ok(());
    }

    let conn: Connection<Route> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!("NetworkConfig connection on '{node_name}': {e}"))
    })?;

    // `NetworkConfig::apply` computes the diff and applies it.
    // Idempotent — re-apply on an unchanged topology completes
    // with `changes_made == 0`.
    let result = cfg
        .apply(&conn)
        .await
        .map_err(|e| Error::deploy_failed(format!("NetworkConfig::apply on '{node_name}': {e}")))?;

    tracing::info!(
        node = %node_name,
        changes = result.changes_made,
        errors = result.errors.len(),
        "NetworkConfig reconcile (addresses + routes)"
    );

    if !result.errors.is_empty() {
        let first = &result.errors[0];
        return Err(Error::deploy_failed(format!(
            "NetworkConfig::apply on '{node_name}' completed with {} error(s); \
             first: {}: {}",
            result.errors.len(),
            first.operation,
            first.error
        )));
    }
    Ok(())
}

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

    let is_v6 = gw.is_some_and(|ip| ip.is_ipv6()) || (!is_default && dest.contains(':'));

    if is_v6 {
        let mut route = if is_default {
            nlink::netlink::route::Ipv6Route::default_route()
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
            nlink::netlink::route::Ipv4Route::default_route()
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

    let is_v6 = gw.is_some_and(|ip| ip.is_ipv6()) || (!is_default && dest.contains(':'));

    if is_v6 {
        let mut route = if is_default {
            nlink::netlink::route::Ipv6Route::default_route()
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
            nlink::netlink::route::Ipv4Route::default_route()
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

/// Plan 159c — per-node Stack-pattern orchestrator.
///
/// Bundles the three declarative reconcile calls
/// (`apply_network_config_for_node`, `apply_nftables_for_node`,
/// `apply_wireguard_for_node`) into a single per-node call site
/// with one aggregated `tracing::info!` for the whole stack.
/// Close to upstream `facade::Stack::apply_in`, which is not adopted
/// because it applies WireGuard *after* the network layer and takes no
/// `ApplyOptions` (we need `ensure_devices` before addresses land and
/// purge on apply — see the nlink 0.26 adoption epic). No pre-flight
/// validation across layers — we don't double-dump;
/// but routes through `NodeHandle::connection<P>()` so the
/// container case (`connection_for_pid`) keeps working
/// alongside the bare-namespace case. Upstream's
/// `Stack::apply_in_namespace(&str)` only accepts a name, so
/// adopting it directly would break containers.
#[cfg(feature = "wireguard")]
async fn apply_stack_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    network: nlink::netlink::config::NetworkConfig,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
    wireguard: Option<nlink::netlink::genl::wireguard::WireguardConfig>,
) -> Result<()> {
    // Plan 160 — bootstrap WG links *before* the NetworkConfig apply
    // so their tunnel addresses (declared on the network layer) land
    // on an existing interface. `ensure_devices` is idempotent, so
    // the live-reconcile path re-runs it harmlessly.
    if let Some(cfg) = &wireguard {
        ensure_wireguard_devices_for_node(node_handle, node_name, cfg).await?;
    }
    apply_network_config_for_node(node_handle, node_name, network).await?;
    if fw.is_some() || nat.is_some() {
        apply_nftables_for_node(node_handle, node_name, fw, nat).await?;
    }
    if let Some(cfg) = wireguard {
        apply_wireguard_for_node(node_handle, node_name, cfg).await?;
    }
    tracing::info!(node = %node_name, "stack reconcile complete");
    Ok(())
}

/// Plan 159c — WG-less variant for builds without
/// `--features wireguard`.
#[cfg(not(feature = "wireguard"))]
async fn apply_stack_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    network: nlink::netlink::config::NetworkConfig,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
    _wireguard: Option<()>,
) -> Result<()> {
    apply_network_config_for_node(node_handle, node_name, network).await?;
    if fw.is_some() || nat.is_some() {
        apply_nftables_for_node(node_handle, node_name, fw, nat).await?;
    }
    tracing::info!(node = %node_name, "stack reconcile complete");
    Ok(())
}

/// Bootstrap a node's declared WireGuard links (Plan 160, nlink
/// 0.24 #169). `WireguardConfig::ensure_devices` creates any
/// declared-but-absent WG interface idempotently (swallowing
/// already-exists) through a same-namespace Route connection —
/// covering both the bare-namespace and container
/// (`connection_for_pid`) cases via `NodeHandle`, which the
/// name-only `facade::apply::wireguard*` helpers would not. Runs
/// before the `NetworkConfig` apply so the WG interfaces exist when
/// their tunnel addresses are assigned; this retired the imperative
/// step-6c pre-create loop.
#[cfg(feature = "wireguard")]
async fn ensure_wireguard_devices_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    cfg: &nlink::netlink::genl::wireguard::WireguardConfig,
) -> Result<()> {
    let route_conn: Connection<Route> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!(
            "failed to open Route connection for WireGuard bootstrap on '{node_name}': {e}"
        ))
    })?;
    cfg.ensure_devices(&route_conn).await.map_err(|e| {
        Error::deploy_failed(format!(
            "WireguardConfig::ensure_devices on '{node_name}': {e}"
        ))
    })?;
    // `ensure_devices` only creates the link. The NetworkConfig applied
    // right after declares routes *via* the tunnel, and the kernel
    // rejects a nexthop on a down device ("Device for nexthop is not
    // up"), so bring every declared WG interface up here.
    for dev in cfg.devices() {
        route_conn
            .set_link_up(dev.ifname.as_str())
            .await
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up WireGuard interface '{}' on '{node_name}': {e}",
                    dev.ifname
                ))
            })?;
    }
    Ok(())
}

/// Apply a node's `WireguardConfig` via `apply_reconcile`.
/// Plan 159a Phase 2 — mirrors `apply_network_config_for_node` /
/// `apply_nftables_for_node` shape for the WG GENL layer.
/// The WG link is bootstrapped earlier (via
/// `ensure_wireguard_devices_for_node`) so `NetworkConfig` can
/// assign its addresses; this fn only configures the GENL device.
#[cfg(feature = "wireguard")]
async fn apply_wireguard_for_node(
    node_handle: &NodeHandle,
    node_name: &str,
    cfg: nlink::netlink::genl::wireguard::WireguardConfig,
) -> Result<()> {
    use nlink::netlink::nftables::config::ReconcileOptions;

    let wg_conn = node_handle.wireguard_connection().await.map_err(|e| {
        Error::deploy_failed(format!(
            "failed to create WireGuard connection for '{node_name}': {e}"
        ))
    })?;

    let report = cfg
        .apply_reconcile(&wg_conn, ReconcileOptions::default())
        .await
        .map_err(|e| {
            Error::deploy_failed(format!(
                "WireguardConfig::apply_reconcile on '{node_name}': {e}"
            ))
        })?;

    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count,
        "WireguardConfig reconcile complete",
    );
    Ok(())
}

/// Compute the full layered diff between a running lab's live
/// state and a desired topology. Plan 158f Phase 2.
///
/// Aggregates three views:
/// - Lab-graph differences (nodes/links/impair/sysctls/etc.) via
///   [`crate::diff::diff_topologies`].
/// - Per-namespace RTNETLINK diff (links/addresses/routes/qdiscs)
///   by building the same [`nlink::netlink::config::NetworkConfig`]
///   step 11c of the deploy uses and calling its `diff()` against
///   a per-node `Connection<Route>`.
/// - Per-namespace nftables diff by building the same
///   [`nlink::netlink::nftables::config::NftablesConfig`] step 13
///   uses and calling its `diff()` against a per-node
///   `Connection<Nftables>`.
///
/// Used by `nlink-lab apply --check` and `apply --dry-run` so
/// CI and operators see the full set of kernel changes that
/// `apply` would commit — not just the lab-graph subset that
/// `TopologyDiff` covers.
///
/// Re-computing the upstream subdiffs on every call costs one
/// dump round-trip per node per protocol family. For a 50-node
/// lab that's 100 dumps; in practice ms-scale on a quiet host.
pub async fn compute_layered_diff(
    running: &RunningLab,
    desired: &Topology,
) -> Result<crate::diff::LayeredDiff> {
    use nlink::netlink::Nftables;

    let topology = crate::diff::diff_topologies(running.topology(), desired);

    let auto_routes = if desired.lab.routing == crate::types::RoutingMode::Auto {
        auto_generate_routes(desired)
    } else {
        BTreeMap::new()
    };

    let mut network = BTreeMap::new();
    let mut nftables = BTreeMap::new();

    for (node_name, node) in &desired.nodes {
        // The handle lookup uses the running-lab state. A node
        // listed in `desired` but not in `running` (i.e. a
        // newly-added node) is captured by the lab-graph diff
        // (`topology.nodes_added`) and gets full creation work
        // during apply. We skip it here to avoid a spurious error.
        let handle = match node_handle_for(running, node_name) {
            Ok(h) => h,
            Err(_) => continue,
        };

        // RTNETLINK side.
        let cfg = topology_to_network_config(node_name, node, desired, auto_routes.get(node_name))?;
        let cfg_is_empty = cfg.links().is_empty()
            && cfg.addresses().is_empty()
            && cfg.routes().is_empty()
            && cfg.qdiscs().is_empty();
        if !cfg_is_empty {
            let conn: Connection<Route> = handle.connection().map_err(|e| {
                Error::deploy_failed(format!("NetworkConfig connection on '{node_name}': {e}"))
            })?;
            let diff = cfg.diff(&conn).await.map_err(|e| {
                Error::deploy_failed(format!("NetworkConfig::diff on '{node_name}': {e}"))
            })?;
            network.insert(node_name.clone(), diff);
        }

        // nftables side.
        let fw = desired.effective_firewall(node);
        let nat = node.nat.as_ref();
        if fw.is_some() || nat.is_some() {
            let cfg = topology_to_nftables_config(fw, nat)?;
            let nft_conn: Connection<Nftables> = handle.connection().map_err(|e| {
                Error::deploy_failed(format!("Nftables connection on '{node_name}': {e}"))
            })?;
            let diff = cfg.diff(&nft_conn).await.map_err(|e| {
                Error::deploy_failed(format!("NftablesConfig::diff on '{node_name}': {e}"))
            })?;
            nftables.insert(node_name.clone(), diff);
        }
    }

    Ok(crate::diff::LayeredDiff {
        topology,
        network,
        nftables,
    })
}

/// Apply a topology diff to a running lab, performing incremental updates.
///
/// Executes changes in dependency order:
/// 1. Remove impairments from endpoints on nodes being removed
/// 2. Remove links connected to nodes being removed
/// 3. Remove nodes (delete namespaces)
/// 4. Add new nodes (create namespaces)
/// 5. Add new links (create veth pairs, set addresses, bring up)
/// 6. Configure new nodes (sysctls, routes, firewall)
/// 7. Apply impairment changes (add, update, remove)
/// 8. Update state file
pub async fn apply_diff(
    running: &mut RunningLab,
    desired: &Topology,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    // Acquire exclusive lock
    let _lock = state::lock(&desired.lab.name)?;

    // ── Phase 1: Remove impairments from endpoints being removed ──
    for ep_str in &diff.impairments_removed {
        running.clear_impairment(ep_str).await?;
    }

    // ── Phase 2: Remove links ──────────────────────────────────────
    // Delete the veth interface from one side — kernel removes the pair.
    for link in &diff.links_removed {
        let ep = EndpointRef::parse(&link.endpoints[0]).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: link.endpoints[0].clone(),
        })?;

        // Get a connection to the node's namespace (bare or container).
        // `del_link_if_exists` (nlink 0.24) treats an already-removed
        // veth (e.g. its pair was deleted first) as Ok(false).
        if let Ok(handle) = node_handle_for(running, &ep.node)
            && let Ok(conn) = handle.connection::<Route>()
            && let Err(e) = conn.del_link_if_exists(ep.iface.as_str()).await
        {
            tracing::warn!("failed to delete link '{}' in '{}': {e}", ep.iface, ep.node);
        }
    }

    // ── Phase 3: Remove nodes ──────────────────────────────────────
    for node_name in &diff.nodes_removed {
        // Kill any background processes on this node
        for (pnode, pid) in running.pids() {
            if pnode == node_name {
                // identity-checked: never signal a reused PID (issue #30)
                let _ = crate::running::kill_tracked(*pid, running.starttimes().get(pid).copied());
            }
        }

        if let Some(ns_name) = running.namespace_names_mut().remove(node_name) {
            if namespace::exists(&ns_name)
                && let Err(e) = namespace::delete(&ns_name)
            {
                tracing::warn!("failed to delete namespace '{ns_name}': {e}");
            }
            crate::netns_tag::untag(&ns_name);
        }
        // Container removal
        if let Some(container) = running.containers_mut().remove(node_name)
            && let Some(binary) = running.runtime_binary()
        {
            let _ = std::process::Command::new(binary)
                .args(["rm", "-f", &container.id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    // ── Phase 4: Add new nodes ─────────────────────────────────────
    // Detect container runtime lazily if any new node needs one.
    let new_container_nodes = diff
        .nodes_added
        .iter()
        .any(|name| desired.nodes.get(name).is_some_and(|n| n.image.is_some()));
    let container_runtime = if new_container_nodes {
        let rt_config = desired.lab.runtime.as_ref().cloned().unwrap_or_default();
        let rt = Runtime::new(&rt_config)?;
        running.set_runtime_binary(rt.binary().to_string());
        Some(rt)
    } else {
        // Reconstruct from existing state if we need it for removal (already handled)
        None
    };

    for node_name in &diff.nodes_added {
        let node = desired
            .nodes
            .get(node_name)
            .ok_or_else(|| Error::NodeNotFound {
                name: node_name.clone(),
            })?;

        if let Some(image) = &node.image {
            // Container node
            let rt = container_runtime.as_ref().unwrap();
            match node.pull.as_deref() {
                Some("never") => {}
                Some("always") => {
                    rt.pull_image(image)?;
                }
                _ => {
                    rt.ensure_image(image)?;
                }
            }
            let container_name = format!("{}-{}", desired.lab.prefix(), node_name);
            let extra_hosts: Vec<String> = if desired.lab.dns == DnsMode::Hosts {
                crate::dns::generate_hosts_entries(desired)
                    .iter()
                    .flat_map(|entry| {
                        entry
                            .names
                            .iter()
                            .map(|name| format!("{name}:{}", entry.ip))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let opts = build_create_opts(node, &extra_hosts);
            let info = rt.create(&container_name, image, &opts)?;
            running.containers_mut().insert(
                node_name.clone(),
                ContainerState {
                    id: info.id,
                    name: info.name,
                    image: image.clone(),
                    pid: info.pid,
                },
            );
        } else {
            // Bare namespace node
            let ns_name = desired.namespace_name(node_name);
            guard_namespace_absent(&ns_name)?;
            namespace::create(&ns_name).map_err(|e| Error::Namespace {
                op: "create",
                ns: ns_name.clone(),
                source: e,
            })?;
            running
                .namespace_names_mut()
                .insert(node_name.clone(), ns_name.clone());
        }

        // Apply sysctls
        let handle = node_handle_for(running, node_name)?;
        let sysctls = desired.effective_sysctls(node);
        if !sysctls.is_empty() {
            let entries: Vec<(&str, &str)> = sysctls
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            handle.set_sysctls(&entries).map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to apply sysctls for node '{node_name}': {e}"
                ))
            })?;
        }
    }

    // ── Phase 5: Add new links ─────────────────────────────────────
    for link in &diff.links_added {
        let ep_a =
            EndpointRef::parse(&link.endpoints[0]).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: link.endpoints[0].clone(),
            })?;
        let ep_b =
            EndpointRef::parse(&link.endpoints[1]).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: link.endpoints[1].clone(),
            })?;

        let handle_a = node_handle_for(running, &ep_a.node)?;
        let handle_b = node_handle_for(running, &ep_b.node)?;

        let ns_b_fd = handle_b.open_ns_fd().map_err(|e| {
            Error::deploy_failed(format!("failed to open namespace for '{}': {e}", ep_b.node))
        })?;

        let conn_a: Connection<Route> = handle_a.connection().map_err(|e| {
            Error::deploy_failed(format!("failed to connect to '{}': {e}", ep_a.node))
        })?;

        let mut veth = nlink::netlink::link::VethLink::new(&ep_a.iface, &ep_b.iface)
            .peer_netns_fd(ns_b_fd.as_raw_fd());

        if let Some(mtu) = link.mtu {
            veth = veth.mtu(mtu);
        }

        conn_a.add_link(veth).await.map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create veth pair ({} <-> {}): {e}",
                link.endpoints[0], link.endpoints[1]
            ))
        })?;

        // Set addresses
        if let Some(addresses) = &link.addresses {
            for (ep_str, addr_str) in link.endpoints.iter().zip(addresses.iter()) {
                let ep = EndpointRef::parse(ep_str).ok_or_else(|| Error::InvalidEndpoint {
                    endpoint: ep_str.clone(),
                })?;
                let ep_handle = node_handle_for(running, &ep.node)?;
                let conn: Connection<Route> = ep_handle.connection().map_err(|e| {
                    Error::deploy_failed(format!("connection for '{}': {e}", ep.node))
                })?;
                let (ip, prefix) = parse_cidr(addr_str)?;
                conn.add_address_by_name(&ep.iface, ip, prefix)
                    .await
                    .map_err(|e| {
                        Error::deploy_failed(format!(
                            "failed to add address '{ip}'/{prefix} to '{}' on '{}': {e}",
                            ep.iface, ep.node
                        ))
                    })?;
            }
        }

        // Bring up interfaces on both sides
        for ep_str in &link.endpoints {
            let ep = EndpointRef::parse(ep_str).ok_or_else(|| Error::InvalidEndpoint {
                endpoint: ep_str.clone(),
            })?;
            let ep_handle = node_handle_for(running, &ep.node)?;
            let conn: Connection<Route> = ep_handle
                .connection()
                .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;
            conn.set_link_up(&ep.iface).await.map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to bring up '{}' on '{}': {e}",
                    ep.iface, ep.node
                ))
            })?;
        }
    }

    // ── Phase 6: Configure new nodes (NetworkConfig + nftables) ────
    //
    // Plan 158e Slice 1+2+3 + polish — apply the per-namespace
    // declarative `NetworkConfig` so newly-added nodes get every
    // address source (interfaces, network ports, WG, macvlan/ipvlan,
    // WiFi), every route (manual + auto), and every declarative link
    // kind (dummies, bonds + bond-member master, VLANs) handled in
    // one atomic-ish per-namespace apply. Without this, dummy and
    // VLAN interfaces with addresses declared on them would silently
    // miss those addresses on apply (a pre-existing gap before
    // Slice 1 — link-pair addresses were set imperatively in Phase 5,
    // but non-link sources were not handled here).
    let auto_routes_for_apply = if desired.lab.routing == crate::types::RoutingMode::Auto {
        auto_generate_routes(desired)
    } else {
        BTreeMap::new()
    };

    // Plan 159a Phase 2 follow-up — `apply_diff` Phase 6 previously
    // didn't configure WireGuard for newly-added nodes; the deploy
    // path did but apply_diff missed it. Build the WG key map once
    // (sync) and let `apply_stack_for_node` orchestrate all three
    // layers per node, matching the deploy step 11c shape.
    #[cfg(feature = "wireguard")]
    let wg_public_keys = build_wg_public_key_map(desired)?;

    for node_name in &diff.nodes_added {
        let node = &desired.nodes[node_name];
        let handle = node_handle_for(running, node_name)?;

        // Plan 160 — the WG interface for a newly-added node is
        // bootstrapped inside `apply_stack_for_node` (via
        // `WireguardConfig::ensure_devices`, before the NetworkConfig
        // apply), so the imperative pre-create loop that mirrored
        // step 6c is gone here too.

        let net = topology_to_network_config(
            node_name,
            node,
            desired,
            auto_routes_for_apply.get(node_name),
        )?;
        let fw = desired.effective_firewall(node);
        let nat = node.nat.as_ref();
        #[cfg(feature = "wireguard")]
        let wg = if node.wireguard.is_empty() {
            None
        } else {
            Some(topology_to_wireguard_config(
                node_name,
                node,
                desired,
                &wg_public_keys,
            )?)
        };
        #[cfg(not(feature = "wireguard"))]
        let wg: Option<()> = None;
        apply_stack_for_node(&handle, node_name, net, fw, nat, wg).await?;
    }

    // ── Phase 7: Apply impairment changes ──────────────────────────
    // Add new impairments
    for (ep_str, impairment) in &diff.impairments_added {
        running.set_impairment(ep_str, impairment).await?;
    }

    // Update changed impairments
    for change in &diff.impairments_changed {
        running
            .set_impairment(&change.endpoint, &change.new)
            .await?;
    }

    // ── Phase 7b: Reconcile network-level per-pair impair ──────────
    // Each NetworkImpairerChange covers one (network, src_node) tree.
    // We use PerPeerImpairer::reconcile() so an unchanged tree makes
    // zero kernel calls; a single-rule edit becomes one
    // change_qdisc/replace_qdisc on the affected leaf.
    apply_network_impair_diff(running, desired, diff).await?;

    // ── Phase 7c: Reconcile per-node static routes ─────────────────
    // Add new routes, replace changed ones (del+add), remove gone
    // ones. Only touches nodes that exist on both sides; routes for
    // added/removed nodes are handled by the lifecycle phases above.
    apply_routes_diff(running, diff).await?;

    // ── Phase 7d: Reconcile per-node sysctls ───────────────────────
    // Apply added + changed entries via set_sysctls. Removed
    // entries get a warning (the kernel default isn't recoverable;
    // overshooting is worse than leaving the previous value).
    apply_sysctls_diff(running, diff)?;

    // ── Phase 7e: Reconcile per-endpoint rate-limits ───────────────
    // For added/changed: apply via RateLimiter (same as deploy
    // step 15). For removed: delete the root qdisc on the iface.
    apply_rate_limits_diff(running, diff).await?;

    // ── Phase 7f: Reconcile per-node nftables (firewall + NAT) ─────
    // Coarse: any change triggers a full atomic flush + rebuild of
    // the node's `nlink-lab` table. nftables transactions ensure
    // the kernel never sees a half-built ruleset.
    apply_nftables_diff(running, diff).await?;

    // ── Phase 8: Update state file ─────────────────────────────────
    running.set_topology(desired.clone());

    // Read-modify-write: keep created_at, wg_public_keys,
    // saved_impairments and process_logs — apply used to rebuild the
    // file from scratch and wipe them (issue #28).
    let mut lab_state = match state::load(&desired.lab.name) {
        Ok((existing, _)) => existing,
        Err(_) => LabState::new(desired.lab.name.clone(), now_iso8601()),
    };
    lab_state.schema_version = state::SCHEMA_VERSION;
    lab_state.namespaces = running.namespace_names().clone();
    lab_state.pids = running.pids().to_vec();
    lab_state.starttimes = running.starttimes().clone();
    lab_state.mgmt_peers = running.mgmt_peers().clone();
    lab_state.containers = running
        .containers()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    lab_state.runtime = running.runtime_binary().map(|s| s.to_string());
    lab_state.dns_injected = running.dns_injected();
    lab_state.wifi_loaded = running.wifi_loaded();
    lab_state.pids.retain(|(n, _)| {
        running.namespace_names().contains_key(n) || running.containers().contains_key(n)
    });
    state::save(&lab_state, desired)?;

    Ok(())
}

/// Reconcile per-node static routes (Plan 152 Phase B).
///
/// For each [`crate::diff::RouteChange`]:
/// - `desired = Some(new)`, `was_present = false`  → add the route
/// - `desired = Some(new)`, `was_present = true`   → del + add (replace)
/// - `desired = None`                              → del the route
///
/// Failures on `del` are downgraded to a warning — a route the
/// kernel claims doesn't exist isn't a deploy-blocker.
async fn apply_routes_diff(
    running: &mut RunningLab,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    if diff.routes_changed.is_empty() {
        return Ok(());
    }
    for change in &diff.routes_changed {
        let handle = node_handle_for(running, &change.node)?;
        let conn: Connection<Route> = handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", change.node)))?;

        if change.was_present
            && let Err(e) = del_route_for_node(&conn, &change.node, &change.dest).await
        {
            tracing::warn!(
                "del route '{}' on '{}' failed: {e} — continuing",
                change.dest,
                change.node,
            );
        }
        if let Some(new) = &change.desired {
            add_route(&conn, &change.node, &change.dest, new).await?;
        }
    }
    Ok(())
}

/// Delete a single static route from a node. Mirrors `add_route` but
/// uses nlink's `del_route_v4_if_exists` / `del_route_v6_if_exists`
/// (nlink 0.24) based on the destination form — an already-absent
/// route is Ok(false), which is the right idempotent semantics for a
/// diff-driven removal.
async fn del_route_for_node(conn: &Connection<Route>, node_name: &str, dest: &str) -> Result<()> {
    let is_default = dest == "default";
    let is_v6 = !is_default && dest.contains(':');

    if is_default {
        // Delete default route — one family won't exist; if_exists
        // returns Ok(false) for it rather than an error.
        conn.del_route_v4_if_exists("0.0.0.0", 0)
            .await
            .map_err(|e| {
                Error::deploy_failed(format!("del default route on '{node_name}': {e}"))
            })?;
        conn.del_route_v6_if_exists("::", 0).await.map_err(|e| {
            Error::deploy_failed(format!("del default route on '{node_name}': {e}"))
        })?;
        return Ok(());
    }

    let (addr, prefix) = parse_cidr(dest)?;
    let result = if is_v6 {
        conn.del_route_v6_if_exists(&addr.to_string(), prefix).await
    } else {
        match addr {
            IpAddr::V4(_) => conn.del_route_v4_if_exists(&addr.to_string(), prefix).await,
            IpAddr::V6(_) => conn.del_route_v6_if_exists(&addr.to_string(), prefix).await,
        }
    };
    result
        .map(|_| ())
        .map_err(|e| Error::deploy_failed(format!("del route '{dest}' on '{node_name}': {e}")))
}

/// Reconcile per-node nftables ruleset (firewall + NAT).
/// Plan 152 Phase B/4 + Plan 158a.
///
/// Per-rule reconcile via `NftablesConfig::apply_reconcile`:
/// each per-rule USERDATA-keyed (`nlink-lab/fw/...` /
/// `nlink-lab/nat/...`) so the diff identifies "our" rules by
/// key. Foreign rules (no `nlink-lab/` USERDATA key) are left
/// untouched, supporting hand-edits via
/// `nlink-lab exec node -- nft -f ...` that survive an apply.
///
/// Editing a single rule in-place no longer rebuilds the
/// table: the diff emits `rules_to_replace` for the changed
/// rule, and `apply` commits the swap atomically in the
/// kernel's nftables batch.
async fn apply_nftables_diff(
    running: &mut RunningLab,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    if diff.nftables_changed.is_empty() {
        return Ok(());
    }

    for change in &diff.nftables_changed {
        let handle = node_handle_for(running, &change.node)?;
        apply_nftables_for_node(
            &handle,
            &change.node,
            change.desired_firewall.as_ref(),
            change.desired_nat.as_ref(),
        )
        .await?;
    }

    Ok(())
}

/// Reconcile per-endpoint rate-limits (Plan 152 Phase B/3).
///
/// Plan 160 (nlink 0.24) — added / changed entries converge via
/// `RateLimiter::reconcile`, which diffs the live HTB tree and
/// mutates only what drifted (no root-qdisc teardown, no
/// packet-drop window), closing the "coarse reconcile / Plan 158g"
/// caveat that used to live here. Removed entries clear the root
/// qdisc via `del_qdisc_if_exists`.
async fn apply_rate_limits_diff(
    running: &mut RunningLab,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    if diff.rate_limits_changed.is_empty() {
        return Ok(());
    }
    for change in &diff.rate_limits_changed {
        let ep = EndpointRef::parse(&change.endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: change.endpoint.clone(),
        })?;
        let handle = node_handle_for(running, &ep.node)?;
        let conn: Connection<Route> = handle
            .connection()
            .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;

        match &change.desired {
            Some(rl) => {
                let mut limiter = RateLimiter::new(&ep.iface);
                if let Some(egress) = &rl.egress {
                    let bits = parse_rate_bps(egress).map_err(|e| {
                        Error::deploy_failed(format!(
                            "bad egress rate on '{}': {e}",
                            change.endpoint,
                        ))
                    })?;
                    limiter = limiter.egress(nlink::util::Rate::bits_per_sec(bits));
                }
                if let Some(ingress) = &rl.ingress {
                    let bits = parse_rate_bps(ingress).map_err(|e| {
                        Error::deploy_failed(format!(
                            "bad ingress rate on '{}': {e}",
                            change.endpoint,
                        ))
                    })?;
                    limiter = limiter.ingress(nlink::util::Rate::bits_per_sec(bits));
                }
                let report = limiter.reconcile(&conn).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "failed to reconcile rate limit on '{}': {e}",
                        change.endpoint,
                    ))
                })?;
                tracing::debug!(
                    endpoint = %change.endpoint,
                    changes = report.changes_made,
                    "rate limit reconcile complete"
                );
            }
            None => {
                use nlink::TcHandle;
                // Plan 160 — idempotent removal; not-found is Ok(false).
                if let Err(e) = conn
                    .del_qdisc_if_exists(ep.iface.as_str(), TcHandle::ROOT)
                    .await
                {
                    tracing::warn!("failed to clear rate-limit on '{}': {e}", change.endpoint,);
                }
            }
        }
    }
    Ok(())
}

/// Reconcile per-node sysctls (Plan 152 Phase B).
///
/// Applies added + changed entries via `NodeHandle::set_sysctls`.
/// Removed entries are reported via `tracing::warn!` only — the
/// kernel default for an arbitrary sysctl isn't recoverable
/// without snapshotting the original value before the original
/// deploy, and overshooting would be worse than leaving the
/// previous setting in place.
fn apply_sysctls_diff(running: &mut RunningLab, diff: &crate::diff::TopologyDiff) -> Result<()> {
    if diff.sysctls_changed.is_empty() {
        return Ok(());
    }
    for change in &diff.sysctls_changed {
        let handle = node_handle_for(running, &change.node)?;

        // Build one set_sysctls call covering both adds and changes.
        let mut entries: Vec<(&str, &str)> = Vec::new();
        for (k, v) in &change.added {
            entries.push((k.as_str(), v.as_str()));
        }
        for (k, _, new) in &change.changed {
            entries.push((k.as_str(), new.as_str()));
        }
        if !entries.is_empty() {
            handle.set_sysctls(&entries).map_err(|e| {
                Error::deploy_failed(format!("set sysctls on '{}': {e}", change.node))
            })?;
        }

        for k in &change.removed {
            tracing::warn!(
                "sysctl '{k}' on node '{}' is no longer in the desired topology — \
                 kernel value left at its previous setting (set explicitly to override)",
                change.node,
            );
        }
    }
    Ok(())
}

/// Reconcile network-level per-pair impair via
/// `PerPeerImpairer::reconcile()`. Each `NetworkImpairerChange`
/// covers one `(network, src_node)` tree; reconcile is
/// non-destructive — unchanged sub-trees make zero kernel calls.
async fn apply_network_impair_diff(
    running: &mut RunningLab,
    desired: &Topology,
    diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    use nlink::netlink::impair::{PeerImpairment, PerPeerImpairer};
    use nlink::util::Rate;

    if diff.network_impairs_changed.is_empty() {
        return Ok(());
    }

    for change in &diff.network_impairs_changed {
        // Look up source-node interface and destination-node IPs from
        // the desired topology's network definition. (For the `clear`
        // path where `desired = None`, we still need the iface — read
        // it from whichever topology has the network.)
        let net_topo = desired
            .networks
            .get(&change.network)
            .or_else(|| running.topology().networks.get(&change.network));

        let Some(net) = net_topo else {
            tracing::warn!(
                "network '{}' not found in current or desired topology — skipping",
                change.network,
            );
            continue;
        };

        // Map node name → its iface in this network, and IP if known.
        let mut node_iface: Option<String> = None;
        let mut node_ips: BTreeMap<String, IpAddr> = BTreeMap::new();
        for member in &net.members {
            let Some(ep) = EndpointRef::parse(member) else {
                continue;
            };
            if ep.node == change.src_node && node_iface.is_none() {
                node_iface = Some(ep.iface.clone());
            }
            if let Some(port) = net.ports.get(member)
                && let Some(addr_with_prefix) = port.addresses.first()
                && let Some((addr_str, _)) = addr_with_prefix.split_once('/')
                && let Ok(ip) = addr_str.parse::<IpAddr>()
            {
                node_ips.entry(ep.node).or_insert(ip);
            }
        }

        let Some(iface) = node_iface else {
            tracing::warn!(
                "network '{}': src node '{}' has no iface — skipping",
                change.network,
                change.src_node,
            );
            continue;
        };

        let handle = node_handle_for(running, &change.src_node)?;
        let conn: Connection<Route> = handle.connection().map_err(|e| {
            Error::deploy_failed(format!(
                "network '{}': connection for '{}': {e}",
                change.network, change.src_node,
            ))
        })?;

        match &change.desired {
            Some(rules) => {
                let mut impairer = PerPeerImpairer::new(iface.as_str());
                for rule in rules {
                    let Some(dst_ip) = node_ips.get(&rule.dst) else {
                        return Err(Error::deploy_failed(format!(
                            "network '{}': cannot resolve IP for dst node '{}'",
                            change.network, rule.dst,
                        )));
                    };
                    let netem = build_netem(&rule.impairment)?;
                    let mut peer = PeerImpairment::new(netem);
                    if let Some(rc) = &rule.rate_cap {
                        let bits = parse_rate_bps(rc).map_err(|e| {
                            Error::deploy_failed(format!(
                                "network '{}': bad rate-cap '{rc}': {e}",
                                change.network,
                            ))
                        })?;
                        peer = peer.rate_cap(Rate::bits_per_sec(bits));
                    }
                    impairer = impairer.impair_dst_ip(*dst_ip, peer);
                }
                let _report = impairer.reconcile(&conn).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "network '{}': failed to reconcile impair on '{}:{}': {e}",
                        change.network, change.src_node, iface,
                    ))
                })?;
            }
            None => {
                let impairer = PerPeerImpairer::new(iface.as_str());
                impairer.clear(&conn).await.map_err(|e| {
                    Error::deploy_failed(format!(
                        "network '{}': failed to clear impair on '{}:{}': {e}",
                        change.network, change.src_node, iface,
                    ))
                })?;
            }
        }
    }

    Ok(())
}

/// Run post-deploy `validate { … }` assertions (step 19).
///
/// Thin wrapper over [`crate::test_runner::run_assertions`] — the one
/// assertion engine shared with `nlink-lab test` and the scenario
/// engine — that additionally emits the `PASS:` / `FAIL:` log lines
/// operators grep for. Target addresses come from
/// [`crate::ipmap::build_ip_map`], so bridge-`network` topologies no
/// longer log `SKIP: no IP found` for every assertion (issue #34).
///
/// Never bails: `deploy()` succeeds regardless of the outcome. The
/// results are stored on the returned [`RunningLab`] (see
/// [`RunningLab::assertion_results`] / [`RunningLab::assertions_failed`])
/// so the CLI can implement `deploy --strict` without changing this
/// function's signature.
fn run_assertions(
    running: &RunningLab,
    topology: &Topology,
) -> Vec<crate::test_runner::AssertionResult> {
    let results = crate::test_runner::run_assertions(running, topology);
    for r in &results {
        match (r.passed, r.detail.as_deref()) {
            (true, Some(detail)) => tracing::info!("PASS: {} ({detail})", r.description),
            (true, None) => tracing::info!("PASS: {}", r.description),
            (false, Some(detail)) => tracing::warn!("FAIL: {}: {detail}", r.description),
            (false, None) => tracing::warn!("FAIL: {}", r.description),
        }
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        tracing::warn!("{failed} of {} validate assertion(s) failed", results.len());
    }
    results
}

fn node_handle_for(running: &RunningLab, node_name: &str) -> Result<NodeHandle> {
    if let Some(ns_name) = running.namespace_names().get(node_name) {
        return Ok(NodeHandle::Namespace {
            ns_name: ns_name.clone(),
        });
    }
    if let Some(container) = running.containers().get(node_name) {
        return Ok(NodeHandle::Container {
            id: container.id.clone(),
            pid: container.pid,
            ns_path: format!("/proc/{}/ns/net", container.pid),
        });
    }
    Err(Error::NodeNotFound {
        name: node_name.to_string(),
    })
}

/// Build a NetemConfig from an Impairment.
/// Pre-create guard for a bare-namespace node (Plan 160, nlink 0.25).
///
/// Rejects only a namespace that is *live* (`is_namespace` — an nsfs
/// bind-mount), so a genuine collision still errors. A leftover marker
/// file with no live mount is a stale artifact of an unclean shutdown
/// (`exists` true but `is_namespace` false); we clear it so the deploy
/// self-heals instead of hard-failing "already exists" and forcing a
/// manual `destroy --orphans`. `create` refuses any existing marker, so
/// the clear is required before it can proceed. Under the deploy flock
/// this detect-and-clear is race-free.
fn guard_namespace_absent(ns_name: &str) -> Result<()> {
    if namespace::is_namespace(ns_name) {
        return Err(Error::AlreadyExists {
            name: format!("namespace '{ns_name}' already exists"),
        });
    }
    if namespace::exists(ns_name) {
        tracing::warn!(
            "clearing stale namespace marker '{ns_name}' (no live mount — likely an unclean prior shutdown)"
        );
        namespace::delete(ns_name).map_err(|e| Error::Namespace {
            op: "clear-stale",
            ns: ns_name.to_string(),
            source: e,
        })?;
    }
    Ok(())
}

/// Convert WiFi channel number to frequency in MHz (as string for iw).
fn freq_from_channel(channel: u32) -> String {
    let freq = match channel {
        1 => 2412,
        2 => 2417,
        3 => 2422,
        4 => 2427,
        5 => 2432,
        6 => 2437,
        7 => 2442,
        8 => 2447,
        9 => 2452,
        10 => 2457,
        11 => 2462,
        12 => 2467,
        13 => 2472,
        14 => 2484,
        // 5 GHz channels
        36 => 5180,
        40 => 5200,
        44 => 5220,
        48 => 5240,
        _ => 2412, // default to channel 1
    };
    freq.to_string()
}

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Cleanup guard that removes namespaces on drop if deployment fails.
struct Cleanup {
    /// Lab being deployed (wifi config cleanup, logs dir, tags).
    lab_name: String,
    namespaces: Vec<String>,
    containers: Vec<String>,
    runtime_binary: Option<String>,
    dns_lab: Option<String>,
    wifi_loaded: bool,
    /// Lab name whose subnet-pool entries should be released on
    /// rollback. Set when `auto/N` placeholders were resolved at the
    /// top of `deploy()`.
    subnet_pool_lab: Option<String>,
    /// Root-namespace links created so far (mgmt bridge + veth peers,
    /// host-side macvlan/ipvlan before their move), in creation order.
    host_links: Vec<String>,
    /// Background processes spawned so far, with the start time that
    /// proves the PID is still theirs.
    pids: Vec<(u32, Option<u64>)>,
    /// Per-lab log directory created for spawned processes.
    logs_dir: Option<std::path::PathBuf>,
    armed: bool,
}

impl Cleanup {
    fn new(lab_name: String) -> Self {
        Self {
            lab_name,
            namespaces: Vec::new(),
            containers: Vec::new(),
            runtime_binary: None,
            dns_lab: None,
            wifi_loaded: false,
            subnet_pool_lab: None,
            host_links: Vec::new(),
            pids: Vec::new(),
            logs_dir: None,
            armed: true,
        }
    }

    fn add_host_link(&mut self, name: String) {
        self.host_links.push(name);
    }

    fn add_pid(&mut self, pid: u32, starttime: Option<u64>) {
        self.pids.push((pid, starttime));
    }

    fn set_logs_dir(&mut self, dir: std::path::PathBuf) {
        self.logs_dir = Some(dir);
    }

    /// Unwind everything recorded so far, newest first where order
    /// matters. Async so root-namespace links go through netlink.
    async fn rollback(&mut self) {
        if !self.armed {
            return;
        }
        for (pid, st) in self.pids.iter().rev() {
            let _ = crate::running::kill_tracked(*pid, *st);
        }
        if !self.host_links.is_empty()
            && let Ok(conn) = Connection::<Route>::new()
        {
            for link in self.host_links.iter().rev() {
                if let Err(e) = conn.del_link_if_exists(link.as_str()).await {
                    tracing::warn!("rollback: failed to delete host link '{link}': {e}");
                }
            }
        }
        self.sync_cleanup();
        self.armed = false;
    }

    /// The synchronous part of a rollback, shared with `Drop`.
    fn sync_cleanup(&mut self) {
        if let Some(lab_name) = &self.subnet_pool_lab {
            let _ = crate::subnet_pool::free_for_lab(lab_name);
        }
        if let Some(lab_name) = &self.dns_lab {
            let _ = crate::dns::remove_hosts(lab_name);
        }
        for ns in &self.namespaces {
            // Clean up per-namespace DNS files
            crate::dns::remove_netns_etc(ns);
            if namespace::exists(ns) {
                let _ = namespace::delete(ns);
            }
            crate::netns_tag::untag(ns);
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
        if self.wifi_loaded {
            crate::wifi::release_hwsim(&self.lab_name);
            crate::wifi::cleanup_configs(&self.lab_name);
        }
        if let Some(dir) = &self.logs_dir {
            let _ = std::fs::remove_dir_all(dir);
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

    fn set_dns_lab(&mut self, name: String) {
        self.dns_lab = Some(name);
    }

    fn set_subnet_pool_lab(&mut self, name: String) {
        self.subnet_pool_lab = Some(name);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Cleanup {
    /// Last resort for panics: `rollback()` is the normal path and
    /// disarms this. Host links cannot be deleted here (no async), so a
    /// panic mid-deploy may still leave `nl*`/`nm*` links for
    /// `destroy --orphans`.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for (pid, st) in self.pids.iter().rev() {
            let _ = crate::running::kill_tracked(*pid, *st);
        }
        self.sync_cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_route_stub_node() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node router : router
node host
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host is a stub node → default route via router
        assert!(routes.contains_key("host"), "host should get auto-route");
        assert!(routes["host"].contains_key("default"));
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn test_auto_route_no_override() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node router : router
node host { route default via 10.0.0.99 }
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host already has a manual default route — auto shouldn't override
        let host_routes = routes.get("host");
        assert!(
            host_routes.is_none() || !host_routes.unwrap().contains_key("default"),
            "auto-route should not override manual default"
        );
    }

    #[test]
    fn test_auto_route_multi_hop() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node r1 : router
node r2 : router
node host
link r1:eth0 -- r2:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
link r2:eth1 -- host:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host → default via r2
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.2.1"));
        // r1 has single neighbor (r2) → gets default route via r2
        assert_eq!(
            routes["r1"]["default"].via.as_deref(),
            Some("10.0.1.2"),
            "r1 should default via r2"
        );
    }

    /// Step 19 wrapper: returns the structured vector (not `()`), and a
    /// rootless / undeployed lab yields non-pass results with details
    /// rather than a silent pass. `parse_ping_avg` tests moved to
    /// `test_runner` alongside the single remaining implementation.
    #[test]
    fn test_run_assertions_returns_structured_results() {
        let topology = crate::parser::parse(
            r#"
lab "t"
node a
node b
network lan {
  members [a:eth0, b:eth0]
  subnet 10.0.1.0/24
}
validate {
  reach a b
  route-has a 10.0.1.0/24
}
"#,
        )
        .unwrap();
        let namespace_names = topology
            .nodes
            .keys()
            .map(|n| (n.clone(), format!("nlink-lab-test-nonexistent-{n}")))
            .collect();
        let mut running = RunningLab::new(
            topology.clone(),
            namespace_names,
            Default::default(),
            None,
            Vec::new(),
            false,
            false,
        );
        assert!(running.assertion_results().is_empty());
        assert!(!running.assertions_failed());

        let results = run_assertions(&running, &topology);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.passed));
        assert!(results.iter().all(|r| r.detail.is_some()));
        // Bridge-network address resolved (issue #34): the failure is
        // the missing namespace, not a missing IP.
        assert!(
            !results[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("no IP found"),
            "{:?}",
            results[0].detail
        );

        running.set_assertion_results(results);
        assert_eq!(running.assertion_results().len(), 2);
        assert!(running.assertions_failed());
    }

    #[test]
    fn test_apply_match_expr_tcp_dport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "tcp dport 80");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_udp_dport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "udp dport 53");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ct_state() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ct state established,related");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_tcp_sport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "tcp sport 8080");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_udp_sport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "udp sport 5353");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_icmp_type() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "icmp type 8");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_icmpv6_type() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "icmpv6 type 128");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_mark() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "mark 42");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_saddr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr 10.0.1.0/24");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_daddr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip daddr 192.168.0.1/32");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_compound_saddr_tcp() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr 10.0.1.0/24 tcp dport 22");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_compound_daddr_udp() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip daddr 10.0.2.0/24 udp dport 53");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_saddr_bad_cidr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr not-a-cidr");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("CIDR"));
    }

    #[test]
    fn test_apply_match_expr_unknown_errors() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "unknown expression");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    // ── Plan 158a: topology_to_nftables_config tests ────────────────

    #[test]
    fn nftables_config_empty_inputs_produce_empty_config() {
        let cfg = topology_to_nftables_config(None, None).unwrap();
        assert!(
            cfg.tables().is_empty(),
            "no fw + no nat must produce zero tables"
        );
    }

    #[test]
    fn nftables_config_firewall_only_has_filter_chains() {
        let fw = crate::types::FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![
                crate::types::FirewallRule {
                    match_expr: Some("tcp dport 80".into()),
                    action: Some("accept".into()),
                },
                crate::types::FirewallRule {
                    match_expr: Some("tcp dport 22".into()),
                    action: Some("accept".into()),
                },
            ],
        };
        let cfg = topology_to_nftables_config(Some(&fw), None).unwrap();
        assert_eq!(cfg.tables().len(), 1, "exactly one table");
        let table = cfg.tables().first().unwrap();
        let chain_names: Vec<&str> = table.chains().iter().map(|c| c.name()).collect();
        assert!(
            chain_names.contains(&"input") && chain_names.contains(&"forward"),
            "expected input + forward chains, got {chain_names:?}"
        );
        assert!(
            !chain_names.contains(&"prerouting"),
            "NAT chains must not be present without NAT config"
        );
    }

    #[test]
    fn nftables_config_nat_only_has_nat_chains() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Masquerade,
                src: Some("10.0.0.0/24".into()),
                dst: None,
                target: None,
                target_port: None,
            }],
        };
        let cfg = topology_to_nftables_config(None, Some(&nat)).unwrap();
        assert_eq!(cfg.tables().len(), 1);
        let chain_names: Vec<&str> = cfg
            .tables()
            .first()
            .unwrap()
            .chains()
            .iter()
            .map(|c| c.name())
            .collect();
        assert!(
            chain_names.contains(&"prerouting") && chain_names.contains(&"postrouting"),
            "expected prerouting + postrouting, got {chain_names:?}"
        );
        assert!(
            !chain_names.contains(&"input"),
            "filter chains must not be present without firewall config"
        );
    }

    #[test]
    fn nftables_config_fw_and_nat_share_one_table() {
        let fw = crate::types::FirewallConfig {
            policy: Some("accept".into()),
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("tcp dport 22".into()),
                action: Some("accept".into()),
            }],
        };
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Dnat,
                src: None,
                dst: Some("203.0.113.1/32".into()),
                target: Some("10.0.0.10".into()),
                target_port: Some(8080),
            }],
        };
        let cfg = topology_to_nftables_config(Some(&fw), Some(&nat)).unwrap();
        assert_eq!(
            cfg.tables().len(),
            1,
            "both fw and nat must collapse into one nlink-lab table"
        );
        let table = cfg.tables().first().unwrap();
        let chain_names: Vec<&str> = table.chains().iter().map(|c| c.name()).collect();
        for expected in ["input", "forward", "prerouting", "postrouting"] {
            assert!(
                chain_names.contains(&expected),
                "expected chain '{expected}' in unified config, got {chain_names:?}"
            );
        }
    }

    #[test]
    fn nftables_config_invalid_match_expr_surfaces_early() {
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip saddr 999.999.999.999/24".into()),
                action: Some("accept".into()),
            }],
        };
        let err = topology_to_nftables_config(Some(&fw), None).unwrap_err();
        assert!(
            err.to_string().contains("invalid IPv4 CIDR"),
            "want validation error from up-front match_expr check, got: {err}"
        );
    }

    #[test]
    fn nftables_config_invalid_nat_cidr_surfaces_via_validate() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Masquerade,
                src: Some("not-a-cidr".into()),
                dst: None,
                target: None,
                target_port: None,
            }],
        };
        let err = validate_nat_rule_literals(&nat).unwrap_err();
        assert!(
            err.to_string().contains("invalid src CIDR"),
            "want CIDR error from validate, got: {err}"
        );
    }

    #[test]
    fn nftables_config_invalid_nat_target_surfaces_via_validate() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Snat,
                src: Some("10.0.0.0/24".into()),
                dst: None,
                target: Some("not-an-ip".into()),
                target_port: None,
            }],
        };
        let err = validate_nat_rule_literals(&nat).unwrap_err();
        assert!(
            err.to_string().contains("invalid NAT target"),
            "want target error from validate, got: {err}"
        );
    }

    // ── Plan 158e Slice 1: topology_to_network_config tests ─────────

    #[test]
    fn network_config_empty_node_produces_empty_config() {
        let topo = crate::parser::parse(
            r#"lab "t"
node alone
"#,
        )
        .unwrap();
        let cfg = topology_to_network_config("alone", &topo.nodes["alone"], &topo, None).unwrap();
        assert!(cfg.addresses().is_empty(), "no addresses expected");
        assert!(cfg.routes().is_empty(), "no routes expected");
    }

    #[test]
    fn network_config_link_addresses_appear_per_node() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let cfg_a = topology_to_network_config("a", &topo.nodes["a"], &topo, None).unwrap();
        let cfg_b = topology_to_network_config("b", &topo.nodes["b"], &topo, None).unwrap();
        assert_eq!(cfg_a.addresses().len(), 1, "a gets one address");
        assert_eq!(cfg_b.addresses().len(), 1, "b gets one address");
    }

    #[test]
    fn network_config_default_route_translates_to_zero_cidr() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host { route default via 10.0.0.1 }
node router
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let cfg = topology_to_network_config("host", &topo.nodes["host"], &topo, None).unwrap();
        let routes = cfg.routes();
        assert_eq!(routes.len(), 1, "exactly one route");
        let dst = routes[0].destination();
        let prefix = routes[0].prefix_len();
        assert_eq!(prefix, 0, "default route has /0 prefix");
        assert!(dst.is_ipv4(), "v4 default via v4 gateway");
    }

    #[test]
    fn network_config_auto_route_merges_with_manual() {
        // Manual route should win on conflict; auto-routes fill gaps.
        let topo = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        let mut node = topo.nodes["a"].clone();
        node.routes.insert(
            "default".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.1".to_string()),
                dev: None,
                metric: None,
            },
        );
        let mut autos = BTreeMap::new();
        autos.insert(
            "default".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.99".to_string()),
                dev: None,
                metric: None,
            },
        );
        autos.insert(
            "10.99.0.0/16".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.50".to_string()),
                dev: None,
                metric: None,
            },
        );
        let cfg = topology_to_network_config("a", &node, &topo, Some(&autos)).unwrap();
        assert_eq!(
            cfg.routes().len(),
            2,
            "manual default + auto 10.99.0.0/16 (auto default suppressed)"
        );
    }

    #[test]
    fn network_config_dummy_iface_appears_as_link() {
        // Build the topology programmatically — NLL surfaces `dummy
        // NAME { ... }` as a top-level node property, not the
        // generic `interface { kind dummy }` shape.
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "lo0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Dummy),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"lo0"),
            "expected 'lo0' in declared links, got {names:?}"
        );
    }

    #[test]
    fn network_config_bond_with_members_emits_master_links() {
        // The bond iface declares one link; each member declares
        // another link with `.master(bond_name)` set.
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "bond0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Bond),
                members: vec!["eth0".into(), "eth1".into()],
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(names.contains(&"bond0"), "expected 'bond0', got {names:?}");
        assert!(
            names.contains(&"eth0") && names.contains(&"eth1"),
            "expected bond members 'eth0' + 'eth1' declared for master assignment, got {names:?}"
        );
    }

    #[test]
    fn network_config_vlan_iface_declares_parent_and_vid() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "eth0.42".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: Some("eth0".into()),
                vni: Some(42),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"eth0.42"),
            "expected 'eth0.42' vlan link, got {names:?}"
        );
    }

    #[test]
    fn network_config_vlan_parent_dummy_declared_first_regardless_of_hashmap_order() {
        // Plan 158e polish — `node.interfaces` is a BTreeMap, so the
        // raw iteration order can put the VLAN child before the
        // parent Dummy. nlink's apply iterates links_to_add in
        // declaration order, so a VLAN declared before its parent
        // would fail with ENODEV at the kernel.
        //
        // The two-pass shape inside `topology_to_network_config`
        // guarantees the Dummy is declared in pass 1 and the VLAN in
        // pass 2. Verify that order shows up in
        // `cfg.links().iter()` for the worst-case hashing — try
        // multiple parent/vlan name pairs to defeat any single
        // hash-seed alignment.
        for (parent_name, vlan_name) in &[
            ("eth0", "eth0.42"),
            ("aaa", "zzz.7"),
            ("zzz", "aaa.99"),
            ("p", "v"),
        ] {
            let topo = crate::parser::parse(
                r#"lab "t"
node host
"#,
            )
            .unwrap();
            let mut node = topo.nodes["host"].clone();
            node.interfaces.insert(
                (*parent_name).into(),
                crate::types::InterfaceConfig {
                    kind: Some(crate::types::InterfaceKind::Dummy),
                    ..Default::default()
                },
            );
            node.interfaces.insert(
                (*vlan_name).into(),
                crate::types::InterfaceConfig {
                    kind: Some(crate::types::InterfaceKind::Vlan),
                    parent: Some((*parent_name).into()),
                    vni: Some(42),
                    ..Default::default()
                },
            );
            let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
            let positions: Vec<usize> = cfg
                .links()
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    if l.name() == *parent_name || l.name() == *vlan_name {
                        Some((i, l.name()))
                    } else {
                        None
                    }
                })
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                positions.len(),
                2,
                "expected both '{parent_name}' and '{vlan_name}' in cfg.links() for case ({parent_name}, {vlan_name}), got {} link(s)",
                positions.len()
            );
            let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
            let parent_idx = names.iter().position(|n| *n == *parent_name).unwrap();
            let vlan_idx = names.iter().position(|n| *n == *vlan_name).unwrap();
            assert!(
                parent_idx < vlan_idx,
                "VLAN '{vlan_name}' must come after parent '{parent_name}' in links order, \
                 got parent@{parent_idx} vlan@{vlan_idx}: {names:?}"
            );
        }
    }

    #[test]
    fn network_config_vlan_missing_parent_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "v0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: None,
                vni: Some(10),
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing parent"),
            "want 'missing parent' error, got: {err}"
        );
    }

    #[test]
    fn network_config_vlan_missing_vid_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "v0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: Some("eth0".into()),
                vni: None,
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing vni"),
            "want 'missing vni' error, got: {err}"
        );
    }

    /// Plan 159a Slice 4 — VRF declared at the link level with
    /// `LinkBuilder::vrf(table)` (upstream Plan 190 §2.3).
    #[test]
    fn network_config_vrf_declares_link_with_table() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.vrfs.insert(
            "vrf-blue".into(),
            crate::types::VrfConfig {
                table: 100,
                interfaces: vec![],
                routes: BTreeMap::new(),
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"vrf-blue"),
            "expected 'vrf-blue' in declared links, got {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VRF enslave runs in pass 3, so the
    /// declared `links_to_add` lists the VRF strictly before any
    /// enslave entries referencing it. Defeats BTreeMap iteration
    /// order over `node.vrfs.interfaces`.
    #[test]
    fn network_config_vrf_master_enslave_after_vrf_link() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.vrfs.insert(
            "vrf-blue".into(),
            crate::types::VrfConfig {
                table: 100,
                interfaces: vec!["eth0".into(), "eth1".into()],
                routes: BTreeMap::new(),
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        let vrf_pos = names.iter().position(|n| *n == "vrf-blue").unwrap();
        let eth0_pos = names.iter().position(|n| *n == "eth0").unwrap();
        let eth1_pos = names.iter().position(|n| *n == "eth1").unwrap();
        assert!(
            vrf_pos < eth0_pos,
            "VRF link must be declared before enslaved 'eth0'; \
             got order {names:?}"
        );
        assert!(
            vrf_pos < eth1_pos,
            "VRF link must be declared before enslaved 'eth1'; \
             got order {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN declared via
    /// `LinkBuilder::vxlan` + `vxlan_local` + `vxlan_remote` +
    /// `vxlan_port` (upstream Plan 190 §2.1).
    #[test]
    fn network_config_vxlan_declares_with_local_remote_port() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx100".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: Some(100),
                local: Some("10.0.0.1".into()),
                remote: Some("10.0.0.2".into()),
                port: Some(4789),
                mtu: Some(1450),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"vx100"),
            "expected 'vx100' VXLAN link declared, got {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN missing VNI produces an
    /// `InvalidTopology` error before any kernel call.
    #[test]
    fn network_config_vxlan_missing_vni_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: None,
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing vni"),
            "want 'missing vni' error, got: {err}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN with a bad IPv4 literal in
    /// `local` errors out at config-build time, not deploy time.
    #[test]
    fn network_config_vxlan_bad_local_addr_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: Some(42),
                local: Some("not-an-ip".into()),
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("bad vxlan local address"),
            "want 'bad vxlan local address' error, got: {rendered}"
        );
    }

    /// Plan 159a Phase 2 — `build_wg_public_key_map` decodes
    /// the explicit base64 private key and returns the
    /// deterministic public key.
    #[cfg(feature = "wireguard")]
    #[test]
    fn build_wg_public_key_map_decodes_explicit_key() {
        // Generate a known key once.
        let priv_key = generate_wg_private_key().unwrap();
        let pub_key = derive_wg_public_key(&priv_key);
        let b64 = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(priv_key)
        };

        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: Some(b64),
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.0.0.1/24".into()],
                peers: vec![],
            },
        );
        let mut topo = topo;
        topo.nodes.insert("host".into(), node);

        let map = build_wg_public_key_map(&topo).unwrap();
        let (got_priv, got_pub) = map["host"]["wg0"];
        assert_eq!(got_priv, priv_key);
        assert_eq!(got_pub, pub_key);
    }

    /// Plan 159a Phase 2 — bad base64 in `private_key` surfaces
    /// as `InvalidTopology` before any kernel call.
    #[cfg(feature = "wireguard")]
    #[test]
    fn build_wg_public_key_map_bad_key_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: Some("not-base64!@#".into()),
                listen_port: None,
                fwmark: None,
                addresses: vec![],
                peers: vec![],
            },
        );
        let mut topo = topo;
        topo.nodes.insert("host".into(), node);

        let err = build_wg_public_key_map(&topo).unwrap_err();
        assert!(matches!(err, Error::InvalidTopology(_)));
    }

    /// Plan 159a Phase 2 — `topology_to_wireguard_config`
    /// declares one device per WG iface and resolves peer
    /// cross-references to the right public key.
    #[cfg(feature = "wireguard")]
    #[test]
    fn topology_to_wireguard_config_declares_devices_and_peers() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
"#,
        )
        .unwrap();
        let mut topo = topo;

        let mut a = topo.nodes["a"].clone();
        a.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.99.0.1/24".into()],
                peers: vec!["b".into()],
            },
        );
        topo.nodes.insert("a".into(), a);

        let mut b = topo.nodes["b"].clone();
        b.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51821),
                fwmark: None,
                addresses: vec!["10.99.0.2/24".into()],
                peers: vec!["a".into()],
            },
        );
        topo.nodes.insert("b".into(), b);

        let keys = build_wg_public_key_map(&topo).unwrap();
        let cfg = topology_to_wireguard_config("a", &topo.nodes["a"], &topo, &keys).unwrap();
        assert_eq!(cfg.devices().len(), 1, "expected 1 WG device for node 'a'");
        let device = &cfg.devices()[0];
        assert_eq!(device.ifname, "wg0");
        // Peer should reference 'b'.wg0's public key.
        let expected_peer_pub = keys["b"]["wg0"].1;
        let has_expected_peer = device
            .peers
            .iter()
            .any(|p| p.public_key == expected_peer_pub);
        assert!(
            has_expected_peer,
            "expected peer with b.wg0's public key in cfg",
        );
    }

    /// Plan 159a Phase 2 — peer reference to a node with no WG
    /// surfaces as `InvalidTopology`.
    #[cfg(feature = "wireguard")]
    #[test]
    fn topology_to_wireguard_config_unknown_peer_node_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
"#,
        )
        .unwrap();
        let mut topo = topo;

        let mut a = topo.nodes["a"].clone();
        a.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.99.0.1/24".into()],
                peers: vec!["b".into()],
            },
        );
        topo.nodes.insert("a".into(), a);
        // b has no WG config — peer-from-a references it.

        let keys = build_wg_public_key_map(&topo).unwrap();
        let err = topology_to_wireguard_config("a", &topo.nodes["a"], &topo, &keys).unwrap_err();
        assert!(matches!(err, Error::InvalidTopology(_)));
    }

    #[test]
    fn network_config_invalid_address_surfaces() {
        let mut topo = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        // Inject an invalid address bypassing the parser, mimicking
        // what would happen if a future feature flowed bad data.
        let mut node = topo.nodes["a"].clone();
        node.interfaces.insert(
            "eth0".to_string(),
            crate::types::InterfaceConfig {
                addresses: vec!["not-a-cidr".to_string()],
                ..Default::default()
            },
        );
        topo.nodes.insert("a".to_string(), node);
        let err = topology_to_network_config("a", &topo.nodes["a"], &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("invalid address"),
            "want address validation error, got: {err}"
        );
    }
}
