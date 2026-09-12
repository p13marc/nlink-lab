//! Planners — pure functions from the topology to declarative configs.
//!
//! Nothing in here touches the kernel; every module is unit-tested
//! without root. `super` (the deployer) and the apply path consume them.

pub(crate) mod network;
pub(crate) mod nftables;
pub(crate) mod process;
pub(crate) mod qdisc;
pub(crate) mod topology;
#[cfg(feature = "wireguard")]
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
        ops.extend(network::vrf_route_ops(node_name, node)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::op::Stage;

    fn topo(src: &str) -> Topology {
        crate::parser::parse(src).unwrap()
    }

    fn plan_of(src: &str) -> Plan {
        let t = topo(src);
        plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap()
    }

    const SIMPLE: &str = r#"lab "t"
profile router { forward ipv4 }
node r : router
node h { route default via 10.0.0.1 }
link r:eth0 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms }
"#;

    #[test]
    fn plan_is_deterministic_and_stage_ordered() {
        let a = plan_of(SIMPLE);
        let b = plan_of(SIMPLE);
        assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops));
        let stages: Vec<Stage> = a.ops.iter().map(|o| o.stage()).collect();
        let mut sorted = stages.clone();
        sorted.sort();
        assert_eq!(stages, sorted, "ops must be in stage order");
        let keys: std::collections::BTreeSet<String> = a.ops.iter().map(|o| o.key()).collect();
        assert_eq!(
            keys.len(),
            a.ops.len(),
            "op keys must be unique: {:?}",
            a.ops
        );
    }

    #[test]
    fn plan_covers_every_layer_of_a_simple_lab() {
        let p = plan_of(SIMPLE);
        let d: Vec<String> = p.ops.iter().map(|o| o.describe()).collect();
        assert!(
            d.iter().any(|s| s.contains("create namespace t-r")),
            "{d:?}"
        );
        assert!(d.iter().any(|s| s.contains("veth r:eth0 ↔ h:eth0")));
        assert!(d.iter().any(|s| s.starts_with("r: 1 sysctl")));
        assert_eq!(p.stage(Stage::Stack).count(), 2);
        assert!(d.iter().any(|s| s == "netem on r:eth0"));
        assert!(d.iter().any(|s| s == "netem on h:eth0"));
    }

    #[test]
    fn every_example_plans() {
        for entry in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples")).unwrap()
        {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "nll") {
                let t = crate::parser::parse_file(&path).unwrap();
                // WireGuard topologies need the feature to plan at all
                if !cfg!(feature = "wireguard") && t.nodes.values().any(|n| !n.wireguard.is_empty())
                {
                    continue;
                }
                let inputs = PlanInputs::for_deploy(&t).unwrap();
                let p = plan(&t, &inputs).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert!(!p.ops.is_empty(), "{}", path.display());
            }
        }
    }

    #[test]
    fn diff_identical_plans_is_reconcile_only() {
        let a = plan_of(SIMPLE);
        let d = Plan::diff(&a, &a);
        assert!(d.ops.iter().all(|o| !o.is_removal()), "{:?}", d.ops);
        assert!(
            d.ops
                .iter()
                .all(|o| matches!(o, Op::Stack { .. } | Op::LinksUp { .. })),
            "only idempotent reconcile ops expected: {:?}",
            d.ops
        );
    }

    #[test]
    fn diff_added_node_creates_only_the_new_pieces() {
        let cur = plan_of(SIMPLE);
        let des = plan_of(&format!(
            "{SIMPLE}\nnode x\nlink r:eth1 -- x:eth0 {{ 10.1.0.1/30 -- 10.1.0.2/30 }}\n"
        ));
        let d = Plan::diff(&cur, &des);
        let desc: Vec<String> = d.ops.iter().map(|o| o.describe()).collect();
        assert!(
            desc.iter().any(|s| s.contains("create namespace t-x")),
            "{desc:?}"
        );
        assert!(desc.iter().any(|s| s.contains("veth r:eth1 ↔ x:eth0")));
        assert!(
            !desc.iter().any(|s| s.contains("veth r:eth0 ↔ h:eth0")),
            "unchanged link recreated: {desc:?}"
        );
        assert!(!d.ops.iter().any(|o| o.is_removal()));
        // removals (none) come first, then stage order
        let stages: Vec<Stage> = d.ops.iter().map(|o| o.stage()).collect();
        let mut sorted = stages.clone();
        sorted.sort();
        assert_eq!(stages, sorted);
    }

    #[test]
    fn diff_removed_node_deletes_it_and_skips_its_links() {
        let cur = plan_of(&format!(
            "{SIMPLE}\nnode x\nlink r:eth1 -- x:eth0 {{ 10.1.0.1/30 -- 10.1.0.2/30 }}\nimpair x:eth0 delay 1ms\n"
        ));
        let des = plan_of(SIMPLE);
        let d = Plan::diff(&cur, &des);
        let removals: Vec<&Op> = d.ops.iter().filter(|o| o.is_removal()).collect();
        assert!(
            removals
                .iter()
                .any(|o| matches!(o, Op::DeleteNamespace { ns, .. } if ns == "t-x")),
            "{removals:?}"
        );
        // the veth's r-side end is deleted (x's namespace goes away with its end)
        assert!(removals.iter().any(
            |o| matches!(o, Op::DeleteLink { node, iface } if node == "r" && iface == "eth1")
        ));
        // but nothing *inside* the dying namespace is touched individually
        assert!(
            !removals
                .iter()
                .any(|o| matches!(o, Op::ClearQdisc { node, .. } if node == "x")),
            "{removals:?}"
        );
        // removals precede additions
        let first_non_removal = d
            .ops
            .iter()
            .position(|o| !o.is_removal())
            .unwrap_or(d.ops.len());
        assert!(d.ops[..first_non_removal].iter().all(|o| o.is_removal()));
    }

    #[test]
    fn diff_changed_link_mtu_recreates_the_veth() {
        let cur = plan_of(SIMPLE);
        let des = plan_of(&SIMPLE.replace("delay 5ms", "delay 5ms  mtu 1400"));
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops.iter().any(
                |o| matches!(o, Op::DeleteLink { node, iface } if node == "r" && iface == "eth0")
            ),
            "{:?}",
            d.ops
        );
        assert!(d.ops.iter().any(|o| matches!(
            o,
            Op::CreateVeth {
                mtu: Some(1400),
                ..
            }
        )));
    }

    #[test]
    fn diff_removed_impairment_clears_the_qdisc() {
        let cur = plan_of(SIMPLE);
        let des = plan_of(&SIMPLE.replace("  delay 5ms", ""));
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops.iter().any(
                |o| matches!(o, Op::ClearQdisc { node, iface } if node == "r" && iface == "eth0")
            ),
            "{:?}",
            d.ops
        );
    }

    const VRF: &str = r#"lab "v"
profile router { forward ipv4 }
node pe : router {
  vrf red table 10 {
    interfaces [eth1]
    route default via 10.10.0.10
    route 192.168.5.0/24 via 10.10.0.10 metric 50
  }
}
node a { route default via 10.10.0.1 }
link pe:eth1 -- a:eth0 { 10.10.0.1/24 -- 10.10.0.10/24 }
"#;

    #[test]
    fn plan_emits_vrf_routes_as_ops_after_the_stack() {
        let p = plan_of(VRF);
        let routes: Vec<&Op> = p.stage(Stage::Routes).collect();
        assert_eq!(routes.len(), 2, "{routes:?}");
        assert!(routes.iter().all(|o| matches!(
            o,
            Op::Route { node, route } if node == "pe" && route.table == 10
        )));
        let d: Vec<String> = routes.iter().map(|o| o.describe()).collect();
        assert!(
            d.contains(&"pe: route 0.0.0.0/0 table 10 via 10.10.0.10".to_string()),
            "{d:?}"
        );
        assert!(
            d.contains(&"pe: route 192.168.5.0/24 table 10 via 10.10.0.10 metric 50".to_string()),
            "{d:?}"
        );
        // the stack no longer declares them
        let stack_pos = p
            .ops
            .iter()
            .position(|o| matches!(o, Op::Stack { node, .. } if node == "pe"))
            .unwrap();
        let route_pos = p
            .ops
            .iter()
            .position(|o| matches!(o, Op::Route { .. }))
            .unwrap();
        assert!(stack_pos < route_pos);
        if let Op::Stack { cfg, .. } = &p.ops[stack_pos] {
            assert!(
                cfg.network
                    .routes()
                    .iter()
                    .all(|r| r.table().is_none_or(|t| t == 254))
            );
        }
    }

    #[test]
    fn diff_removed_vrf_route_emits_del_route() {
        let cur = plan_of(VRF);
        let des = plan_of(&VRF.replace(
            "    route 192.168.5.0/24 via 10.10.0.10 metric 50
",
            "",
        ));
        let d = Plan::diff(&cur, &des);
        let dels: Vec<&Op> = d
            .ops
            .iter()
            .filter(|o| matches!(o, Op::DelRoute { .. }))
            .collect();
        assert_eq!(dels.len(), 1, "{:?}", d.ops);
        assert_eq!(
            dels[0].describe(),
            "pe: delete route 192.168.5.0/24 table 10 via 10.10.0.10 metric 50"
        );
        assert!(
            !d.ops.iter().any(|o| matches!(o, Op::Route { .. })),
            "unchanged routes must not be re-added: {:?}",
            d.ops
        );
    }

    #[test]
    fn diff_changed_vrf_route_metric_deletes_then_replaces() {
        let cur = plan_of(VRF);
        let des = plan_of(&VRF.replace("metric 50", "metric 60"));
        let d = Plan::diff(&cur, &des);
        let del = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::DelRoute { route, .. } if route.metric == Some(50)));
        let add = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::Route { route, .. } if route.metric == Some(60)));
        assert!(del.is_some() && add.is_some(), "{:?}", d.ops);
        assert!(del < add, "delete must precede the replacement");
    }

    #[test]
    fn diff_removed_vrf_node_skips_del_route() {
        let cur = plan_of(VRF);
        let des = plan_of(
            &VRF.replace(
                "node pe : router {
  vrf red table 10 {
    interfaces [eth1]
    route default via 10.10.0.10
    route 192.168.5.0/24 via 10.10.0.10 metric 50
  }
}
",
                "node pe : router
",
            )
            .replace(
                "link pe:eth1 -- a:eth0 { 10.10.0.1/24 -- 10.10.0.10/24 }
",
                "",
            ),
        );
        // pe still exists but the link and the VRF are gone: DelRoute ops
        // are emitted (pe is not dying)
        let d = Plan::diff(&cur, &des);
        assert_eq!(
            d.ops
                .iter()
                .filter(|o| matches!(o, Op::DelRoute { .. }))
                .count(),
            2,
            "{:?}",
            d.ops
        );
        // whereas a node that disappears entirely takes its routes with it
        let des2 = plan_of("lab \"v\"\nnode a { route default via 10.10.0.1 }\n");
        let d2 = Plan::diff(&cur, &des2);
        assert!(
            !d2.ops.iter().any(|o| matches!(o, Op::DelRoute { .. })),
            "{:?}",
            d2.ops
        );
        assert!(
            d2.ops
                .iter()
                .any(|o| matches!(o, Op::DeleteNamespace { ns, .. } if ns == "v-pe"))
        );
    }

    #[test]
    fn vrf_route_with_mismatched_family_is_a_plan_error() {
        // `default via <v6>` is a valid v6 default route; a v4 destination
        // with a v6 gateway is the mismatch.
        let t = topo(&VRF.replace(
            "route 192.168.5.0/24 via 10.10.0.10 metric 50",
            "route 192.168.5.0/24 via fd00::1",
        ));
        let err = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("different address families"),
            "{err}"
        );
    }

    #[test]
    fn diff_edited_background_exec_kills_then_reexecs() {
        let src = format!(
            "{SIMPLE}\nnode s {{ run [\"sleep\", \"1000\"] background  healthcheck \"true\" }}\n"
        );
        let cur = plan_of(&src);
        let des = plan_of(&src.replace("1000", "999"));
        let d = Plan::diff(&cur, &des);
        let kill = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::KillExec { node, index: 0 } if node == "s"));
        let exec = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::Exec { node, .. } if node == "s"));
        assert!(kill.is_some() && exec.is_some(), "{:?}", d.ops);
        assert!(kill < exec, "stop must precede the re-exec");
        assert!(
            d.ops
                .iter()
                .any(|o| matches!(o, Op::Healthcheck { node, .. } if node == "s")),
            "healthcheck must re-run after a restart: {:?}",
            d.ops
        );
        assert!(
            !d.ops
                .iter()
                .any(|o| matches!(o, Op::KillNodeProcesses { .. })),
            "a targeted kill, never the node-wide one: {:?}",
            d.ops
        );
        // unchanged: nothing on the process layer
        let same = Plan::diff(&cur, &plan_of(&src));
        assert!(
            !same.ops.iter().any(|o| o.stage() == Stage::Processes),
            "{:?}",
            same.ops
        );
    }

    #[test]
    fn diff_removed_exec_line_stops_only_that_process() {
        let src = format!(
            "{SIMPLE}\nnode s {{ run [\"sleep\", \"1000\"] background  run [\"sleep\", \"2000\"] background }}\n"
        );
        let cur = plan_of(&src);
        let des = plan_of(&src.replace("  run [\"sleep\", \"2000\"] background", ""));
        let d = Plan::diff(&cur, &des);
        let kills: Vec<&Op> = d
            .ops
            .iter()
            .filter(|o| matches!(o, Op::KillExec { .. }))
            .collect();
        assert_eq!(kills.len(), 1, "{:?}", d.ops);
        assert!(matches!(kills[0], Op::KillExec { node, index: 1 } if node == "s"));
        assert!(!d.ops.iter().any(|o| matches!(o, Op::Exec { .. })));
    }

    #[test]
    fn diff_one_shot_process_ops_only_run_for_new_nodes() {
        let src = format!("{SIMPLE}\nnode s {{ run [\"sleep\", \"1\"] background }}\n");
        let cur = plan_of(&src);
        let des = plan_of(&format!(
            "{src}\nnode s2 {{ run [\"sleep\", \"2\"] background }}\n"
        ));
        let d = Plan::diff(&cur, &des);
        let execs: Vec<&Op> = d
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Exec { .. }))
            .collect();
        assert_eq!(execs.len(), 1, "{execs:?}");
        assert!(matches!(execs[0], Op::Exec { node, .. } if node == "s2"));
    }
}
