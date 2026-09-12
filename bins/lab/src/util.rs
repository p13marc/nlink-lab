//! Small helpers shared by several subcommands: log tailing, flag
//! parsers, BPF filter glue, and the `impair --show` / `status --json`
//! report builders.

/// Follow `path` from `start_offset`, writing each new chunk to `out`
/// until `should_continue()` returns false or an I/O error occurs.
/// Handles file truncation/rotation by reopening from offset 0 when the
/// file shrinks below the last-read position.
///
/// Production callers use `|| true` for `should_continue` and exit on
/// SIGINT (Ctrl-C terminates the process as usual). Tests can pass a
/// closure that stops after a deterministic number of iterations.
fn tail_follow_to<W: std::io::Write>(
    path: &std::path::Path,
    start_offset: u64,
    out: &mut W,
    should_continue: impl Fn() -> bool,
) -> nlink_lab::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("failed to open log file: {e}")))?;
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("seek on log file: {e}")))?;
    let mut pos = start_offset;
    let mut buf = [0u8; 8192];
    while should_continue() {
        match file.read(&mut buf) {
            Ok(0) => {
                let meta = std::fs::metadata(path).ok();
                if let Some(m) = meta
                    && m.len() < pos
                {
                    file = std::fs::File::open(path).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("reopen log file: {e}"))
                    })?;
                    pos = 0;
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(n) => {
                out.write_all(&buf[..n]).ok();
                out.flush().ok();
                pos += n as u64;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "read from log file: {e}"
                )));
            }
        }
    }
    Ok(())
}

/// Last `n` lines of a file, read backwards in 64 KiB chunks so a
/// multi-gigabyte log is never loaded into memory (#46).
pub fn tail_lines(path: &std::path::Path, n: usize) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    if n == 0 {
        return Ok(String::new());
    }
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut end = len;
    let mut buf: Vec<u8> = Vec::new();
    const CHUNK: u64 = 64 * 1024;
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        end = start;
        // count newlines, ignoring a single trailing one
        let body = if buf.last() == Some(&b'\n') {
            &buf[..buf.len() - 1]
        } else {
            &buf[..]
        };
        if body.iter().filter(|&&b| b == b'\n').count() >= n {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].join("\n"))
}

/// Wrapper used by the CLI: runs forever (until Ctrl-C) and writes to
/// stdout.
pub fn tail_follow(path: &std::path::Path, start_offset: u64) -> nlink_lab::Result<()> {
    let mut stdout = std::io::stdout();
    tail_follow_to(path, start_offset, &mut stdout, || true)
}

/// Parse a CIDR string (`10.0.0.0/24`, `2001:db8::/32`) into the
/// netring `IpNet` type used by the typed BPF filter builder.
/// Surfaces the originating flag name in the error so the user can
/// tell which `--filter-*-net` was malformed.
pub fn parse_filter_cidr(flag: &str, s: &str) -> nlink_lab::Result<netring::IpNet> {
    s.parse::<netring::IpNet>()
        .map_err(|e| nlink_lab::Error::invalid_topology(format!("invalid {flag} value {s:?}: {e}")))
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
    Err(nlink_lab::Error::invalid_topology(
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
    Ok(n * mul)
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
                    nlink_lab::Error::invalid_topology(format!(
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

/// One row of `nlink-lab impair --show --json` output. `ImpairShow`'s
/// fields (qdisc, delay_ms, jitter_ms, loss_pct, rate_bps) flatten in
/// next to a `partition` flag pulled from `RunningLab::is_partitioned`.
#[derive(serde::Serialize)]
pub struct ImpairShowEntry {
    #[serde(flatten)]
    fields: nlink_lab::impair_parse::ImpairShow,
    /// `true` iff the endpoint is in `partition` state (i.e. its
    /// pre-partition impairment is in `saved_impairments`). A user who
    /// installed `--loss 100%` directly is *not* partitioned by this
    /// definition — the flag tracks the partition/heal lifecycle.
    partition: bool,
}

/// Walk every endpoint in the topology, exec `tc qdisc show dev <iface>`
/// inside the right namespace, and parse the result. Endpoints with no
/// impairment installed serialize as `null`. Used by `--show --json`.
pub fn collect_impair_show(
    running: &nlink_lab::RunningLab,
) -> nlink_lab::Result<std::collections::BTreeMap<String, Option<ImpairShowEntry>>> {
    use nlink_lab::EndpointRef;
    let mut out = std::collections::BTreeMap::new();
    for ep_str in nlink_lab::impair_parse::topology_endpoints(running.topology()) {
        let ep = EndpointRef::parse(&ep_str).ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!("malformed endpoint {ep_str:?} in topology"))
        })?;
        // Skip endpoints whose node isn't a known namespace (e.g.
        // container-only nodes — `tc qdisc show` via `running.exec`
        // would route through docker/podman and is meaningless).
        if running.namespace_for(&ep.node).is_err() {
            continue;
        }
        let tc = running.exec(&ep.node, "tc", &["qdisc", "show", "dev", &ep.iface])?;
        let parsed = nlink_lab::impair_parse::parse_tc_qdisc_show(&tc.stdout);
        let entry = parsed.map(|fields| ImpairShowEntry {
            fields,
            partition: running.is_partitioned(&ep_str),
        });
        out.insert(ep_str, entry);
    }
    Ok(out)
}

/// Build the argv to pass to `nsenter` for entering a lab node's network
/// namespace and exec'ing a shell.
///
/// Must emit `--net=<path>` as a single argument; splitting it into two
/// (`--net`, `<path>`) makes nsenter treat `--net` as "enter target's netns"
/// and then look for a target it never got, failing with
/// "neither filename nor target pid supplied for ns/net".
pub fn nsenter_shell_args(ns: &str, shell: &str) -> Vec<String> {
    vec![
        format!("--net=/var/run/netns/{ns}"),
        "--".into(),
        shell.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_reads_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let body: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, &body).unwrap();
        assert_eq!(
            tail_lines(&p, 3).unwrap(),
            "line 9997\nline 9998\nline 9999"
        );
        assert_eq!(tail_lines(&p, 0).unwrap(), "");
        assert_eq!(tail_lines(&p, 20_000).unwrap().lines().count(), 10_000);
        // file without trailing newline, spanning several 64 KiB chunks
        let big: String = (0..200_000).map(|i| format!("{i:08}\n")).collect();
        std::fs::write(&p, big.trim_end()).unwrap();
        assert_eq!(tail_lines(&p, 2).unwrap(), "00199998\n00199999");
    }

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

    #[test]
    fn nsenter_shell_args_uses_equals_form() {
        let args = nsenter_shell_args("mylab-router", "/bin/bash");
        assert_eq!(
            args,
            vec![
                "--net=/var/run/netns/mylab-router".to_string(),
                "--".to_string(),
                "/bin/bash".to_string(),
            ]
        );
        // Guard against the split `--net` regression: no argument may be
        // exactly `--net`, which nsenter interprets as the flag alone.
        assert!(
            !args.iter().any(|a| a == "--net"),
            "bare --net would be misparsed by nsenter"
        );
    }

    #[test]
    fn tail_follow_reads_appended_data() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"initial\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Append after a short delay from a background thread.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_w)
                .unwrap();
            f.write_all(b"appended\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            // Stop after roughly 1 second of polling (4×250ms sleeps).
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 6
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("appended"),
            "expected appended content, got: {captured:?}"
        );
        assert!(
            !captured.contains("initial"),
            "should not re-read data before start_offset: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_follow_handles_truncation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"old content that will be truncated\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Truncate then write fresh content.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::File::create(&path_w).unwrap(); // truncates
            f.write_all(b"fresh\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 8
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("fresh"),
            "expected post-truncation content, got: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
