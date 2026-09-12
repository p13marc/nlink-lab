//! Plain-text rendering (`render --ascii`).

pub fn topology_to_ascii(topo: &nlink_lab::Topology) -> String {
    use std::collections::HashSet;

    let mut out = String::new();
    out.push_str(&format!("Lab: {}\n", topo.lab.name));
    if let Some(desc) = &topo.lab.description {
        out.push_str(&format!("  {desc}\n"));
    }
    out.push('\n');

    out.push_str("Nodes:\n");
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    for name in &nodes {
        let node = &topo.nodes[*name];
        let kind = if node.image.is_some() {
            " [container]"
        } else {
            ""
        };
        out.push_str(&format!("  {name}{kind}\n"));
    }

    out.push_str("\nLinks:\n");
    let mut shown: HashSet<String> = HashSet::new();
    for link in &topo.links {
        let key = format!("{} -- {}", link.endpoints[0], link.endpoints[1]);
        if shown.insert(key.clone()) {
            let mut parts = vec![format!("  {}", key)];
            if let Some(addrs) = &link.addresses {
                parts.push(format!("{} -- {}", addrs[0], addrs[1]));
            }
            if let Some(mtu) = link.mtu {
                parts.push(format!("mtu={mtu}"));
            }
            out.push_str(&format!("{}\n", parts.join("  ")));
        }
    }

    if !topo.networks.is_empty() {
        out.push_str("\nNetworks:\n");
        let mut names: Vec<&String> = topo.networks.keys().collect();
        names.sort();
        for name in names {
            let net = &topo.networks[name];
            let subnet = net
                .subnet
                .as_deref()
                .map(|s| format!("  subnet={s}"))
                .unwrap_or_default();
            out.push_str(&format!("  {name}{subnet}\n"));
            for member in &net.members {
                let addr = net
                    .ports
                    .get(member)
                    .and_then(|p| p.addresses.first())
                    .map(|a| format!("  {a}"))
                    .unwrap_or_default();
                out.push_str(&format!("    {member}{addr}\n"));
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
                    parts.push(format!("rate-cap={r}"));
                }
                out.push_str(&format!(
                    "    {} -> {}  {}\n",
                    imp.src,
                    imp.dst,
                    parts.join(" ")
                ));
            }
        }
    }

    if !topo.assertions.is_empty() {
        out.push_str("\nAssertions:\n");
        for a in &topo.assertions {
            match a {
                nlink_lab::types::Assertion::Reach { from, to } => {
                    out.push_str(&format!("  reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::NoReach { from, to } => {
                    out.push_str(&format!("  no-reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::TcpConnect {
                    from,
                    to,
                    port,
                    timeout,
                    retries,
                    interval,
                } => {
                    let t = timeout
                        .as_deref()
                        .map(|t| format!(" timeout {t}"))
                        .unwrap_or_default();
                    let r = retries.map(|r| format!(" retries {r}")).unwrap_or_default();
                    let i = interval
                        .as_deref()
                        .map(|i| format!(" interval {i}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  tcp-connect {from} -> {to}:{port}{t}{r}{i}\n"));
                }
                nlink_lab::types::Assertion::LatencyUnder {
                    from,
                    to,
                    max,
                    samples,
                } => {
                    let s = samples.map(|s| format!(" samples {s}")).unwrap_or_default();
                    out.push_str(&format!("  latency-under {from} -> {to} < {max}{s}\n"));
                }
                nlink_lab::types::Assertion::RouteHas {
                    node,
                    destination,
                    via,
                    dev,
                } => {
                    let v = via
                        .as_deref()
                        .map(|v| format!(" via {v}"))
                        .unwrap_or_default();
                    let d = dev
                        .as_deref()
                        .map(|d| format!(" dev {d}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  route-has {node} {destination}{v}{d}\n"));
                }
                nlink_lab::types::Assertion::DnsResolves {
                    from,
                    name,
                    expected_ip,
                } => {
                    out.push_str(&format!("  dns-resolves {from} {name} -> {expected_ip}\n"));
                }
            }
        }
    }

    out
}
