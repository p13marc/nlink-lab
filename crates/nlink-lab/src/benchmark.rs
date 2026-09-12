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
    pub metrics: std::collections::HashMap<String, f64>,
    pub assertions: Vec<AssertionEval>,
    pub passed: bool,
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

/// Run all tests in a benchmark.
pub fn run_benchmark(lab: &RunningLab, benchmark: &Benchmark) -> Result<BenchmarkResult> {
    let ip_map = crate::ipmap::build_ip_map(lab.topology());

    let mut test_results = Vec::new();
    let mut all_passed = true;

    for test in &benchmark.tests {
        let result = match test {
            BenchmarkTest::Ping {
                from,
                to,
                count,
                assertions,
            } => run_ping_benchmark(lab, from, to, *count, assertions, &ip_map),
            BenchmarkTest::Iperf3 {
                from,
                to,
                duration,
                streams,
                udp,
                assertions,
            } => run_iperf3_benchmark(
                lab,
                from,
                to,
                duration.as_deref(),
                *streams,
                *udp,
                assertions,
                &ip_map,
            ),
        };
        if !result.passed {
            all_passed = false;
        }
        test_results.push(result);
    }

    Ok(BenchmarkResult {
        name: benchmark.name.clone(),
        tests: test_results,
        passed: all_passed,
    })
}

fn run_ping_benchmark(
    lab: &RunningLab,
    from: &str,
    to: &str,
    count: Option<u32>,
    assertions: &[BenchmarkAssertion],
    ip_map: &std::collections::HashMap<String, String>,
) -> BenchmarkTestResult {
    let desc = format!("ping {from} -> {to}");
    let count = count.unwrap_or(10);

    let Some(target_ip) = ip_map.get(to) else {
        return BenchmarkTestResult {
            description: desc,
            metrics: Default::default(),
            assertions: vec![],
            passed: false,
        };
    };

    let count_str = count.to_string();
    let output = match lab.exec(from, "ping", &["-c", &count_str, "-q", target_ip]) {
        Ok(out) => out,
        Err(_) => {
            return BenchmarkTestResult {
                description: desc,
                metrics: Default::default(),
                assertions: vec![],
                passed: false,
            };
        }
    };

    let mut metrics = std::collections::HashMap::new();

    // Parse ping output: "rtt min/avg/max/mdev = 0.1/0.2/0.3/0.1 ms"
    for line in output.stdout.lines() {
        if line.contains("min/avg/max")
            && let Some(stats_part) = line.split('=').nth(1)
        {
            let parts: Vec<&str> = stats_part.trim().split('/').collect();
            if parts.len() >= 4 {
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
        // Parse loss: "5 packets transmitted, 5 received, 0% packet loss"
        if line.contains("packet loss")
            && let Some(pct) = line.split(',').find(|s| s.contains("packet loss"))
        {
            let pct = pct.trim().trim_end_matches("% packet loss").trim();
            if let Ok(loss) = pct.parse::<f64>() {
                metrics.insert("loss".into(), loss);
            }
        }
    }

    let evals = evaluate_assertions(assertions, &metrics);
    let passed = evals.iter().all(|e| e.passed);

    BenchmarkTestResult {
        description: desc,
        metrics,
        assertions: evals,
        passed,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_iperf3_benchmark(
    lab: &RunningLab,
    from: &str,
    to: &str,
    duration: Option<&str>,
    streams: Option<u32>,
    udp: bool,
    assertions: &[BenchmarkAssertion],
    ip_map: &std::collections::HashMap<String, String>,
) -> BenchmarkTestResult {
    let desc = format!("iperf3 {from} -> {to}");

    let Some(target_ip) = ip_map.get(to) else {
        return BenchmarkTestResult {
            description: desc,
            metrics: Default::default(),
            assertions: vec![],
            passed: false,
        };
    };

    // Check if iperf3 is available. `exec` returns `Ok` with a
    // non-zero exit code when `which` finds nothing, so the exit code
    // is the signal — `is_err()` only fires when the exec itself fails.
    let iperf3_present = matches!(
        lab.exec(from, "which", &["iperf3"]),
        Ok(out) if out.exit_code == 0
    );
    if !iperf3_present {
        tracing::warn!("iperf3 not found in namespace '{from}'; skipping benchmark");
        return BenchmarkTestResult {
            description: desc,
            metrics: Default::default(),
            assertions: assertions
                .iter()
                .map(|a| AssertionEval {
                    metric: a.metric.clone(),
                    op: format!("{:?}", a.op),
                    threshold: a.value.clone(),
                    actual: None,
                    passed: false,
                })
                .collect(),
            passed: false,
        };
    }

    let dur = duration.unwrap_or("5s");
    let secs = crate::helpers::parse_duration(dur)
        .map(|d| d.as_secs().max(1))
        .unwrap_or(5);
    let secs_str = secs.to_string();

    // Start iperf3 server in target namespace
    // We use exec to run it in foreground with a timeout
    let server_cmd = format!("timeout {} iperf3 -s -1 &>/dev/null &", secs + 5);
    let _ = lab.exec(to, "bash", &["-c", &server_cmd]);

    // Brief pause for server to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Run client
    let args = iperf3_client_args(target_ip, &secs_str, streams, udp);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = lab.exec(from, "iperf3", &arg_refs);

    let mut metrics = std::collections::HashMap::new();

    if let Ok(out) = &output {
        parse_iperf3_json(&out.stdout, &mut metrics);
    }

    let evals = evaluate_assertions(assertions, &metrics);
    let passed = evals.iter().all(|e| e.passed);

    BenchmarkTestResult {
        description: desc,
        metrics,
        assertions: evals,
        passed,
    }
}

/// Build the iperf3 client argument list. `streams` maps to `-P`, `udp`
/// to `-u`; both were previously parsed from NLL and silently dropped.
fn iperf3_client_args(
    target_ip: &str,
    secs_str: &str,
    streams: Option<u32>,
    udp: bool,
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
    args
}

/// Extract `bandwidth` / `jitter` / `loss` from `iperf3 -J` output.
///
/// TCP runs report the sender side under `end.sum_sent`; UDP runs
/// (and multi-stream summaries) put everything under `end.sum`, which
/// also carries `jitter_ms` and `lost_percent`.
fn parse_iperf3_json(stdout: &str, metrics: &mut std::collections::HashMap<String, f64>) {
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
    if let Some(bps) = bps_of("sum_sent").or_else(|| bps_of("sum")) {
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

fn evaluate_assertions(
    assertions: &[BenchmarkAssertion],
    metrics: &std::collections::HashMap<String, f64>,
) -> Vec<AssertionEval> {
    assertions
        .iter()
        .map(|a| {
            let actual = metrics.get(&a.metric).copied();
            let threshold = parse_metric_value(&a.value);
            let passed = match (actual, threshold) {
                (Some(actual), Some(threshold)) => match a.op {
                    CompareOp::Gt => actual > threshold,
                    CompareOp::Lt => actual < threshold,
                    CompareOp::Gte => actual >= threshold,
                    CompareOp::Lte => actual <= threshold,
                },
                _ => false,
            };
            let op_str = match a.op {
                CompareOp::Gt => "above",
                CompareOp::Lt => "below",
                CompareOp::Gte => ">=",
                CompareOp::Lte => "<=",
            };
            AssertionEval {
                metric: a.metric.clone(),
                op: op_str.into(),
                threshold: a.value.clone(),
                actual,
                passed,
            }
        })
        .collect()
}

/// Parse a metric value string to f64.
/// Supports: "5ms" -> 5.0, "1%" -> 1.0, "900mbit" -> 900_000_000.0
fn parse_metric_value(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(v) = s.strip_suffix("ms") {
        return v.trim().parse().ok();
    }
    if let Some(v) = s.strip_suffix("us") {
        return v.trim().parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(v) = s.strip_suffix('%') {
        return v.trim().parse().ok();
    }
    if let Some(v) = s.strip_suffix("gbit") {
        return v.trim().parse::<f64>().ok().map(|v| v * 1_000_000_000.0);
    }
    if let Some(v) = s.strip_suffix("mbit") {
        return v.trim().parse::<f64>().ok().map(|v| v * 1_000_000.0);
    }
    if let Some(v) = s.strip_suffix("kbit") {
        return v.trim().parse::<f64>().ok().map(|v| v * 1_000.0);
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_metric_value() {
        assert_eq!(parse_metric_value("5ms"), Some(5.0));
        assert_eq!(parse_metric_value("100us"), Some(0.1));
        assert_eq!(parse_metric_value("1%"), Some(1.0));
        assert_eq!(parse_metric_value("900mbit"), Some(900_000_000.0));
        assert_eq!(parse_metric_value("1gbit"), Some(1_000_000_000.0));
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
        let mut metrics = std::collections::HashMap::new();
        metrics.insert("avg".into(), 10.0);
        metrics.insert("loss".into(), 0.0);

        let evals = evaluate_assertions(&assertions, &metrics);
        assert!(evals[0].passed); // 10 < 50
        assert!(evals[1].passed); // 0 < 5
    }

    #[test]
    fn test_iperf3_client_args_defaults() {
        let args = iperf3_client_args("10.0.0.2", "5", None, false);
        assert_eq!(args, vec!["-c", "10.0.0.2", "-t", "5", "-J"]);
        // A single stream is iperf3's default; don't emit a no-op -P 1.
        let args = iperf3_client_args("10.0.0.2", "5", Some(1), false);
        assert_eq!(args, vec!["-c", "10.0.0.2", "-t", "5", "-J"]);
    }

    #[test]
    fn test_iperf3_client_args_streams_and_udp() {
        let args = iperf3_client_args("10.0.0.2", "10", Some(4), true);
        assert_eq!(
            args,
            vec!["-c", "10.0.0.2", "-t", "10", "-J", "-P", "4", "-u"]
        );
    }

    #[test]
    fn test_parse_iperf3_json_tcp() {
        let json = r#"{"end":{"sum_sent":{"bits_per_second":941000000.0},"sum_received":{"bits_per_second":939000000.0}}}"#;
        let mut m = std::collections::HashMap::new();
        parse_iperf3_json(json, &mut m);
        assert_eq!(m.get("bandwidth"), Some(&941000000.0));
        assert!(!m.contains_key("jitter"));
    }

    #[test]
    fn test_parse_iperf3_json_udp() {
        let json =
            r#"{"end":{"sum":{"bits_per_second":1048576.0,"jitter_ms":0.031,"lost_percent":0.5}}}"#;
        let mut m = std::collections::HashMap::new();
        parse_iperf3_json(json, &mut m);
        assert_eq!(m.get("bandwidth"), Some(&1048576.0));
        assert_eq!(m.get("jitter"), Some(&0.031));
        assert_eq!(m.get("loss"), Some(&0.5));
    }

    #[test]
    fn test_parse_iperf3_json_garbage() {
        let mut m = std::collections::HashMap::new();
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
        let mut metrics = std::collections::HashMap::new();
        metrics.insert("avg".into(), 10.0);

        let evals = evaluate_assertions(&assertions, &metrics);
        assert!(!evals[0].passed); // 10 > 5 — fails
    }
}
