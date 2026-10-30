//! Small helpers shared by several subcommands: flag parsers, BPF filter glue, and the `impair --show` / `status --json`
//! report builders.

/// [`nlink_lab::logs::follow`] to stdout, until Ctrl-C.
pub fn tail_follow(path: &std::path::Path, start_offset: u64) -> nlink_lab::Result<()> {
    nlink_lab::logs::follow(path, start_offset, &mut std::io::stdout(), || true)
}

/// Parse a CIDR string (`10.0.0.0/24`, `2001:db8::/32`) into the
/// netring `IpNet` type used by the typed BPF filter builder.
/// Surfaces the originating flag name in the error so the user can
/// tell which `--filter-*-net` was malformed.
pub fn parse_filter_cidr(flag: &str, s: &str) -> nlink_lab::Result<netring::IpNet> {
    s.parse::<netring::IpNet>()
        .map_err(|e| nlink_lab::Error::invalid_argument(format!("invalid {flag} value {s:?}: {e}")))
}

/// `impair --show`'s human line for an endpoint: its root qdisc, netem's
/// values, and whether it is partitioned.
pub fn impair_show_line(entry: Option<&nlink_lab::impair_parse::ShownImpairment>) -> String {
    let Some(entry) = entry else {
        return "-".to_string();
    };
    let f = &entry.shown;
    let mut words = vec![f.qdisc.clone()];
    let ms = |v: f64| format!("{}ms", (v * 1000.0).round() / 1000.0);
    if let Some(d) = f.delay_ms {
        words.push(format!("delay {}", ms(d)));
    }
    if let Some(j) = f.jitter_ms {
        words.push(format!("jitter {}", ms(j)));
    }
    if let Some(l) = f.loss_pct {
        words.push(format!("loss {l}%"));
    }
    if let Some(m) = &f.loss_model {
        words.push(format!("loss {m}"));
    }
    if let Some(r) = f.rate_bps {
        words.push(format!("rate {}", rate_text(r)));
    }
    if entry.partition {
        words.push("(partitioned)".to_string());
    }
    words.join(" ")
}

/// A rate in bits per second as tc writes one (`10Mbit`, `1500Kbit`).
fn rate_text(bits: u64) -> String {
    match bits {
        b if b >= 1_000_000_000 && b % 1_000_000_000 == 0 => format!("{}Gbit", b / 1_000_000_000),
        b if b >= 1_000_000 && b % 1_000_000 == 0 => format!("{}Mbit", b / 1_000_000),
        b if b >= 1_000 && b % 1_000 == 0 => format!("{}Kbit", b / 1_000),
        b => format!("{b}bit"),
    }
}

#[cfg(test)]
mod tests {

    /// `topology_endpoints` must collect from every place a topology
    /// can declare an endpoint — `links`, `networks.members`, and
    /// declared impairments. Round-4 follow-up: the first `--show
    /// --json` impl only walked `links` and emitted `endpoints: {}`
    /// for any topology built around bridge networks.
    #[test]
    fn topology_endpoints_collects_links_networks_and_impairments() {
        let nll = r#"
lab "tep"

node a
node b
node c

# point-to-point link → endpoints visible in `links`
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

# bridge network → endpoints visible in `networks.members`
network lan {
  members [b:eth1, c:eth0]
  subnet 10.1.0.0/24
}

# top-level impair → endpoint visible in `impairments`
impair c:lo delay 5ms
"#;
        let topo = nlink_lab::parser::parse(nll).unwrap();
        let endpoints = nlink_lab::impair_parse::topology_endpoints(&topo);

        // BTreeSet ordering is alphabetical.
        assert_eq!(
            endpoints,
            vec![
                "a:eth0".to_string(),
                "b:eth0".to_string(),
                "b:eth1".to_string(),
                "c:eth0".to_string(),
                "c:lo".to_string(),
            ],
            "must collect link endpoints + network members + impairment keys"
        );
    }

    /// Network-only topology must produce a non-empty endpoint list.
    /// Specifically guards against the round-4 follow-up regression
    /// where `--show --json` returned `endpoints: {}` for the harness
    /// team's 3-machine topology (built around networks, no `link`
    /// declarations).
    #[test]
    fn topology_endpoints_handles_network_only_topology() {
        let nll = r#"
lab "net-only"

node router
node site_a
node site_b

network lan_a {
  members [router:eth0, site_a:eth0]
  subnet 10.0.0.0/24
}
network lan_b {
  members [router:eth1, site_b:eth0]
  subnet 10.1.0.0/24
}
"#;
        let topo = nlink_lab::parser::parse(nll).unwrap();
        let endpoints = nlink_lab::impair_parse::topology_endpoints(&topo);
        assert!(
            endpoints.contains(&"router:eth0".to_string()),
            "missing router:eth0: {endpoints:?}"
        );
        assert!(
            endpoints.contains(&"site_a:eth0".to_string()),
            "missing site_a:eth0: {endpoints:?}"
        );
        assert!(
            endpoints.contains(&"site_b:eth0".to_string()),
            "missing site_b:eth0 (was the regression trigger): {endpoints:?}"
        );
    }
}
