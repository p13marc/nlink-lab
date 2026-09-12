//! Mermaid rendering (`graph --mermaid`).

/// Mermaid `graph LR` rendering (renders inline in Forgejo/GitHub markdown).
pub fn topology_to_mermaid(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;
    fn id(s: &str) -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
    let mut out = String::from("graph LR\n");
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    for n in nodes {
        let kind = if topo.nodes[n].image.is_some() {
            "[["
        } else {
            "["
        };
        let close = if topo.nodes[n].image.is_some() {
            "]]"
        } else {
            "]"
        };
        out += &format!("  {}{kind}\"{n}\"{close}\n", id(n));
    }
    for link in &topo.links {
        let (Some(a), Some(b)) = (
            EndpointRef::parse(&link.endpoints[0]),
            EndpointRef::parse(&link.endpoints[1]),
        ) else {
            continue;
        };
        let mut label = format!("{} — {}", a.iface, b.iface);
        if let Some(addrs) = &link.addresses {
            label = format!("{label}<br/>{} / {}", addrs[0], addrs[1]);
        }
        if let Some(imp) = topo.impairments.get(&link.endpoints[0]) {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay {d}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss {l}"));
            }
            if !parts.is_empty() {
                label = format!("{label}<br/>{}", parts.join(", "));
            }
        }
        out += &format!("  {} ---|\"{label}\"| {}\n", id(&a.node), id(&b.node));
    }
    let mut net_names: Vec<&String> = topo.networks.keys().collect();
    net_names.sort();
    for name in net_names {
        let net = &topo.networks[name];
        let label = match &net.subnet {
            Some(sn) => format!("{name}<br/>{sn}"),
            None => name.clone(),
        };
        out += &format!("  net_{}((\"{label}\"))\n", id(name));
        for member in &net.members {
            if let Some(ep) = EndpointRef::parse(member) {
                out += &format!(
                    "  {} ---|\"{}\"| net_{}\n",
                    id(&ep.node),
                    ep.iface,
                    id(name)
                );
            }
        }
        for imp in &net.impairments {
            let mut parts = Vec::new();
            if let Some(d) = &imp.impairment.delay {
                parts.push(format!("delay {d}"));
            }
            if let Some(l) = &imp.impairment.loss {
                parts.push(format!("loss {l}"));
            }
            out += &format!(
                "  {} -.->|\"{}\"| {}\n",
                id(&imp.src),
                parts.join(", "),
                id(&imp.dst)
            );
        }
    }
    out
}
