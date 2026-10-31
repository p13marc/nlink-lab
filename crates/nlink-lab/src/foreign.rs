//! Foreign nodes (#344): a network namespace, another tool's container or
//! a VM's tap, which a lab links to and never creates, stops or deletes.
//!
//! Nothing about a foreign node is kept in the lab's state: its namespace
//! is found again from the topology every time. A netns is its path, a
//! container is whatever namespace the runtime says it has now (so a
//! restarted container is followed, and a reused pid is never), and a tap
//! is in the host's namespace, on the lab's `nt…` bridge.
//!
//! What nlink-lab makes for one: the lab's end of each link (a veth
//! whose other end is moved into the netns or container), and for a tap a
//! bridge in the host's namespace that the tap and the links' far ends
//! join. The tap is never moved: a tap is a virtual link, and moving it
//! into a namespace would have it deleted with that namespace. Destroy
//! deletes the lab's ends, which takes their peers along, and the bridge,
//! which releases the tap.

use crate::deploy::NsRef;
use crate::error::{Error, Result};
use crate::types::{EndpointRef, Foreign, ForeignTarget, Topology};

/// Where `node`'s namespace is now. `runtime` is the container runtime's
/// binary, for a container target (detected when `None`).
pub fn resolve(node: &str, foreign: &Foreign, runtime: Option<&str>) -> Result<NsRef> {
    match &foreign.target {
        ForeignTarget::Netns(_) => {
            let path = foreign.target.netns_path().expect("a netns target");
            if !path.exists() {
                return Err(missing(node, &foreign.target, "it does not exist"));
            }
            Ok(NsRef::Path { path })
        }
        ForeignTarget::Container(name) => {
            let rt = match runtime {
                Some(bin) => crate::container::Runtime::with_binary(bin),
                None => crate::container::Runtime::detect()
                    .map_err(|e| missing(node, &foreign.target, &e.to_string()))?,
            };
            let pid = rt
                .inspect_pid(name)
                .map_err(|e| missing(node, &foreign.target, &e.to_string()))?;
            if pid == 0 {
                return Err(missing(node, &foreign.target, "it is not running"));
            }
            Ok(NsRef::Container {
                id: name.clone(),
                pid,
            })
        }
        ForeignTarget::Tap(_) => Ok(NsRef::Root),
    }
}

/// The deploy-time error for a target that is not there (`foreign-target-missing`).
fn missing(node: &str, target: &ForeignTarget, why: &str) -> Error {
    Error::deploy_failed(format!(
        "foreign-target-missing: foreign node '{node}': {target}: {}",
        why.trim()
    ))
}

/// The interfaces a lab makes on `node`'s side: its link ends, and its
/// network members' ends.
pub(crate) fn lab_made_ifaces(topology: &Topology, node: &str) -> Vec<String> {
    let mut out = Vec::new();
    for link in &topology.links {
        for ep in &link.endpoints {
            if let Some(ep) = EndpointRef::parse(ep)
                && ep.node == node
            {
                out.push(ep.iface);
            }
        }
    }
    for network in topology.networks.values() {
        for member in &network.members {
            if let Some(ep) = EndpointRef::parse(member)
                && ep.node == node
            {
                out.push(ep.iface);
            }
        }
    }
    out
}

/// The runtime binary `topology` names, if it names one (else detected).
pub(crate) fn runtime_of(topology: &Topology) -> Option<&'static str> {
    match topology.lab.runtime {
        Some(crate::types::ContainerRuntime::Docker) => Some("docker"),
        Some(crate::types::ContainerRuntime::Podman) => Some("podman"),
        _ => None,
    }
}

/// Everything a deploy needs of its foreign nodes, checked before its
/// first op so a missing one leaves nothing behind: each target is there,
/// a tap has no master already, and no name nlink-lab is about to give an
/// interface in a foreign namespace is taken there.
pub async fn preflight(topology: &Topology, runtime: Option<&str>) -> Result<()> {
    use nlink::netlink::Route;
    for (name, node) in &topology.nodes {
        let Some(foreign) = &node.foreign else {
            continue;
        };
        let ns = resolve(name, foreign, runtime)?;
        let conn: nlink::Connection<Route> = ns
            .connection()
            .map_err(|e| Error::nlink(format!("foreign node '{name}': its namespace"), e))?;
        if let ForeignTarget::Tap(tap) = &foreign.target {
            let link = conn
                .get_link_by_name(tap)
                .await
                .map_err(|e| Error::nlink(format!("foreign node '{name}': tap '{tap}'"), e))?;
            let Some(link) = link else {
                return Err(missing(name, &foreign.target, "no such interface"));
            };
            if link.master().is_some() {
                return Err(Error::deploy_failed(format!(
                    "foreign node '{name}': tap '{tap}' already has a master: a lab \
                     bridges it itself (with libvirt, an unmanaged `type='ethernet'` \
                     interface)"
                )));
            }
            continue;
        }
        for iface in lab_made_ifaces(topology, name) {
            let taken = conn
                .get_link_by_name(&iface)
                .await
                .map_err(|e| Error::nlink(format!("foreign node '{name}': {iface}"), e))?;
            if taken.is_some() {
                return Err(Error::deploy_failed(format!(
                    "foreign node '{name}': {} already has an interface '{iface}': \
                     name the lab's link end something else",
                    foreign.target
                )));
            }
        }
    }
    Ok(())
}

/// Put `tap` on `bridge`, both in the host's namespace, and both up.
pub(crate) async fn enslave_tap(tap: &str, bridge: &str) -> Result<()> {
    let host = nlink::Connection::<nlink::netlink::Route>::new()
        .map_err(|e| Error::nlink("root connection", e))?;
    host.set_link_master(tap, bridge)
        .await
        .map_err(|e| Error::nlink(format!("put tap {tap} on {bridge}"), e))?;
    for dev in [bridge, tap] {
        host.set_link_up(dev)
            .await
            .map_err(|e| Error::nlink(format!("bring up {dev}"), e))?;
    }
    Ok(())
}

/// What `topology`'s foreign taps take in the host's namespace, in the
/// order to delete it: each link's port (`nt…p{k}`, numbered per tap in
/// link order, as the planner does), then each bridge. The taps themselves
/// are not in it.
pub fn tap_host_links(topology: &Topology) -> Vec<String> {
    let is_tap = |node: &str| {
        topology
            .nodes
            .get(node)
            .and_then(|n| n.foreign.as_ref())
            .is_some_and(|f| matches!(f.target, ForeignTarget::Tap(_)))
    };
    let mut count: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut out = Vec::new();
    for link in &topology.links {
        for ep in &link.endpoints {
            if let Some((node, _)) = ep.split_once(':')
                && is_tap(node)
            {
                let k = count.entry(node).or_insert(0);
                out.push(topology.lab.tap_port_name(node, *k));
                *k += 1;
            }
        }
    }
    out.extend(tap_bridges(topology));
    out
}

/// The bridges of the foreign tap nodes of `topology`, in the host's
/// namespace: what its destroy deletes.
pub fn tap_bridges(topology: &Topology) -> Vec<String> {
    topology
        .nodes
        .iter()
        .filter(|(_, n)| {
            n.foreign
                .as_ref()
                .is_some_and(|f| matches!(f.target, ForeignTarget::Tap(_)))
        })
        .map(|(name, _)| topology.lab.tap_bridge_name(name))
        .collect()
}

/// `node` is foreign: what a command that runs or tracks processes there
/// answers.
pub fn refuse(node: &str, what: &str) -> Error {
    Error::Unsupported(format!(
        "node '{node}' is foreign: nlink-lab does not {what} in a node it did not create"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::op::Op;

    const LAB: &str = r#"lab "f"
node r { forward ipv4 }
node fw kind foreign { netns "other-fw" }
node ceos kind foreign {
  container "clab-site-ceos1"
  manage addresses
  manage tc
}
node vm kind foreign { tap "vnet3" }
link fw:eth0 -- r:eth0 { 10.0.0.2/24 -- 10.0.0.1/24 }
link r:eth1 -- ceos:eth9 { 10.0.1.1/24 -- 10.0.1.2/24 }
link r:eth2 -- vm:nic { 10.0.2.1/24 -- 10.0.2.2/24 }
link r:eth3 -- vm:nic2
"#;

    fn rules(t: &Topology) -> Vec<&'static str> {
        t.validate().issues().iter().map(|i| i.rule).collect()
    }

    #[test]
    fn foreign_nodes_parse_and_render_back() {
        let t = crate::parser::parse(LAB).unwrap();
        let fw = t.nodes["fw"].foreign.clone().unwrap();
        assert_eq!(fw.target, ForeignTarget::Netns("other-fw".into()));
        assert_eq!(
            fw.target.netns_path().unwrap(),
            std::path::Path::new("/run/netns/other-fw")
        );
        assert!(!fw.manage_addresses && !fw.manage_tc);
        let ceos = t.nodes["ceos"].foreign.clone().unwrap();
        assert_eq!(
            ceos.target,
            ForeignTarget::Container("clab-site-ceos1".into())
        );
        assert!(ceos.manage_addresses && ceos.manage_tc);
        assert_eq!(
            t.nodes["vm"].foreign.as_ref().unwrap().target,
            ForeignTarget::Tap("vnet3".into())
        );
        let text = crate::render::try_render(&t).unwrap();
        assert!(text.contains("node fw kind foreign {"), "{text}");
        assert!(text.contains("  manage tc\n"), "{text}");
        let again = crate::parser::parse(&text).unwrap();
        for n in ["fw", "ceos", "vm"] {
            assert_eq!(again.nodes[n].foreign, t.nodes[n].foreign, "{n}");
        }
    }

    #[test]
    fn a_foreign_block_names_one_target() {
        for (nll, says) in [
            ("lab \"x\"\nnode a kind foreign\n", "needs a block"),
            ("lab \"x\"\nnode a kind foreign { manage tc }\n", "names no"),
            (
                "lab \"x\"\nnode a kind foreign { tap \"t0\" netns \"n\" }\n",
                "one target",
            ),
            (
                "lab \"x\"\nnode a kind foreign { tap \"t0\" manage routes }\n",
                "manage addresses",
            ),
            // the target keywords mean nothing on a node of the lab's own
            ("lab \"x\"\nnode a { tap \"t0\" }\n", ""),
        ] {
            let e = crate::parser::parse(nll).expect_err(nll).to_string();
            assert!(e.contains(says), "{nll}: {e}");
        }
    }

    #[test]
    fn the_plan_attaches_and_never_creates() {
        let t = crate::parser::parse(LAB).unwrap();
        assert!(!t.validate().has_errors(), "{:?}", t.validate().issues());
        let plan = crate::deploy::plan_for(&t).unwrap();
        let ops = &plan.ops;
        for n in ["fw", "ceos", "vm"] {
            assert!(
                ops.iter()
                    .any(|o| matches!(o, Op::AttachForeign { node, .. } if node == n)),
                "{n}"
            );
            assert!(
                !ops.iter().any(|o| matches!(o,
                    Op::CreateNamespace { node, .. } | Op::CreateContainer { node, .. }
                        if node == n)),
                "{n} is not created"
            );
            assert!(
                !ops.iter()
                    .any(|o| matches!(o, Op::Sysctls { node, .. } if node == n)),
                "{n} gets no sysctls"
            );
        }
        // the tap's bridge is the lab's; the netns and container have none
        let bridge = t.lab.tap_bridge_name("vm");
        assert!(ops.iter().any(|o| matches!(o,
            Op::AttachForeign { node, bridge: Some(b), .. } if node == "vm" && *b == bridge)));
        // the lab's end is the one made, even written second
        assert!(ops.iter().any(|o| matches!(o,
            Op::CreateVeth { a, b, .. } if a.node == "r" && a.iface == "eth0" && b.node == "fw")));
        // a tap's links: a veth whose far end joins its bridge, numbered
        let ports: Vec<&str> = ops
            .iter()
            .filter_map(|o| match o {
                Op::CreateTapVeth {
                    port, bridge: b, ..
                } if *b == bridge => Some(port.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ports,
            [t.lab.tap_port_name("vm", 0), t.lab.tap_port_name("vm", 1)]
        );
        assert_eq!(
            tap_host_links(&t),
            [
                t.lab.tap_port_name("vm", 0),
                t.lab.tap_port_name("vm", 1),
                bridge.clone()
            ]
        );
        // a stack only where it asked to be managed
        let stacks: Vec<&str> = ops
            .iter()
            .filter_map(|o| match o {
                Op::Stack { node, .. } => Some(node.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stacks, ["ceos", "r"]);
        // nothing brought up in a tap's node, and not a foreign node's `lo`
        assert!(
            !ops.iter()
                .any(|o| matches!(o, Op::LinksUp { node, .. } if node == "vm"))
        );
        assert!(ops.iter().any(|o| matches!(o,
            Op::LinksUp { node, ifaces } if node == "fw" && ifaces == &["eth0".to_string()])));
        // the inverse leaves the target: only the bridge goes
        let attach = ops
            .iter()
            .find(|o| matches!(o, Op::AttachForeign { node, .. } if node == "vm"))
            .unwrap();
        assert_eq!(
            attach.inverse(),
            Some(Op::DetachForeign {
                node: "vm".into(),
                bridge: Some(bridge)
            })
        );
    }

    #[test]
    fn a_foreign_node_cannot_be_given_what_runs_inside() {
        let t = crate::parser::parse(
            r#"lab "f"
profile router { forward ipv4 }
node fw : router kind foreign {
  netns "/run/netns/f-fw"
  run ["sleep", "1"]
  route default via 10.0.0.1
}
node vm kind foreign { tap "t0"  manage tc }
node vm2 kind foreign { tap "t1" }
node r
link fw:eth0 -- r:eth0 { 10.0.0.2/24 -- 10.0.0.1/24  -> delay 5ms }
link vm:a -- vm2:b
network lan { members [r:eth1, vm2:c] }
"#,
        )
        .unwrap();
        let got = rules(&t);
        for rule in [
            "foreign-unsupported-property",
            "foreign-link-needs-lab-side",
            "foreign-tap-link-only",
            "foreign-unmanaged-tc",
            "foreign-name-under-prefix",
            "foreign-address-unmanaged",
        ] {
            assert!(got.contains(&rule), "{rule}: {got:?}");
        }
        let unsupported: Vec<String> = t
            .validate()
            .issues()
            .iter()
            .filter(|i| i.rule == "foreign-unsupported-property")
            .map(|i| i.message.clone())
            .collect();
        assert!(
            unsupported.iter().any(|m| m.contains("profiles")),
            "{unsupported:?}"
        );
        assert!(
            unsupported.iter().any(|m| m.contains("`run`")),
            "{unsupported:?}"
        );
        assert!(
            unsupported
                .iter()
                .any(|m| m.contains("routes but no `manage addresses`")),
            "{unsupported:?}"
        );
        // the lab side's own impairment is fine; the foreign side's is not
        let tc: Vec<String> = t
            .validate()
            .issues()
            .iter()
            .filter(|i| i.rule == "foreign-unmanaged-tc")
            .map(|i| i.message.clone())
            .collect();
        assert!(tc.iter().all(|m| m.contains("fw:eth0")), "{tc:?}");
    }
}
