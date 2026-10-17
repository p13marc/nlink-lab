//! Live topology edits on top of `apply` (issue #69): add and remove
//! nodes and links, set and clear impairments, qdiscs, per-pair network
//! impairments and link MTUs. `edit`, the daemon and the desktop app share
//! one model: a list of [`Edit`]s, applied to a copy of the running
//! topology ([`edited`]) and then to the lab ([`apply_edited`]).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::{EndpointRef, Impairment, Link, Node, QdiscConfig, QdiscKind, Topology};

/// One change to a topology. Endpoints are `node:iface`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    /// A namespace node, with a profile.
    AddNode {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    /// A node and everything attached to it: its links (both ends),
    /// impairments, network memberships, dependencies and WireGuard peers.
    RemoveNode {
        name: String,
    },
    AddLink {
        a: String,
        b: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addresses: Option<[String; 2]>,
    },
    /// The link that has this endpoint.
    RemoveLink {
        endpoint: String,
    },
    /// Replaces the endpoint's whole impairment.
    SetImpairment {
        endpoint: String,
        impairment: Impairment,
    },
    ClearImpairment {
        endpoint: String,
    },
    /// Replaces the endpoint's qdisc block.
    SetQdisc {
        endpoint: String,
        qdisc: QdiscKind,
    },
    ClearQdisc {
        endpoint: String,
    },
    /// A network's impairment from `src` to `dst` (node names); `None`
    /// removes it.
    SetPairImpairment {
        network: String,
        src: String,
        dst: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        impairment: Option<Impairment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate_cap: Option<String>,
    },
    /// The MTU of both ends of the endpoint's link. Set live, not through
    /// the plan, which only knows an MTU when it creates a veth.
    SetMtu {
        endpoint: String,
        mtu: u32,
    },
}

/// A copy of a topology with edits applied: what [`apply_edited`] makes
/// of the lab.
#[derive(Debug, Clone)]
pub struct Edited {
    pub topology: Topology,
    /// One line per edit, in order.
    pub log: Vec<String>,
    /// The validator's warnings about `topology`.
    pub warnings: Vec<String>,
    /// MTU edits: endpoint, its link's other end, MTU.
    pub mtus: Vec<(String, String, u32)>,
    /// Whether an edit other than an MTU change is in it: then the lab is
    /// changed through `apply`.
    pub structural: bool,
}

fn endpoint(s: &str) -> Result<EndpointRef> {
    EndpointRef::parse(s).ok_or_else(|| Error::InvalidEndpoint {
        endpoint: s.to_string(),
    })
}

/// `node:iface`, normalized.
fn key(s: &str) -> Result<String> {
    let ep = endpoint(s.trim())?;
    Ok(format!("{}:{}", ep.node, ep.iface))
}

/// The topology `edits` make of `topology`, validated: an edit that
/// leaves it invalid is refused before anything changes.
pub fn edited(topology: &Topology, edits: &[Edit]) -> Result<Edited> {
    if edits.is_empty() {
        return Err(Error::invalid_argument("nothing to do: no edits"));
    }
    let mut desired = topology.clone();
    let mut mtus = Vec::new();
    let mut log = Vec::new();
    for edit in edits {
        if let Edit::SetMtu { endpoint: ep, mtu } = edit {
            if !(68..=65535).contains(mtu) {
                return Err(Error::invalid_argument(format!(
                    "MTU {mtu} is outside 68..=65535"
                )));
            }
            let ep = key(ep)?;
            let peer = link_peer(&desired, &ep)?;
            for link in desired
                .links
                .iter_mut()
                .filter(|l| l.endpoints.contains(&ep))
            {
                link.mtu = Some(*mtu);
            }
            log.push(format!("set mtu {mtu} on {ep} and {peer}"));
            mtus.push((ep, peer, *mtu));
        }
    }
    let structural: Vec<Edit> = edits
        .iter()
        .filter(|e| !matches!(e, Edit::SetMtu { .. }))
        .cloned()
        .collect();
    let mut warnings = Vec::new();
    if !structural.is_empty() {
        log.extend(apply_edits(&mut desired, &structural)?);
        let result = desired.validate();
        warnings = result.warnings().map(|w| w.to_string()).collect();
        result.bail()?;
    }
    Ok(Edited {
        topology: desired,
        log,
        warnings,
        mtus,
        structural: !structural.is_empty(),
    })
}

/// The other end of the link `endpoint` is on.
fn link_peer(topology: &Topology, endpoint: &str) -> Result<String> {
    topology
        .links
        .iter()
        .find_map(|l| match &l.endpoints {
            [a, b] if a == endpoint => Some(b.clone()),
            [a, b] if b == endpoint => Some(a.clone()),
            _ => None,
        })
        .ok_or_else(|| Error::invalid_argument(format!("no link has endpoint '{endpoint}'")))
}

/// Change the lab to `edited`: its MTUs live, then the rest through
/// `apply`. Returns the apply's report when there was one.
pub async fn apply_edited(
    running: &mut crate::RunningLab,
    edited: &Edited,
) -> Result<Option<crate::ApplyReport>> {
    for (ep, _, mtu) in &edited.mtus {
        running.set_link_mtu(ep, *mtu).await?;
    }
    if !edited.structural {
        return Ok(None);
    }
    crate::apply(running, &edited.topology).await.map(Some)
}

/// Apply `edits` to `topology` in place, without validating it, and
/// return one line per edit. [`Edit::SetMtu`] only records the MTU.
pub fn apply_edits(topology: &mut Topology, edits: &[Edit]) -> Result<Vec<String>> {
    let mut log = Vec::new();
    for edit in edits {
        log.push(apply_edit(topology, edit)?);
    }
    Ok(log)
}

fn apply_edit(topo: &mut Topology, edit: &Edit) -> Result<String> {
    Ok(match edit {
        Edit::AddNode { name, profile } => {
            if topo.nodes.contains_key(name) {
                return Err(Error::invalid_argument(format!(
                    "node '{name}' already exists"
                )));
            }
            let mut node = Node::default();
            if let Some(p) = profile {
                if !topo.profiles.contains_key(p) {
                    return Err(Error::invalid_argument(format!(
                        "profile '{p}' does not exist"
                    )));
                }
                node.profiles = vec![p.clone()];
            }
            topo.nodes.insert(name.clone(), node);
            format!("add node {name}")
        }
        Edit::RemoveNode { name } => {
            remove_node(topo, name)?;
            format!("remove node {name} (and its links, impairments, memberships)")
        }
        Edit::AddLink { a, b, addresses } => {
            let (a, b) = (key(a)?, key(b)?);
            for e in [&a, &b] {
                let node = endpoint(e)?.node;
                if !topo.nodes.contains_key(&node) {
                    return Err(Error::NodeNotFound { name: node });
                }
            }
            topo.links.push(Link {
                endpoints: [a.clone(), b.clone()],
                addresses: addresses
                    .as_ref()
                    .map(|[x, y]| [x.trim().to_string(), y.trim().to_string()]),
                mtu: None,
            });
            format!("add link {a} -- {b}")
        }
        Edit::RemoveLink { endpoint: ep } => {
            let key = key(ep)?;
            let ends: Vec<String> = topo
                .links
                .iter()
                .filter(|l| l.endpoints.contains(&key))
                .flat_map(|l| l.endpoints.iter().cloned())
                .collect();
            if ends.is_empty() {
                return Err(Error::invalid_argument(format!(
                    "no link has endpoint '{key}'"
                )));
            }
            topo.links.retain(|l| !l.endpoints.contains(&key));
            drop_link_ends(topo, &ends);
            format!("remove link at {key}")
        }
        Edit::SetImpairment {
            endpoint: ep,
            impairment,
        } => {
            let key = key(ep)?;
            topo.impairments.insert(key.clone(), impairment.clone());
            format!("set impairment on {key}: {}", impairment.summary())
        }
        Edit::ClearImpairment { endpoint: ep } => {
            let key = key(ep)?;
            if topo.impairments.remove(&key).is_none() {
                return Err(Error::invalid_argument(format!("no impairment on '{key}'")));
            }
            format!("clear impairment on {key}")
        }
        Edit::SetQdisc {
            endpoint: ep,
            qdisc,
        } => {
            let key = key(ep)?;
            topo.qdiscs.insert(
                key.clone(),
                QdiscConfig {
                    kind: qdisc.clone(),
                },
            );
            format!("set qdisc on {key}: {}", qdisc.name())
        }
        Edit::ClearQdisc { endpoint: ep } => {
            let key = key(ep)?;
            if topo.qdiscs.remove(&key).is_none() {
                return Err(Error::invalid_argument(format!("no qdisc on '{key}'")));
            }
            format!("clear qdisc on {key}")
        }
        Edit::SetPairImpairment {
            network,
            src,
            dst,
            impairment,
            rate_cap,
        } => {
            let net = topo.networks.get_mut(network).ok_or_else(|| {
                Error::invalid_argument(format!("network '{network}' does not exist"))
            })?;
            let at = net
                .impairments
                .iter()
                .position(|i| i.src == *src && i.dst == *dst);
            match (impairment, at) {
                (None, None) if rate_cap.is_none() => {
                    return Err(Error::invalid_argument(format!(
                        "no impairment from '{src}' to '{dst}' on network '{network}'"
                    )));
                }
                (None, Some(at)) if rate_cap.is_none() => {
                    net.impairments.remove(at);
                    return Ok(format!(
                        "clear impairment from {src} to {dst} on network {network}"
                    ));
                }
                _ => {}
            }
            let pair = crate::types::NetworkImpairment {
                src: src.clone(),
                dst: dst.clone(),
                impairment: impairment.clone().unwrap_or_default(),
                rate_cap: rate_cap.clone(),
            };
            match at {
                Some(at) => net.impairments[at] = pair,
                None => net.impairments.push(pair),
            }
            format!("set impairment from {src} to {dst} on network {network}")
        }
        Edit::SetMtu { endpoint: ep, mtu } => {
            let key = key(ep)?;
            let mut found = false;
            for link in topo.links.iter_mut().filter(|l| l.endpoints.contains(&key)) {
                link.mtu = Some(*mtu);
                found = true;
            }
            if !found {
                return Err(Error::invalid_argument(format!(
                    "no link has endpoint '{key}'"
                )));
            }
            format!("set mtu {mtu} on the link at {key}")
        }
    })
}

fn remove_node(topo: &mut Topology, name: &str) -> Result<()> {
    if topo.nodes.remove(name).is_none() {
        return Err(Error::NodeNotFound {
            name: name.to_string(),
        });
    }
    let on_node = |e: &str| endpoint(e).map(|e| e.node == name).unwrap_or(false);
    let touches = |l: &Link| l.endpoints.iter().any(|e| on_node(e));
    // Both ends of each removed link: the peer's end goes with it.
    let ends: Vec<String> = topo
        .links
        .iter()
        .filter(|l| touches(l))
        .flat_map(|l| l.endpoints.iter().cloned())
        .collect();
    topo.links.retain(|l| !touches(l));
    drop_link_ends(topo, &ends);
    topo.impairments.retain(|k, _| !on_node(k));
    topo.rate_limits.retain(|k, _| !on_node(k));
    topo.qdiscs.retain(|k, _| !on_node(k));
    for net in topo.networks.values_mut() {
        net.members.retain(|m| !on_node(m));
        net.ports.retain(|k, _| !on_node(k));
        net.impairments.retain(|i| i.src != name && i.dst != name);
    }
    for node in topo.nodes.values_mut() {
        node.depends_on.retain(|d| d != name);
        for wg in node.wireguard.values_mut() {
            wg.peers.retain(|p| p != name);
        }
    }
    Ok(())
}

/// Drop what a removed link's ends carried: a link's `delay` is stored on
/// both endpoints, and an entry left on the peer's end names an interface
/// that no longer exists, which fails validation (#204).
fn drop_link_ends(topo: &mut Topology, ends: &[String]) {
    for e in ends {
        topo.impairments.remove(e);
        topo.rate_limits.remove(e);
        topo.qdiscs.remove(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topo() -> Topology {
        crate::parser::parse(
            "lab \"t\"\nprofile r { forward ipv4 }\nnode a : r\nnode b\nnode c\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms }\nnetwork lan { subnet 10.9.0.0/24 members [a:eth1, b:eth1, c:eth1] }\n",
        )
        .unwrap()
    }

    fn imp(spec: &str) -> Impairment {
        Impairment::parse_spec(spec).unwrap()
    }

    #[test]
    fn edits_apply_in_order_and_validate() {
        let ed = edited(
            &topo(),
            &[
                Edit::AddNode {
                    name: "d".into(),
                    profile: Some("r".into()),
                },
                Edit::AddLink {
                    a: "b:eth2".into(),
                    b: "d:eth0".into(),
                    addresses: Some(["10.1.0.1/24".into(), "10.1.0.2/24".into()]),
                },
                Edit::SetImpairment {
                    endpoint: "d:eth0".into(),
                    impairment: imp("delay 20ms loss 1%"),
                },
            ],
        )
        .unwrap();
        assert_eq!(ed.log.len(), 3, "{:?}", ed.log);
        assert!(ed.structural && ed.mtus.is_empty());
        assert_eq!(ed.topology.nodes["d"].profiles, vec!["r".to_string()]);
        assert_eq!(
            ed.topology.impairments["d:eth0"].loss.as_deref(),
            Some("1%")
        );
    }

    #[test]
    fn an_edit_that_breaks_the_topology_is_refused() {
        // an impairment on an interface nothing creates
        let err = edited(
            &topo(),
            &[Edit::SetImpairment {
                endpoint: "a:eth9".into(),
                impairment: imp("delay 1ms"),
            }],
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::ValidationErrors(_) | Error::Validation(_)),
            "{err}"
        );
        assert!(edited(&topo(), &[]).is_err());
    }

    #[test]
    fn mtu_edits_are_live_and_find_the_peer() {
        let ed = edited(
            &topo(),
            &[Edit::SetMtu {
                endpoint: "a:eth0".into(),
                mtu: 1280,
            }],
        )
        .unwrap();
        assert!(!ed.structural);
        assert_eq!(ed.mtus, vec![("a:eth0".into(), "b:eth0".into(), 1280)]);
        assert_eq!(ed.topology.links[0].mtu, Some(1280));
        for bad in [("a:eth1", 1280), ("a:eth0", 40)] {
            let e = Edit::SetMtu {
                endpoint: bad.0.into(),
                mtu: bad.1,
            };
            assert!(edited(&topo(), &[e]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn pair_impairments_are_set_replaced_and_cleared() {
        let set = |spec: Option<&str>| Edit::SetPairImpairment {
            network: "lan".into(),
            src: "a".into(),
            dst: "b".into(),
            impairment: spec.map(imp),
            rate_cap: None,
        };
        let mut t = topo();
        apply_edits(&mut t, &[set(Some("delay 10ms"))]).unwrap();
        apply_edits(&mut t, &[set(Some("delay 30ms"))]).unwrap();
        let pairs = &t.networks["lan"].impairments;
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].impairment.delay.as_deref(), Some("30ms"));
        assert!(t.validate().bail().is_ok());
        apply_edits(&mut t, &[set(None)]).unwrap();
        assert!(t.networks["lan"].impairments.is_empty());
        let err = apply_edits(&mut t, &[set(None)]).unwrap_err().to_string();
        assert!(err.contains("no impairment from 'a' to 'b'"), "{err}");
    }

    #[test]
    fn removing_a_node_takes_its_pairs_and_memberships() {
        let mut t = topo();
        apply_edits(
            &mut t,
            &[
                Edit::SetPairImpairment {
                    network: "lan".into(),
                    src: "c".into(),
                    dst: "a".into(),
                    impairment: Some(imp("loss 5%")),
                    rate_cap: None,
                },
                Edit::RemoveNode { name: "c".into() },
            ],
        )
        .unwrap();
        let lan = &t.networks["lan"];
        assert!(lan.impairments.is_empty());
        assert_eq!(
            lan.members,
            vec!["a:eth1".to_string(), "b:eth1".to_string()]
        );
        assert!(t.validate().bail().is_ok(), "{:?}", t.validate());
    }

    #[test]
    fn edits_round_trip_as_json() {
        let e = Edit::SetQdisc {
            endpoint: "a:eth0".into(),
            qdisc: QdiscKind::parse_spec("fq_codel,target=5ms").unwrap(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"edit\":\"set_qdisc\""), "{json}");
        assert_eq!(serde_json::from_str::<Edit>(&json).unwrap(), e);
    }
}
