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

    /// Set a non-netem root qdisc on an endpoint:
    /// `a:eth0=tbf,rate=10mbit,burst=32kb,limit=100kb` / `a:eth0=fq_codel,target=5ms,ecn`
    /// / `a:eth0=sfq,perturb=10s` / `a:eth0=prio,bands=3` (repeatable;
    /// replaces the endpoint's qdisc block). fq_codel takes `ecn` or
    /// `noecn`; without either, ECN is the kernel's default, on.
    #[arg(long, value_name = "NODE:IFACE=KIND[,K=V…]")]
    pub set_qdisc: Vec<String>,

    /// Remove the qdisc block of an endpoint (repeatable).
    #[arg(long, value_name = "NODE:IFACE")]
    pub clear_qdisc: Vec<String>,

    /// Set the MTU of a link, live, on both ends (`a:eth0=1280`).  The
    /// topology's declaration is updated so `verify` stays clean.
    #[arg(long, value_name = "NODE:IFACE=MTU")]
    pub set_mtu: Vec<String>,

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
            return Err(nlink_lab::Error::invalid_argument(format!(
                "node '{name}' already exists"
            )));
        }
        let mut node = nlink_lab::types::Node::default();
        if let Some(p) = profile {
            if !topo.profiles.contains_key(&p) {
                return Err(nlink_lab::Error::invalid_argument(format!(
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
        let touches = |l: &nlink_lab::types::Link| {
            l.endpoints
                .iter()
                .any(|e| endpoint(e).map(|e| e.node == *name).unwrap_or(false))
        };
        // Both ends of each removed link: the peer's end goes with it.
        let ends: Vec<String> = topo
            .links
            .iter()
            .filter(|l| touches(l))
            .flat_map(|l| l.endpoints.iter().cloned())
            .collect();
        topo.links.retain(|l| !touches(l));
        drop_link_ends(topo, &ends);
        topo.impairments
            .retain(|k, _| endpoint(k).map(|e| e.node != *name).unwrap_or(true));
        topo.rate_limits
            .retain(|k, _| endpoint(k).map(|e| e.node != *name).unwrap_or(true));
        topo.qdiscs
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
            nlink_lab::Error::invalid_argument(format!("--add-link {spec:?}: expected A:IF--B:IF"))
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
                    nlink_lab::Error::invalid_argument(format!(
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
        let ends: Vec<String> = topo
            .links
            .iter()
            .filter(|l| l.endpoints.contains(&key))
            .flat_map(|l| l.endpoints.iter().cloned())
            .collect();
        if ends.is_empty() {
            return Err(nlink_lab::Error::invalid_argument(format!(
                "no link has endpoint '{key}'"
            )));
        }
        topo.links.retain(|l| !l.endpoints.contains(&key));
        drop_link_ends(topo, &ends);
        log.push(format!("remove link at {key}"));
    }
    for spec in &args.set_impair {
        let (ep, props) = spec.split_once('=').ok_or_else(|| {
            nlink_lab::Error::invalid_argument(format!(
                "--set-impair {spec:?}: expected NODE:IFACE=K=V[,K=V…]"
            ))
        })?;
        let ep = endpoint(ep.trim())?;
        let key = format!("{}:{}", ep.node, ep.iface);
        let mut imp = nlink_lab::types::Impairment::default();
        for kv in props.split(',') {
            let (k, v) = kv.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_argument(format!(
                    "--set-impair {spec:?}: bad item {kv:?}"
                ))
            })?;
            // One list of property names for `edit`, `top` and NLL.
            imp.set_property(k, v).map_err(|e| {
                nlink_lab::Error::invalid_argument(format!("--set-impair {spec:?}: {e}"))
            })?;
        }
        topo.impairments.insert(key.clone(), imp);
        log.push(format!("set impairment on {key}: {}", props.trim()));
    }
    for ep in &args.clear_impair {
        let ep = endpoint(ep)?;
        let key = format!("{}:{}", ep.node, ep.iface);
        if topo.impairments.remove(&key).is_none() {
            return Err(nlink_lab::Error::invalid_argument(format!(
                "no impairment on '{key}'"
            )));
        }
        log.push(format!("clear impairment on {key}"));
    }
    for spec in &args.set_qdisc {
        let (ep, rest) = spec.split_once('=').ok_or_else(|| {
            nlink_lab::Error::invalid_argument(format!(
                "--set-qdisc {spec:?}: expected NODE:IFACE=KIND[,K=V…]"
            ))
        })?;
        let ep = endpoint(ep.trim())?;
        let key = format!("{}:{}", ep.node, ep.iface);
        let kind = parse_qdisc_spec(spec, rest)?;
        topo.qdiscs
            .insert(key.clone(), nlink_lab::types::QdiscConfig { kind });
        log.push(format!("set qdisc on {key}: {}", rest.trim()));
    }
    for ep in &args.clear_qdisc {
        let ep = endpoint(ep)?;
        let key = format!("{}:{}", ep.node, ep.iface);
        if topo.qdiscs.remove(&key).is_none() {
            return Err(nlink_lab::Error::invalid_argument(format!(
                "no qdisc on '{key}'"
            )));
        }
        log.push(format!("clear qdisc on {key}"));
    }
    if log.is_empty() {
        return Err(nlink_lab::Error::invalid_argument(
            "nothing to do: pass --add-node/--remove-node/--add-link/--remove-link/--set-impair/--clear-impair/--set-qdisc/--clear-qdisc",
        ));
    }
    Ok(log)
}

/// `KIND[,K=V…]` → [`QdiscKind`](nlink_lab::types::QdiscKind). Bare
/// `ecn` / `noecn` turn fq_codel's ECN on or off (the kernel's default is
/// on).
fn parse_qdisc_spec(spec: &str, rest: &str) -> nlink_lab::Result<nlink_lab::types::QdiscKind> {
    use nlink_lab::types::QdiscKind;
    let bad =
        |what: String| nlink_lab::Error::invalid_argument(format!("--set-qdisc {spec:?}: {what}"));
    let mut items = rest.split(',').map(str::trim).filter(|s| !s.is_empty());
    let kind = items
        .next()
        .ok_or_else(|| bad("missing qdisc kind".into()))?;
    let mut kv: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    let mut ecn: Option<bool> = None;
    for item in items {
        match item.split_once('=') {
            Some((k, v)) => {
                kv.insert(k.trim(), v.trim().to_string());
            }
            None if item == "ecn" || item == "noecn" => {
                let on = item == "ecn";
                if ecn.is_some_and(|was| was != on) {
                    return Err(bad("`ecn` and `noecn` contradict each other".into()));
                }
                ecn = Some(on);
            }
            None => return Err(bad(format!("bad item {item:?} (expected K=V)"))),
        }
    }
    let num = |k: &str| -> nlink_lab::Result<Option<u32>> {
        kv.get(k)
            .map(|v| {
                v.parse::<u32>()
                    .map_err(|_| bad(format!("{k}: expected an integer, got {v:?}")))
            })
            .transpose()
    };
    let allowed = |names: &[&str]| -> nlink_lab::Result<()> {
        for k in kv.keys() {
            if !names.contains(k) {
                return Err(bad(format!(
                    "unknown {kind} parameter {k:?} (allowed: {})",
                    names.join(", ")
                )));
            }
        }
        Ok(())
    };
    Ok(match kind {
        _ if ecn.is_some() && kind != "fq_codel" => {
            return Err(bad(format!(
                "`ecn`/`noecn` is a fq_codel setting, not {kind}'s"
            )));
        }
        "tbf" => {
            allowed(&["rate", "burst", "limit", "peakrate", "mtu"])?;
            QdiscKind::Tbf {
                rate: kv
                    .get("rate")
                    .cloned()
                    .ok_or_else(|| bad("tbf requires rate=".into()))?,
                burst: kv
                    .get("burst")
                    .cloned()
                    .ok_or_else(|| bad("tbf requires burst=".into()))?,
                // No limit means no queue: the TBF would drop everything.
                limit: Some(kv.get("limit").cloned().ok_or_else(|| {
                    bad("tbf requires limit= (a byte size, e.g. limit=64kb)".into())
                })?),
                peakrate: kv.get("peakrate").cloned(),
                mtu: num("mtu")?,
            }
        }
        "fq_codel" => {
            allowed(&["target", "interval", "limit", "flows", "quantum"])?;
            QdiscKind::FqCodel {
                target: kv.get("target").cloned(),
                interval: kv.get("interval").cloned(),
                limit: num("limit")?,
                flows: num("flows")?,
                quantum: num("quantum")?,
                ecn,
            }
        }
        "sfq" => {
            allowed(&["perturb", "limit", "quantum"])?;
            QdiscKind::Sfq {
                perturb: kv.get("perturb").cloned(),
                limit: num("limit")?,
                quantum: num("quantum")?,
            }
        }
        "prio" => {
            allowed(&["bands"])?;
            QdiscKind::Prio {
                bands: num("bands")?
                    .map(|b| u8::try_from(b).map_err(|_| bad(format!("bands {b} out of range"))))
                    .transpose()?,
            }
        }
        other => {
            return Err(bad(format!(
                "unknown qdisc kind {other:?} (tbf, fq_codel, sfq, prio)"
            )));
        }
    })
}

/// `NODE:IFACE=MTU` -> (endpoint, mtu).
pub fn parse_mtu_spec(spec: &str) -> nlink_lab::Result<(String, u32)> {
    let (ep, mtu) = spec.split_once('=').ok_or_else(|| {
        nlink_lab::Error::invalid_argument(format!(
            "--set-mtu expects NODE:IFACE=MTU, got {spec:?}"
        ))
    })?;
    let mtu: u32 = mtu.trim().parse().map_err(|_| {
        nlink_lab::Error::invalid_argument(format!("--set-mtu: {mtu:?} is not an MTU"))
    })?;
    if !(68..=65535).contains(&mtu) {
        return Err(nlink_lab::Error::invalid_argument(format!(
            "--set-mtu: {mtu} is outside 68..=65535"
        )));
    }
    Ok((ep.trim().to_string(), mtu))
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&args.lab)?;
    // MTU changes are applied directly (both ends, live) rather than through
    // the plan: the plan only knows an MTU at veth creation. They are
    // checked first, with the other edits, and applied only once those
    // validate: a bad edit next to one used to fail after the MTU changed
    // (#209).
    let mut mtus = Vec::new();
    for spec in &args.set_mtu {
        let (ep, mtu) = parse_mtu_spec(spec)?;
        let peer = running.peer_endpoint(&ep)?;
        mtus.push((ep, peer, mtu));
    }
    let mut desired = running.topology().clone();
    for (ep, _, mtu) in &mtus {
        for link in desired
            .links
            .iter_mut()
            .filter(|l| l.endpoints.contains(ep))
        {
            link.mtu = Some(*mtu);
        }
    }
    let edit_log = apply_edits_unless_only_mtu(&mut desired, &args)?;
    if edit_log.is_some() {
        let result = desired.validate();
        for w in result.warnings() {
            eprintln!("  {} {w}", yellow("WARN"));
        }
        result.bail()?;
    }
    let mut mtu_log = Vec::new();
    for (ep, peer, mtu) in &mtus {
        if args.dry_run {
            mtu_log.push(format!("set mtu {mtu} on {ep} and {peer} (live)"));
        } else {
            running.set_link_mtu(ep, *mtu).await?;
            mtu_log.push(format!("set mtu {mtu} on {ep} and {peer}"));
        }
    }
    if edit_log.is_none() {
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "lab": args.lab, "edits": mtu_log, "ops": 0, "removed": 0, "applied": [] })
            );
        } else if !ctx.quiet {
            for l in &mtu_log {
                println!("  {l}");
            }
        }
        return Ok(());
    }
    let mut log = edit_log.unwrap_or_default();
    log.splice(0..0, mtu_log);
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
            let plan_ops: Vec<String> = plan.work().map(|o| o.describe()).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &DryRunReport::new(&args.lab, &layered, &removals).with_ops(&plan_ops)
                )?
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

/// [`apply_edits`], or `None` when `--set-mtu` is the only edit (which
/// `apply_edits` refuses as "nothing to do").
fn apply_edits_unless_only_mtu(
    topo: &mut nlink_lab::Topology,
    args: &Args,
) -> nlink_lab::Result<Option<Vec<String>>> {
    let only_mtu = !args.set_mtu.is_empty()
        && args.add_node.is_empty()
        && args.remove_node.is_empty()
        && args.add_link.is_empty()
        && args.remove_link.is_empty()
        && args.set_impair.is_empty()
        && args.clear_impair.is_empty()
        && args.set_qdisc.is_empty()
        && args.clear_qdisc.is_empty();
    if only_mtu {
        Ok(None)
    } else {
        apply_edits(topo, args).map(Some)
    }
}

/// Drop what a removed link's ends carried: a link's `delay` is stored on
/// both endpoints, and an entry left on the peer's end names an interface
/// that no longer exists, which fails validation (#204).
fn drop_link_ends(topo: &mut nlink_lab::Topology, ends: &[String]) {
    for e in ends {
        topo.impairments.remove(e);
        topo.rate_limits.remove(e);
        topo.qdiscs.remove(e);
    }
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
            set_qdisc: vec![],
            set_mtu: vec![],
            clear_qdisc: vec![],
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
        // The peer's end of the removed link carried the link's delay; it
        // goes too, or the topology names a missing interface (#204).
        assert!(!t.impairments.contains_key("a:eth0"), "{:?}", t.impairments);
        assert_eq!(t.networks["lan"].members, vec!["a:eth1".to_string()]);
        assert!(t.validate().bail().is_ok(), "{:?}", t.validate());
    }

    #[test]
    fn remove_link_drops_both_ends() {
        let mut t = topo();
        let mut a = args();
        a.remove_link = vec!["a:eth0".into()];
        apply_edits(&mut t, &a).unwrap();
        assert!(t.links.is_empty());
        assert!(
            !t.impairments.contains_key("a:eth0") && !t.impairments.contains_key("b:eth0"),
            "{:?}",
            t.impairments
        );
        assert!(t.validate().bail().is_ok(), "{:?}", t.validate());
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
    #[test]
    fn set_qdisc_parses_and_clears() {
        use nlink_lab::types::QdiscKind;
        let mut topo = topo();
        let mut a = args();
        a.set_qdisc = vec![
            "a:eth0=tbf,rate=10mbit,burst=32kb,limit=100kb".into(),
            "b:eth0=fq_codel,target=5ms,ecn".into(),
        ];
        let log = apply_edits(&mut topo, &a).unwrap();
        assert_eq!(log.len(), 2);
        assert!(matches!(
            &topo.qdiscs["a:eth0"].kind,
            QdiscKind::Tbf { rate, burst, limit: Some(l), .. } if rate == "10mbit" && burst == "32kb" && l == "100kb"
        ));
        assert!(
            matches!(&topo.qdiscs["b:eth0"].kind, QdiscKind::FqCodel { ecn: Some(true), target: Some(t), .. } if t == "5ms")
        );

        let mut a = args();
        a.clear_qdisc = vec!["a:eth0".into()];
        apply_edits(&mut topo, &a).unwrap();
        assert!(!topo.qdiscs.contains_key("a:eth0"));

        let mut a = args();
        a.set_qdisc = vec!["a:eth0=tbf,rate=10mbit".into()];
        let err = apply_edits(&mut topo, &a).unwrap_err().to_string();
        assert!(err.contains("requires burst"), "{err}");
        let mut a = args();
        a.set_qdisc = vec!["a:eth0=tbf,rate=10mbit,burst=32kb".into()];
        let err = apply_edits(&mut topo, &a).unwrap_err().to_string();
        assert!(err.contains("requires limit"), "{err}");
        let mut a = args();
        a.set_qdisc = vec!["a:eth0=sfq,rate=1mbit".into()];
        let err = apply_edits(&mut topo, &a).unwrap_err().to_string();
        assert!(err.contains("unknown sfq parameter"), "{err}");
    }
}
