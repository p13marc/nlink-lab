//! `nlink-lab conntrack` (#303).

use std::collections::BTreeMap;

use nlink_lab::api::{ConntrackFlow, ConntrackQuery, FlowTuple};

use crate::ctx::{Ctx, require_root};
use crate::output::{Report, Table, emit};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Only this node's flows. Default: every node.
    #[arg(add = crate::ctx::node_completer())]
    pub node: Option<String>,

    /// Only this protocol: tcp, udp, icmp, icmpv6, sctp, … or a number.
    #[arg(long, value_name = "PROTO")]
    pub proto: Option<String>,

    /// Only flows from this source (an address or a CIDR), as they began.
    #[arg(long, value_name = "ADDR")]
    pub src: Option<String>,

    /// Only flows to this destination (an address or a CIDR), as they
    /// began: before a DNAT.
    #[arg(long, value_name = "ADDR")]
    pub dst: Option<String>,

    /// Only this source port (with --proto tcp or udp).
    #[arg(long, value_name = "PORT")]
    pub sport: Option<u16>,

    /// Only this destination port (with --proto tcp or udp).
    #[arg(long, value_name = "PORT")]
    pub dport: Option<u16>,

    /// Only flows with this connection mark.
    #[arg(long, value_parser = crate::args::u32_dec_or_hex, value_name = "MARK")]
    pub mark: Option<u32>,

    /// Only flows in this conntrack zone.
    #[arg(long, value_name = "ZONE")]
    pub zone: Option<u16>,

    /// Only flows with these status bits (comma-separated): snat, dnat,
    /// assured, seen-reply, confirmed, expected, dying.
    #[arg(long, value_delimiter = ',', value_name = "STATUS")]
    pub status: Vec<String>,

    /// Delete the flows selected (every flow, without a filter) instead of
    /// listing them. Their next packet starts a new flow, through the
    /// rules as they are now.
    #[arg(long)]
    pub flush: bool,
}

/// `conntrack`'s result: the flows selected, or how many went per node.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ConntrackReport {
    pub lab: String,
    /// The flows selected (a listing; absent after `--flush`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<Vec<ConntrackFlow>>,
    /// Flows deleted per node (with `--flush`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<BTreeMap<String, usize>>,
}

/// `ADDR:PORT`, or `ADDR` without a port.
fn end(addr: &Option<String>, port: Option<u16>) -> String {
    let addr = addr.as_deref().unwrap_or("?");
    match port {
        Some(p) if addr.contains(':') => format!("[{addr}]:{p}"),
        Some(p) => format!("{addr}:{p}"),
        None => addr.to_string(),
    }
}

/// The translation a flow's reply direction shows, if it differs from
/// its original one swapped.
fn translated(orig: &FlowTuple, reply: &FlowTuple) -> Option<String> {
    let swapped = reply.src == orig.dst
        && reply.dst == orig.src
        && reply.sport == orig.dport
        && reply.dport == orig.sport;
    (!swapped).then(|| {
        format!(
            "{} <- {}",
            end(&reply.dst, reply.dport),
            end(&reply.src, reply.sport)
        )
    })
}

impl Report for ConntrackReport {
    const VERSION: u32 = 1;

    fn text(&self) -> String {
        if let Some(deleted) = &self.deleted {
            let total: usize = deleted.values().sum();
            let per: Vec<String> = deleted
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(node, n)| format!("{node} {n}"))
                .collect();
            return if per.is_empty() {
                "No flow matched: nothing deleted.\n".into()
            } else {
                format!("Deleted {total} flow(s): {}.\n", per.join(", "))
            };
        }
        let flows = self.flows.as_deref().unwrap_or_default();
        if flows.is_empty() {
            return "No tracked flows.\n".into();
        }
        let mut t =
            Table::new(&["NODE", "PROTO", "FLOW", "NAT REPLY", "STATE", "TIMEOUT"]).right(5);
        for f in flows {
            t.row([
                f.node.clone(),
                f.proto.clone(),
                format!(
                    "{} -> {}",
                    end(&f.orig.src, f.orig.sport),
                    end(&f.orig.dst, f.orig.dport)
                ),
                translated(&f.orig, &f.reply).unwrap_or_default(),
                f.state
                    .clone()
                    .into_iter()
                    .chain(f.status.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(","),
                f.timeout.map(|s| format!("{s}s")).unwrap_or_default(),
            ]);
        }
        t.render(crate::output::table::terminal_width())
    }

    fn quiet(&self) -> Option<String> {
        Some(match &self.deleted {
            Some(deleted) => format!("{}\n", deleted.values().sum::<usize>()),
            None => format!("{}\n", self.flows.as_ref().map_or(0, Vec::len)),
        })
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        proto,
        src,
        dst,
        sport,
        dport,
        mark,
        zone,
        status,
        flush,
    } = args;
    let lab = lab.resolve()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    if let Some(node) = &node {
        crate::cmd::common::require_node(&running, node)?;
    }
    require_root()?;
    let query = ConntrackQuery {
        proto,
        src,
        dst,
        sport,
        dport,
        mark,
        zone,
        status,
    };
    let report = if flush {
        ConntrackReport {
            deleted: Some(running.flush_conntrack_of(node.as_deref(), &query).await?),
            flows: None,
            lab,
        }
    } else {
        ConntrackReport {
            flows: Some(running.conntrack_of(node.as_deref(), &query).await?),
            deleted: None,
            lab,
        }
    };
    emit(ctx, &report)
}
