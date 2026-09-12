//! `nlink-lab validate`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology, validation_failed, yellow};
use crate::output::{EXIT_VALIDATION, set_exit_code};
use crate::render::print_topology_summary;

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Show resolved IP addresses for all interfaces.
    #[arg(long)]
    pub show_ips: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        show_ips,
    } = args;
    let topo = parse_topology(&topology, &params)?;
    let result = topo.validate();

    if ctx.json {
        // One envelope for both outcomes; exit 2 on errors (#46).
        let issues: Vec<&nlink_lab::ValidationIssue> = result.issues().iter().collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "lab": topo.lab.name,
                "valid": !result.has_errors(),
                "nodes": topo.nodes.len(),
                "links": topo.links.len(),
                "networks": topo.networks.len(),
                "errors": result.errors().count(),
                "warnings": result.warnings().count(),
                "issues": issues,
            }))?
        );
        if result.has_errors() {
            set_exit_code(EXIT_VALIDATION);
        }
        return Ok(());
    }

    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }

    if result.has_errors() {
        return Err(validation_failed(&topo.lab.name, &result));
    }

    println!("Topology {:?} is valid", topo.lab.name);
    print_topology_summary(&topo);

    if show_ips {
        println!("\n  Addresses:");
        // From links
        for link in &topo.links {
            if let Some(ref addrs) = link.addresses {
                for (i, ep_str) in link.endpoints.iter().enumerate() {
                    println!("    {:<24} {} (link)", ep_str, addrs[i]);
                }
            }
        }
        // From network ports
        for (net_name, network) in &topo.networks {
            for member in &network.members {
                if let Some(ep) = nlink_lab::EndpointRef::parse(member) {
                    // Port keys can be either "node:iface" or "node"
                    let port = network
                        .ports
                        .get(member)
                        .or_else(|| network.ports.get(&ep.node));
                    if let Some(port) = port {
                        for addr in &port.addresses {
                            println!("    {:<24} {} (network {:?})", member, addr, net_name);
                        }
                    }
                }
            }
        }
        // From node interfaces (loopback, etc.)
        for (name, node) in &topo.nodes {
            for (iface, cfg) in &node.interfaces {
                for addr in &cfg.addresses {
                    println!("    {name}:{iface:<18} {addr} (interface)");
                }
            }
        }
    }
    Ok(())
}
