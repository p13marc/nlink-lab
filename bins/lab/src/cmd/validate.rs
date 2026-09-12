//! `nlink-lab validate`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology, validation_failed, yellow};
use crate::output::{EXIT_VALIDATION, set_exit_code};
use crate::render::print_topology_summary;

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    #[arg(required_unless_present = "list_rules")]
    pub topology: Option<PathBuf>,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Show resolved IP addresses for all interfaces.
    #[arg(long)]
    pub show_ips: bool,

    /// Treat every warning as an error (exit 2).
    #[arg(long)]
    pub strict: bool,

    /// Promote one warning rule to an error (repeatable).
    #[arg(long, value_name = "RULE")]
    pub deny: Vec<String>,

    /// Silence one warning rule (repeatable). Errors cannot be silenced.
    #[arg(long, value_name = "RULE")]
    pub allow: Vec<String>,

    /// Print every validation rule with its default severity and exit.
    #[arg(long, exclusive = true)]
    pub list_rules: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        show_ips,
        strict,
        deny,
        allow,
        list_rules,
    } = args;
    if list_rules {
        return print_rules(ctx);
    }
    let opts = nlink_lab::RuleOptions {
        strict,
        deny,
        allow,
    };
    opts.check_known()
        .map_err(nlink_lab::Error::invalid_topology)?;
    let topology = topology.expect("clap: topology is required unless --list-rules");
    let topo = parse_topology(&topology, &params)?;
    let result = topo.validate_with(&opts);

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

/// `--list-rules`: id + default severity, as a table or a JSON array.
fn print_rules(ctx: &Ctx) -> nlink_lab::Result<()> {
    let rules: Vec<serde_json::Value> = nlink_lab::rule_ids()
        .iter()
        .map(|id| {
            let sev = match nlink_lab::rule_severity(id) {
                Some(nlink_lab::Severity::Warning) => "warning",
                _ => "error",
            };
            serde_json::json!({ "rule": id, "severity": sev })
        })
        .collect();
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&rules)?);
        return Ok(());
    }
    println!("{:<36} SEVERITY", "RULE");
    for r in &rules {
        println!(
            "{:<36} {}",
            r["rule"].as_str().unwrap_or(""),
            r["severity"].as_str().unwrap_or("")
        );
    }
    println!(
        "\n--deny RULE promotes a warning to an error, --allow RULE silences it, --strict promotes all."
    );
    Ok(())
}
