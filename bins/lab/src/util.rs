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

/// Legacy `--filter "<tcpdump expr>"` path. Default builds reject
/// at parse time with a migration suggestion; opting into the
/// `legacy-tcpdump-filter` feature reinstates the `tcpdump -dd`
/// shell-out.
#[cfg(feature = "legacy-tcpdump-filter")]
pub fn compile_legacy_bpf_filter(expr: &str) -> nlink_lab::Result<netring::BpfFilter> {
    nlink_lab::capture::compile_bpf_filter(expr)
}

#[cfg(not(feature = "legacy-tcpdump-filter"))]
pub fn compile_legacy_bpf_filter(_expr: &str) -> nlink_lab::Result<netring::BpfFilter> {
    Err(nlink_lab::Error::invalid_argument(
        "--filter \"<tcpdump expr>\" requires nlink-lab built with the \
         `legacy-tcpdump-filter` feature (which shells out to `tcpdump -dd`). \
         Default builds use the typed --filter-tcp / --filter-dst-port / \
         --filter-host / --filter-src-net flags instead.",
    ))
}

/// Parse a byte-size CLI argument with optional K/M/G suffix.
/// Decimal-SI units (1K = 1000, 1M = 1_000_000) — same convention as
/// `tcpdump -C`. Round-5 §2.3.
pub fn parse_byte_size(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    let (num_str, mul) = if let Some(rest) = s.strip_suffix(['G', 'g']) {
        (rest, 1_000_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['M', 'm']) {
        (rest, 1_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['K', 'k']) {
        (rest, 1_000u64)
    } else {
        (s, 1u64)
    };
    let n: u64 = num_str
        .parse()
        .map_err(|e| format!("invalid byte size {s:?}: {e}"))?;
    // overflowed (a panic in a debug build, a wrapped size otherwise)
    n.checked_mul(mul)
        .ok_or_else(|| format!("byte size {s:?} is too large"))
}

/// Parse a `SECS` CLI argument: a finite, non-negative number of seconds
/// that fits a `Duration`. `Duration::from_secs_f64` panics on anything
/// else (`--duration -1`, `nan`, `1e30`, #209).
pub fn parse_secs(s: &str) -> std::result::Result<f64, String> {
    let secs: f64 = s
        .trim()
        .parse()
        .map_err(|e| format!("invalid number of seconds {s:?}: {e}"))?;
    std::time::Duration::try_from_secs_f64(secs)
        .map(|_| secs)
        .map_err(|_| format!("{s:?} is not a duration in seconds (finite, not negative)"))
}

/// Parse repeated `--env KEY=VALUE` strings into `(key, value)` pairs.
///
/// Returns an error on entries missing `=`. The parsed pairs are
/// applied to the child process via `Command::env(k, v)` (additive on
/// top of the inherited environment) — *not* by wrapping the command
/// in `/usr/bin/env`. The wrapper approach was the previous behaviour
/// and silently broke the per-process logfile naming convention,
/// because `argv[0]` ended up being `env` rather than the user's
/// binary. See round-3 feedback §3.1.
pub fn parse_env_pairs(env_vars: &[String]) -> nlink_lab::Result<Vec<(String, String)>> {
    env_vars
        .iter()
        .map(|s| {
            s.split_once('=')
                .ok_or_else(|| {
                    nlink_lab::Error::invalid_argument(format!(
                        "invalid --env {s:?} (expected KEY=VALUE)"
                    ))
                })
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Build the `host_resources` block for `status --json <lab>`.
///
/// Reports the host-side artefacts a lab installs, so consumers can
/// detect cross-lab collisions client-side without netlink:
///
/// - `mgmt_bridge`: the `nl{hash8}` bridge in the host network ns
///   (always present for namespace-only labs).
/// - `subnets`: every declared subnet in the topology (links +
///   networks). Useful to verify two labs don't claim the same
///   private range.
///
/// Round-5 §1.2 bonus.
pub fn host_resources_json(lab: &nlink_lab::RunningLab) -> serde_json::Value {
    let topo = lab.topology();
    let mgmt_bridge = nlink_lab::mgmt_bridge_name_for(lab.name());

    let mut subnets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for link in &topo.links {
        if let Some(addrs) = &link.addresses {
            for a in addrs {
                if let Some((_, prefix)) = a.split_once('/')
                    && let Some(network) = subnet_of(a)
                {
                    subnets.insert(format!("{network}/{prefix}"));
                }
            }
        }
    }
    for network in topo.networks.values() {
        if let Some(s) = &network.subnet {
            subnets.insert(s.clone());
        }
    }

    serde_json::json!({
        "mgmt_bridge": mgmt_bridge,
        "subnets": subnets.into_iter().collect::<Vec<_>>(),
    })
}

/// Compute the network-address portion of a CIDR (e.g.
/// `"10.0.0.5/24"` → `"10.0.0.0"`). Returns None on malformed input.
fn subnet_of(cidr: &str) -> Option<String> {
    let (ip_str, prefix_str) = cidr.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip_str.parse().ok()?;
    let prefix: u32 = prefix_str.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 {
        0u32
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(ip) & mask;
    Some(std::net::Ipv4Addr::from(network).to_string())
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
    use super::*;

    #[test]
    fn parse_env_pairs_accepts_valid() {
        let inputs = vec![
            "FOO=bar".to_string(),
            "EMPTY=".to_string(),
            "X=y=z".to_string(),
        ];
        let pairs = parse_env_pairs(&inputs).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("EMPTY".to_string(), "".to_string()),
                // `split_once` splits on the *first* `=`, so the value gets
                // `y=z`. Keeps `--env DSN=postgres://u@h/db?x=y` working.
                ("X".to_string(), "y=z".to_string()),
            ]
        );
    }

    #[test]
    fn parse_env_pairs_rejects_missing_equals() {
        let inputs = vec!["NOEQ".to_string()];
        let err = parse_env_pairs(&inputs).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("NOEQ"), "error should name the bad entry: {s}");
    }

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
