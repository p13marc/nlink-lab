//! Benchmark execution engine.
//!
//! Runs performance benchmarks (ping, iperf3) against a deployed lab
//! and evaluates assertions against collected metrics.

use crate::error::Result;
use crate::running::RunningLab;
use crate::types::{Benchmark, BenchmarkAssertion, BenchmarkTest, CompareOp};

/// Result of running a benchmark.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub tests: Vec<BenchmarkTestResult>,
    pub passed: bool,
}

/// Result of a single benchmark test.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BenchmarkTestResult {
    pub description: String,
    pub metrics: std::collections::BTreeMap<String, f64>,
    pub assertions: Vec<AssertionEval>,
    pub passed: bool,
    /// Why the test produced no measurement (no address for the target,
    /// a missing tool, a failed run). A failed test used to give no
    /// reason (#208).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Evaluation of a single assertion.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssertionEval {
    pub metric: String,
    pub op: String,
    pub threshold: String,
    pub actual: Option<f64>,
    pub passed: bool,
}

/// What a benchmark metric is measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    /// Milliseconds (a duration threshold: `500us`, `20ms`, `1s`).
    Millis,
    /// Bits per second (a rate threshold: `900mbit`, `1g`, `10mbyte`).
    BitsPerSecond,
    /// A percentage (`1%`).
    Percent,
}

/// The unit of `metric` for `test`, or `None` when the test does not
/// report it: ping reports `min`, `avg`, `max`, `p99` and `loss`; iperf3
/// `bandwidth`, and with `udp` also `jitter` and `loss`.
pub fn metric_unit(test: &BenchmarkTest, metric: &str) -> Option<MetricUnit> {
    match (test, metric) {
        (BenchmarkTest::Ping { .. }, "min" | "avg" | "max" | "p99") => Some(MetricUnit::Millis),
        (BenchmarkTest::Ping { .. }, "loss") => Some(MetricUnit::Percent),
        (BenchmarkTest::Iperf3 { .. }, "bandwidth") => Some(MetricUnit::BitsPerSecond),
        (BenchmarkTest::Iperf3 { udp: true, .. }, "jitter") => Some(MetricUnit::Millis),
        (BenchmarkTest::Iperf3 { udp: true, .. }, "loss") => Some(MetricUnit::Percent),
        _ => None,
    }
}

/// A threshold in `unit`, with the units the NLL lexer accepts: any
/// duration (`1s`, `100ns`) as milliseconds, any rate (`1g`, `10mbyte`) as
/// bit/s, a percentage. A bare number is taken in the metric's unit. Only
/// `ms`, `us`, `%` and `*bit` used to parse; anything else never passed.
pub fn parse_threshold(unit: MetricUnit, value: &str) -> Option<f64> {
    let v = value.trim();
    if let Ok(n) = v.parse::<f64>() {
        return n.is_finite().then_some(n);
    }
    match unit {
        MetricUnit::Millis => crate::helpers::parse_duration(v)
            .ok()
            .map(|d| d.as_secs_f64() * 1000.0),
        MetricUnit::BitsPerSecond => crate::helpers::parse_rate_bps(v).ok().map(|b| b as f64),
        MetricUnit::Percent => v
            .strip_suffix('%')?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite()),
    }
}

/// Run all tests in a benchmark.
pub fn run_benchmark(lab: &RunningLab, benchmark: &Benchmark) -> Result<BenchmarkResult> {
    run_benchmark_with(lab, benchmark, &crate::ops::OpCtx::default())
}

/// [`run_benchmark`], reporting each test's result to `ctx` and stopping
/// between tests when it is cancelled (#236). Blocking, like the tools it
/// runs: an async caller runs it in `spawn_blocking`.
pub fn run_benchmark_with(
    lab: &RunningLab,
    benchmark: &Benchmark,
    ctx: &crate::ops::OpCtx,
) -> Result<BenchmarkResult> {
    ctx.set_lab(lab.name());
    ctx.started();
    let result = benchmark_ctx(lab, benchmark, ctx);
    ctx.finished(&result, false);
    result
}

fn benchmark_ctx(
    lab: &RunningLab,
    benchmark: &Benchmark,
    ctx: &crate::ops::OpCtx,
) -> Result<BenchmarkResult> {
    let ip_map = crate::ipmap::build_ip_map(lab.topology());

    let mut test_results = Vec::new();
    let mut all_passed = true;

    let total = benchmark.tests.len();
    for (i, test) in benchmark.tests.iter().enumerate() {
        ctx.check()?;
        let result = match test {
            BenchmarkTest::Ping {
                from, to, count, ..
            } => run_ping_benchmark(lab, test, from, to, *count, &ip_map),
            BenchmarkTest::Iperf3 {
                from,
                to,
                duration,
                streams,
                udp,
                rate,
                ..
            } => run_iperf3_benchmark(
                lab,
                test,
                Iperf3Run {
                    from,
                    to,
                    duration: duration.as_deref(),
                    streams: *streams,
                    udp: *udp,
                    rate: rate.as_deref(),
                },
                &ip_map,
            ),
        };
        if !result.passed {
            all_passed = false;
        }
        ctx.emit(nlink_lab_shared::api::OpPhase::Assertion {
            description: result.description.clone(),
            passed: result.passed,
            detail: result.detail.clone(),
        });
        ctx.step(i + 1, total, || result.description.clone());
        test_results.push(result);
    }

    Ok(BenchmarkResult {
        name: benchmark.name.clone(),
        tests: test_results,
        passed: all_passed,
    })
}

fn assertions_of(test: &BenchmarkTest) -> &[BenchmarkAssertion] {
    match test {
        BenchmarkTest::Ping { assertions, .. } | BenchmarkTest::Iperf3 { assertions, .. } => {
            assertions
        }
    }
}

/// A test that measured nothing: every assertion fails, with `detail`.
fn not_measured(test: &BenchmarkTest, desc: String, detail: String) -> BenchmarkTestResult {
    let metrics = Default::default();
    BenchmarkTestResult {
        description: desc,
        assertions: evaluate_assertions(test, &metrics),
        metrics,
        passed: false,
        detail: Some(detail),
    }
}

fn run_ping_benchmark(
    lab: &RunningLab,
    test: &BenchmarkTest,
    from: &str,
    to: &str,
    count: Option<u32>,
    ip_map: &std::collections::BTreeMap<String, String>,
) -> BenchmarkTestResult {
    let desc = format!("ping {from} -> {to}");
    let count = count.unwrap_or(10);

    let Some(target_ip) = ip_map.get(to) else {
        return not_measured(test, desc, format!("no address for '{to}'"));
    };

    let count_str = count.to_string();
    let output = match lab.exec(from, "ping", &["-c", &count_str, "-q", target_ip]) {
        Ok(out) => out,
        Err(e) => return not_measured(test, desc, format!("ping on '{from}': {e}")),
    };
    let metrics = parse_ping(&output.stdout);
    if metrics.is_empty() {
        return not_measured(
            test,
            desc,
            format!(
                "no ping statistics (exit {}): {}",
                output.exit_code,
                output.stderr.trim()
            ),
        );
    }
    let evals = evaluate_assertions(test, &metrics);
    let passed = evals.iter().all(|e| e.passed);

    BenchmarkTestResult {
        description: desc,
        metrics,
        assertions: evals,
        passed,
        detail: None,
    }
}

/// `min` / `avg` / `max` / `p99` (ms) and `loss` (%) from `ping -q`
/// output: iputils' `rtt min/avg/max/mdev = a/b/c/d ms`, or busybox's
/// `round-trip min/avg/max = a/b/c ms`, whose three fields gave no RTT
/// metrics at all (#208).
fn parse_ping(stdout: &str) -> std::collections::BTreeMap<String, f64> {
    let mut metrics = std::collections::BTreeMap::new();
    for line in stdout.lines() {
        if line.contains("min/avg/max")
            && let Some(stats_part) = line.split('=').nth(1)
        {
            let parts: Vec<&str> = stats_part
                .trim()
                .trim_end_matches("ms")
                .split('/')
                .collect();
            if parts.len() >= 3 {
                if let Ok(min) = parts[0].trim().parse::<f64>() {
                    metrics.insert("min".into(), min);
                }
                if let Ok(avg) = parts[1].trim().parse::<f64>() {
                    metrics.insert("avg".into(), avg);
                }
                if let Ok(max) = parts[2].trim().parse::<f64>() {
                    metrics.insert("max".into(), max);
                    // p99 approximation: use max for small sample sizes
                    metrics.insert("p99".into(), max);
                }
            }
        }
        // "5 packets transmitted, 5 received, 0% packet loss"
        if line.contains("packet loss")
            && let Some(pct) = line.split(',').find(|s| s.contains("packet loss"))
        {
            let pct = pct.trim().trim_end_matches("% packet loss").trim();
            if let Ok(loss) = pct.parse::<f64>() {
                metrics.insert("loss".into(), loss);
            }
        }
    }
    metrics
}

/// One iperf3 test's settings.
struct Iperf3Run<'a> {
    from: &'a str,
    to: &'a str,
    duration: Option<&'a str>,
    streams: Option<u32>,
    udp: bool,
    rate: Option<&'a str>,
}

fn run_iperf3_benchmark(
    lab: &RunningLab,
    test: &BenchmarkTest,
    run: Iperf3Run<'_>,
    ip_map: &std::collections::BTreeMap<String, String>,
) -> BenchmarkTestResult {
    let Iperf3Run { from, to, .. } = run;
    let desc = format!("iperf3 {from} -> {to}");

    let Some(target_ip) = ip_map.get(to) else {
        return not_measured(test, desc, format!("no address for '{to}'"));
    };

    // Both ends run iperf3. `exec` returns `Ok` with a non-zero exit code
    // when `which` finds nothing, so the exit code is the signal.
    for node in [from, to] {
        let present = matches!(lab.exec(node, "which", &["iperf3"]), Ok(out) if out.exit_code == 0);
        if !present {
            tracing::warn!("iperf3 not found on '{node}'; skipping benchmark");
            return not_measured(test, desc, format!("iperf3 not found on '{node}'"));
        }
    }

    // Whole seconds, rounded up: iperf3's `-t` takes an integer
    let secs = crate::helpers::parse_duration(run.duration.unwrap_or("5s"))
        .map(|d| (d.as_secs_f64().ceil() as u64).max(1))
        .unwrap_or(5);
    let secs_str = secs.to_string();
    let rate_bps = run
        .rate
        .and_then(|r| crate::helpers::parse_rate_bps(r).ok());

    // A one-shot server in the target namespace, in the background; `sh`,
    // since a busybox image has no bash
    let server_cmd = format!("timeout {} iperf3 -s -1 >/dev/null 2>&1 &", secs + 5);
    let _ = lab.exec(to, "sh", &["-c", &server_cmd]);

    // Brief pause for server to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    let args = iperf3_client_args(target_ip, &secs_str, run.streams, run.udp, rate_bps);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = match lab.exec(from, "iperf3", &arg_refs) {
        Ok(out) => out,
        Err(e) => return not_measured(test, desc, format!("iperf3 on '{from}': {e}")),
    };

    let mut metrics = std::collections::BTreeMap::new();
    parse_iperf3_json(&output.stdout, &mut metrics);
    if metrics.is_empty() {
        let why = serde_json::from_str::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|j| j.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| output.stderr.trim().to_string());
        return not_measured(
            test,
            desc,
            format!("iperf3 gave no result (exit {}): {why}", output.exit_code),
        );
    }

    let evals = evaluate_assertions(test, &metrics);
    let passed = evals.iter().all(|e| e.passed);

    BenchmarkTestResult {
        description: desc,
        metrics,
        assertions: evals,
        passed,
        detail: None,
    }
}

/// Build the iperf3 client argument list. `streams` maps to `-P`, `udp`
/// to `-u`, `rate` to `-b` (bit/s): without it a UDP test runs at
/// iperf3's 1 Mbit/s default, whatever the link (#208).
fn iperf3_client_args(
    target_ip: &str,
    secs_str: &str,
    streams: Option<u32>,
    udp: bool,
    rate_bps: Option<u64>,
) -> Vec<String> {
    let mut args = vec![
        "-c".to_string(),
        target_ip.to_string(),
        "-t".to_string(),
        secs_str.to_string(),
        "-J".to_string(),
    ];
    if let Some(n) = streams.filter(|n| *n > 1) {
        args.push("-P".to_string());
        args.push(n.to_string());
    }
    if udp {
        args.push("-u".to_string());
    }
    if let Some(bps) = rate_bps {
        args.push("-b".to_string());
        args.push(bps.to_string());
    }
    args
}

/// Extract `bandwidth` / `jitter` / `loss` from `iperf3 -J` output.
///
/// A TCP run's bandwidth is what the receiver got (`end.sum_received`):
/// the sender's figure counts what is still queued, which overstates a
/// short run through a shaper. UDP runs (and multi-stream summaries) put
/// everything under `end.sum`, which also carries `jitter_ms` and
/// `lost_percent`.
fn parse_iperf3_json(stdout: &str, metrics: &mut std::collections::BTreeMap<String, f64>) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return;
    };
    let Some(end) = json.get("end") else {
        return;
    };
    let bps_of = |key: &str| {
        end.get(key)
            .and_then(|s| s.get("bits_per_second"))
            .and_then(|v| v.as_f64())
    };
    if let Some(bps) = bps_of("sum_received")
        .or_else(|| bps_of("sum"))
        .or_else(|| bps_of("sum_sent"))
    {
        metrics.insert("bandwidth".into(), bps);
    }
    if let Some(sum) = end.get("sum") {
        if let Some(jitter) = sum.get("jitter_ms").and_then(|v| v.as_f64()) {
            metrics.insert("jitter".into(), jitter);
        }
        if let Some(loss) = sum.get("lost_percent").and_then(|v| v.as_f64()) {
            metrics.insert("loss".into(), loss);
        }
    }
}

/// `above` / `below` / `==` …: the NLL spelling of an operator. The
/// iperf3-missing path printed the enum (`Gt`).
fn op_word(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Gt => "above",
        CompareOp::Lt => "below",
        other => other.as_str(),
    }
}

fn evaluate_assertions(
    test: &BenchmarkTest,
    metrics: &std::collections::BTreeMap<String, f64>,
) -> Vec<AssertionEval> {
    assertions_of(test)
        .iter()
        .map(|a| {
            let actual = metrics.get(&a.metric).copied();
            let threshold =
                metric_unit(test, &a.metric).and_then(|unit| parse_threshold(unit, &a.value));
            let passed = match (actual, threshold) {
                (Some(actual), Some(threshold)) => a.op.holds(actual, threshold),
                _ => false,
            };
            AssertionEval {
                metric: a.metric.clone(),
                op: op_word(a.op).into(),
                threshold: a.value.clone(),
                actual,
                passed,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_threshold() {
        use MetricUnit::*;
        assert_eq!(parse_threshold(Millis, "5ms"), Some(5.0));
        assert_eq!(parse_threshold(Millis, "100us"), Some(0.1));
        assert_eq!(parse_threshold(Percent, "1%"), Some(1.0));
        assert_eq!(
            parse_threshold(BitsPerSecond, "900mbit"),
            Some(900_000_000.0)
        );
        assert_eq!(
            parse_threshold(BitsPerSecond, "1gbit"),
            Some(1_000_000_000.0)
        );
        // every unit the lexer accepts (#208)
        assert_eq!(parse_threshold(Millis, "1s"), Some(1000.0));
        assert_eq!(parse_threshold(Millis, "500ns"), Some(0.0005));
        assert_eq!(parse_threshold(BitsPerSecond, "1g"), Some(1e9));
        assert_eq!(parse_threshold(BitsPerSecond, "10mbyte"), Some(80e6));
        assert_eq!(parse_threshold(Millis, "1%"), None);
    }

    #[test]
    fn metrics_a_test_does_not_report_are_unknown() {
        let ping = BenchmarkTest::Ping {
            from: "a".into(),
            to: "b".into(),
            count: None,
            assertions: vec![],
        };
        let iperf = |udp| BenchmarkTest::Iperf3 {
            from: "a".into(),
            to: "b".into(),
            duration: None,
            streams: None,
            udp,
            rate: None,
            assertions: vec![],
        };
        assert_eq!(metric_unit(&ping, "avg"), Some(MetricUnit::Millis));
        assert_eq!(metric_unit(&ping, "jitter"), None);
        assert_eq!(metric_unit(&iperf(false), "jitter"), None);
        assert_eq!(
            metric_unit(&iperf(true), "jitter"),
            Some(MetricUnit::Millis)
        );
    }

    /// Busybox's three-field summary (Alpine) gave no RTT metrics (#208).
    #[test]
    fn busybox_ping_output_parses() {
        let m = parse_ping(
            "--- 10.0.0.2 ping statistics ---\n4 packets transmitted, 4 packets received, 0% packet loss\nround-trip min/avg/max = 0.064/0.070/0.077 ms\n",
        );
        assert_eq!(m.get("avg"), Some(&0.070));
        assert_eq!(m.get("max"), Some(&0.077));
        assert_eq!(m.get("loss"), Some(&0.0));
        let m = parse_ping("rtt min/avg/max/mdev = 0.1/0.2/0.3/0.05 ms\n");
        assert_eq!(m.get("min"), Some(&0.1));
    }

    #[test]
    fn test_evaluate_assertions() {
        let assertions = vec![
            BenchmarkAssertion {
                metric: "avg".into(),
                op: CompareOp::Lt,
                value: "50ms".into(),
            },
            BenchmarkAssertion {
                metric: "loss".into(),
                op: CompareOp::Lt,
                value: "5%".into(),
            },
        ];
        let mut metrics = std::collections::BTreeMap::new();
        metrics.insert("avg".into(), 10.0);
        metrics.insert("loss".into(), 0.0);
        let test = BenchmarkTest::Ping {
            from: "a".into(),
            to: "b".into(),
            count: None,
            assertions,
        };

        let evals = evaluate_assertions(&test, &metrics);
        assert!(evals[0].passed); // 10 < 50
        assert!(evals[1].passed); // 0 < 5
    }

    #[test]
    fn test_iperf3_client_args_defaults() {
        let args = iperf3_client_args("10.0.0.2", "5", None, false, None);
        assert_eq!(args, vec!["-c", "10.0.0.2", "-t", "5", "-J"]);
        // A single stream is iperf3's default; don't emit a no-op -P 1.
        let args = iperf3_client_args("10.0.0.2", "5", Some(1), false, None);
        assert_eq!(args, vec!["-c", "10.0.0.2", "-t", "5", "-J"]);
    }

    #[test]
    fn test_iperf3_client_args_streams_and_udp() {
        let args = iperf3_client_args("10.0.0.2", "10", Some(4), true, Some(50_000_000));
        assert_eq!(
            args,
            vec![
                "-c", "10.0.0.2", "-t", "10", "-J", "-P", "4", "-u", "-b", "50000000"
            ]
        );
    }

    #[test]
    fn test_parse_iperf3_json_tcp() {
        let json = r#"{"end":{"sum_sent":{"bits_per_second":941000000.0},"sum_received":{"bits_per_second":939000000.0}}}"#;
        let mut m = std::collections::BTreeMap::new();
        parse_iperf3_json(json, &mut m);
        // what the receiver got
        assert_eq!(m.get("bandwidth"), Some(&939000000.0));
        assert!(!m.contains_key("jitter"));
    }

    #[test]
    fn test_parse_iperf3_json_udp() {
        let json =
            r#"{"end":{"sum":{"bits_per_second":1048576.0,"jitter_ms":0.031,"lost_percent":0.5}}}"#;
        let mut m = std::collections::BTreeMap::new();
        parse_iperf3_json(json, &mut m);
        assert_eq!(m.get("bandwidth"), Some(&1048576.0));
        assert_eq!(m.get("jitter"), Some(&0.031));
        assert_eq!(m.get("loss"), Some(&0.5));
    }

    #[test]
    fn test_parse_iperf3_json_garbage() {
        let mut m = std::collections::BTreeMap::new();
        parse_iperf3_json("iperf3: error - unable to connect", &mut m);
        assert!(m.is_empty());
    }

    #[test]
    fn test_evaluate_assertions_fail() {
        let assertions = vec![BenchmarkAssertion {
            metric: "avg".into(),
            op: CompareOp::Lt,
            value: "5ms".into(),
        }];
        let mut metrics = std::collections::BTreeMap::new();
        metrics.insert("avg".into(), 10.0);
        let test = BenchmarkTest::Ping {
            from: "a".into(),
            to: "b".into(),
            count: None,
            assertions,
        };

        let evals = evaluate_assertions(&test, &metrics);
        assert!(!evals[0].passed); // 10 > 5 — fails
        assert_eq!(evals[0].op, "below");
    }
}
