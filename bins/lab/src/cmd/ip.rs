//! `nlink-lab ip`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Filter by interface name.
    #[arg(long)]
    pub iface: Option<String>,

    /// Show CIDR notation (include prefix length).
    #[arg(long)]
    pub cidr: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        iface,
        cidr,
    } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let addrs = running.node_addresses(&node)?;

    if let Some(ref iface_name) = iface {
        let iface_addrs = addrs.get(iface_name).ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!(
                "interface '{iface_name}' not found on node '{node}'"
            ))
        })?;

        if ctx.json {
            println!("{}", serde_json::to_string_pretty(&iface_addrs)?);
        } else if let Some(first) = iface_addrs.first() {
            if cidr {
                println!("{first}");
            } else {
                println!("{}", first.split('/').next().unwrap_or(first));
            }
        }
    } else if ctx.json {
        println!("{}", serde_json::to_string_pretty(&addrs)?);
    } else {
        for (iface_name, iface_addrs) in &addrs {
            for addr in iface_addrs {
                if cidr {
                    println!("{iface_name}: {addr}");
                } else {
                    println!("{iface_name}: {}", addr.split('/').next().unwrap_or(addr));
                }
            }
        }
    }
    Ok(())
}
