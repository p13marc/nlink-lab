//! `nlink-lab ip`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// The node, or one of its interfaces: `server`, `server:eth0`.
    #[arg(value_name = "NODE[:IFACE]", add = crate::ctx::node_completer())]
    pub node: String,

    /// Only this interface (`NODE:IFACE` says the same).
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
    let lab = lab.resolve()?;
    let (node, iface) = match node.split_once(':') {
        Some((n, i)) if iface.is_none() => (n.to_string(), Some(i.to_string())),
        Some(_) => {
            return Err(nlink_lab::Error::invalid_argument(
                "give the interface once: NODE:IFACE or --iface",
            ));
        }
        None => (node, iface),
    };
    let running = nlink_lab::RunningLab::load(&lab)?;
    crate::cmd::common::require_node(&running, &node)?;
    let addrs = running.node_addresses(&node)?;

    if let Some(ref iface_name) = iface {
        let iface_addrs = addrs.get(iface_name).ok_or_else(|| {
            nlink_lab::Error::invalid_argument(format!(
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
