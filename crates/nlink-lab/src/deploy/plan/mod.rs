//! Planners — pure functions from the topology to declarative configs.
//!
//! Nothing in here touches the kernel; every module is unit-tested
//! without root. `super` (the deployer) and the apply path consume them.

pub(crate) mod network;
pub(crate) mod nftables;
pub(crate) mod process;
pub(crate) mod qdisc;
pub(crate) mod tc;
pub(crate) mod topology;
#[cfg(feature = "wireguard")]
pub(crate) mod wireguard;

use crate::deploy::op::{Op, Plan, StackConfig};
use crate::error::Result;
use crate::types::{DnsMode, Topology};

/// Inputs a plan needs that are not derivable from the topology
/// alone: WireGuard key material (random, or persisted from a previous
/// deploy so `apply` keeps peers' keys stable), and the endpoints a
/// runtime `impair --partition` cut off.
#[derive(Debug, Default, Clone)]
pub struct PlanInputs {
    #[cfg(feature = "wireguard")]
    pub wg_keys: Option<wireguard::WgKeys>,
    /// Partitioned endpoints (`node:iface`): their traffic is dropped by
    /// a filter on both hooks, under whatever qdisc they have (#168),
    /// silently for the sender (#347).
    pub partitions: std::collections::BTreeSet<String>,
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
                partitions: Default::default(),
            })
        }
        #[cfg(not(feature = "wireguard"))]
        {
            let _ = topology;
            Ok(Self::default())
        }
    }
}

/// The name the implicit management network has in the plan: not an NLL
/// identifier, so no `network` block can take it.
pub const MGMT_NETWORK: &str = "@mgmt";

/// `topology`, with a `mgmt SUBNET` that is not `host-reachable` as a
/// bridge network of every node's `mgmt0`, numbered as the host-reachable
/// one is (the bridge's address `.1` reserved, nodes from `.2` in name
/// order, which `nlink-lab ip` reports). It was parsed and did nothing:
/// no node got a `mgmt0` (#210).
fn with_mgmt_network(topology: &Topology) -> Result<std::borrow::Cow<'_, Topology>> {
    let Some(subnet) = topology
        .lab
        .mgmt_subnet
        .as_ref()
        .filter(|_| !topology.lab.mgmt_host_reachable)
    else {
        return Ok(std::borrow::Cow::Borrowed(topology));
    };
    let mgmt = crate::helpers::mgmt_addresses(subnet, topology.nodes.len())?;
    let mut net = crate::types::Network {
        kind: Some("bridge".to_string()),
        subnet: Some(subnet.clone()),
        ..Default::default()
    };
    for (idx, node) in topology.nodes.keys().enumerate() {
        let ep = format!("{node}:mgmt0");
        net.members.push(ep.clone());
        net.ports.insert(
            ep,
            crate::types::PortConfig {
                addresses: vec![format!("{}/{}", mgmt.nodes[idx], mgmt.prefix)],
                ..Default::default()
            },
        );
    }
    let mut owned = topology.clone();
    owned.networks.insert(MGMT_NETWORK.to_string(), net);
    Ok(std::borrow::Cow::Owned(owned))
}

/// The complete plan for `topology`: every op a deploy performs, in
/// stage order. Pure — nothing here touches the kernel.
pub fn plan(topology: &Topology, inputs: &PlanInputs) -> Result<Plan> {
    // Routing and DNS see the topology as declared: the management network
    // is out of band. Given to `routing auto` and FRR it was one more
    // segment between every pair of nodes: OSPF ran over it, a BGP
    // neighbour had two segments and failed (#210).
    let declared = topology;
    let topology = &*with_mgmt_network(topology)?;
    #[cfg(not(feature = "wireguard"))]
    if topology.nodes.values().any(|n| !n.wireguard.is_empty()) {
        return Err(crate::Error::deploy_failed(
            "topology uses WireGuard but the 'wireguard' feature is not enabled. \
             Rebuild with: cargo build --features wireguard",
        ));
    }

    let dns_extra_hosts: Vec<String> = if topology.lab.dns == DnsMode::Hosts {
        crate::dns::generate_hosts_entries(declared)
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
    let auto_routes = network::auto_routes_for(declared);
    for (node_name, node) in &topology.nodes {
        let net = network::topology_to_network_config(
            node_name,
            node,
            topology,
            auto_routes.get(node_name),
        )?;
        let net = tc::declare_node_tc(net, node_name, topology, &inputs.partitions)?;
        #[cfg(feature = "wireguard")]
        let wg = match (&inputs.wg_keys, node.wireguard.is_empty()) {
            (Some(keys), false) => Some(wireguard::topology_to_wireguard_config(
                node_name, node, topology, keys,
            )?),
            _ => None,
        };
        ops.push(Op::Stack {
            node: node_name.clone(),
            cfg: Box::new(StackConfig {
                network: net,
                firewall: topology
                    .effective_firewall(node)
                    .map(std::borrow::Cow::into_owned),
                nat: node.nat.clone(),
                #[cfg(feature = "wireguard")]
                wireguard: wg,
            }),
        });
    }

    // ── FRR routing daemons (#65) ──
    for (node_name, node) in &declared.nodes {
        if let Some(cfg) = declared.effective_frr(node) {
            ops.push(Op::FrrDaemons {
                node: node_name.clone(),
                spec: Box::new(crate::frr::build_spec(declared, node_name, node, &cfg)?),
            });
        }
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
        // A container block's `exec "..."` is a one-shot post-start command,
        // i.e. a foreground `run`. It used to parse and then be dropped on the
        // floor (#111). Indices continue after `node.exec` so the `exec:N:I`
        // diff keys stay unique; these are never background, so they never
        // produce a `KillExec`.
        for (offset, cmd) in node.container_exec.iter().enumerate() {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                continue;
            }
            ops.push(Op::Exec {
                node: node_name.clone(),
                index: node.exec.len() + offset,
                exec: crate::types::ExecConfig {
                    cmd: vec!["sh".into(), "-c".into(), cmd.to_string()],
                    background: false,
                },
            });
        }
        if let Some(cmd) = &node.healthcheck {
            ops.push(Op::Healthcheck {
                node: node_name.clone(),
                cmd: cmd.clone(),
                interval: node.healthcheck_interval.clone(),
                timeout: node.healthcheck_timeout.clone(),
                retries: node.healthcheck_retries,
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

    /// A `mgmt` subnet without `host-reachable` gave no node a `mgmt0`
    /// (#210): it is a bridge network now, numbered as `nlink-lab ip`
    /// reports.
    #[test]
    fn a_mgmt_subnet_is_a_bridge_of_every_nodes_mgmt0() {
        let t = crate::parser::parse("lab \"m\" { mgmt 172.20.0.0/24 }\nnode b\nnode a\n").unwrap();
        let p = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap();
        let veths: Vec<(&str, &str)> = p
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::CreateNetworkVeth { node, iface, .. } => Some((node.as_str(), iface.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(veths, vec![("a", "mgmt0"), ("b", "mgmt0")]);
        let net = &with_mgmt_network(&t).unwrap().networks[MGMT_NETWORK];
        assert_eq!(net.ports["a:mgmt0"].addresses, vec!["172.20.0.2/24"]);
        assert_eq!(net.ports["b:mgmt0"].addresses, vec!["172.20.0.3/24"]);
        // routing does not see it: `routing auto` adds no route over mgmt0
        let t = crate::parser::parse(
            "lab \"m\" { mgmt 172.20.0.0/24  routing auto }\nnode a\nnode b\nnode c\n\
             link a:eth0 -- b:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }\n\
             link b:eth1 -- c:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }\n",
        )
        .unwrap();
        let routes = network::auto_routes_for(&t);
        assert!(
            routes
                .values()
                .flat_map(|r| r.values())
                .all(|r| r.via.as_deref().is_none_or(|v| !v.starts_with("172.20."))),
            "{routes:?}"
        );
        // host-reachable keeps its own bridge
        let t = crate::parser::parse("lab \"m\" { mgmt 172.20.0.0/24 host-reachable }\nnode a\n")
            .unwrap();
        assert!(with_mgmt_network(&t).unwrap().networks.is_empty());
    }

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
    fn containers_are_created_in_depends_on_order() {
        // #217: created in name order, `app` before the `db` it depends on
        let p = plan_of(
            r#"lab "t"
node app image "alpine" { depends-on [db] healthcheck "true" { retries 3 } }
node db image "alpine" { healthcheck "true" }
"#,
        );
        let created: Vec<&str> = p
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::CreateContainer { node, .. } => Some(node.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(created, ["db", "app"]);
        let checks: Vec<(&str, Option<u32>)> = p
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Healthcheck { node, retries, .. } => Some((node.as_str(), *retries)),
                _ => None,
            })
            .collect();
        assert_eq!(checks, [("db", None), ("app", Some(3))]);
    }

    #[test]
    fn container_exec_becomes_foreground_exec_ops() {
        // #111: `exec "..."` inside a container block parsed, validated and was
        // then dropped -- the command never ran.
        let p = plan_of(
            r#"lab "t"
node c image "alpine" {
  cmd ["sleep", "1"]
  run ["true"] background
  exec "nginx -t"
  exec "echo second"
}
"#,
        );
        let execs: Vec<(usize, &crate::types::ExecConfig)> = p
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Exec { node, index, exec } if node == "c" => Some((*index, exec)),
                _ => None,
            })
            .collect();
        // one from `run`, two from the container-block `exec`s
        assert_eq!(execs.len(), 3, "{execs:?}");
        let from_exec_key: Vec<_> = execs.iter().filter(|(i, _)| *i >= 1).collect();
        assert_eq!(from_exec_key.len(), 2);
        for (_, e) in &from_exec_key {
            assert!(
                !e.background,
                "container `exec` is a one-shot, never background"
            );
            assert_eq!(e.cmd[0], "sh");
            assert_eq!(e.cmd[1], "-c");
        }
        assert_eq!(from_exec_key[0].1.cmd[2], "nginx -t");
        assert_eq!(from_exec_key[1].1.cmd[2], "echo second");

        // Indices must not collide with `run`'s, or the diff keys clash.
        let keys: std::collections::BTreeSet<String> = p.ops.iter().map(|o| o.key()).collect();
        assert_eq!(keys.len(), p.ops.len(), "op keys must stay unique");
    }

    #[test]
    fn container_config_env_file_and_overlay_reach_create_opts() {
        // #111: all three parsed and were then never read at deploy time.
        let t = topo(
            r#"lab "t"
node c image "alpine" {
  volumes ["/pre:/pre"]
  config "/host/a.conf" "/etc/a.conf"
  config "/host/b.conf" "/etc/b.conf"
  env-file "/host/app.env"
  overlay "/host/overlay"
}
"#,
        );
        let opts = process::build_create_opts("c", &t.nodes["c"], &[], None);
        // the hostname defaults to the node name (#217)
        assert_eq!(opts.hostname.as_deref(), Some("c"));
        let named = crate::types::Node {
            hostname: Some("web".into()),
            ..t.nodes["c"].clone()
        };
        let opts2 = process::build_create_opts("c", &named, &[], None);
        assert_eq!(opts2.hostname.as_deref(), Some("web"));
        let odd = process::build_create_opts("not_a_hostname", &t.nodes["c"], &[], None);
        assert_eq!(
            odd.hostname, None,
            "left to the runtime when not a hostname"
        );
        assert_eq!(
            opts.volumes,
            vec!["/pre:/pre".to_string()],
            "explicit `volumes` stay verbatim -- a bare name is a podman volume"
        );
        assert_eq!(
            opts.configs,
            vec![
                ("/host/a.conf".to_string(), "/etc/a.conf".to_string()),
                ("/host/b.conf".to_string(), "/etc/b.conf".to_string()),
            ],
            "config pairs reach the runtime layer, which mounts them read-only"
        );
        assert_eq!(opts.env_file.as_deref(), Some("/host/app.env"));
        assert_eq!(opts.overlay.as_deref(), Some("/host/overlay"));
    }

    /// A container's relative host files are its topology file's, from
    /// wherever it is deployed (#285); absolute ones and volumes stay.
    #[test]
    fn relative_host_files_are_under_the_source_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(
            dir.path().join("sub/web.nll"),
            "lab \"w\"\nnode web image \"nginx\" {\n  env-file \"web.env\"\n  config \"conf/a.conf\" \"/etc/a.conf\"\n}\n",
        )
        .unwrap();
        let entry = dir.path().join("lab.nll");
        std::fs::write(
            &entry,
            "import \"sub/web.nll\" as s\nlab \"t\"\nnode c image \"alpine\" {\n  overlay \"ov\"\n  config \"/abs/b.conf\" \"/etc/b.conf\"\n  volumes [\"data:/data\"]\n}\n",
        )
        .unwrap();
        let t = crate::parser::parse_file(&entry).unwrap();
        let root = std::path::absolute(dir.path()).unwrap();
        assert_eq!(t.source_dir.as_deref(), Some(root.to_str().unwrap()));
        // the import's, under its directory; still relative in the topology
        assert_eq!(t.nodes["s-web"].env_file.as_deref(), Some("sub/web.env"));
        let plan = crate::deploy::plan_for(&t).unwrap();
        let opts = |node: &str| {
            plan.ops
                .iter()
                .find_map(|o| match o {
                    Op::CreateContainer { node: n, opts, .. } if n == node => Some(opts.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let web = opts("s-web");
        assert_eq!(
            web.env_file,
            Some(root.join("sub/web.env").display().to_string())
        );
        assert_eq!(
            web.configs[0].0,
            root.join("sub/conf/a.conf").display().to_string()
        );
        let c = opts("c");
        assert_eq!(c.overlay, Some(root.join("ov").display().to_string()));
        assert_eq!(c.configs[0].0, "/abs/b.conf");
        assert_eq!(c.volumes, vec!["data:/data".to_string()]);
        // parsed from text: left to the runtime, as before
        let text = crate::parser::parse(
            &std::fs::read_to_string(&entry)
                .unwrap()
                .replace("import \"sub/web.nll\" as s\n", ""),
        )
        .unwrap();
        assert_eq!(text.source_dir, None);
        let plan = crate::deploy::plan_for(&text).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, Op::CreateContainer { opts, .. } if opts.overlay.as_deref() == Some("ov"))));
    }

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
        // the impairment rides in each end's stack
        for n in ["r", "h"] {
            assert_eq!(stack_of(&p, n).network.qdiscs().len(), 1, "{n}");
        }
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
                .any(|o| matches!(o, Op::DeleteLink { node, .. } if node == "x")),
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

    fn stack_of<'a>(p: &'a Plan, n: &str) -> &'a StackConfig {
        p.ops
            .iter()
            .find_map(|o| match o {
                Op::Stack { node, cfg } if node == n => Some(&**cfg),
                _ => None,
            })
            .expect("stack op")
    }

    /// Traffic control is declared in the stack (#108). An impairment
    /// removed from the topology is a qdisc the desired stack no longer
    /// declares; the apply removes it through `PlanOptions::previous`.
    #[test]
    fn impairments_are_declared_in_the_stack() {
        let cur = plan_of(SIMPLE);
        let q = stack_of(&cur, "r").network.qdiscs();
        assert_eq!(q.len(), 1, "{q:?}");
        assert_eq!(q[0].dev(), "eth0");
        assert!(matches!(
            q[0].qdisc_type(),
            nlink::netlink::tc_spec::QdiscSpec::Netem(_)
        ));
        let des = plan_of(&SIMPLE.replace("  delay 5ms", ""));
        assert!(stack_of(&des, "r").network.qdiscs().is_empty());
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops
                .iter()
                .all(|o| matches!(o, Op::Stack { .. } | Op::LinksUp { .. })),
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
    fn qdisc_blocks_are_declared_in_the_stack() {
        use nlink::netlink::tc_spec::QdiscSpec;
        let src = |kind: &str| {
            plan_of(&format!(
                r#"lab "q"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
qdisc a:eth0 {kind}
"#
            ))
        };
        let kind = |p: &Plan| match stack_of(p, "a").network.qdiscs() {
            [q] if q.dev() == "eth0" => q.qdisc_type().clone(),
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            kind(&src("tbf { rate 10mbit burst 32kb limit 100kb }")),
            QdiscSpec::Tbf(_)
        ));
        assert!(matches!(
            kind(&src("sfq { perturb 10s }")),
            QdiscSpec::Sfq(_)
        ));
        // `noecn` writes ECN off; leaving it out is the kernel's default (#173).
        match kind(&src("fq_codel { noecn }")) {
            QdiscSpec::FqCodel(f) => assert_eq!(f.ecn, Some(false)),
            other => panic!("{other:?}"),
        }
        match kind(&src("fq_codel { limit 1000 }")) {
            QdiscSpec::FqCodel(f) => assert_eq!(f.ecn, None),
            other => panic!("{other:?}"),
        }
    }

    /// One root per interface (#169): a per-pair source's own `impair` is
    /// the tree's default leaf; a rate limit on an impaired interface is
    /// skipped; a rate limit's ingress half is a declared IFB.
    #[test]
    fn tc_roots_compose_per_interface() {
        use nlink::netlink::tc_spec::QdiscSpec;
        let p = plan_of(
            r#"lab "c"
node a
node b
node c
network lan {
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.50.0.0/24
  impair a -- b { delay 20ms }
}
impair a:eth0 delay 100ms
node d
node e
link d:eth0 -- e:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  rate egress 10mbit ingress 10mbit }
link d:eth1 -- e:eth1 { 10.0.1.1/24 -- 10.0.1.2/24  delay 5ms  rate 1mbit }
"#,
        );
        let a = &stack_of(&p, "a").network;
        assert!(matches!(
            a.qdiscs(),
            [q] if q.dev() == "eth0" && matches!(q.qdisc_type(), QdiscSpec::Htb(_))
        ));
        let tree = &a.tc_trees()[0];
        assert_eq!(tree.leaves().len(), 2, "the pair's netem and the default's");

        let d = &stack_of(&p, "d").network;
        let roots: Vec<(&str, bool)> = d
            .qdiscs()
            .iter()
            .map(|q| (q.dev(), matches!(q.qdisc_type(), QdiscSpec::Netem(_))))
            .collect();
        assert!(
            roots.contains(&("eth1", true)),
            "impaired: netem only {roots:?}"
        );
        assert!(
            !roots.iter().any(|(dev, netem)| *dev == "eth1" && !netem),
            "{roots:?}"
        );
        assert!(
            d.links().iter().any(|l| l.name() == "ifb_eth0"),
            "ingress shaping declares its IFB"
        );
    }

    /// A partition is a clsact with a matchall on both hooks (#168): the
    /// interface's root qdisc stays declared. Ingress drops, egress steals
    /// (#347): a drop there would be seen by the sender's own stack.
    #[test]
    fn a_partition_keeps_the_root_and_adds_partition_filters() {
        use nlink::TcHandle;
        use nlink::netlink::action::{ActionList, GactAction};
        use nlink::netlink::config::FilterSpec;
        use nlink::netlink::filter::MatchallFilter;
        use nlink::netlink::tc_spec::QdiscSpec;
        let t = topo(
            r#"lab "p"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
qdisc a:eth0 prio { bands 3 }
"#,
        );
        let inputs = PlanInputs {
            partitions: ["a:eth0".to_string()].into(),
            ..PlanInputs::for_deploy(&t).unwrap()
        };
        let p = plan(&t, &inputs).unwrap();
        let a = &stack_of(&p, "a").network;
        let kinds: Vec<bool> = a
            .qdiscs()
            .iter()
            .map(|q| matches!(q.qdisc_type(), QdiscSpec::Prio(_)))
            .collect();
        assert_eq!(kinds.len(), 2, "prio and clsact: {:?}", a.qdiscs());
        assert!(kinds.contains(&true));
        assert!(
            a.qdiscs()
                .iter()
                .any(|q| matches!(q.qdisc_type(), QdiscSpec::Clsact))
        );
        let filters = a.tc_trees()[0].filters();
        assert_eq!(filters.len(), 2);
        let on = |hook: TcHandle| {
            &filters
                .iter()
                .find(|f| f.parent == hook)
                .unwrap_or_else(|| panic!("no filter on {hook:?}: {filters:?}"))
                .spec
        };
        let matchall = |action| {
            FilterSpec::from(MatchallFilter::new().actions(ActionList::new().with(action)))
        };
        assert_eq!(on(TcHandle::CLSACT_INGRESS), &matchall(GactAction::drop()));
        assert_eq!(on(TcHandle::CLSACT_EGRESS), &matchall(GactAction::stolen()));
        assert!(stack_of(&p, "b").network.tc_trees().is_empty());
    }

    #[test]
    fn bond_members_are_not_brought_up_before_enslaving() {
        let plan = plan_of(
            r#"lab "b"
node left {
  bond bond0 { members [eth0, eth1] mode active-backup }
}
node right
link left:eth0 -- right:eth0
link left:eth1 -- right:eth1
link left:eth2 -- right:eth2
"#,
        );
        let up: Vec<&Vec<String>> = plan
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::LinksUp { node, ifaces } if node == "left" => Some(ifaces),
                _ => None,
            })
            .collect();
        assert_eq!(up.len(), 1);
        // the kernel refuses to enslave an up device: members stay down
        // until the stack sets their master (the bond opens them)
        assert_eq!(up[0], &vec!["eth2".to_string(), "lo".to_string()]);
    }

    #[test]
    fn plan_topology_mgmt_v6_ops() {
        let plan = plan_of(
            r#"lab "m6" { mgmt fd00:20::/64 host-reachable }
node a
node b
link a:eth0 -- b:eth0 { fd00:1::1/64 -- fd00:1::2/64 }
"#,
        );
        let bridge: Vec<_> = plan
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::CreateMgmtBridge { ip, prefix, .. } => Some((ip.to_string(), *prefix)),
                _ => None,
            })
            .collect();
        assert_eq!(bridge, vec![("fd00:20::1".to_string(), 64)]);
        let nodes: Vec<_> = plan
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::CreateMgmtVeth { node, node_ip, .. } => {
                    Some((node.clone(), node_ip.to_string()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            nodes,
            vec![
                ("a".to_string(), "fd00:20::2".to_string()),
                ("b".to_string(), "fd00:20::3".to_string())
            ]
        );
    }

    #[test]
    fn a_declared_links_sysctls_ride_on_its_declaration() {
        // #160: the Sysctls stage runs before Stack creates vl0, so its
        // keys are devconf on vl0's declaration; eth0 (a veth, created in
        // Links) and the global key stay sysctls.
        let p = plan_of(
            r#"lab "t"
node r {
  vlan vl0 { parent eth0 id 10 address 10.10.0.1/24 }
  sysctl "net.ipv4.conf.vl0.rp_filter" "2"
  sysctl "net/ipv6/conf/vl0/accept_dad" "0"
  sysctl "net.ipv4.conf.eth0.rp_filter" "1"
  sysctl "net.ipv4.ip_forward" "1"
}
node h
link r:eth0 -- h:eth0
"#,
        );
        let sysctls: Vec<&String> = p
            .ops
            .iter()
            .find_map(|o| match o {
                Op::Sysctls { node, entries } if node == "r" => Some(entries.keys().collect()),
                _ => None,
            })
            .expect("r's sysctls");
        assert_eq!(
            sysctls,
            ["net.ipv4.conf.eth0.rp_filter", "net.ipv4.ip_forward"],
            "{sysctls:?}"
        );
        let cfg = p
            .ops
            .iter()
            .find_map(|o| match o {
                Op::Stack { node, cfg } if node == "r" => Some(cfg),
                _ => None,
            })
            .expect("r's stack");
        let vl0 = cfg
            .network
            .links()
            .iter()
            .find(|l| l.name() == "vl0")
            .expect("vl0 declared");
        let shown = format!("{vl0:?}");
        assert!(shown.contains(r#""rp_filter": 2"#), "{shown}");
        assert!(shown.contains(r#""accept_dad": 0"#), "{shown}");
        assert_eq!(
            cfg.network
                .links()
                .iter()
                .filter(|l| l.name() == "vl0")
                .count(),
            1,
            "one declaration"
        );
    }

    #[test]
    fn link_devconf_takes_only_settings_nlink_can_set() {
        use super::network::{LinkDevconf, link_devconf};
        let t = topo(
            r#"lab "t"
node r {
  vlan vl0 { parent eth0 id 10 }
  vrf red table 10 { interfaces [vl0] }
}
node h
link r:eth0 -- h:eth0
"#,
        );
        let r = &t.nodes["r"];
        assert!(matches!(
            link_devconf(r, "net.ipv4.conf.red.forwarding", "1"),
            Some((ref i, LinkDevconf::V4(..))) if i == "red"
        ));
        assert!(matches!(
            link_devconf(r, "net.ipv6.conf.vl0.accept_ra", " 2 "),
            Some((ref i, LinkDevconf::V6(_, 2))) if i == "vl0"
        ));
        // not a declared kind, not per-interface, unknown, not a number
        assert_eq!(link_devconf(r, "net.ipv4.conf.eth0.rp_filter", "1"), None);
        assert_eq!(link_devconf(r, "net.ipv4.conf.all.rp_filter", "1"), None);
        assert_eq!(link_devconf(r, "net.ipv4.ip_forward", "1"), None);
        assert_eq!(link_devconf(r, "net.ipv4.conf.vl0.no_such", "1"), None);
        assert_eq!(link_devconf(r, "net.ipv4.conf.vl0.rp_filter", "-1"), None);
    }

    /// The WireGuard devices `node`'s stack declares, by name.
    #[cfg(feature = "wireguard")]
    fn wg_devices<'a>(
        p: &'a Plan,
        n: &str,
    ) -> std::collections::BTreeMap<&'a str, &'a nlink::netlink::genl::wireguard::DeclaredWgDevice>
    {
        stack_of(p, n)
            .wireguard
            .as_ref()
            .map(|w| w.devices().iter().map(|d| (d.ifname.as_str(), d)).collect())
            .unwrap_or_default()
    }

    /// #147: two tunnels between two hubs. Each tunnel gets its own peer
    /// (by interface name, with that interface's key), the WAN address as
    /// its endpoint, and the far site's prefix in its allowed-ips.
    #[cfg(feature = "wireguard")]
    #[test]
    fn wireguard_peers_pair_by_name_and_carry_the_routes() {
        let t = crate::parser::parse_file(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/cookbook/multi-tenant-wan.nll"
        ))
        .unwrap();
        let p = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap();
        let (a, b) = (wg_devices(&p, "hub-a"), wg_devices(&p, "hub-b"));
        for (dev, port, far, tunnel) in [
            ("wg-red", 51820, "10.20.0.0/16", "192.168.255.2/32"),
            ("wg-blue", 51821, "10.21.0.0/16", "192.168.255.4/32"),
        ] {
            let peers = &a[dev].peers;
            assert_eq!(peers.len(), 1, "{dev}: {peers:?}");
            let peer = &peers[0];
            let b_pub = super::wireguard::derive_wg_public_key(&b[dev].private_key.unwrap());
            assert_eq!(peer.public_key, b_pub, "{dev}: the peer is b's {dev}");
            assert_eq!(
                peer.endpoint,
                Some(format!("10.0.0.2:{port}").parse().unwrap()),
                "{dev}: the WAN address, not a customer one"
            );
            let ips: Vec<String> = peer
                .allowed_ips
                .iter()
                .map(|ai| format!("{}/{}", ai.addr, ai.cidr))
                .collect();
            assert_eq!(ips, [tunnel, far], "{dev}");
        }
    }

    /// A hub's spokes get their tunnel address as a host each, not the
    /// shared /24 (which overlapped), and a route into the tunnel goes to
    /// the spoke its gateway is.
    #[cfg(feature = "wireguard")]
    #[test]
    fn wireguard_hub_peers_do_not_overlap() {
        let src = |hub_route: &str| {
            format!(
                r#"lab "hub"
node hub {{
  wireguard wg0 {{ key auto listen 51820 address 10.99.0.1/24 peers [s1, s2] }}
  {hub_route}
}}
node s1 {{ wireguard wg0 {{ key auto listen 51820 address 10.99.0.2/24 peers [hub] }} }}
node s2 {{ wireguard wg0 {{ key auto listen 51820 address 10.99.0.3/24 peers [hub] }} }}
link hub:eth0 -- s1:eth0 {{ 10.0.1.1/30 -- 10.0.1.2/30 }}
link hub:eth1 -- s2:eth0 {{ 10.0.2.1/30 -- 10.0.2.2/30 }}
"#
            )
        };
        let t = topo(&src("route 172.16.0.0/16 via 10.99.0.3 dev wg0"));
        assert!(!t.validate().has_errors());
        let p = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap();
        let hub = wg_devices(&p, "hub");
        let ips: Vec<Vec<String>> = hub["wg0"]
            .peers
            .iter()
            .map(|peer| {
                peer.allowed_ips
                    .iter()
                    .map(|ai| format!("{}/{}", ai.addr, ai.cidr))
                    .collect()
            })
            .collect();
        assert_eq!(
            ips,
            [vec!["10.99.0.2/32"], vec!["10.99.0.3/32", "172.16.0.0/16"]],
            "{ips:?}"
        );
        // Without a gateway the route names no spoke: a warning.
        let t = topo(&src("route 172.16.0.0/16 dev wg0"));
        let result = t.validate();
        assert!(
            result.warnings().any(|w| w.rule == "wireguard-route-peer"),
            "{:?}",
            result.warnings().collect::<Vec<_>>()
        );
    }

    /// A longhand `peer` block replaces the inference (#147), and renders
    /// back as itself.
    #[cfg(feature = "wireguard")]
    #[test]
    fn wireguard_longhand_peer_overrides_the_inference() {
        let t = topo(
            r#"lab "lh"
node a {
  wireguard wg0 {
    key auto listen 51820 address 10.99.0.1/32
    peer b { endpoint 192.0.2.7:5555 allowed-ips [10.99.0.2/32, 172.16.0.0/12] }
  }
}
node b { wireguard wg0 { key auto listen 51820 address 10.99.0.2/32 peers [a] } }
link a:eth0 -- b:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
"#,
        );
        assert_eq!(t.nodes["a"].wireguard["wg0"].peers, ["b"]);
        assert!(!t.validate().has_errors());
        let p = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap();
        let peer = &wg_devices(&p, "a")["wg0"].peers[0];
        assert_eq!(peer.endpoint, Some("192.0.2.7:5555".parse().unwrap()));
        assert_eq!(peer.allowed_ips.len(), 2);
        let rendered = crate::render::try_render(&t).unwrap();
        assert!(
            rendered.contains(
                "peer b { endpoint 192.0.2.7:5555 allowed-ips [10.99.0.2/32, 172.16.0.0/12] }"
            ),
            "{rendered}"
        );
        assert_eq!(
            crate::parser::parse(&rendered).unwrap().nodes["a"].wireguard,
            t.nodes["a"].wireguard
        );
        // NLL refuses a bad endpoint as it parses; a JSON or builder
        // topology is checked by the validator.
        assert!(crate::parser::parse(&rendered.replace("192.0.2.7:5555", "nowhere")).is_err());
        let mut bad = t.clone();
        let peer = bad
            .nodes
            .get_mut("a")
            .unwrap()
            .wireguard
            .get_mut("wg0")
            .unwrap()
            .peer_settings
            .get_mut("b")
            .unwrap();
        peer.endpoint = Some("nowhere".into());
        peer.allowed_ips = vec!["10.0.0.0/33".into()];
        let errs: Vec<_> = bad
            .validate()
            .errors()
            .filter(|e| e.rule == "wireguard-peer-settings")
            .map(|e| e.message.clone())
            .collect();
        assert_eq!(errs.len(), 2, "{errs:?}");
    }

    #[test]
    fn plan_declares_vrf_routes_in_the_stack() {
        // VRF-table routes are declared routes again: purge has covered
        // every table a config declares into since nlink 0.27, and a
        // declared route is what nlink restores after a link recreate.
        let p = plan_of(VRF);
        let cfg = p
            .ops
            .iter()
            .find_map(|o| match o {
                Op::Stack { node, cfg } if node == "pe" => Some(cfg),
                _ => None,
            })
            .expect("pe's stack");
        let vrf: Vec<_> = cfg
            .network
            .routes()
            .iter()
            .filter(|r| r.table() == Some(10))
            .collect();
        assert_eq!(vrf.len(), 2, "{vrf:?}");
    }

    #[test]
    fn diff_vrf_route_change_is_a_stack_change() {
        let cur = plan_of(VRF);
        let des = plan_of(&VRF.replace("metric 50", "metric 60"));
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops
                .iter()
                .any(|o| matches!(o, Op::Stack { node, .. } if node == "pe")),
            "{:?}",
            d.ops
        );
    }

    #[test]
    fn vrf_route_via_an_ipv6_gateway_is_declared() {
        // An IPv4 route via an IPv6 gateway (RFC 5549, `RTA_VIA`) is a
        // route nlink 0.32 declares, in a VRF table as in main.
        let t = topo(&VRF.replace(
            "route 192.168.5.0/24 via 10.10.0.10 metric 50",
            "route 192.168.5.0/24 via fd00::1",
        ));
        let p = plan(&t, &PlanInputs::for_deploy(&t).unwrap()).unwrap();
        let routes: Vec<_> = p
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Stack { node, cfg } if node == "pe" => Some(cfg.network.routes().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(
            routes
                .iter()
                .any(|r| r.gateway() == Some("fd00::1".parse().unwrap())),
            "{routes:?}"
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

    const NETS: &str = r#"lab "n"
node a
node b
node c
network lan { subnet 10.1.0.0/24  members [a:eth0, b:eth0] }
network dmz { subnet 10.2.0.0/24  members [b:eth1, c:eth0] }
"#;

    #[test]
    fn diff_removed_network_deletes_bridge_and_member_links() {
        let cur = plan_of(NETS);
        let des = plan_of(&NETS.replace(
            "network dmz { subnet 10.2.0.0/24  members [b:eth1, c:eth0] }\n",
            "",
        ));
        let d = Plan::diff(&cur, &des);
        let bridges: Vec<&Op> = d
            .ops
            .iter()
            .filter(|o| matches!(o, Op::DeleteBridge { .. }))
            .collect();
        assert_eq!(bridges.len(), 1, "{:?}", d.ops);
        assert!(matches!(bridges[0], Op::DeleteBridge { ns, .. } if ns == "n-mgmt"));
        assert!(d.ops.iter().any(
            |o| matches!(o, Op::DeleteLink { node, iface } if node == "b" && iface == "eth1")
        ));
        assert!(
            !d.ops
                .iter()
                .any(|o| matches!(o, Op::DeleteNamespace { .. })),
            "{:?}",
            d.ops
        );
        // member links (Links stage) are deleted before the bridge (Networks)
        let link = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::DeleteLink { node, .. } if node == "b"))
            .unwrap();
        let bridge = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::DeleteBridge { .. }))
            .unwrap();
        assert!(link < bridge, "{:?}", d.ops);
    }

    #[test]
    fn diff_removing_every_network_deletes_the_mgmt_namespace_not_bridges() {
        let cur = plan_of(NETS);
        let des = plan_of("lab \"n\"\nnode a\nnode b\nnode c\n");
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops
                .iter()
                .any(|o| matches!(o, Op::DeleteNamespace { ns, .. } if ns == "n-mgmt")),
            "{:?}",
            d.ops
        );
        assert!(
            !d.ops.iter().any(|o| matches!(o, Op::DeleteBridge { .. })),
            "bridges die with their namespace: {:?}",
            d.ops
        );
    }

    const WIFI: &str = r#"lab "w"
node ap { wifi wlan0 mode ap { ssid "labnet" channel 6 wpa2 "testpassword" 10.0.0.1/24 } }
node sta { wifi wlan0 mode station { ssid "labnet" wpa2 "testpassword" 10.0.0.2/24 } }
"#;

    #[test]
    fn diff_changed_wifi_restarts_the_daemon_and_removed_wifi_stops_it() {
        let cur = plan_of(WIFI);
        let des = plan_of(&WIFI.replace("channel 6", "channel 11"));
        let d = Plan::diff(&cur, &des);
        let kill = d.ops.iter().position(
            |o| matches!(o, Op::KillWifiDaemon { node, name, .. } if node == "ap" && name == "wlan0"),
        );
        let start = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::WifiDaemon { node, .. } if node == "ap"));
        assert!(kill.is_some() && start.is_some(), "{:?}", d.ops);
        assert!(kill < start);
        assert!(
            !d.ops
                .iter()
                .any(|o| matches!(o, Op::WifiDaemon { node, .. } if node == "sta")),
            "unchanged station must not restart: {:?}",
            d.ops
        );

        let des = plan_of(&WIFI.replace(
            "node sta { wifi wlan0 mode station { ssid \"labnet\" wpa2 \"testpassword\" 10.0.0.2/24 } }\n",
            "node sta\n",
        ));
        let d = Plan::diff(&cur, &des);
        assert!(
            d.ops.iter().any(|o| matches!(
                o,
                Op::KillWifiDaemon { node, mode: crate::types::WifiMode::Station, .. } if node == "sta"
            )),
            "{:?}",
            d.ops
        );
    }

    /// One radio per Wi-Fi interface, all made in the first Wi-Fi node's
    /// namespace (one netgroup), named after the namespace (#176).
    #[test]
    fn every_wifi_interface_gets_a_radio_made_in_one_namespace() {
        let radios: Vec<_> = plan_of(WIFI)
            .ops
            .into_iter()
            .filter_map(|o| match o {
                Op::CreateRadio {
                    node,
                    iface,
                    radio,
                    mode,
                    maker,
                } => Some((node, iface, radio, mode, maker)),
                _ => None,
            })
            .collect();
        use crate::types::WifiMode::{Ap, Station};
        let r = |n: &str, radio: &str, m| (n.into(), "wlan0".into(), radio.into(), m, "ap".into());
        assert_eq!(
            radios,
            [r("ap", "w-ap-wlan0", Ap), r("sta", "w-sta-wlan0", Station)]
        );
        assert!(
            plan_of(SIMPLE)
                .ops
                .iter()
                .all(|o| !matches!(o, Op::CreateRadio { .. }))
        );
    }

    /// A removed Wi-Fi interface loses its radio after its daemon stops; a
    /// changed mode makes the radio again (the interface type is set when
    /// it is created).
    #[test]
    fn diff_removed_or_retyped_wifi_deletes_its_radio() {
        let cur = plan_of(WIFI);
        let des = plan_of(&WIFI.replace(
            "node sta { wifi wlan0 mode station { ssid \"labnet\" wpa2 \"testpassword\" 10.0.0.2/24 } }\n",
            "node sta\n",
        ));
        let d = Plan::diff(&cur, &des);
        let kill = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::KillWifiDaemon { node, .. } if node == "sta"));
        let delete = d.ops.iter().position(
            |o| matches!(o, Op::DeleteRadio { node, radio, .. } if node == "sta" && radio == "w-sta-wlan0"),
        );
        assert!(
            kill.is_some() && delete.is_some() && kill < delete,
            "{:?}",
            d.ops
        );
        assert!(
            !d.ops.iter().any(|o| matches!(o, Op::CreateRadio { .. })),
            "{:?}",
            d.ops
        );

        let des = plan_of(&WIFI.replace(
            "mode station { ssid \"labnet\" wpa2 \"testpassword\"",
            "mode mesh { mesh-id \"labmesh\"",
        ));
        let d = Plan::diff(&cur, &des);
        let delete = d
            .ops
            .iter()
            .position(|o| matches!(o, Op::DeleteRadio { node, .. } if node == "sta"));
        let create = d.ops.iter().position(|o| {
            matches!(o, Op::CreateRadio { node, mode: crate::types::WifiMode::Mesh, .. } if node == "sta")
        });
        assert!(
            delete.is_some() && create.is_some() && delete < create,
            "{:?}",
            d.ops
        );
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
