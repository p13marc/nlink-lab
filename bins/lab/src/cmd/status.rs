//! `nlink-lab status`: one lab, all of it — nodes and their addresses,
//! containers, links, impairments as they are, partitions, processes,
//! named firewall counters. `ls` lists the labs.

use crate::ctx::{Ctx, LabArg, bold, caller_uid, green};
use crate::output::{Report, Table, emit, table::terminal_width};

#[derive(clap::Args)]
pub struct Args {
    /// The lab (default: as `-L` resolves it).
    #[arg(add = crate::ctx::lab_completer(), conflicts_with = "lab")]
    pub name: Option<String>,

    #[command(flatten)]
    pub lab: LabArg,
}

/// `status`'s result: the lab's [`LabStatus`](nlink_lab::ops::view::LabStatus),
/// and whether it is the caller's current lab.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct StatusReport {
    #[serde(flatten)]
    pub status: nlink_lab::ops::view::LabStatus,
    /// The caller's current lab (`nlink-lab use`).
    pub current: bool,
}

impl Report for StatusReport {
    const VERSION: u32 = 2;

    fn text(&self) -> String {
        let w = terminal_width();
        let s = &self.status;
        let mut out = format!(
            "{}{}\n",
            bold(&format!("Lab {}", s.lab)),
            if self.current {
                green(" (current)")
            } else {
                String::new()
            }
        );
        out.push_str(&format!("  created  {}\n", s.created_at));
        if let Some(path) = s.source.as_ref().and_then(|s| s.path.as_deref()) {
            out.push_str(&format!("  source   {path}\n"));
        }
        let mut t = Table::new(&["NODE", "KIND", "IMAGE", "ADDRESSES"]);
        for n in &s.nodes {
            let addrs: Vec<String> = n
                .addresses
                .iter()
                .flat_map(|(iface, a)| a.iter().map(move |a| format!("{iface} {a}")))
                .collect();
            t.row([
                n.node.clone(),
                n.kind.to_string(),
                n.image.clone().unwrap_or_else(|| "-".into()),
                addrs.join(", "),
            ]);
        }
        out.push('\n');
        out.push_str(&t.render(w));
        if !s.links.is_empty() {
            let mut t = Table::new(&["LINK", "ADDRESSES"]);
            for l in &s.links {
                t.row([
                    format!("{} -- {}", l.endpoints[0], l.endpoints[1]),
                    l.addresses
                        .as_ref()
                        .map(|a| format!("{} -- {}", a[0], a[1]))
                        .unwrap_or_else(|| "-".into()),
                ]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        if !s.networks.is_empty() {
            out.push_str(&format!(
                "\n{} {}\n",
                bold("NETWORKS"),
                s.networks.join(", ")
            ));
        }
        if !s.impairments.is_empty() {
            let mut t = Table::new(&["ENDPOINT", "IMPAIRMENT"]);
            for i in &s.impairments {
                let mut s = i.impairment.summary();
                if i.runtime {
                    s.push_str(" (runtime)");
                }
                t.row([i.endpoint.clone(), s]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        if !s.partitions.is_empty() {
            out.push_str(&format!(
                "\n{} {}\n",
                bold("PARTITIONED"),
                s.partitions.join(", ")
            ));
        }
        let alive: Vec<_> = s.processes.iter().filter(|p| p.alive).collect();
        if !alive.is_empty() {
            let mut t = Table::new(&["PROCESS", "NODE"]).right(0);
            for p in alive {
                t.row([p.pid.to_string(), p.node.clone()]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        if let Some(counters) = s.counters.as_ref().filter(|c| !c.is_empty()) {
            let mut t = Table::new(&["NODE", "COUNTER", "PACKETS", "BYTES"])
                .right(2)
                .right(3);
            for c in counters {
                t.row([
                    c.node.clone(),
                    c.name.clone(),
                    c.packets.to_string(),
                    c.bytes.to_string(),
                ]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        out
    }
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let lab = match args.name {
        Some(name) => {
            nlink_lab::state::validate_lab_name(&name)?;
            name
        }
        None => args.lab.resolve()?,
    };
    let running = nlink_lab::RunningLab::load(&lab)?;
    let (state, _) = nlink_lab::state::load(&lab)?;
    let status = nlink_lab::ops::view::lab_status(&running, &state, crate::ctx::has_privileges());
    let report = StatusReport {
        current: nlink_lab::state::context_get(caller_uid()).as_deref() == Some(lab.as_str()),
        status,
    };
    emit(ctx, &report)
}
