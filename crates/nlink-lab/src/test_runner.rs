//! Test runner for network topology testing.
//!
//! Provides deploy → validate → destroy test execution with structured output
//! in JUnit XML and TAP formats for CI/CD integration.

use std::path::Path;
use std::time::Instant;

use crate::error::Result;
use crate::types::Assertion;

/// Result of running all assertions in a single topology.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestResult {
    /// Topology file path.
    pub file: String,
    /// Individual assertion results.
    pub assertions: Vec<AssertionResult>,
    /// Time spent deploying (milliseconds).
    pub deploy_ms: u64,
    /// Total test time (milliseconds).
    pub total_ms: u64,
    /// Whether all assertions passed.
    pub passed: bool,
    /// Why the test could not run to the end: the file did not parse,
    /// validate or deploy, or the lab did not destroy. A test with an
    /// error has not passed; JUnit and TAP report it as an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The lab left deployed for inspection (`keep_failed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<String>,
    /// The seed the lab ran with (#337): `test --seed N` replays it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

/// What a test runs besides deploy, `validate` and destroy.
#[derive(Debug, Clone, Copy, Default)]
pub struct TestOptions {
    /// Every `scenario` block, in order: each action is a result.
    pub scenarios: bool,
    /// Every `benchmark` block: each test is a result.
    pub benchmarks: bool,
    /// Leave a failed lab deployed, for inspection, instead of destroying
    /// it.
    pub keep_failed: bool,
    /// The lab's seed, in place of the topology's own (#337).
    pub seed: Option<u64>,
}

impl TestResult {
    /// A test that failed before its assertions ran.
    pub fn errored(file: impl Into<String>, error: impl ToString, total_ms: u64) -> Self {
        Self {
            file: file.into(),
            assertions: Vec::new(),
            deploy_ms: 0,
            total_ms,
            passed: false,
            error: Some(error.to_string()),
            kept: None,
            seed: None,
        }
    }
}

/// Result of a single assertion.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct AssertionResult {
    /// Human-readable assertion description.
    pub description: String,
    /// Whether the assertion passed.
    pub passed: bool,
    /// Detail message (e.g., actual latency, route output).
    pub detail: Option<String>,
    /// Time to evaluate this assertion (milliseconds).
    pub duration_ms: u64,
}

/// Run a topology test: parse → deploy → validate → destroy.
pub async fn run_test(path: &Path) -> Result<TestResult> {
    run_test_with_params(path, &[]).await
}

/// [`run_test`] with `param` values (`--set key=value`) applied to the file.
pub async fn run_test_with_params(path: &Path, params: &[(String, String)]) -> Result<TestResult> {
    run_test_with(path, params, &crate::ops::OpCtx::default()).await
}

/// [`run_test_with_params`], reporting the deploy (stages, waits,
/// assertions) and the destroy to `ctx` (#236). A cancel during the
/// deploy rolls it back; the test then fails with `Error::Cancelled`.
pub async fn run_test_with(
    path: &Path,
    params: &[(String, String)],
    ctx: &crate::ops::OpCtx,
) -> Result<TestResult> {
    run_test_opts(path, params, TestOptions::default(), ctx).await
}

/// [`run_test_with`], also running the topology's scenarios and
/// benchmarks as `opts` says: each scenario action and benchmark test is
/// one more result, in JUnit and TAP too.
pub async fn run_test_opts(
    path: &Path,
    params: &[(String, String)],
    opts: TestOptions,
    ctx: &crate::ops::OpCtx,
) -> Result<TestResult> {
    ctx.started();
    let result = test_ctx(path, params, opts, ctx).await;
    ctx.finished(&result, false);
    result
}

async fn test_ctx(
    path: &Path,
    params: &[(String, String)],
    opts: TestOptions,
    ctx: &crate::ops::OpCtx,
) -> Result<TestResult> {
    let file = path.display().to_string();
    let total_start = Instant::now();

    // Parse
    let mut topology = if params.is_empty() {
        crate::parser::parse_file(path)?
    } else {
        crate::parser::parse_file_with_params(path, params)?
    };
    if let Some(seed) = opts.seed {
        crate::seed::parse(&seed.to_string()).map_err(crate::Error::invalid_argument)?;
        topology.lab.seed = Some(seed);
    }
    topology.validate().bail()?;

    // Deploy
    let deploy_start = Instant::now();
    let lab = topology.deploy_with(ctx).await?;
    let deploy_ms = deploy_start.elapsed().as_millis() as u64;
    let seed = lab.topology().lab.seed;

    // `deploy()` ran the assertions once already and kept the outcome.
    // Running them again doubled a failing lab's retry budget, and a
    // stateful assertion (`counter`) saw the first pass's packets.
    let mut assertions = lab.assertion_results().to_vec();
    let mut lab = lab;
    let row = |description: String, passed: bool, detail: Option<String>| AssertionResult {
        description,
        passed,
        detail,
        duration_ms: 0,
    };
    if opts.scenarios {
        for sc in lab.topology().scenarios.clone() {
            match crate::scenario::run_scenario_with(&mut lab, &sc, ctx).await {
                Ok(r) => {
                    for step in r.steps {
                        for a in step.actions {
                            assertions.push(row(
                                format!(
                                    "scenario {}: {} (at {}ms)",
                                    r.name, a.description, step.time_ms
                                ),
                                a.ok,
                                a.detail,
                            ));
                        }
                    }
                    // putting the lab back is part of the scenario's result
                    for a in r.cleanup {
                        assertions.push(row(
                            format!("scenario {}: {}", r.name, a.description),
                            a.ok,
                            a.detail,
                        ));
                    }
                }
                Err(e) => assertions.push(row(
                    format!("scenario {}", sc.name),
                    false,
                    Some(e.to_string()),
                )),
            }
        }
    }
    if opts.benchmarks {
        for b in lab.topology().benchmarks.clone() {
            // blocking (ping, iperf3): off the runtime's threads
            let (l, bench, c) = (lab.clone(), b.clone(), ctx.clone());
            let ran = tokio::task::spawn_blocking(move || {
                crate::benchmark::run_benchmark_with(&l, &bench, &c)
            })
            .await
            .map_err(|e| crate::Error::Runtime(format!("benchmark {}: {e}", b.name)))
            .and_then(|r| r);
            match ran {
                Ok(r) => {
                    for t in r.tests {
                        assertions.push(row(
                            format!("benchmark {}: {}", r.name, t.description),
                            t.passed,
                            t.detail,
                        ));
                    }
                }
                Err(e) => assertions.push(row(
                    format!("benchmark {}", b.name),
                    false,
                    Some(e.to_string()),
                )),
            }
        }
    }

    let failed = assertions.iter().any(|a| !a.passed);
    let (error, kept) = if failed && opts.keep_failed {
        (None, Some(lab.name().to_string()))
    } else {
        // A lab that does not destroy is an error of the test, but its
        // assertion results still stand.
        let error = lab
            .destroy_with(ctx)
            .await
            .err()
            .map(|e| format!("destroy failed: {e}"));
        (error, None)
    };
    let passed = error.is_none() && !failed;

    let total_ms = total_start.elapsed().as_millis() as u64;

    Ok(TestResult {
        file,
        assertions,
        deploy_ms,
        total_ms,
        passed,
        error,
        kept,
        seed,
    })
}

/// Evaluate every `validate { … }` assertion of `topology` against a
/// deployed lab and return one structured result per assertion, in
/// declaration order.
///
/// This is the single assertion engine: `deploy()` step 19, the CI
/// `test` runner, and the scenario engine's `validate` action all go
/// through it. Target addresses come from
/// [`crate::ipmap::build_ip_map`], so nodes addressed only through a
/// bridge `network`, a dummy / loopback interface, or a WireGuard
/// tunnel resolve just like link endpoints (issue #34).
///
/// Never panics and never bails: an assertion whose target has no
/// address, or whose `exec` fails (missing namespace, rootless run),
/// comes back as `passed == false` with the reason in `detail`.
pub fn run_assertions(
    lab: &crate::running::RunningLab,
    topology: &crate::types::Topology,
) -> Vec<AssertionResult> {
    let ip_map = crate::ipmap::build_ip_map(topology);

    let mut results = Vec::with_capacity(topology.assertions.len());

    // Block-level settle policy: re-evaluate a failing assertion rather
    // than judging a lab that has not converged yet. Assertions run the
    // moment the deploy finishes, which is far too early for anything
    // dynamic — a `routing frr { bgp }` lab needs tens of seconds before
    // `reach` or `route-has` can pass.
    //
    // Only failures are retried, so a lab that is already converged
    // costs nothing, and a `no-reach` that is *meant* to fail settles on
    // the first attempt.
    let attempts = topology.assertion_retries.unwrap_or(1).max(1);
    let interval = topology
        .assertion_interval
        .as_deref()
        .and_then(|i| crate::helpers::parse_duration(i).ok())
        .unwrap_or(std::time::Duration::from_secs(1));

    for assertion in &topology.assertions {
        let start = Instant::now();
        let mut outcome = eval_assertion(lab, assertion, &ip_map);
        for attempt in 1..attempts {
            if outcome.1 {
                break;
            }
            let _ = attempt;
            std::thread::sleep(interval);
            outcome = eval_assertion(lab, assertion, &ip_map);
        }
        let (desc, passed, detail) = outcome;
        let duration_ms = start.elapsed().as_millis() as u64;
        results.push(AssertionResult {
            description: desc,
            passed,
            detail,
            duration_ms,
        });
    }

    results
}

/// Evaluate a single assertion (public for use by scenario engine).
pub fn eval_assertion_pub(
    lab: &crate::running::RunningLab,
    assertion: &Assertion,
    ip_map: &std::collections::BTreeMap<String, String>,
) -> (String, bool, Option<String>) {
    eval_assertion(lab, assertion, ip_map)
}

fn eval_assertion(
    lab: &crate::running::RunningLab,
    assertion: &Assertion,
    ip_map: &std::collections::BTreeMap<String, String>,
) -> (String, bool, Option<String>) {
    match assertion {
        Assertion::Reach { from, to } => {
            let desc = format!("reach {from} {to}");
            if let Some(ip) = ip_map.get(to) {
                match lab.exec(from, "ping", &["-c1", "-W2", ip]) {
                    Ok(out) if out.exit_code == 0 => (desc, true, None),
                    Ok(out) => {
                        let err = out.stderr.trim();
                        let detail = if err.is_empty() {
                            format!("no reply from {ip}")
                        } else {
                            err.to_string()
                        };
                        (desc, false, Some(detail))
                    }
                    Err(e) => (desc, false, Some(e.to_string())),
                }
            } else {
                (desc, false, Some(format!("no IP found for node '{to}'")))
            }
        }
        Assertion::NoReach { from, to } => {
            let desc = format!("no-reach {from} {to}");
            if let Some(ip) = ip_map.get(to) {
                match lab.exec(from, "ping", &["-c1", "-W2", ip]) {
                    // A ping that could not run proves nothing: a container
                    // without `ping` (exit 126/127) used to pass (#208).
                    Ok(out) if ping_did_not_run(out.exit_code, &out.stderr) => (
                        desc,
                        false,
                        Some(format!(
                            "ping could not run on '{from}' (exit {}): {}",
                            out.exit_code,
                            out.stderr.trim()
                        )),
                    ),
                    Ok(out) if out.exit_code != 0 => (desc, true, None),
                    Ok(_) => (
                        desc,
                        false,
                        Some("host is reachable (expected unreachable)".into()),
                    ),
                    Err(e) => (desc, false, Some(e.to_string())),
                }
            } else {
                (desc, false, Some(format!("no IP found for node '{to}'")))
            }
        }
        Assertion::TcpConnect {
            from,
            to,
            port,
            timeout,
            retries,
            interval,
        } => {
            let desc = format!("tcp-connect {from} {to}:{port}");
            let addr = ip_map
                .get(to)
                .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
                .map(|ip| std::net::SocketAddr::new(ip, *port));
            if let Some(addr) = addr {
                let t = timeout.as_deref().unwrap_or("3s");
                let timeout = crate::helpers::parse_duration(t)
                    .ok()
                    .filter(|d| !d.is_zero())
                    .unwrap_or(std::time::Duration::from_secs(3));
                // `retries 0` made no attempt and failed without a reason
                let max_attempts = retries.unwrap_or(1).max(1);
                let retry_interval = interval
                    .as_deref()
                    .and_then(|i| crate::helpers::parse_duration(i).ok())
                    .unwrap_or(std::time::Duration::from_millis(500));

                let mut result = (desc.clone(), false, None);
                for attempt in 0..max_attempts {
                    // A socket in `from`'s namespace: no shell needed there (#179).
                    match lab.tcp_connect(from, addr, timeout) {
                        Ok(()) => {
                            result = (desc.clone(), true, None);
                            break;
                        }
                        Err(e) => {
                            result = (desc.clone(), false, Some(e.to_string()));
                        }
                    }
                    if attempt + 1 < max_attempts {
                        std::thread::sleep(retry_interval);
                    }
                }
                result
            } else {
                (desc, false, Some(format!("no IP found for node '{to}'")))
            }
        }
        Assertion::LatencyUnder {
            from,
            to,
            max,
            samples,
        } => {
            let desc = format!("latency-under {from} {to} {max}");
            if let Some(ip) = ip_map.get(to) {
                let count = samples.unwrap_or(5).to_string();
                match lab.exec(from, "ping", &["-c", &count, "-q", ip]) {
                    Ok(out) if out.exit_code == 0 => {
                        if let Some(avg_ms) = parse_ping_avg(&out.stdout) {
                            let max_ms = crate::helpers::parse_duration(max)
                                .map(|d| d.as_secs_f64() * 1000.0)
                                .unwrap_or(f64::MAX);
                            if avg_ms <= max_ms {
                                (desc, true, Some(format!("{avg_ms:.1}ms")))
                            } else {
                                (desc, false, Some(format!("{avg_ms:.1}ms > {max}")))
                            }
                        } else {
                            (desc, false, Some("could not parse ping output".into()))
                        }
                    }
                    _ => (desc, false, Some("ping failed".into())),
                }
            } else {
                (desc, false, Some(format!("no IP found for node '{to}'")))
            }
        }
        Assertion::RouteHas {
            node,
            destination,
            via,
            dev,
        } => {
            let desc = format!("route-has {node} {destination}");
            // What `ip route show DESTINATION` lists: main-table routes with
            // exactly that prefix, read over netlink (#179). A route passes
            // when one of its hops has the gateway and the device asked for.
            let Some((dst, len)) = parse_route_destination(destination) else {
                return (
                    desc,
                    false,
                    Some(format!("'{destination}' is not a prefix or `default`")),
                );
            };
            match lab.main_routes_to_blocking(node, dst, len) {
                Ok(routes) if !routes.is_empty() => {
                    let via: Option<std::net::IpAddr> = via.as_ref().and_then(|v| v.parse().ok());
                    let shown: Vec<String> = routes
                        .iter()
                        .map(|hops| {
                            let hops: Vec<String> = hops.iter().map(ToString::to_string).collect();
                            format!("{destination} {}", hops.join(" nexthop "))
                        })
                        .collect();
                    let ok = routes.iter().flatten().any(|hop| {
                        via.is_none_or(|v| hop.via == Some(v))
                            && dev.as_ref().is_none_or(|d| hop.dev.as_deref() == Some(d))
                    });
                    (desc, ok, Some(shown.join("; ")))
                }
                Ok(_) => (desc, false, Some("no route found".into())),
                Err(e) => (desc, false, Some(e.to_string())),
            }
        }
        Assertion::DnsResolves {
            from,
            name,
            expected_ip,
        } => {
            let desc = format!("dns-resolves {from} {name} {expected_ip}");
            match lab.exec(from, "getent", &["hosts", name]) {
                Ok(out) if out.exit_code == 0 && resolves_to(&out.stdout, expected_ip) => {
                    (desc, true, None)
                }
                Ok(out) => (desc, false, Some(format!("got: {}", out.stdout.trim()))),
                Err(e) => (desc, false, Some(e.to_string())),
            }
        }
        Assertion::Counter {
            node,
            name,
            metric,
            op,
            value,
        } => {
            use crate::types::CounterMetric;
            let what = match metric {
                CounterMetric::Packets => "packets",
                CounterMetric::Bytes => "bytes",
            };
            let desc = format!("counter {node} {name} {what} {} {value}", op.as_str());
            match lab.firewall_counters_blocking(node) {
                Ok(counters) => match counters.iter().find(|c| &c.name == name) {
                    Some(c) => {
                        let got = match metric {
                            CounterMetric::Packets => c.packets,
                            CounterMetric::Bytes => c.bytes,
                        };
                        if op.holds(got, *value) {
                            (desc, true, None)
                        } else {
                            (desc, false, Some(format!("counted {got} {what}")))
                        }
                    }
                    None => (
                        desc,
                        false,
                        Some(format!("node '{node}' has no counter '{name}'")),
                    ),
                },
                Err(e) => (desc, false, Some(e.to_string())),
            }
        }
    }
}

/// A `route-has` destination: `default` (IPv4, as `ip route show default`),
/// a prefix, or an address (a host route).
fn parse_route_destination(s: &str) -> Option<(std::net::IpAddr, u8)> {
    if s == "default" {
        return Some((std::net::Ipv4Addr::UNSPECIFIED.into(), 0));
    }
    if let Ok(cidr) = crate::helpers::parse_cidr(s) {
        return Some(cidr);
    }
    let ip: std::net::IpAddr = s.parse().ok()?;
    Some((ip, if ip.is_ipv4() { 32 } else { 128 }))
}

/// Parse average latency from ping -q output.
/// Looks for "rtt min/avg/max/mdev = X/Y/Z/W ms" and returns Y.
pub(crate) fn parse_ping_avg(output: &str) -> Option<f64> {
    for line in output.lines() {
        if line.contains("min/avg/max") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() >= 2 {
                let stats: Vec<&str> = parts[1].trim().split('/').collect();
                if stats.len() >= 2 {
                    return stats[1].trim().parse::<f64>().ok();
                }
            }
        }
    }
    None
}

/// Whether a `ping` exit says it never ran, rather than that the target
/// did not answer: the runtime could not exec it (125–127: `docker exec`
/// / `podman exec` failures, not found, not executable), or the kernel
/// refused its socket.
fn ping_did_not_run(exit_code: i32, stderr: &str) -> bool {
    (125..=127).contains(&exit_code)
        || stderr.contains("Operation not permitted")
        || stderr.contains("Permission denied")
}

/// Whether `getent hosts` output lists `expected` as an address: the first
/// field of a line, compared exactly (as an IP when both parse). A
/// substring match passed `10.0.0.1` for `10.0.0.12`, and any name (#208).
fn resolves_to(getent: &str, expected: &str) -> bool {
    let want = expected.parse::<std::net::IpAddr>().ok();
    getent
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|addr| match (want, addr.parse::<std::net::IpAddr>()) {
            (Some(w), Ok(a)) => w == a,
            _ => addr == expected,
        })
}

// ─── Output Formatters ──────────────────────────────────────

/// Format test results as JUnit XML.
pub fn format_junit(results: &[TestResult]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");

    for result in results {
        let errors = usize::from(result.error.is_some());
        let tests = result.assertions.len() + errors;
        let failures = result.assertions.iter().filter(|a| !a.passed).count();
        let time_secs = result.total_ms as f64 / 1000.0;

        xml.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" time=\"{time_secs:.3}\">\n",
            escape_xml(&result.file)
        ));
        // the seed the lab ran with: `test --seed N` replays its losses
        if let Some(seed) = result.seed {
            xml.push_str(&format!(
                "    <properties>\n      <property name=\"seed\" value=\"{seed}\"/>\n    </properties>\n"
            ));
        }
        // A file that did not parse, validate, deploy or destroy: it was
        // missing from the report, which then read as all green (#208).
        if let Some(err) = &result.error {
            xml.push_str(&format!(
                "    <testcase name=\"{}\">\n      <error message=\"{}\">{}</error>\n    </testcase>\n",
                escape_xml(&result.file),
                escape_xml(err),
                escape_xml(err)
            ));
        }

        for a in &result.assertions {
            let a_time = a.duration_ms as f64 / 1000.0;
            if a.passed {
                xml.push_str(&format!(
                    "    <testcase name=\"{}\" time=\"{a_time:.3}\"/>\n",
                    escape_xml(&a.description)
                ));
            } else {
                xml.push_str(&format!(
                    "    <testcase name=\"{}\" time=\"{a_time:.3}\">\n",
                    escape_xml(&a.description)
                ));
                let msg = a.detail.as_deref().unwrap_or("assertion failed");
                xml.push_str(&format!(
                    "      <failure message=\"{}\">{}</failure>\n",
                    escape_xml(msg),
                    escape_xml(msg)
                ));
                xml.push_str("    </testcase>\n");
            }
        }

        xml.push_str("  </testsuite>\n");
    }

    xml.push_str("</testsuites>\n");
    xml
}

/// Format test results as TAP (Test Anything Protocol).
pub fn format_tap(results: &[TestResult]) -> String {
    let mut out = String::from("TAP version 13\n");
    let total: usize = results
        .iter()
        .map(|r| r.assertions.len() + usize::from(r.error.is_some()))
        .sum();
    out.push_str(&format!("1..{total}\n"));

    let mut n = 0;
    for result in results {
        if let Some(seed) = result.seed {
            out.push_str(&format!("# {}: seed {seed}\n", tap_text(&result.file)));
        }
        if let Some(err) = &result.error {
            n += 1;
            out.push_str(&format!("not ok {n} - {}\n", tap_text(&result.file)));
            tap_yaml(&mut out, err, "error");
        }
        for a in &result.assertions {
            n += 1;
            let prefix = if a.passed { "ok" } else { "not ok" };
            out.push_str(&format!(
                "{prefix} {n} - {} ({}ms)\n",
                tap_text(&a.description),
                a.duration_ms
            ));
            if !a.passed {
                tap_yaml(
                    &mut out,
                    a.detail.as_deref().unwrap_or("assertion failed"),
                    "fail",
                );
            }
        }
    }

    out
}

/// A TAP description: a `#` starts a directive (`# SKIP`), so it is
/// escaped, and a newline would end the line.
fn tap_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('#', "\\#")
        .replace('\n', " ")
}

/// A TAP YAML diagnostic block. The message is a literal block scalar:
/// written in quotes, a newline, `"` or `\\` in it (raw ping stderr ends
/// in one) broke the YAML (#208).
fn tap_yaml(out: &mut String, message: &str, severity: &str) {
    out.push_str("  ---\n  message: |-\n");
    for line in message.trim_end().lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("  severity: {severity}\n  ...\n"));
}

fn escape_xml(s: &str) -> String {
    // XML 1.0 has no escape for most control characters (a colour code
    // in a command's stderr): they are dropped
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `getent hosts` output, exactly: `10.0.0.1` is not `10.0.0.12`, and
    /// a name is not an address (#208).
    #[test]
    fn dns_resolves_compares_the_address_column() {
        let out = "10.0.0.12       b b-eth0\n";
        assert!(!resolves_to(out, "10.0.0.1"));
        assert!(resolves_to(out, "10.0.0.12"));
        assert!(!resolves_to(out, "b"));
        assert!(resolves_to("fd00::1   b\n", "fd00:0::1"));
    }

    /// A ping that could not run is not a `no-reach` pass (#208); one that
    /// got no answer, or found no route, is.
    #[test]
    fn a_ping_that_did_not_run_is_told_apart() {
        assert!(ping_did_not_run(
            127,
            "exec: \"ping\": executable file not found"
        ));
        assert!(ping_did_not_run(126, ""));
        assert!(ping_did_not_run(2, "ping: socket: Operation not permitted"));
        assert!(!ping_did_not_run(1, ""));
        assert!(!ping_did_not_run(
            2,
            "ping: connect: Network is unreachable"
        ));
    }

    /// A file that did not deploy is an error in both reports, and a
    /// multi-line detail with quotes stays valid TAP YAML (#208).
    #[test]
    fn errors_and_awkward_details_reach_the_reports() {
        let results = vec![
            TestResult::errored("bad.nll", "deploy failed: <boom> \"x\"", 5),
            TestResult {
                file: "good #1.nll".into(),
                assertions: vec![AssertionResult {
                    description: "reach a b".into(),
                    passed: false,
                    detail: Some(
                        "ping: connect: Network is unreachable\n\"quoted\" \\ back\n".into(),
                    ),
                    duration_ms: 3,
                }],
                deploy_ms: 1,
                total_ms: 9,
                passed: false,
                error: None,
                kept: None,
                seed: None,
            },
        ];
        let tap = format_tap(&results);
        assert!(tap.contains("1..2\n"), "{tap}");
        assert!(tap.contains("not ok 1 - bad.nll\n"), "{tap}");
        assert!(
            tap.contains("  message: |-\n    ping: connect: Network is unreachable\n    \"quoted\" \\ back\n  severity: fail\n"),
            "{tap}"
        );
        assert!(tap.contains("  severity: error\n"), "{tap}");
        let xml = format_junit(&results);
        assert!(
            xml.contains("tests=\"1\" failures=\"0\" errors=\"1\""),
            "{xml}"
        );
        assert!(
            xml.contains("<error message=\"deploy failed: &lt;boom&gt; &quot;x&quot;\">"),
            "{xml}"
        );
        assert!(
            !format_junit(&[TestResult {
                assertions: vec![AssertionResult {
                    description: "x".into(),
                    passed: false,
                    detail: Some("\u{1b}[31mred".into()),
                    duration_ms: 0,
                }],
                ..TestResult::errored("f", "e", 0)
            }])
            .contains('\u{1b}')
        );
    }

    #[test]
    fn test_format_junit_pass() {
        let results = vec![TestResult {
            file: "test.nll".into(),
            assertions: vec![AssertionResult {
                description: "reach a b".into(),
                passed: true,
                detail: None,
                duration_ms: 500,
            }],
            deploy_ms: 100,
            total_ms: 700,
            passed: true,
            error: None,
            kept: None,
            seed: None,
        }];
        let xml = format_junit(&results);
        assert!(xml.contains("<testsuite name=\"test.nll\" tests=\"1\" failures=\"0\""));
        assert!(xml.contains("testcase name=\"reach a b\""));
        assert!(!xml.contains("<failure"));
    }

    #[test]
    fn test_format_junit_fail() {
        let results = vec![TestResult {
            file: "test.nll".into(),
            assertions: vec![AssertionResult {
                description: "tcp-connect a b:80".into(),
                passed: false,
                detail: Some("connection refused".into()),
                duration_ms: 200,
            }],
            deploy_ms: 100,
            total_ms: 400,
            passed: false,
            error: None,
            kept: None,
            seed: None,
        }];
        let xml = format_junit(&results);
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("<failure message=\"connection refused\""));
    }

    #[test]
    fn test_format_tap() {
        let results = vec![TestResult {
            file: "test.nll".into(),
            assertions: vec![
                AssertionResult {
                    description: "reach a b".into(),
                    passed: true,
                    detail: None,
                    duration_ms: 500,
                },
                AssertionResult {
                    description: "no-reach a c".into(),
                    passed: false,
                    detail: Some("host reachable".into()),
                    duration_ms: 300,
                },
            ],
            deploy_ms: 100,
            total_ms: 900,
            passed: false,
            error: None,
            kept: None,
            seed: None,
        }];
        let tap = format_tap(&results);
        assert!(tap.starts_with("TAP version 13\n1..2\n"));
        assert!(tap.contains("ok 1 - reach a b"));
        assert!(tap.contains("not ok 2 - no-reach a c"));
        assert!(tap.contains("  message: |-\n    host reachable\n"), "{tap}");
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn test_parse_ping_avg_roundtrip() {
        let output = "rtt min/avg/max/mdev = 0.1/2.5/5.0/1.0 ms\n";
        assert_eq!(parse_ping_avg(output), Some(2.5));
    }

    #[test]
    fn test_parse_ping_avg_no_stats() {
        assert_eq!(parse_ping_avg("no rtt line"), None);
    }

    #[test]
    fn test_parse_ping_avg_full_output() {
        let output = "PING 10.0.0.1 (10.0.0.1) 56(84) bytes of data.\n\
            --- 10.0.0.1 ping statistics ---\n\
            5 packets transmitted, 5 received, 0% packet loss, time 4006ms\n\
            rtt min/avg/max/mdev = 0.123/0.456/0.789/0.012 ms\n";
        assert_eq!(parse_ping_avg(output), Some(0.456));
    }

    /// Build a `RunningLab` without deploying anything. Every node maps
    /// to a namespace that does not exist, so `exec` fails with
    /// `NamespaceNotFound` — exactly what a rootless run sees.
    fn undeployed_lab(src: &str) -> (crate::running::RunningLab, crate::types::Topology) {
        let topology = crate::parser::parse(src).unwrap();
        let namespace_names = topology
            .nodes
            .keys()
            .map(|n| (n.clone(), format!("nlink-lab-test-nonexistent-{n}")))
            .collect();
        let lab = crate::running::RunningLab::new(
            topology.clone(),
            namespace_names,
            Default::default(),
            None,
            Vec::new(),
            false,
            false,
        );
        (lab, topology)
    }

    /// Issue #34: a `reach` whose target has no address must surface
    /// as a structured non-pass, not vanish into a log line.
    #[test]
    fn test_run_assertions_no_ip_is_structured_failure() {
        let (lab, topology) = undeployed_lab(
            r#"
lab "t"
node a
node b
link a:eth0 -- b:eth0
validate {
  reach a b
  no-reach b a
  tcp-connect a b 80
  latency-under a b 10ms
}
"#,
        );
        let results = run_assertions(&lab, &topology);
        assert_eq!(results.len(), 4);
        for r in &results {
            assert!(
                !r.passed,
                "{} must not pass without an address",
                r.description
            );
            let detail = r.detail.as_deref().unwrap_or("");
            assert!(
                detail.contains("no IP found"),
                "{}: expected a 'no IP found' detail, got {detail:?}",
                r.description
            );
        }
        assert_eq!(results[0].description, "reach a b");
        assert_eq!(results[1].description, "no-reach b a");
        assert_eq!(results[2].description, "tcp-connect a b:80");
        assert_eq!(results[3].description, "latency-under a b 10ms");
    }

    /// With an address resolved but no namespace to exec in, every
    /// assertion kind must report the exec error rather than pass —
    /// including `no-reach`, whose "ping failed" branch is the easy
    /// one to get wrong.
    #[test]
    fn test_run_assertions_exec_failure_is_structured_failure() {
        let (lab, topology) = undeployed_lab(
            r#"
lab "t"
node a
node b
network lan {
  members [a:eth0, b:eth0]
  subnet 10.0.1.0/24
}
validate {
  reach a b
  no-reach a b
  tcp-connect a b 22
  latency-under a b 10ms
  route-has a 10.0.1.0/24 dev eth0
  dns-resolves a b 10.0.1.2
}
"#,
        );
        let results = run_assertions(&lab, &topology);
        assert_eq!(results.len(), 6);
        for r in &results {
            assert!(!r.passed, "{} must not pass on exec failure", r.description);
            assert!(
                r.detail.is_some(),
                "{} must carry a detail on failure",
                r.description
            );
            assert!(
                !r.detail.as_deref().unwrap().contains("no IP found"),
                "{}: bridge-network address must have been resolved (issue #34), got {:?}",
                r.description,
                r.detail
            );
        }
    }

    #[test]
    fn test_run_assertions_empty_topology() {
        let (lab, topology) = undeployed_lab("lab \"t\"\nnode a\n");
        assert!(run_assertions(&lab, &topology).is_empty());
    }

    #[test]
    fn test_format_tap_empty() {
        let results = vec![TestResult {
            file: "empty.nll".into(),
            assertions: vec![],
            deploy_ms: 50,
            total_ms: 50,
            passed: true,
            error: None,
            kept: None,
            seed: None,
        }];
        let tap = format_tap(&results);
        assert!(tap.contains("1..0"));
    }

    #[test]
    fn test_assertion_result_types() {
        let result = AssertionResult {
            description: "reach a b".into(),
            passed: true,
            detail: Some("ok".into()),
            duration_ms: 100,
        };
        assert!(result.passed);
        assert_eq!(result.duration_ms, 100);
    }
}
