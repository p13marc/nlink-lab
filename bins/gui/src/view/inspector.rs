//! The inspector: what is selected, from the topology's declaration; the
//! lab and its params when nothing is.

use iced::widget::{Column, button, column, row, rule, scrollable, text, text_input};
use iced::{Alignment, Element, Length};
use nlink_lab::Topology;
use nlink_lab_view::graph::EdgeKind;
use nlink_lab_view::{Hit, NodeKind};

use nlink_lab_shared::metrics::format_rate;

use super::controls;
use super::{field, heading, muted};
use crate::message::{CanvasMsg, Message};
use crate::model::{App, Tab};

pub fn view<'a>(app: &'a App, tab: &'a Tab) -> Element<'a, Message> {
    let Some(topo) = &tab.topology else {
        return column![muted("Nothing to inspect yet.")].padding(10).into();
    };
    let op = app.operator();
    let body = match &tab.view.selection {
        None => lab(tab, topo, op),
        Some(Hit::Region(site)) => region(tab, site),
        Some(Hit::Node(id)) => node(tab, topo, id, op),
        Some(Hit::Edge(id)) => edge(tab, topo, id),
        Some(Hit::Endpoint(ep)) => endpoint(tab, topo, ep, op),
    };
    scrollable(body.spacing(6).padding(10).width(Length::Fill)).into()
}

/// A button that selects `hit`.
fn link_to<'a>(label: String, hit: Hit) -> Element<'a, Message> {
    button(text(label).size(13))
        .style(button::text)
        .padding([1, 4])
        .on_press(Message::Canvas(CanvasMsg::Select(Some(hit))))
        .into()
}

fn kind_name(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Host => "host",
        NodeKind::Router => "router (forwards)",
        NodeKind::Container => "container",
        NodeKind::Bridge => "network (bridge)",
        NodeKind::External => "host interface",
    }
}

fn lab<'a>(tab: &'a Tab, topo: &'a Topology, op: bool) -> Column<'a, Message> {
    let mut c = column![heading(if topo.lab.name.is_empty() {
        "(unnamed lab)"
    } else {
        topo.lab.name.as_str()
    })];
    if let Some(d) = &topo.lab.description {
        c = c.push(text(d).size(13));
    }
    if let Some(p) = tab.design().and_then(|d| d.source.path()) {
        c = c.push(field("file", p.display().to_string()));
    }
    if let Some(l) = tab.live() {
        if let Some(s) = &l.summary {
            c = c.push(field("health", s.health.clone()));
            c = c.push(field("deployed", s.created_at.replace('T', " ")));
            c = c.push(field("processes", s.processes.to_string()));
            if s.impaired + s.partitioned > 0 {
                c = c.push(field(
                    "impaired",
                    format!("{} live, {} partitioned", s.impaired, s.partitioned),
                ));
            }
        }
        if !l.revision.is_empty() {
            c = c.push(field(
                "revision",
                l.revision.chars().take(12).collect::<String>(),
            ));
        }
    }
    let count = |k: NodeKind| tab.view.graph.nodes.iter().filter(|n| n.kind == k).count();
    let nodes = [NodeKind::Router, NodeKind::Host, NodeKind::Container]
        .iter()
        .filter_map(|k| {
            let n = count(*k);
            (n > 0).then(|| format!("{n} {}", kind_name(*k).split(' ').next().unwrap_or("")))
        })
        .collect::<Vec<_>>()
        .join(", ");
    c = c.push(field(
        "nodes",
        if nodes.is_empty() {
            "none".into()
        } else {
            nodes
        },
    ));
    c = c.push(field("links", topo.links.len().to_string()));
    if !topo.networks.is_empty() {
        c = c.push(field("networks", topo.networks.len().to_string()));
    }
    if !topo.sites.is_empty() {
        c = c.push(field(
            "sites",
            topo.sites.keys().cloned().collect::<Vec<_>>().join(", "),
        ));
    }
    if let Some(m) = &topo.lab.mgmt_subnet {
        c = c.push(field("management", m.as_str()));
    }
    if !topo.assertions.is_empty() {
        c = c.push(field("assertions", topo.assertions.len().to_string()));
    }
    if let Some(d) = tab.design().filter(|d| !d.declared.is_empty()) {
        c = c.push(rule::horizontal(1));
        c = c.push(heading("Params"));
        for (name, default) in &d.declared {
            let value = d.param_edits.get(name).map(String::as_str).unwrap_or("");
            let name_owned = name.clone();
            let overridden = d.params.contains_key(name);
            c = c.push(
                row![
                    text(if overridden {
                        format!("{name} *")
                    } else {
                        name.clone()
                    })
                    .size(13)
                    .width(Length::Fixed(96.0)),
                    text_input(default.as_deref().unwrap_or("(required)"), value)
                        .size(13)
                        .on_input(move |v| Message::ParamEdited {
                            name: name_owned.clone(),
                            value: v,
                        })
                        .on_submit(Message::ApplyParams),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        c = c.push(
            row![
                button(text("Apply").size(13))
                    .style(button::primary)
                    .on_press(Message::ApplyParams),
                button(text("Defaults").size(13))
                    .style(button::secondary)
                    .on_press_maybe((!d.params.is_empty()).then_some(Message::ResetParams)),
            ]
            .spacing(8),
        );
        c = c.push(muted(
            "Enter applies; * marks a value set over its default.",
        ));
    }
    c = c.push(rule::horizontal(1));
    let pinned = tab.view.pinned.len();
    c = c.push(field(
        "placed",
        if pinned == 0 {
            "no node placed by hand".to_string()
        } else {
            format!("{pinned} node(s) placed by hand")
        },
    ));
    if pinned > 0 {
        c = c.push(
            button(text("Forget placements").size(13))
                .style(button::secondary)
                .on_press(Message::Unpin),
        );
    }
    c = c.push(muted("Click a node, a link or an interface to inspect it."));
    if let Some(l) = tab.live() {
        c = c.push(controls::snapshots(l, op));
        c = c.push(controls::destroy(l, op));
    }
    c
}

fn region<'a>(tab: &'a Tab, site: &'a str) -> Column<'a, Message> {
    let mut c = column![heading(format!("Site {site}"))];
    if let Some(r) = tab.view.graph.regions.iter().find(|r| r.id == site) {
        for n in &r.nodes {
            c = c.push(link_to(n.clone(), Hit::Node(n.clone())));
        }
    }
    c
}

fn node<'a>(tab: &'a Tab, topo: &'a Topology, id: &'a str, op: bool) -> Column<'a, Message> {
    let g = &tab.view.graph;
    let Some(gn) = g.node(id) else {
        return column![muted("Not in the topology any more.")];
    };
    let mut c = column![heading(gn.label.clone()), field("kind", kind_name(gn.kind))];
    if let Some(site) = &gn.site {
        c = c.push(field("site", site.as_str()));
    }
    if let Some(net) = id.strip_prefix("net:")
        && let Some(n) = topo.networks.get(net)
    {
        if let Some(s) = &n.subnet {
            c = c.push(field("subnet", s.as_str()));
        }
        if let Some(m) = n.mtu {
            c = c.push(field("mtu", m.to_string()));
        }
        if !n.impairments.is_empty() {
            c = c.push(field("pair impairments", n.impairments.len().to_string()));
        }
        c = c.push(heading("Members"));
        for m in &n.members {
            c = c.push(link_to(m.clone(), Hit::Endpoint(m.clone())));
        }
        if let Some(l) = tab.live()
            && let Some(p) = l.pairs.as_ref().filter(|p| p.network == net)
        {
            c = c.push(controls::pairs(p, l, topo, op));
        }
        return c;
    }
    // the interfaces, from the edges at this node
    let mut ifaces: Vec<(String, String, Option<String>)> = Vec::new();
    for e in &g.edges {
        for (mine, other, i) in [(&e.a, &e.b, 0), (&e.b, &e.a, 1)] {
            if mine.node == id {
                let iface = mine.iface.clone().unwrap_or_else(|| "?".into());
                let peer = other.endpoint().unwrap_or_else(|| other.node.clone());
                let peer = match e.kind {
                    EdgeKind::Port => format!("network {}", other.node.trim_start_matches("net:")),
                    _ => peer,
                };
                let addr = e.addresses.as_ref().map(|a| a[i].clone());
                ifaces.push((iface, peer, addr));
            }
        }
    }
    let Some(n) = topo.nodes.get(id) else {
        // a host NIC: the nodes on it
        c = c.push(heading("Attached"));
        for (_, peer, _) in ifaces {
            c = c.push(text(peer).size(13));
        }
        return c;
    };
    if let Some(image) = &n.image {
        c = c.push(field("image", image.as_str()));
        if let Some(l) = tab.live().filter(|l| op && !l.gone) {
            c = c.push(
                button(text("Restart").size(13))
                    .style(button::secondary)
                    .on_press_maybe((l.pending == 0).then(|| {
                        Message::Control(crate::message::Control::Restart(id.to_string()))
                    })),
            );
        }
        if let Some(cpu) = &n.cpu {
            c = c.push(field("cpu", cpu.as_str()));
        }
        if let Some(mem) = &n.memory {
            c = c.push(field("memory", mem.as_str()));
        }
        if let Some(h) = &n.healthcheck {
            c = c.push(field("healthcheck", h.as_str()));
        }
    }
    if !n.profiles.is_empty() {
        c = c.push(field("profiles", n.profiles.join(", ")));
    }
    c = live_node(c, tab, id);
    if !ifaces.is_empty() || !n.interfaces.is_empty() {
        c = c.push(heading("Interfaces"));
        for (iface, peer, addr) in ifaces {
            let ep = format!("{id}:{iface}");
            c = c.push(
                column![
                    row![
                        link_to(iface.clone(), Hit::Endpoint(ep.clone())),
                        text(addr.unwrap_or_default()).size(13),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                    muted(format!("   → {peer}")),
                ]
                .spacing(0),
            );
            if let Some(imp) = tab.view.impairments.get(&ep) {
                c = c.push(muted(format!("   netem {}", imp.summary)));
            }
        }
        for (name, i) in &n.interfaces {
            let what = i
                .kind
                .as_ref()
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_else(|| "interface".into());
            let addrs = if i.addresses.is_empty() {
                String::new()
            } else {
                format!("  {}", i.addresses.join(", "))
            };
            c = c.push(muted(format!("{name}: {what}{addrs}")));
        }
    }
    if !n.routes.is_empty() {
        c = c.push(heading("Routes"));
        for (dest, r) in &n.routes {
            let mut s = dest.clone();
            if let Some(v) = &r.via {
                s += &format!(" via {v}");
            }
            if let Some(d) = &r.dev {
                s += &format!(" dev {d}");
            }
            if let Some(m) = r.metric {
                s += &format!(" metric {m}");
            }
            c = c.push(text(s).size(13).font(iced::Font::MONOSPACE));
        }
    }
    if let Some(fw) = &n.firewall {
        c = c.push(field(
            "firewall",
            format!(
                "{} input rule(s){}",
                fw.rules.len(),
                fw.policy
                    .as_ref()
                    .map(|p| format!(", policy {p}"))
                    .unwrap_or_default()
            ),
        ));
    }
    if let Some(nat) = &n.nat {
        c = c.push(field("nat", format!("{} rule(s)", nat.rules.len())));
    }
    if !n.wireguard.is_empty() {
        c = c.push(field(
            "wireguard",
            n.wireguard.keys().cloned().collect::<Vec<_>>().join(", "),
        ));
    }
    if !n.vrfs.is_empty() {
        c = c.push(field(
            "vrf",
            n.vrfs.keys().cloned().collect::<Vec<_>>().join(", "),
        ));
    }
    if !n.sysctls.is_empty() {
        c = c.push(heading("Sysctls"));
        for (k, v) in &n.sysctls {
            c = c.push(muted(format!("{k} = {v}")).font(iced::Font::MONOSPACE));
        }
    }
    if !n.exec.is_empty() {
        c = c.push(heading("Processes"));
        for e in &n.exec {
            c = c.push(
                text(format!(
                    "{}{}",
                    e.cmd.join(" "),
                    if e.background { "  (background)" } else { "" }
                ))
                .size(12)
                .font(iced::Font::MONOSPACE),
            );
        }
    }
    if !n.depends_on.is_empty() {
        c = c.push(field("depends on", n.depends_on.join(", ")));
    }
    c
}

fn edge<'a>(tab: &'a Tab, topo: &'a Topology, id: &'a str) -> Column<'a, Message> {
    let Some(e) = tab.view.graph.edge(id) else {
        return column![muted("Not in the topology any more.")];
    };
    let kind = match e.kind {
        EdgeKind::Link => "link (veth pair)",
        EdgeKind::Port => "port of a network",
        EdgeKind::Tunnel => "WireGuard tunnel",
        EdgeKind::Vxlan => "VXLAN tunnel",
        EdgeKind::HostLink => "on a host interface",
        EdgeKind::Mgmt => "management network",
    };
    let mut c = column![heading(kind)];
    for (i, end) in [&e.a, &e.b].into_iter().enumerate() {
        match end.endpoint() {
            Some(ep) => {
                let addr = e
                    .addresses
                    .as_ref()
                    .map(|a| format!("  {}", a[i]))
                    .unwrap_or_default();
                c = c.push(
                    row![link_to(ep.clone(), Hit::Endpoint(ep.clone())), muted(addr)]
                        .align_y(Alignment::Center),
                );
                if let Some(imp) = tab.view.impairments.get(&ep) {
                    c = c.push(muted(format!("   netem {}", imp.summary)));
                }
            }
            None => c = c.push(link_to(end.node.clone(), Hit::Node(end.node.clone()))),
        }
    }
    if let Some(i) = id
        .strip_prefix("link:")
        .and_then(|i| i.parse::<usize>().ok())
        && let Some(l) = topo.links.get(i)
        && let Some(mtu) = l.mtu
    {
        c = c.push(field("mtu", mtu.to_string()));
    }
    c
}

fn endpoint<'a>(tab: &'a Tab, topo: &'a Topology, ep: &'a str, op: bool) -> Column<'a, Message> {
    let (node, iface) = ep.split_once(':').unwrap_or((ep, ""));
    let mut c = column![
        heading(ep),
        row![
            muted("node").width(Length::Fixed(96.0)),
            link_to(node.to_string(), Hit::Node(node.to_string()))
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        field("interface", iface),
    ];
    if let Some(e) = tab.view.graph.edge_at(ep) {
        let (other, i) = if e.a.endpoint().as_deref() == Some(ep) {
            (&e.b, 0)
        } else {
            (&e.a, 1)
        };
        if let Some(a) = &e.addresses {
            c = c.push(field("address", a[i].clone()));
        }
        let peer = other.endpoint().unwrap_or_else(|| other.node.clone());
        c = c.push(
            row![
                muted("peer").width(Length::Fixed(96.0)),
                link_to(
                    peer.clone(),
                    match other.endpoint() {
                        Some(p) => Hit::Endpoint(p),
                        None => Hit::Node(other.node.clone()),
                    }
                )
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if let Some((s, (rx, tx))) = tab
        .view
        .metrics
        .as_ref()
        .filter(|m| m.has_data() && !tab.view.stale)
        .and_then(|m| Some((m.series(node, iface)?, m.rate(node, iface)?)))
    {
        c = c.push(field("state", s.state.clone()));
        c = c.push(field(
            "rates",
            format!("↓ {}  ↑ {}", format_rate(rx), format_rate(tx)),
        ));
        c = c.push(crate::spark::spark(&s.rx_bps, &s.tx_bps));
    }
    match tab.view.impairments.get(ep) {
        Some(imp) if tab.live().is_some() => {
            c = c.push(field("netem", format!("{} ({})", imp.summary, imp.origin)))
        }
        Some(imp) => c = c.push(field("netem", imp.summary.clone())),
        None => c = c.push(field("netem", "none")),
    }
    if let Some(r) = topo.rate_limits.get(ep) {
        let parts: Vec<String> = [
            ("egress", &r.egress),
            ("ingress", &r.ingress),
            ("burst", &r.burst),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}")))
        .collect();
        c = c.push(field("rate", parts.join(", ")));
    }
    if let Some(q) = topo.qdiscs.get(ep) {
        let kind = format!("{:?}", q.kind);
        let name = kind
            .split(['(', ' ', '{'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        c = c.push(field("qdisc", name));
    }
    if let Some(l) = tab.live()
        && let Some(e) = l.editor.as_ref().filter(|e| e.endpoint == ep)
        && !l.gone
    {
        c = c.push(controls::impair(e, l, op));
    }
    c
}

/// A running node's interfaces as they are (their rates, recent and now)
/// and its processes.
fn live_node<'a>(mut c: Column<'a, Message>, tab: &'a Tab, id: &'a str) -> Column<'a, Message> {
    let Some(l) = tab.live() else {
        return c;
    };
    if let Some(m) = tab
        .view
        .metrics
        .as_ref()
        .filter(|m| m.has_data() && !tab.view.stale)
    {
        let mut rows: Vec<_> = m.interfaces(id).collect();
        rows.sort_by(|a, b| nlink_lab_view::layout::natural(a.0, b.0));
        if !rows.is_empty() {
            c = c.push(heading("Now"));
            for (iface, s) in rows {
                let (rx, tx) = s.rate();
                let hot = s.heat > 0.0;
                c = c.push(
                    row![
                        link_to(iface.to_string(), Hit::Endpoint(format!("{id}:{iface}"))),
                        column![
                            text(format!("↓ {}  ↑ {}", format_rate(rx), format_rate(tx))).size(12),
                            muted(if hot {
                                format!("{} · errors growing", s.state)
                            } else {
                                s.state.clone()
                            }),
                        ]
                        .width(Length::Fill),
                        crate::spark::spark(&s.rx_bps, &s.tx_bps),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
        }
    }
    let procs: Vec<_> = l.processes.iter().filter(|p| p.node == id).collect();
    if !procs.is_empty() {
        c = c.push(heading("Tracked processes"));
        for p in procs {
            c = c.push(muted(format!(
                "pid {} — {}",
                p.pid,
                if p.alive { "running" } else { "exited" }
            )));
        }
    }
    c
}
