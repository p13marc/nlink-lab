//! Planners — pure functions from the topology to declarative configs.
//!
//! Nothing in here touches the kernel; every module is unit-tested
//! without root. `super` (the deployer) and the apply path consume them.

pub(crate) mod network;
pub(crate) mod nftables;
pub(crate) mod process;
pub(crate) mod qdisc;
pub(crate) mod topology;
pub(crate) mod wireguard;

use crate::deploy::op::{Op, Plan, StackConfig};
use crate::error::Result;
use crate::types::{DnsMode, EndpointRef, Topology};

/// Inputs a plan needs that are not derivable from the topology
/// alone: WireGuard key material (random, or persisted from a previous
/// deploy so `apply` keeps peers' keys stable).
#[derive(Debug, Default)]
pub struct PlanInputs {
    #[cfg(feature = "wireguard")]
    pub wg_keys: Option<wireguard::WgKeys>,
}

impl PlanInputs {
    /// Inputs for a fresh deploy: generate WireGuard keys now.
    pub fn for_deploy(topology: &Topology) -> Result<Self> {
        #[cfg(feature = "wireguard")]
        {
            let has_wg = topology.nodes.values().any(|n| !n.wireguard.is_empty());
            Ok(Self {
                wg_keys: if has_wg {
                    Some(wireguard::build_wg_public_key_map(topology)?)
                } else {
                    None
                },
            })
        }
        #[cfg(not(feature = "wireguard"))]
        {
            let _ = topology;
            Ok(Self {})
        }
    }
}

/// The complete plan for `topology`: every op a deploy performs, in
/// stage order. Pure — nothing here touches the kernel.
pub fn plan(topology: &Topology, inputs: &PlanInputs) -> Result<Plan> {
    #[cfg(not(feature = "wireguard"))]
    if topology.nodes.values().any(|n| !n.wireguard.is_empty()) {
        return Err(crate::Error::deploy_failed(
            "topology uses WireGuard but the 'wireguard' feature is not enabled. \
             Rebuild with: cargo build --features wireguard",
        ));
    }

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

    let mut ops = topology::plan_topology(topology, &dns_extra_hosts)?;

    // ── per-node declarative stack ──
    let auto_routes = if topology.lab.routing == crate::types::RoutingMode::Auto {
        network::auto_generate_routes(topology)
    } else {
        Default::default()
    };
    for (node_name, node) in &topology.nodes {
        let net = network::topology_to_network_config(
            node_name,
            node,
            topology,
            auto_routes.get(node_name),
        )?;
        #[cfg(feature = "wireguard")]
        let wg = match (&inputs.wg_keys, node.wireguard.is_empty()) {
            (Some(keys), false) => Some(wireguard::topology_to_wireguard_config(
                node_name, node, topology, keys,
            )?),
            _ => None,
        };
        #[cfg(not(feature = "wireguard"))]
        let _ = inputs;
        ops.push(Op::Stack {
            node: node_name.clone(),
            cfg: Box::new(StackConfig {
                network: net,
                firewall: topology.effective_firewall(node).cloned(),
                nat: node.nat.clone(),
                #[cfg(feature = "wireguard")]
                wireguard: wg,
            }),
        });
    }

    // ── traffic control ──
    for (endpoint, impairment) in &topology.impairments {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| crate::Error::InvalidEndpoint {
            endpoint: endpoint.clone(),
        })?;
        ops.push(Op::Netem {
            node: ep.node,
            iface: ep.iface,
            impairment: impairment.clone(),
        });
    }
    if topology
        .networks
        .values()
        .any(|n| !n.impairments.is_empty())
    {
        ops.push(Op::NetworkImpairments);
    }
    for (endpoint, limit) in &topology.rate_limits {
        if topology.impairments.contains_key(endpoint) {
            tracing::warn!(
                "rate limit on '{endpoint}' skipped: netem impairment already configured (use impairment.rate instead)"
            );
            continue;
        }
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| crate::Error::InvalidEndpoint {
            endpoint: endpoint.clone(),
        })?;
        ops.push(Op::RateLimit {
            node: ep.node,
            iface: ep.iface,
            limit: limit.clone(),
        });
    }

    // ── processes, dependency-ordered ──
    for node_name in process::topo_sort_nodes(&topology.nodes) {
        let node = &topology.nodes[&node_name];
        if let Some(delay) = &node.startup_delay {
            ops.push(Op::StartupDelay {
                node: node_name.clone(),
                delay: delay.clone(),
            });
        }
        for (index, exec) in node.exec.iter().enumerate() {
            if exec.cmd.is_empty() {
                continue;
            }
            ops.push(Op::Exec {
                node: node_name.clone(),
                index,
                exec: exec.clone(),
            });
        }
        if let Some(cmd) = &node.healthcheck {
            ops.push(Op::Healthcheck {
                node: node_name.clone(),
                cmd: cmd.clone(),
                interval: node.healthcheck_interval.clone(),
                timeout: node.healthcheck_timeout.clone(),
            });
        }
    }

    // ── wifi daemons ──
    for (node_name, node) in &topology.nodes {
        for wifi in &node.wifi {
            ops.push(Op::WifiDaemon {
                node: node_name.clone(),
                wifi: wifi.clone(),
            });
        }
    }

    Ok(Plan { ops }.sorted())
}
