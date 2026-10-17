//! Mermaid rendering (`graph --mermaid`, `render --mermaid`).

/// Mermaid `graph LR` rendering (renders inline in Forgejo/GitHub markdown).
pub fn topology_to_mermaid(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;
    // An id is the name with every other character an `_`, made unique
    // (`x-y` and `x_y` were both `x_y`) and kept off Mermaid's keywords (a
    // node named `end` broke the chart, #209). Networks are `net_<name>`.
    const KEYWORDS: &[&str] = &[
        "end",
        "graph",
        "subgraph",
        "flowchart",
        "direction",
        "click",
        "style",
        "class",
        "classDef",
        "linkStyle",
    ];
    let sanitise = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    let mut net_names: Vec<&String> = topo.networks.keys().collect();
    net_names.sort();
    let mut taken = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeMap::new();
    let wanted = nodes
        .iter()
        .map(|n| ((false, n.as_str()), sanitise(n)))
        .chain(
            net_names
                .iter()
                .map(|n| ((true, n.as_str()), format!("net_{}", sanitise(n)))),
        );
    for (key, base) in wanted {
        let mut id = base.clone();
        let mut n = 1;
        while taken.contains(&id) || KEYWORDS.contains(&id.as_str()) {
            n += 1;
            id = format!("{base}_{n}");
        }
        taken.insert(id.clone());
        ids.insert(key, id);
    }
    let id = |name: &str| {
        ids.get(&(false, name))
            .cloned()
            .unwrap_or_else(|| sanitise(name))
    };
    let net_id = |name: &str| ids[&(true, name)].clone();
    let mut out = String::from("graph LR\n");
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
    for name in net_names {
        let net = &topo.networks[name];
        let label = match &net.subnet {
            Some(sn) => format!("{name}<br/>{sn}"),
            None => name.clone(),
        };
        out += &format!("  {}((\"{label}\"))\n", net_id(name));
        for member in &net.members {
            if let Some(ep) = EndpointRef::parse(member) {
                out += &format!(
                    "  {} ---|\"{}\"| {}\n",
                    id(&ep.node),
                    ep.iface,
                    net_id(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_links_networks_and_pair_impairments() {
        let topo = nlink_lab::parser::parse(
            r#"lab "m"
node r { forward ipv4 }
node h
node c image "alpine"
link r:eth0 -- h:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms }
network lan { subnet 10.9.0.0/24  members [h:eth1, c:eth0]  impair h -- c { loss 1% } }
"#,
        )
        .unwrap();
        let out = topology_to_mermaid(&topo);
        assert!(out.starts_with("graph LR\n"), "{out}");
        assert!(out.contains("c[[\"c\"]]"), "containers use [[ ]]: {out}");
        assert!(
            out.contains("r ---|\"eth0 — eth0<br/>10.0.0.1/24 / 10.0.0.2/24<br/>delay 5ms\"| h"),
            "{out}"
        );
        assert!(out.contains("net_lan((\"lan<br/>10.9.0.0/24\"))"), "{out}");
        assert!(out.contains("h ---|\"eth1\"| net_lan"), "{out}");
        assert!(out.contains("h -.->|\"loss 1%\"| c"), "{out}");
        // deterministic: identical on a second render
        assert_eq!(out, topology_to_mermaid(&topo));
    }

    #[test]
    fn identifiers_are_sanitised() {
        let topo = nlink_lab::parser::parse("lab \"m\"\nnode dc1-fw\n").unwrap();
        let out = topology_to_mermaid(&topo);
        assert!(out.contains("dc1_fw[\"dc1-fw\"]"), "{out}");
    }

    /// `x-y` and `x_y` were one id, and a node named `end` ended the chart
    /// (#209).
    #[test]
    fn identifiers_are_unique_and_not_keywords() {
        let topo = nlink_lab::parser::parse(
            "lab \"m\"\nnode x-y\nnode x_y\nnode end\nlink x-y:eth0 -- end:eth0\n",
        )
        .unwrap();
        let out = topology_to_mermaid(&topo);
        assert!(out.contains("end_2[\"end\"]"), "{out}");
        assert!(
            out.contains("x_y[\"x-y\"]") && out.contains("x_y_2[\"x_y\"]"),
            "{out}"
        );
        assert!(out.contains("x_y ---|\"eth0 — eth0\"| end_2"), "{out}");
    }
}
