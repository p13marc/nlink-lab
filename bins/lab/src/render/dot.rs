//! Graphviz DOT rendering (`graph`, `render --dot`).

pub fn topology_to_dot(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;

    let mut out = format!("graph {:?} {{\n", topo.lab.name);
    out += "  rankdir=LR;\n";
    out += "  node [shape=box];\n";

    for link in &topo.links {
        let (Some(a), Some(b)) = (
            EndpointRef::parse(&link.endpoints[0]),
            EndpointRef::parse(&link.endpoints[1]),
        ) else {
            continue;
        };

        let mut label_parts = Vec::new();
        if let Some(addrs) = &link.addresses {
            label_parts.push(format!("{} / {}", addrs[0], addrs[1]));
        }
        if let Some(mtu) = link.mtu {
            label_parts.push(format!("MTU {mtu}"));
        }
        // Check for impairment
        if let Some(imp) = topo.impairments.get(&link.endpoints[0]) {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss={l}"));
            }
            if !parts.is_empty() {
                label_parts.push(parts.join(" "));
            }
        }

        let label = label_parts.join("\\n");
        if label.is_empty() {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface
            );
        } else {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\", label=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface, label
            );
        }
    }

    // Bridge networks: one ellipse per network, an edge per member,
    // per-pair impairments as dashed labelled edges (#47).
    let mut net_names: Vec<&String> = topo.networks.keys().collect();
    net_names.sort();
    for name in net_names {
        let net = &topo.networks[name];
        let label = match &net.subnet {
            Some(sn) => format!("{name}\\n{sn}"),
            None => name.clone(),
        };
        out += &format!("  \"net:{name}\" [shape=ellipse, label=\"{label}\"];\n");
        for member in &net.members {
            if let Some(ep) = EndpointRef::parse(member) {
                let addr = net
                    .ports
                    .get(member)
                    .and_then(|p| p.addresses.first())
                    .map(|a| format!(", label=\"{a}\""))
                    .unwrap_or_default();
                out += &format!(
                    "  \"{}\" -- \"net:{name}\" [taillabel=\"{}\"{addr}];\n",
                    ep.node, ep.iface
                );
            }
        }
        for imp in &net.impairments {
            let mut parts = Vec::new();
            if let Some(d) = &imp.impairment.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(l) = &imp.impairment.loss {
                parts.push(format!("loss={l}"));
            }
            if let Some(r) = &imp.rate_cap {
                parts.push(format!("cap={r}"));
            }
            out += &format!(
                "  \"{}\" -> \"{}\" [style=dashed, color=gray, label=\"{}\"];\n",
                imp.src,
                imp.dst,
                parts.join(" ")
            );
        }
    }

    out += "}\n";
    out
}
