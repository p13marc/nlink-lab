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

/// A node of the lab.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct NodeRow {
    pub node: String,
    /// `namespace` or `container`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The container's id and init pid, for a container node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerRow>,
    /// Interface → addresses, `mgmt0` included.
    pub addresses: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ContainerRow {
    pub id: String,
    pub pid: u32,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct LinkRow {
    pub endpoints: [String; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addresses: Option<[String; 2]>,
}

/// An endpoint's impairment as it is: a runtime one (`impair`) replaces
/// the declared one.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ImpairmentRow {
    pub endpoint: String,
    pub impairment: nlink_lab::Impairment,
    /// Set at runtime, not declared.
    pub runtime: bool,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct CounterRow {
    pub node: String,
    pub name: String,
    pub packets: u64,
    pub bytes: u64,
}

/// `status`'s result.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct StatusReport {
    pub lab: String,
    /// RFC 3339.
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<nlink_lab::state::LabSource>,
    /// The caller's current lab (`nlink-lab use`).
    pub current: bool,
    pub nodes: Vec<NodeRow>,
    pub links: Vec<LinkRow>,
    pub networks: Vec<String>,
    pub impairments: Vec<ImpairmentRow>,
    pub partitions: Vec<String>,
    pub processes: Vec<nlink_lab::ProcessInfo>,
    /// Named firewall counters, read live (root only: absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters: Option<Vec<CounterRow>>,
    /// The mgmt bridge's name and every declared subnet, to spot
    /// collisions between labs.
    pub host_resources: serde_json::Value,
}

impl Report for StatusReport {
    const VERSION: u32 = 2;

    fn text(&self) -> String {
        let w = terminal_width();
        let mut out = format!(
            "{}{}\n",
            bold(&format!("Lab {}", self.lab)),
            if self.current {
                green(" (current)")
            } else {
                String::new()
            }
        );
        out.push_str(&format!("  created  {}\n", self.created_at));
        if let Some(path) = self.source.as_ref().and_then(|s| s.path.as_deref()) {
            out.push_str(&format!("  source   {path}\n"));
        }
        let mut t = Table::new(&["NODE", "KIND", "IMAGE", "ADDRESSES"]);
        for n in &self.nodes {
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
        if !self.links.is_empty() {
            let mut t = Table::new(&["LINK", "ADDRESSES"]);
            for l in &self.links {
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
        if !self.networks.is_empty() {
            out.push_str(&format!(
                "\n{} {}\n",
                bold("NETWORKS"),
                self.networks.join(", ")
            ));
        }
        if !self.impairments.is_empty() {
            let mut t = Table::new(&["ENDPOINT", "IMPAIRMENT"]);
            for i in &self.impairments {
                let mut s = i.impairment.summary();
                if i.runtime {
                    s.push_str(" (runtime)");
                }
                t.row([i.endpoint.clone(), s]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        if !self.partitions.is_empty() {
            out.push_str(&format!(
                "\n{} {}\n",
                bold("PARTITIONED"),
                self.partitions.join(", ")
            ));
        }
        let alive: Vec<_> = self.processes.iter().filter(|p| p.alive).collect();
        if !alive.is_empty() {
            let mut t = Table::new(&["PROCESS", "NODE"]).right(0);
            for p in alive {
                t.row([p.pid.to_string(), p.node.clone()]);
            }
            out.push('\n');
            out.push_str(&t.render(w));
        }
        if let Some(counters) = self.counters.as_ref().filter(|c| !c.is_empty()) {
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
    let topo = running.topology();
    let containers = running.containers();
    let nodes = topo
        .nodes
        .iter()
        .map(|(name, node)| NodeRow {
            node: name.clone(),
            kind: if node.image.is_some() {
                "container"
            } else {
                "namespace"
            },
            image: node.image.clone(),
            container: containers.get(name).map(|c| ContainerRow {
                id: c.id.clone(),
                pid: c.pid,
            }),
            addresses: running.node_addresses(name).unwrap_or_default(),
        })
        .collect();
    let links = topo
        .links
        .iter()
        .map(|l| LinkRow {
            endpoints: l.endpoints.clone(),
            addresses: l.addresses.clone(),
        })
        .collect();
    // Impairments as they are: what `impair` set at runtime replaces the
    // declaration, and a cleared one is gone (#223).
    let live = running.live_impairments();
    let mut impairments: Vec<ImpairmentRow> = topo
        .impairments
        .iter()
        .filter(|(ep, _)| !live.contains_key(*ep))
        .map(|(ep, imp)| ImpairmentRow {
            endpoint: ep.clone(),
            impairment: imp.clone(),
            runtime: false,
        })
        .chain(
            live.iter()
                .filter(|(_, imp)| **imp != nlink_lab::Impairment::default())
                .map(|(ep, imp)| ImpairmentRow {
                    endpoint: ep.clone(),
                    impairment: imp.clone(),
                    runtime: true,
                }),
        )
        .collect();
    impairments.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    // Named firewall counters (`counter name …`, #166), read live: root.
    let counters = crate::ctx::has_privileges().then(|| {
        let mut rows = Vec::new();
        for (name, node) in &topo.nodes {
            let counts = topo
                .effective_firewall(node)
                .is_some_and(|fw| fw.all_rules().any(|r| r.counter_name.is_some()));
            if !counts {
                continue;
            }
            match running.firewall_counters_blocking(name) {
                Ok(counters) => rows.extend(counters.into_iter().map(|c| CounterRow {
                    node: name.clone(),
                    name: c.name,
                    packets: c.packets,
                    bytes: c.bytes,
                })),
                Err(e) => tracing::warn!("firewall counters of '{name}': {e}"),
            }
        }
        rows
    });
    let report = StatusReport {
        current: nlink_lab::state::context_get(caller_uid()).as_deref() == Some(lab.as_str()),
        created_at: state.created_at.clone(),
        source: state.source.clone(),
        nodes,
        links,
        networks: topo.networks.keys().cloned().collect(),
        impairments,
        partitions: running.partitions().keys().cloned().collect(),
        processes: running.process_status(),
        counters,
        host_resources: crate::util::host_resources_json(&running),
        lab,
    };
    emit(ctx, &report)
}
