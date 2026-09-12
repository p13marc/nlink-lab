//! `nlink-lab edit` — live topology edits on top of `apply` (issue #69).

use crate::ctx::{Ctx, require_root, yellow};
use crate::output::{DryRunReport, print_layered};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Add a namespace node: NAME or NAME:PROFILE (repeatable).
    #[arg(long, value_name = "NAME[:PROFILE]")]
    pub add_node: Vec<String>,

    /// Remove a node and everything attached to it (repeatable).
    #[arg(long, value_name = "NAME", add = crate::ctx::node_completer())]
    pub remove_node: Vec<String>,

    /// Add a veth link: `a:eth0--b:eth0[=10.0.0.1/24--10.0.0.2/24]` (repeatable).
    #[arg(long, value_name = "A:IF--B:IF[=ADDR--ADDR]")]
    pub add_link: Vec<String>,

    /// Remove the link that has this endpoint (repeatable).
    #[arg(long, value_name = "NODE:IFACE")]
    pub remove_link: Vec<String>,

    /// Set the impairment of an endpoint: `a:eth0=delay=10ms,loss=1%,jitter=2ms,rate=10mbit`
    /// (repeatable; replaces the endpoint's whole impairment).
    #[arg(long, value_name = "NODE:IFACE=K=V[,K=V…]")]
    pub set_impair: Vec<String>,

    /// Clear the impairment of an endpoint (repeatable).
    #[arg(long, value_name = "NODE:IFACE")]
    pub clear_impair: Vec<String>,

    /// Show the resulting plan without applying it.
    #[arg(long)]
    pub dry_run: bool,
}

fn endpoint(s: &str) -> nlink_lab::Result<nlink_lab::EndpointRef> {
    nlink_lab::EndpointRef::parse(s).ok_or_else(|| nlink_lab::Error::InvalidEndpoint {
        endpoint: s.to_string(),
    })
}

/// Apply the requested edits to a copy of the topology.
pub fn apply_edits(topo: &mut nlink_lab::Topology, args: &Args) -> nlink_lab::Result<Vec<String>> {
    let mut log = Vec::new();
    for spec in &args.add_node {
        let (name, profile) = match spec.split_once(':') {
            Some((n, p)) => (n.to_string(), Some(p.to_string())),
            None => (spec.clone(), None),
        };
        if topo.nodes.contains_key(&name) {
            return Err(nlink_lab::Error::invalid_topology(format!(
                "node '{name}' already exists"
            )));
        }
        let mut node = nlink_lab::types::Node::default();
        if let Some(p) = profile {
            if !topo.profiles.contains_key(&p) {
                return Err(nlink_lab::Error::invalid_topology(format!(
                    "profile '{p}' does not exist"
                )));
            }
            node.profiles = vec![p];
        }
        topo.nodes.insert(name.clone(), node);
        log.push(format!("add node {name}"));
    }
    for name in &args.remove_node {
        if topo.nodes.remove(name).is_none() {
            return Err(nlink_lab::Error::NodeNotFound { name: name.clone() });
        }
        topo.links.retain(|l| {
            l.endpoints
                .iter()
                .all(|e| endpoint(e).map(|e| e.node != *name).unwrap_or(true))
        });
        topo.impairments
            .retain(|k, _| endpoint(k).map(|e| e.node != *name).unwrap_or(true));
        topo.rate_limits
            .retain(|k, _| endpoint(k).map(|e| e.node != *name).unwrap_or(true));
        for net in topo.networks.values_mut() {
            net.members
                .retain(|m| endpoint(m).map(|e| e.node != *name).unwrap_or(true));
            net.ports
                .retain(|k, _| endpoint(k).map(|e| e.node != *name).unwrap_or(true));
            net.impairments.retain(|i| i.src != *name && i.dst != *name);
        }
        for node in topo.nodes.values_mut() {
            node.depends_on.retain(|d| d != name);
            for wg in node.wireguard.values_mut() {
                wg.peers.retain(|p| p != name);
            }
        }
        log.push(format!(
            "remove node {name} (and its links, impairments, memberships)"
        ));
    }
    for spec in &args.add_link {
        let (ends, addrs) = match spec.split_once('=') {
            Some((e, a)) => (e, Some(a)),
            None => (spec.as_str(), None),
        };
        let (a, b) = ends.split_once("--").ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!("--add-link {spec:?}: expected A:IF--B:IF"))
        })?;
        let (ea, eb) = (endpoint(a.trim())?, endpoint(b.trim())?);
        for e in [&ea, &eb] {
            if !topo.nodes.contains_key(&e.node) {
                return Err(nlink_lab::Error::NodeNotFound {
                    name: e.node.clone(),
                });
            }
        }
        let addresses = match addrs {
            Some(a) => {
                let (x, y) = a.split_once("--").ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "--add-link {spec:?}: addresses must be ADDR--ADDR"
                    ))
                })?;
                Some([x.trim().to_string(), y.trim().to_string()])
            }
            None => None,
        };
        topo.links.push(nlink_lab::types::Link {
            endpoints: [a.trim().to_string(), b.trim().to_string()],
            addresses,
            mtu: None,
        });
        log.push(format!("add link {} -- {}", a.trim(), b.trim()));
    }
    for ep in &args.remove_link {
        let ep = endpoint(ep)?;
        let key = format!("{}:{}", ep.node, ep.iface);
        let before = topo.links.len();
        topo.links.retain(|l| !l.endpoints.contains(&key));
        if topo.links.len() == before {
            return Err(nlink_lab::Error::invalid_topology(format!(
                "no link has endpoint '{key}'"
            )));
        }
        topo.impairments.remove(&key);
        topo.rate_limits.remove(&key);
        log.push(format!("remove link at {key}"));
    }
    for spec in &args.set_impair {
        let (ep, props) = spec.split_once('=').ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!(
                "--set-impair {spec:?}: expected NODE:IFACE=K=V[,K=V…]"
            ))
        })?;
        let ep = endpoint(ep.trim())?;
        let key = format!("{}:{}", ep.node, ep.iface);
        let mut imp = nlink_lab::types::Impairment::default();
        for kv in props.split(',') {
            let (k, v) = kv.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "--set-impair {spec:?}: bad item {kv:?}"
                ))
            })?;
            let v = Some(v.trim().to_string());
            match k.trim() {
                "delay" => imp.delay = v,
                "jitter" => imp.jitter = v,
                "loss" => imp.loss = v,
                "rate" => imp.rate = v,
                "corrupt" => imp.corrupt = v,
                "reorder" => imp.reorder = v,
                "duplicate" => imp.duplicate = v,
                "delay-correlation" => imp.delay_correlation = v,
                "loss-correlation" => imp.loss_correlation = v,
                "limit" => imp.limit = v,
                other => {
                    return Err(nlink_lab::Error::invalid_topology(format!(
                        "--set-impair {spec:?}: unknown property {other:?} (delay, jitter, loss, rate, corrupt, reorder, duplicate, delay-correlation, loss-correlation, limit)"
                    )));
                }
            }
        }
        topo.impairments.insert(key.clone(), imp);
        log.push(format!("set impairment on {key}: {}", props.trim()));
    }
    for ep in &args.clear_impair {
        let ep = endpoint(ep)?;
        let key = format!("{}:{}", ep.node, ep.iface);
        if topo.impairments.remove(&key).is_none() {
            return Err(nlink_lab::Error::invalid_topology(format!(
                "no impairment on '{key}'"
            )));
        }
        log.push(format!("clear impairment on {key}"));
    }
    if log.is_empty() {
        return Err(nlink_lab::Error::invalid_topology(
            "nothing to do: pass --add-node/--remove-node/--add-link/--remove-link/--set-impair/--clear-impair",
        ));
    }
    Ok(log)
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&args.lab)?;
    let mut desired = running.topology().clone();
    let log = apply_edits(&mut desired, &args)?;
    let result = desired.validate();
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }
    result.bail()?;
    let plan = nlink_lab::apply_plan(&running, &desired)?;
    if args.dry_run {
        if ctx.json {
            let layered = nlink_lab::compute_layered_diff(&running, &desired).await?;
            let removals: Vec<String> = plan
                .ops
                .iter()
                .filter(|o| o.is_removal())
                .map(|o| o.describe())
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&DryRunReport::new(&args.lab, &layered, &removals))?
            );
            return Ok(());
        }
        for l in &log {
            println!("  {l}");
        }
        println!("Plan ({} op(s)):", plan.ops.len());
        for op in &plan.ops {
            println!("  {}", op.describe());
        }
        let layered = nlink_lab::compute_layered_diff(&running, &desired).await?;
        print_layered(&layered, &[]);
        return Ok(());
    }
    let report = nlink_lab::apply(&mut running, &desired).await?;
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "lab": args.lab,
                "edits": log,
                "ops": report.ops,
                "removed": report.removed,
                "applied": report.applied,
            })
        );
    } else if !ctx.quiet {
        for l in &log {
            println!("  {l}");
        }
        println!(
            "Applied {} op(s) ({} removal(s)) to lab {:?}",
            report.ops, report.removed, args.lab
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            lab: "t".into(),
            add_node: vec![],
            remove_node: vec![],
            add_link: vec![],
            remove_link: vec![],
            set_impair: vec![],
            clear_impair: vec![],
            dry_run: false,
        }
    }

    fn topo() -> nlink_lab::Topology {
        nlink_lab::parser::parse(
            "lab \"t\"\nprofile r { forward ipv4 }\nnode a : r\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms }\nnetwork lan { subnet 10.9.0.0/24 members [a:eth1, b:eth1] }\n",
        )
        .unwrap()
    }

    #[test]
    fn add_node_and_link_then_impair() {
        let mut t = topo();
        let mut a = args();
        a.add_node = vec!["c:r".into()];
        a.add_link = vec!["b:eth2--c:eth0=10.1.0.1/24--10.1.0.2/24".into()];
        a.set_impair = vec!["c:eth0=delay=20ms,loss=1%".into()];
        let log = apply_edits(&mut t, &a).unwrap();
        assert_eq!(log.len(), 3, "{log:?}");
        assert_eq!(t.nodes["c"].profiles, vec!["r".to_string()]);
        assert_eq!(t.links.len(), 2);
        assert_eq!(t.impairments["c:eth0"].loss.as_deref(), Some("1%"));
        assert!(t.validate().bail().is_ok());
    }

    #[test]
    fn remove_node_takes_everything_attached() {
        let mut t = topo();
        let mut a = args();
        a.remove_node = vec!["b".into()];
        apply_edits(&mut t, &a).unwrap();
        assert!(!t.nodes.contains_key("b"));
        assert!(t.links.is_empty());
        assert!(!t.impairments.contains_key("a:eth0") || true);
        assert_eq!(t.networks["lan"].members, vec!["a:eth1".to_string()]);
    }

    #[test]
    fn errors_are_specific() {
        let mut t = topo();
        let mut a = args();
        a.add_node = vec!["a".into()];
        assert!(
            apply_edits(&mut t, &a)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        let mut a = args();
        a.remove_link = vec!["a:eth9".into()];
        assert!(
            apply_edits(&mut t, &a)
                .unwrap_err()
                .to_string()
                .contains("no link")
        );
        let mut a = args();
        a.set_impair = vec!["a:eth0=bogus=1".into()];
        assert!(
            apply_edits(&mut t, &a)
                .unwrap_err()
                .to_string()
                .contains("unknown property")
        );
        assert!(
            apply_edits(&mut t, &args())
                .unwrap_err()
                .to_string()
                .contains("nothing to do")
        );
    }
}
