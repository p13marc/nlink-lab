//! Scenario execution engine.
//!
//! Runs timed fault-injection scenarios against a deployed lab. Each scenario
//! consists of timestamped steps with actions like `down`, `up`, `clear`,
//! `validate`, `exec`, and `log`.

use nlink::{Connection, Route};

use crate::error::{Error, Result};
use crate::running::RunningLab;
use crate::types::{EndpointRef, Scenario, ScenarioAction, ScenarioStep};

/// Result of running a scenario.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScenarioResult {
    /// Scenario name.
    pub name: String,
    /// Per-step results.
    pub steps: Vec<StepResult>,
    /// Whether all steps passed.
    pub passed: bool,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
}

/// Result of a single scenario step.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepResult {
    /// Time offset from start (milliseconds).
    pub time_ms: u64,
    /// Per-action results.
    pub actions: Vec<ActionResult>,
}

/// Result of a single action.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActionResult {
    /// Action description.
    pub description: String,
    /// Whether the action succeeded.
    pub ok: bool,
    /// Detail message.
    pub detail: Option<String>,
}

/// Run a scenario against a deployed lab.
pub async fn run_scenario(lab: &RunningLab, scenario: &Scenario) -> Result<ScenarioResult> {
    let start = std::time::Instant::now();
    let mut step_results = Vec::new();
    let mut all_passed = true;

    for step in &scenario.steps {
        // Wait until the step's time offset
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if step.time_ms > elapsed_ms {
            tokio::time::sleep(std::time::Duration::from_millis(step.time_ms - elapsed_ms)).await;
        }

        let step_result = execute_step(lab, step).await?;
        if step_result.actions.iter().any(|a| !a.ok) {
            all_passed = false;
        }
        step_results.push(step_result);
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(ScenarioResult {
        name: scenario.name.clone(),
        steps: step_results,
        passed: all_passed,
        duration_ms,
    })
}

async fn execute_step(lab: &RunningLab, step: &ScenarioStep) -> Result<StepResult> {
    let mut action_results = Vec::new();

    for action in &step.actions {
        let result = execute_action(lab, action).await;
        action_results.push(result);
    }

    Ok(StepResult {
        time_ms: step.time_ms,
        actions: action_results,
    })
}

async fn execute_action(lab: &RunningLab, action: &ScenarioAction) -> ActionResult {
    match action {
        ScenarioAction::Down(endpoint) => {
            let desc = format!("down {endpoint}");
            match set_link_state(lab, endpoint, false).await {
                Ok(()) => ActionResult {
                    description: desc,
                    ok: true,
                    detail: None,
                },
                Err(e) => ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some(e.to_string()),
                },
            }
        }
        ScenarioAction::Up(endpoint) => {
            let desc = format!("up {endpoint}");
            match set_link_state(lab, endpoint, true).await {
                Ok(()) => ActionResult {
                    description: desc,
                    ok: true,
                    detail: None,
                },
                Err(e) => ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some(e.to_string()),
                },
            }
        }
        ScenarioAction::Clear(endpoint) => {
            let desc = format!("clear {endpoint}");
            match clear_impairment(lab, endpoint).await {
                Ok(()) => ActionResult {
                    description: desc,
                    ok: true,
                    detail: None,
                },
                Err(e) => ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some(e.to_string()),
                },
            }
        }
        ScenarioAction::Validate(assertions) => {
            let mut all_ok = true;
            let mut details = Vec::new();
            // Use the test runner's assertion evaluator with the shared
            // node → IP lookup (links, bridge networks, dummies, WG, …).
            let ip_map = build_ip_map(lab.topology());
            for assertion in assertions {
                let (d, passed, detail) =
                    crate::test_runner::eval_assertion_pub(lab, assertion, &ip_map);
                if !passed {
                    all_ok = false;
                    details.push(format!(
                        "FAIL: {d}{}",
                        detail.map(|d| format!(": {d}")).unwrap_or_default()
                    ));
                } else {
                    details.push(format!("PASS: {d}"));
                }
            }
            let detail = if details.is_empty() {
                None
            } else {
                Some(details.join("; "))
            };
            ActionResult {
                description: "validate".into(),
                ok: all_ok,
                detail,
            }
        }
        ScenarioAction::Exec { node, cmd } => {
            let desc = format!("exec {node} {:?}", cmd);
            if cmd.is_empty() {
                return ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some("empty command".into()),
                };
            }
            let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
            match lab.exec(node, &cmd[0], &args) {
                Ok(out) if out.exit_code == 0 => ActionResult {
                    description: desc,
                    ok: true,
                    detail: None,
                },
                Ok(out) => ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some(format!(
                        "exit code {}: {}",
                        out.exit_code,
                        out.stderr.trim()
                    )),
                },
                Err(e) => ActionResult {
                    description: desc,
                    ok: false,
                    detail: Some(e.to_string()),
                },
            }
        }
        ScenarioAction::Log(msg) => {
            tracing::info!("SCENARIO LOG: {msg}");
            ActionResult {
                description: format!("log \"{msg}\""),
                ok: true,
                detail: None,
            }
        }
    }
}

/// Set interface up or down via netlink.
async fn set_link_state(lab: &RunningLab, endpoint: &str, up: bool) -> Result<()> {
    let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
        endpoint: endpoint.to_string(),
    })?;
    let ns_name = lab.namespace_for(&ep.node)?;
    let conn: Connection<Route> = nlink::netlink::namespace::connection_for(ns_name)
        .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;
    if up {
        conn.set_link_up(&ep.iface)
            .await
            .map_err(|e| Error::deploy_failed(format!("failed to bring up {endpoint}: {e}")))?;
    } else {
        conn.set_link_down(&ep.iface)
            .await
            .map_err(|e| Error::deploy_failed(format!("failed to bring down {endpoint}: {e}")))?;
    }
    Ok(())
}

/// Remove all TC qdiscs from an interface (clear impairments).
async fn clear_impairment(lab: &RunningLab, endpoint: &str) -> Result<()> {
    let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
        endpoint: endpoint.to_string(),
    })?;
    let ns_name = lab.namespace_for(&ep.node)?;
    let conn: Connection<Route> = nlink::netlink::namespace::connection_for(ns_name)
        .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;
    // Delete root qdisc — this removes all child qdiscs too
    let _ = conn.del_qdisc(&ep.iface, nlink::TcHandle::ROOT).await;
    Ok(())
}

/// Node → first-IP lookup, kept as a re-export for callers that reached
/// it through this module. The canonical implementation (and its
/// tests) live in [`crate::ipmap`].
pub use crate::ipmap::build_ip_map;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scenario_result_types() {
        let result = ScenarioResult {
            name: "test".into(),
            steps: vec![StepResult {
                time_ms: 0,
                actions: vec![
                    ActionResult {
                        description: "down a:eth0".into(),
                        ok: true,
                        detail: None,
                    },
                    ActionResult {
                        description: "validate".into(),
                        ok: false,
                        detail: Some("FAIL: reach a b".into()),
                    },
                ],
            }],
            passed: false,
            duration_ms: 5000,
        };
        assert!(!result.passed);
        assert_eq!(result.steps[0].actions.len(), 2);
        assert!(result.steps[0].actions[0].ok);
        assert!(!result.steps[0].actions[1].ok);
    }
}
