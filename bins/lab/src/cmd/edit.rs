//! `nlink-lab edit` — live topology edits on top of `apply` (issue #69).

use crate::ctx::{Ctx, require_root};
use crate::output::{DryRunReport, print_layered};
use anstream::println;
use nlink_lab::edit::Edit;

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
    #[arg(long, value_name = "NODE:IFACE", value_parser = crate::args::endpoint)]
    pub remove_link: Vec<String>,

    /// Set the impairment of an endpoint: `a:eth0=delay=10ms,loss=1%,jitter=2ms,rate=10mbit`
    /// (repeatable; replaces the endpoint's whole impairment).
    #[arg(long, value_name = "NODE:IFACE=K=V[,K=V…]", value_parser = crate::args::endpoint_impairment)]
    pub set_impair: Vec<(String, nlink_lab::Impairment)>,

    /// Clear the impairment of an endpoint (repeatable).
    #[arg(long, value_name = "NODE:IFACE", value_parser = crate::args::endpoint)]
    pub clear_impair: Vec<String>,

    /// Set a non-netem root qdisc on an endpoint:
    /// `a:eth0=tbf,rate=10mbit,burst=32kb,limit=100kb` / `a:eth0=fq_codel,target=5ms,ecn`
    /// / `a:eth0=sfq,perturb=10s` / `a:eth0=prio,bands=3` (repeatable;
    /// replaces the endpoint's qdisc block). fq_codel takes `ecn` or
    /// `noecn`; without either, ECN is the kernel's default, on.
    #[arg(long, value_name = "NODE:IFACE=KIND[,K=V…]", value_parser = crate::args::endpoint_qdisc)]
    pub set_qdisc: Vec<(String, nlink_lab::types::QdiscKind)>,

    /// Remove the qdisc block of an endpoint (repeatable).
    #[arg(long, value_name = "NODE:IFACE", value_parser = crate::args::endpoint)]
    pub clear_qdisc: Vec<String>,

    /// Set the MTU of a link, live, on both ends (`a:eth0=1280`).  The
    /// topology's declaration is updated so `verify` stays clean.
    #[arg(long, value_name = "NODE:IFACE=MTU", value_parser = crate::args::endpoint_mtu)]
    pub set_mtu: Vec<(String, u32)>,

    /// Show the resulting plan without applying it.
    #[arg(long)]
    pub dry_run: bool,
}

/// The edits `args` ask for, in flag order. Nothing to do is an error.
pub fn edits_of(args: &Args) -> nlink_lab::Result<Vec<Edit>> {
    let bad = |m: String| nlink_lab::Error::invalid_argument(m);
    let mut edits = Vec::new();
    for spec in &args.add_node {
        let (name, profile) = match spec.split_once(':') {
            Some((n, p)) => (n.to_string(), Some(p.to_string())),
            None => (spec.clone(), None),
        };
        edits.push(Edit::AddNode { name, profile });
    }
    for name in &args.remove_node {
        edits.push(Edit::RemoveNode { name: name.clone() });
    }
    for spec in &args.add_link {
        let (ends, addrs) = match spec.split_once('=') {
            Some((e, a)) => (e, Some(a)),
            None => (spec.as_str(), None),
        };
        let (a, b) = ends
            .split_once("--")
            .ok_or_else(|| bad(format!("--add-link {spec:?}: expected A:IF--B:IF")))?;
        let addresses = match addrs {
            Some(a) => {
                let (x, y) = a.split_once("--").ok_or_else(|| {
                    bad(format!("--add-link {spec:?}: addresses must be ADDR--ADDR"))
                })?;
                Some([x.trim().to_string(), y.trim().to_string()])
            }
            None => None,
        };
        edits.push(Edit::AddLink {
            a: a.trim().to_string(),
            b: b.trim().to_string(),
            addresses,
        });
    }
    for ep in &args.remove_link {
        edits.push(Edit::RemoveLink {
            endpoint: ep.clone(),
        });
    }
    for (endpoint, impairment) in &args.set_impair {
        edits.push(Edit::SetImpairment {
            endpoint: endpoint.clone(),
            impairment: impairment.clone(),
        });
    }
    for ep in &args.clear_impair {
        edits.push(Edit::ClearImpairment {
            endpoint: ep.clone(),
        });
    }
    for (endpoint, qdisc) in &args.set_qdisc {
        edits.push(Edit::SetQdisc {
            endpoint: endpoint.clone(),
            qdisc: qdisc.clone(),
        });
    }
    for ep in &args.clear_qdisc {
        edits.push(Edit::ClearQdisc {
            endpoint: ep.clone(),
        });
    }
    for (endpoint, mtu) in &args.set_mtu {
        edits.push(Edit::SetMtu {
            endpoint: endpoint.clone(),
            mtu: *mtu,
        });
    }
    if edits.is_empty() {
        return Err(bad(
            "nothing to do: pass --add-node/--remove-node/--add-link/--remove-link/--set-impair/--clear-impair/--set-qdisc/--clear-qdisc/--set-mtu".into(),
        ));
    }
    Ok(edits)
}

/// Apply the requested edits to `topo`, without validating it.
#[cfg(test)]
fn apply_edits(topo: &mut nlink_lab::Topology, args: &Args) -> nlink_lab::Result<Vec<String>> {
    nlink_lab::edit::apply_edits(topo, &edits_of(args)?)
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&args.lab)?;
    // Every edit is checked, and the result validated, before anything
    // changes: a bad edit next to an MTU change used to fail after the MTU
    // changed (#209).
    let ed = nlink_lab::edit::edited(running.topology(), &edits_of(&args)?)?;
    crate::cmd::common::print_warnings(&ed.warnings);
    if !ed.structural {
        // MTU changes are applied directly (both ends, live) rather than
        // through the plan: the plan only knows an MTU at veth creation.
        if !args.dry_run {
            nlink_lab::edit::apply_edited(&mut running, &ed).await?;
        }
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "lab": args.lab, "edits": ed.log, "ops": 0, "removed": 0, "applied": [] })
            );
        } else if !ctx.quiet {
            for l in &ed.log {
                println!("  {l}");
            }
        }
        return Ok(());
    }
    if args.dry_run {
        let plan = nlink_lab::apply_plan(&running, &ed.topology)?;
        let layered = nlink_lab::compute_layered_diff(&running, &ed.topology).await?;
        if ctx.json {
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
        for l in &ed.log {
            println!("  {l}");
        }
        crate::cmd::common::print_plan(&plan);
        print_layered(&layered, &[]);
        return Ok(());
    }
    let report = nlink_lab::edit::apply_edited(&mut running, &ed)
        .await?
        .unwrap_or_default();
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "lab": args.lab,
                "edits": ed.log,
                "ops": report.ops,
                "removed": report.removed,
                "applied": report.applied,
            })
        );
    } else if !ctx.quiet {
        for l in &ed.log {
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
        a.set_impair = vec![crate::args::endpoint_impairment("c:eth0=delay=20ms,loss=1%").unwrap()];
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
        // a bad impairment is the parser's: a usage error before anything
        let e = crate::args::endpoint_impairment("a:eth0=bogus=1").unwrap_err();
        assert!(e.contains("unknown impairment"), "{e}");
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
        let q = |s: &str| crate::args::endpoint_qdisc(s);
        let mut topo = topo();
        let mut a = args();
        a.set_qdisc = vec![
            q("a:eth0=tbf,rate=10mbit,burst=32kb,limit=100kb").unwrap(),
            q("b:eth0=fq_codel,target=5ms,ecn").unwrap(),
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

        let err = q("a:eth0=tbf,rate=10mbit").unwrap_err();
        assert!(err.contains("requires burst"), "{err}");
        let err = q("a:eth0=tbf,rate=10mbit,burst=32kb").unwrap_err();
        assert!(err.contains("requires limit"), "{err}");
        let err = q("a:eth0=sfq,rate=1mbit").unwrap_err();
        assert!(err.contains("unknown sfq parameter"), "{err}");
    }
}
