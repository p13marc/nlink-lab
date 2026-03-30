//! Benchmark execution engine.
//!
//! Runs performance benchmarks (ping, iperf3) against a deployed lab
//! and evaluates assertions against collected metrics.

use crate::error::Result;
use crate::running::RunningLab;
use crate::types::{Benchmark, BenchmarkAssertion, BenchmarkTest, CompareOp, EndpointRef};

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
    let topology = lab.topology();

    // Build IP map
    let mut ip_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for (ep, addr) in link.endpoints.iter().zip(addrs.iter()) {
                if let Some(ep_ref) = EndpointRef::parse(ep) {
                    let ip = addr.split('/').next().unwrap_or(addr);
                    ip_map
                        .entry(ep_ref.node.clone())
                        .or_insert_with(|| ip.to_string());
                }
            }
        }
    }

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
    _streams: Option<u32>,
    _udp: bool,
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

    // Check if iperf3 is available
    if lab.exec(from, "which", &["iperf3"]).is_err() {
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
    let output = lab.exec(from, "iperf3", &["-c", target_ip, "-t", &secs_str, "-J"]);

    let mut metrics = std::collections::HashMap::new();

    if let Ok(out) = &output {
        // Parse JSON output
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&out.stdout)
            && let Some(end) = json.get("end")
        {
            if let Some(sum_sent) = end.get("sum_sent")
                && let Some(bps) = sum_sent.get("bits_per_second").and_then(|v| v.as_f64())
            {
                metrics.insert("bandwidth".into(), bps);
            }
            if let Some(sum) = end.get("sum")
                && let Some(jitter) = sum.get("jitter_ms").and_then(|v| v.as_f64())
            {
                metrics.insert("jitter".into(), jitter);
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
