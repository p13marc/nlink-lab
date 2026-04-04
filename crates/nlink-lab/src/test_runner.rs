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
}

/// Result of a single assertion.
#[derive(Debug, Clone, serde::Serialize)]
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
    let file = path.display().to_string();
    let total_start = Instant::now();

    // Parse
    let topology = crate::parser::parse_file(path)?;
    topology.validate().bail()?;

    // Deploy
    let deploy_start = Instant::now();
    let lab = topology.deploy().await?;
    let deploy_ms = deploy_start.elapsed().as_millis() as u64;

    // Run assertions
    let assertions = run_assertions_with_results(&lab, &topology);

    let passed = assertions.iter().all(|a| a.passed);

    // Destroy
    lab.destroy().await?;

    let total_ms = total_start.elapsed().as_millis() as u64;

    Ok(TestResult {
        file,
        assertions,
        deploy_ms,
        total_ms,
        passed,
    })
}

/// Run assertions and return structured results.
fn run_assertions_with_results(
    lab: &crate::running::RunningLab,
    topology: &crate::types::Topology,
) -> Vec<AssertionResult> {
    use std::collections::HashMap;

    // Build IP map
    let mut ip_map: HashMap<String, String> = HashMap::new();
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for (ep, addr) in link.endpoints.iter().zip(addrs.iter()) {
                if let Some(ep_ref) = crate::types::EndpointRef::parse(ep) {
                    let ip = addr.split('/').next().unwrap_or(addr);
                    ip_map
                        .entry(ep_ref.node.clone())
                        .or_insert_with(|| ip.to_string());
                }
            }
        }
    }

    let mut results = Vec::new();

    for assertion in &topology.assertions {
        let start = Instant::now();
        let (desc, passed, detail) = eval_assertion(lab, assertion, &ip_map);
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
    ip_map: &std::collections::HashMap<String, String>,
) -> (String, bool, Option<String>) {
    eval_assertion(lab, assertion, ip_map)
}

fn eval_assertion(
    lab: &crate::running::RunningLab,
    assertion: &Assertion,
    ip_map: &std::collections::HashMap<String, String>,
) -> (String, bool, Option<String>) {
    match assertion {
        Assertion::Reach { from, to } => {
            let desc = format!("reach {from} {to}");
            if let Some(ip) = ip_map.get(to) {
                match lab.exec(from, "ping", &["-c1", "-W2", ip]) {
                    Ok(out) if out.exit_code == 0 => (desc, true, None),
                    Ok(out) => (desc, false, Some(out.stderr)),
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
            if let Some(ip) = ip_map.get(to) {
                let t = timeout.as_deref().unwrap_or("3s");
                let secs = crate::helpers::parse_duration(t)
                    .map(|d| d.as_secs().max(1))
                    .unwrap_or(3);
                let cmd = format!("timeout {secs} bash -c 'echo > /dev/tcp/{ip}/{port}'");
                let max_attempts = retries.unwrap_or(1);
                let retry_interval = interval
                    .as_deref()
                    .and_then(|i| crate::helpers::parse_duration(i).ok())
                    .unwrap_or(std::time::Duration::from_millis(500));

                let mut result = (desc.clone(), false, None);
                for attempt in 0..max_attempts {
                    match lab.exec(from, "bash", &["-c", &cmd]) {
                        Ok(out) if out.exit_code == 0 => {
                            result = (desc.clone(), true, None);
                            break;
                        }
                        Ok(out) => {
                            result = (
                                desc.clone(),
                                false,
                                Some(format!("exit code {}", out.exit_code)),
                            );
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
            match lab.exec(node, "ip", &["route", "show", destination]) {
                Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
                    let line = out.stdout.trim().to_string();
                    let via_ok = via
                        .as_ref()
                        .is_none_or(|v| line.contains(&format!("via {v}")));
                    let dev_ok = dev
                        .as_ref()
                        .is_none_or(|d| line.contains(&format!("dev {d}")));
                    if via_ok && dev_ok {
                        (desc, true, Some(line))
                    } else {
                        (desc, false, Some(line))
                    }
                }
                _ => (desc, false, Some("no route found".into())),
            }
        }
        Assertion::DnsResolves {
            from,
            name,
            expected_ip,
        } => {
            let desc = format!("dns-resolves {from} {name} {expected_ip}");
            match lab.exec(from, "getent", &["hosts", name]) {
                Ok(out) if out.exit_code == 0 && out.stdout.contains(expected_ip) => {
                    (desc, true, None)
                }
                Ok(out) => (desc, false, Some(format!("got: {}", out.stdout.trim()))),
                Err(e) => (desc, false, Some(e.to_string())),
            }
        }
    }
}

/// Parse average latency from ping -q output.
fn parse_ping_avg(output: &str) -> Option<f64> {
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

// ─── Output Formatters ──────────────────────────────────────

/// Format test results as JUnit XML.
pub fn format_junit(results: &[TestResult]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");

    for result in results {
        let tests = result.assertions.len();
        let failures = result.assertions.iter().filter(|a| !a.passed).count();
        let time_secs = result.total_ms as f64 / 1000.0;

        xml.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" time=\"{time_secs:.3}\">\n",
            escape_xml(&result.file)
        ));

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
    let total: usize = results.iter().map(|r| r.assertions.len()).sum();
    out.push_str(&format!("1..{total}\n"));

    let mut n = 0;
    for result in results {
        for a in &result.assertions {
            n += 1;
            let prefix = if a.passed { "ok" } else { "not ok" };
            out.push_str(&format!(
                "{prefix} {n} - {} ({}ms)\n",
                a.description, a.duration_ms
            ));
            if !a.passed
                && let Some(detail) = &a.detail
            {
                out.push_str("  ---\n");
                out.push_str(&format!("  message: \"{detail}\"\n"));
                out.push_str("  severity: fail\n");
                out.push_str("  ...\n");
            }
        }
    }

    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }];
        let tap = format_tap(&results);
        assert!(tap.starts_with("TAP version 13\n1..2\n"));
        assert!(tap.contains("ok 1 - reach a b"));
        assert!(tap.contains("not ok 2 - no-reach a c"));
        assert!(tap.contains("message: \"host reachable\""));
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
    fn test_format_tap_empty() {
        let results = vec![TestResult {
            file: "empty.nll".into(),
            assertions: vec![],
            deploy_ms: 50,
            total_ms: 50,
            passed: true,
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
