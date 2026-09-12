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

            for (peer_wg_name, peer_wg_config) in &peer_node.wireguard {
                if !peer_wg_config.peers.iter().any(|p| p == node_name) {
                    continue;
                }
                let (_peer_priv, peer_pub) = peer_keys.get(peer_wg_name).ok_or_else(|| {
                    Error::deploy_failed(format!(
                        "missing public key for '{peer_node_name}'.{peer_wg_name}"
                    ))
                })?;

                let endpoint = peer_wg_config.listen_port.and_then(|port| {
                    find_peer_endpoint(topology, peer_node_name)
                        .map(|addr| std::net::SocketAddr::new(addr, port))
                });

                let mut allowed_ips = Vec::new();
                for addr_str in &peer_wg_config.addresses {
                    if let Ok((ip, prefix)) = parse_cidr(addr_str) {
                        let allowed = match ip {
                            IpAddr::V4(v4) => AllowedIp::v4(v4, prefix),
                            IpAddr::V6(v6) => AllowedIp::v6(v6, prefix),
                        };
                        allowed_ips.push(allowed);
                    }
                }

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
