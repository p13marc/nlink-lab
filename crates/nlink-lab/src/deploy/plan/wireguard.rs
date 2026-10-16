//! Pure planner: WireGuard key material and `WireguardConfig` per node (Plan 159a).

use crate::error::{Error, Result};
use crate::helpers::parse_cidr;
use crate::types::EndpointRef;
use std::collections::BTreeMap;
use std::net::IpAddr;

#[cfg(feature = "wireguard")]
/// Generate a random WireGuard private key.
pub(crate) fn generate_wg_private_key() -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|e| Error::deploy_failed(format!("failed to generate WireGuard key: {e}")))?;
    // Clamp per Curve25519 convention
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
    Ok(key)
}

#[cfg(feature = "wireguard")]
/// Derive a WireGuard public key from a private key.
pub(crate) fn derive_wg_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let secret = x25519_dalek::StaticSecret::from(*private_key);
    let public = x25519_dalek::PublicKey::from(&secret);
    public.to_bytes()
}

#[cfg(feature = "wireguard")]
/// Decode a base64-encoded WireGuard key.
pub(crate) fn decode_wg_key(s: &str) -> std::result::Result<[u8; 32], String> {
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

/// The address `peer` has on a link or network it shares with `node`: the
/// underlay a tunnel between them runs over (#147). A tunnel's own
/// interfaces are never links or network members, so they are not
/// candidates; neither is an address on another segment of the peer,
/// which may be one the tunnel carries.
#[cfg(feature = "wireguard")]
pub(crate) fn shared_segment_endpoint(
    topology: &crate::types::Topology,
    node: &str,
    peer: &str,
) -> Option<IpAddr> {
    for link in &topology.links {
        let eps: Vec<Option<EndpointRef>> = link
            .endpoints
            .iter()
            .map(|e| EndpointRef::parse(e))
            .collect();
        let (Some(Some(a)), Some(Some(b))) = (eps.first(), eps.get(1)) else {
            continue;
        };
        let peer_side = match (a.node.as_str(), b.node.as_str()) {
            (x, y) if x == node && y == peer => 1,
            (x, y) if x == peer && y == node => 0,
            _ => continue,
        };
        if let Some(addresses) = &link.addresses
            && let Some(addr) = addresses.get(peer_side)
            && let Ok((ip, _)) = parse_cidr(addr)
        {
            return Some(ip);
        }
    }
    for network in topology.networks.values() {
        let on = |n: &str| {
            network
                .members
                .iter()
                .find(|m| EndpointRef::parse(m).is_some_and(|e| e.node == n))
        };
        if on(node).is_none() {
            continue;
        }
        if let Some(member) = on(peer)
            && let Some(port) = network.ports.get(member)
            && let Some(addr) = port.addresses.first()
            && let Ok((ip, _)) = parse_cidr(addr)
        {
            return Some(ip);
        }
    }
    None
}

/// A longhand peer's `endpoint`: `IP`, `IP:PORT` or `[IPv6]:PORT`, the
/// port defaulting to `default_port`.
#[cfg(feature = "wireguard")]
fn parse_wg_endpoint(s: &str, default_port: Option<u16>) -> Result<std::net::SocketAddr> {
    if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
        return Ok(sa);
    }
    let ip: IpAddr = s
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|e| Error::invalid_topology(format!("WireGuard endpoint '{s}': {e}")))?;
    let port = default_port.ok_or_else(|| {
        Error::invalid_topology(format!(
            "WireGuard endpoint '{s}' has no port, and the peer interface no `listen` port"
        ))
    })?;
    Ok(std::net::SocketAddr::new(ip, port))
}

/// A prefix as an allowed IP: its network address (`default` is
/// `0.0.0.0/0`, a bare address a host route).
#[cfg(feature = "wireguard")]
fn allowed_ip(prefix: &str) -> Option<nlink::netlink::genl::wireguard::AllowedIp> {
    use nlink::netlink::genl::wireguard::AllowedIp;
    let (ip, len) = match prefix {
        "default" => (IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0),
        p => parse_cidr(p).ok().or_else(|| {
            let ip: IpAddr = p.parse().ok()?;
            Some((ip, if ip.is_ipv4() { 32 } else { 128 }))
        })?,
    };
    Some(match crate::helpers::network_address(ip, len) {
        IpAddr::V4(v4) => AllowedIp::v4(v4, len),
        IpAddr::V6(v6) => AllowedIp::v6(v6, len),
    })
}

/// Find a reachable IP address for a peer node (from link or interface addresses).
#[cfg(feature = "wireguard")]
pub(crate) fn find_peer_endpoint(
    topology: &crate::types::Topology,
    peer_name: &str,
) -> Option<IpAddr> {
    // Check link addresses first
    for link in &topology.links {
        if let Some(addresses) = &link.addresses {
            for (i, ep_str) in link.endpoints.iter().enumerate() {
                if let Some(ep) = EndpointRef::parse(ep_str)
                    && ep.node == peer_name
                    && let Ok((ip, _)) = parse_cidr(&addresses[i])
                {
                    return Some(ip);
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

/// Map of `(node, wg_iface) → (private_key, public_key)`.
///
/// Plan 159a Phase 2 — replaces the imperative pass-1 logic in
/// step 10d that generated/decoded keys and called
/// `set_device(private_key + listen_port)` in the same loop. We
/// split key resolution from device application so peer cross-
/// references resolve before any kernel mutation happens.
#[cfg(feature = "wireguard")]
pub(crate) type WgKeys = BTreeMap<String, BTreeMap<String, ([u8; 32], [u8; 32])>>;

#[cfg(feature = "wireguard")]
pub(crate) fn build_wg_public_key_map(topology: &crate::types::Topology) -> Result<WgKeys> {
    let mut out: WgKeys = BTreeMap::new();
    for (node_name, node) in &topology.nodes {
        if node.wireguard.is_empty() {
            continue;
        }
        let mut per_node = BTreeMap::new();
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
            per_node.insert(wg_name.clone(), (private_key, public_key));
        }
        out.insert(node_name.clone(), per_node);
    }
    Ok(out)
}

/// Build a declarative `WireguardConfig` for one node from its
/// `node.wireguard` entries. Plan 159a Phase 2 — replaces the
/// imperative pass-2 `set_device(peer)` loops in step 10d.
///
/// Resolves peer cross-references via the pre-computed
/// `public_keys` map. Each peer's allowed_ips come from the
/// peer's declared WG addresses; the endpoint resolves through
/// `find_peer_endpoint` (existing helper, unchanged).
#[cfg(feature = "wireguard")]
pub(crate) fn topology_to_wireguard_config(
    node_name: &str,
    node: &crate::types::Node,
    topology: &crate::types::Topology,
    public_keys: &WgKeys,
) -> Result<nlink::netlink::genl::wireguard::WireguardConfig> {
    use nlink::netlink::genl::wireguard::{AllowedIp, WireguardConfig};

    let mut cfg = WireguardConfig::new();
    let own_keys = public_keys.get(node_name).ok_or_else(|| {
        Error::deploy_failed(format!(
            "internal: no key map for WireGuard node '{node_name}'"
        ))
    })?;

    for (wg_name, wg_config) in &node.wireguard {
        let (private_key, _own_pub) = own_keys.get(wg_name).ok_or_else(|| {
            Error::deploy_failed(format!("internal: no key for '{node_name}'.{wg_name}"))
        })?;

        // Snapshot the per-peer data before moving into the
        // builder closure (the closure takes `self` by value).
        let mut peer_specs: Vec<([u8; 32], Option<std::net::SocketAddr>, Vec<AllowedIp>)> =
            Vec::new();

        // Every route `node` sends into this tunnel (`route … dev wg0`, in
        // the main table or a VRF), with its gateway.
        let tunnel_routes: Vec<(&String, Option<&String>)> = node
            .routes
            .iter()
            .chain(node.vrfs.values().flat_map(|v| v.routes.iter()))
            .filter(|(_, r)| r.dev.as_deref() == Some(wg_name.as_str()))
            .map(|(dst, r)| (dst, r.via.as_ref()))
            .collect();

        for peer_node_name in &wg_config.peers {
            let peer_keys = public_keys.get(peer_node_name).ok_or_else(|| {
                Error::invalid_topology(format!(
                    "WireGuard peer '{peer_node_name}' referenced by \
                     '{node_name}'.{wg_name} has no WireGuard interfaces"
                ))
            })?;

            let peer_node = topology.nodes.get(peer_node_name).ok_or_else(|| {
                Error::invalid_topology(format!(
                    "WireGuard peer '{peer_node_name}' referenced by \
                     '{node_name}'.{wg_name} is not a topology node"
                ))
            })?;

            // The peer's interfaces that peer back with this node. With
            // several (a tunnel per tenant), the one of the same name is
            // this tunnel's other end; every one of them used to be
            // added, so each tunnel got the other's key too (#147).
            let mut ends: Vec<(&String, &crate::types::WireguardConfig)> = peer_node
                .wireguard
                .iter()
                .filter(|(_, c)| c.peers.iter().any(|p| p == node_name))
                .collect();
            if ends.iter().any(|(n, _)| *n == wg_name) {
                ends.retain(|(n, _)| *n == wg_name);
            }
            let only_peer = wg_config.peers.len() == 1 && ends.len() == 1;
            let longhand = wg_config.peer_settings.get(peer_node_name);

            for (peer_wg_name, peer_wg_config) in ends {
                let (_peer_priv, peer_pub) = peer_keys.get(peer_wg_name).ok_or_else(|| {
                    Error::deploy_failed(format!(
                        "missing public key for '{peer_node_name}'.{peer_wg_name}"
                    ))
                })?;

                // The underlay address both nodes are on, not the peer's
                // first address, which may be inside what the tunnel
                // carries (#147).
                let endpoint = match longhand.and_then(|l| l.endpoint.as_deref()) {
                    Some(ep) => Some(parse_wg_endpoint(ep, peer_wg_config.listen_port)?),
                    None => peer_wg_config.listen_port.and_then(|port| {
                        shared_segment_endpoint(topology, node_name, peer_node_name)
                            .or_else(|| find_peer_endpoint(topology, peer_node_name))
                            .map(|addr| std::net::SocketAddr::new(addr, port))
                    }),
                };

                let mut allowed_ips = Vec::new();
                if let Some(l) = longhand.filter(|l| !l.allowed_ips.is_empty()) {
                    for prefix in &l.allowed_ips {
                        allowed_ips.push(allowed_ip(prefix).ok_or_else(|| {
                            Error::invalid_topology(format!(
                                "WireGuard peer '{peer_node_name}' of '{node_name}'.{wg_name}: \
                                 allowed-ips '{prefix}' is not a prefix"
                            ))
                        })?);
                    }
                } else {
                    // The peer's tunnel addresses, each as a host: with its
                    // interface prefix, the peers of a hub overlapped, and
                    // the kernel keeps a prefix on one peer only (#147).
                    let mut tunnel_ips = Vec::new();
                    for addr_str in &peer_wg_config.addresses {
                        if let Ok((ip, _)) = parse_cidr(addr_str) {
                            tunnel_ips.push(ip);
                            allowed_ips.extend(allowed_ip(&ip.to_string()));
                        }
                    }
                    // And what this node routes into the tunnel towards it:
                    // all of it when the tunnel has this one peer, else
                    // the routes via one of its tunnel addresses. The
                    // traffic a tunnel carries was dropped by cryptokey
                    // routing without them (#147).
                    for (dst, via) in &tunnel_routes {
                        let to_this_peer = match via {
                            Some(via) => {
                                via.parse::<IpAddr>().is_ok_and(|v| tunnel_ips.contains(&v))
                            }
                            None => only_peer,
                        };
                        if to_this_peer && let Some(ai) = allowed_ip(dst) {
                            allowed_ips.push(ai);
                        }
                    }
                }
                // A route to the peer's tunnel address repeats it.
                let mut unique = Vec::with_capacity(allowed_ips.len());
                for ai in allowed_ips {
                    if !unique.contains(&ai) {
                        unique.push(ai);
                    }
                }
                let allowed_ips = unique;

                peer_specs.push((*peer_pub, endpoint, allowed_ips));
            }
        }

        let private_key = *private_key;
        let listen_port = wg_config.listen_port;
        let fwmark = wg_config.fwmark;
        cfg = cfg.device(wg_name.as_str(), move |mut d| {
            d = d.private_key(private_key);
            if let Some(p) = listen_port {
                d = d.listen_port(p);
            }
            if let Some(fw) = fwmark {
                d = d.fwmark(fw);
            }
            for (pubkey, endpoint, allowed_ips) in peer_specs {
                d = d.peer(pubkey, move |mut p| {
                    if let Some(ep) = endpoint {
                        p = p.endpoint(ep);
                    }
                    for ai in allowed_ips {
                        p = p.allowed_ip(ai);
                    }
                    p
                });
            }
            d
        });
    }

    Ok(cfg)
}
