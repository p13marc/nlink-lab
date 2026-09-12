//! `nlink-lab status`.

use crate::ctx::Ctx;
use crate::host_scan::{Orphans, find_orphans};
use crate::util::host_resources_json;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (omit to list all).
    pub name: Option<String>,

    /// Also scan the host for mgmt bridges / namespaces with no
    /// matching state file (orphans), and labs whose state file
    /// claims namespaces no longer present on the host (stale).
    #[arg(long)]
    pub scan: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { name, scan } = args;
    match name {
        None => {
            let labs = nlink_lab::RunningLab::list()?;
            let orphans = if scan {
                find_orphans(&labs).await
            } else {
                Orphans::default()
            };
            if ctx.json {
                if scan {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crate::output::StatusScanReport {
                            labs: &labs,
                            orphans: &orphans,
                        })?
                    );
                } else {
                    println!("{}", serde_json::to_string_pretty(&labs)?);
                }
            } else if labs.is_empty() {
                println!("No running labs.");
            } else {
                println!("{:<18} {:<6} CREATED", "NAME", "NODES");
                for info in labs {
                    println!(
                        "{:<18} {:<6} {}",
                        info.name, info.node_count, info.created_at
                    );
                }
            }
            if scan && !ctx.json && !orphans.is_empty() {
                let has_orphans = !orphans.bridges.is_empty()
                    || !orphans.veths.is_empty()
                    || !orphans.netns.is_empty();
                if has_orphans {
                    println!();
                    println!("Orphans detected (no matching state file):");
                    for b in &orphans.bridges {
                        println!("  bridge {b}");
                    }
                    for v in &orphans.veths {
                        println!("  veth   {v}");
                    }
                    for n in &orphans.netns {
                        println!("  netns  {n}");
                    }
                    println!();
                    println!("Run `nlink-lab destroy --orphans` to clean up.");
                }
                if !orphans.stale.is_empty() {
                    println!();
                    println!("Stale labs detected (state file with missing resources):");
                    for s in &orphans.stale {
                        println!(
                            "  {}  (missing: {})",
                            s.name,
                            s.missing_namespaces.join(", ")
                        );
                    }
                    println!();
                    println!("Run `nlink-lab destroy <lab>` to clean up each stale state file.");
                }
            }
            if scan && !ctx.json && ctx.verbose && orphans.untagged_ignored > 0 {
                println!();
                println!(
                    "{} untagged namespace(s) ignored (not created by nlink-lab).",
                    orphans.untagged_ignored
                );
            }
            Ok(())
        }
        Some(name) => {
            let lab = nlink_lab::RunningLab::load(&name)?;
            if ctx.json {
                let mut output = serde_json::to_value(lab.topology())?;
                // Add resolved addresses per node (including mgmt0)
                if let Some(nodes) = output.get_mut("nodes")
                    && let Some(nodes_obj) = nodes.as_object_mut()
                {
                    for node_name in nodes_obj.keys().cloned().collect::<Vec<_>>() {
                        if let Ok(addrs) = lab.node_addresses(&node_name)
                            && !addrs.is_empty()
                            && let Some(n) = nodes_obj.get_mut(&node_name)
                            && let Some(o) = n.as_object_mut()
                        {
                            o.insert("addresses".to_string(), serde_json::json!(addrs));
                        }
                    }
                }
                // Add host_resources — round-5 §1.2 bonus. Lets
                // consumers detect parallel-lab collisions
                // client-side (mgmt bridge name, declared subnets).
                if let Some(o) = output.as_object_mut() {
                    o.insert("host_resources".to_string(), host_resources_json(&lab));
                }
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                let topo = lab.topology();
                println!("Lab: {}", lab.name());
                println!(
                    "Nodes: {}  Links: {}  Impairments: {}",
                    lab.namespace_count(),
                    topo.links.len(),
                    topo.impairments.len()
                );
                println!();
                println!("  {:<20} {:<12} IMAGE", "NODE", "TYPE");
                let mut names: Vec<&String> = topo.nodes.keys().collect();
                names.sort();
                for name in names {
                    let node = &topo.nodes[name];
                    let kind = if node.image.is_some() {
                        "container"
                    } else {
                        "namespace"
                    };
                    let image = node.image.as_deref().unwrap_or("--");
                    println!("  {:<20} {:<12} {}", name, kind, image);
                }
            }
            Ok(())
        }
    }
}
