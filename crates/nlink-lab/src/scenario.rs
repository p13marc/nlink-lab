//! Scenario execution engine.
//!
//! Runs timed fault-injection scenarios against a deployed lab. Each scenario
//! consists of timestamped steps with actions like `down`, `up`, `clear`,
//! `impair`, `partition`, `heal`, `validate`, `exec`, and `log`. When it
//! ends, what it changed is put back, unless it says `keep` (#326).

use std::collections::BTreeSet;

use nlink::{Connection, Route};

use crate::error::{Error, Result};
use crate::running::{RunningLab, RuntimeTc};
use crate::types::{EndpointRef, NetworkImpairment, Scenario, ScenarioAction, ScenarioStep};

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
    /// What putting the lab back did when the scenario ended: one action
    /// per kind of change undone (none with `keep`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cleanup: Vec<ActionResult>,
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

/// Run a scenario against a deployed lab. Mutable: `clear` records what it
/// cleared, as `impair --clear` does.
pub async fn run_scenario(lab: &mut RunningLab, scenario: &Scenario) -> Result<ScenarioResult> {
    run_scenario_with(lab, scenario, &crate::ops::OpCtx::default()).await
}

/// [`run_scenario`], reporting each action to `ctx` and stopping between
/// steps when it is cancelled (#236). When it ends, failed or cancelled
/// too, the lab is put back as it found it: impairments, partitions, pair
/// impairments, interfaces it took down (#326); `keep` leaves them.
pub async fn run_scenario_with(
    lab: &mut RunningLab,
    scenario: &Scenario,
    ctx: &crate::ops::OpCtx,
) -> Result<ScenarioResult> {
    ctx.set_lab(lab.name());
    ctx.started();
    let result = scenario_ctx(lab, scenario, ctx).await;
    ctx.finished(&result, false);
    result
}

async fn scenario_ctx(
    lab: &mut RunningLab,
    scenario: &Scenario,
    ctx: &crate::ops::OpCtx,
) -> Result<ScenarioResult> {
    let start = std::time::Instant::now();
    let mut undo = Undo::of(lab);
    let mut step_results = Vec::new();
    let ran = run_steps(lab, scenario, ctx, start, &mut undo, &mut step_results).await;

    let cleanup = if scenario.keep {
        Vec::new()
    } else {
        undo.put_back(lab).await
    };
    let elapsed = start.elapsed().as_millis() as u64;
    for action in &cleanup {
        // also when the scenario was cancelled: then no result carries it
        if !action.ok {
            tracing::warn!(
                "scenario '{}': {} failed: {}",
                scenario.name,
                action.description,
                action.detail.as_deref().unwrap_or_default()
            );
        }
        ctx.emit(nlink_lab_shared::api::OpPhase::ScenarioAction {
            time_ms: elapsed,
            description: action.description.clone(),
            ok: action.ok,
            detail: action.detail.clone(),
        });
    }
    ran?;

    let passed = step_results
        .iter()
        .flat_map(|s: &StepResult| &s.actions)
        .chain(&cleanup)
        .all(|a| a.ok);
    Ok(ScenarioResult {
        name: scenario.name.clone(),
        steps: step_results,
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        cleanup,
    })
}

/// The steps, each at its time; stops (with the error) when cancelled.
async fn run_steps(
    lab: &mut RunningLab,
    scenario: &Scenario,
    ctx: &crate::ops::OpCtx,
    start: std::time::Instant,
    undo: &mut Undo,
    step_results: &mut Vec<StepResult>,
) -> Result<()> {
    for step in &scenario.steps {
        // Wait until the step's time offset
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if step.time_ms > elapsed_ms {
            ctx.sleep(std::time::Duration::from_millis(step.time_ms - elapsed_ms))
                .await?;
        }
        ctx.check()?;

        let step_result = execute_step(lab, &scenario.name, step, undo).await?;
        for action in &step_result.actions {
            ctx.emit(nlink_lab_shared::api::OpPhase::ScenarioAction {
                time_ms: step.time_ms,
                description: action.description.clone(),
                ok: action.ok,
                detail: action.detail.clone(),
            });
        }
        step_results.push(step_result);
    }
    Ok(())
}

/// What a scenario changed, kept to put back when it ends (#326): the
/// runtime traffic control as it found it, each pair rule it touched as
/// it was before the first touch, and the interfaces it took down and
/// did not bring back. `exec` steps are not undone.
struct Undo {
    tc: RuntimeTc,
    /// The endpoints whose impairment or partition it changed: only they
    /// are put back, so an operator's `impair` elsewhere during the
    /// scenario stays (#332).
    touched: BTreeSet<String>,
    pairs: Vec<PairWas>,
    down: BTreeSet<String>,
}

/// A network's pair rule `src → dst` before the scenario changed it
/// (`None`: there was none).
struct PairWas {
    network: String,
    src: String,
    dst: String,
    was: Option<NetworkImpairment>,
}

impl Undo {
    fn of(lab: &RunningLab) -> Self {
        Self {
            tc: lab.runtime_tc(),
            touched: BTreeSet::new(),
            pairs: Vec::new(),
            down: BTreeSet::new(),
        }
    }

    /// Note what `action` is about to change: a pair rule (the first
    /// time), an interface it takes down (before it runs, so one that
    /// fails half way is still brought up: `up` on an up link is
    /// harmless).
    fn note(&mut self, lab: &RunningLab, action: &ScenarioAction) {
        match action {
            ScenarioAction::Down(ep) => {
                self.down.insert(ep.clone());
            }
            ScenarioAction::Clear(ep) | ScenarioAction::Impair { endpoint: ep, .. } => {
                self.touched.insert(ep.clone());
            }
            ScenarioAction::Partition { endpoint, both }
            | ScenarioAction::Heal { endpoint, both } => {
                self.touched.insert(endpoint.clone());
                if *both && let Ok(peer) = lab.peer_endpoint(endpoint) {
                    self.touched.insert(peer);
                }
            }
            ScenarioAction::PairImpair {
                network, src, dst, ..
            } if !self
                .pairs
                .iter()
                .any(|p| p.network == *network && p.src == *src && p.dst == *dst) =>
            {
                let was = lab.topology().networks.get(network).and_then(|n| {
                    n.impairments
                        .iter()
                        .find(|i| i.src == *src && i.dst == *dst)
                        .cloned()
                });
                self.pairs.push(PairWas {
                    network: network.clone(),
                    src: src.clone(),
                    dst: dst.clone(),
                    was,
                });
            }
            _ => {}
        }
    }

    /// Track what `action`, which succeeded, did: an interface it
    /// brought up needs no cleanup.
    fn done(&mut self, action: &ScenarioAction) {
        if let ScenarioAction::Up(ep) = action {
            self.down.remove(ep);
        }
    }

    /// Put the lab back: the interfaces up, the pair rules as they were
    /// (one `apply`), then the runtime traffic control. One result per
    /// kind of change that needed undoing.
    async fn put_back(self, lab: &mut RunningLab) -> Vec<ActionResult> {
        let mut out = Vec::new();
        for ep in &self.down {
            let description = format!("cleanup: up {ep}");
            out.push(match set_link_state(lab, ep, true).await {
                Ok(()) => ActionResult::ok(description),
                Err(e) => ActionResult::failed(description, e),
            });
        }

        let edits: Vec<crate::edit::Edit> = self
            .pairs
            .iter()
            .filter(|p| {
                let now = lab.topology().networks.get(&p.network).and_then(|n| {
                    n.impairments
                        .iter()
                        .find(|i| i.src == p.src && i.dst == p.dst)
                });
                now != p.was.as_ref()
            })
            .map(|p| crate::edit::Edit::SetPairImpairment {
                network: p.network.clone(),
                src: p.src.clone(),
                dst: p.dst.clone(),
                impairment: p.was.as_ref().map(|w| w.impairment.clone()),
                rate_cap: p.was.as_ref().and_then(|w| w.rate_cap.clone()),
            })
            .collect();
        if !edits.is_empty() {
            let description = format!("cleanup: {} pair impairment(s) as they were", edits.len());
            // its own context: an apply reports started/finished, which are
            // the scenario's to report
            let restored = async {
                let edited = crate::edit::edited(lab.topology(), &edits)?;
                crate::edit::apply_edited_with(lab, &edited, &crate::ops::OpCtx::default()).await
            }
            .await;
            out.push(match restored {
                Ok(_) => ActionResult::ok(description),
                Err(e) => ActionResult::failed(description, e),
            });
        }

        match lab.restore_runtime_tc(self.tc, Some(&self.touched)).await {
            Ok(changed) if changed.is_empty() => {}
            Ok(changed) => out.push(ActionResult::ok(format!(
                "cleanup: impairments and partitions as they were ({})",
                changed.join(", ")
            ))),
            Err(e) => out.push(ActionResult::failed(
                "cleanup: impairments and partitions as they were".into(),
                e,
            )),
        }
        out
    }
}

impl ActionResult {
    fn ok(description: String) -> Self {
        Self {
            description,
            ok: true,
            detail: None,
        }
    }

    fn failed(description: String, e: impl std::fmt::Display) -> Self {
        Self {
            description,
            ok: false,
            detail: Some(e.to_string()),
        }
    }
}

async fn execute_step(
    lab: &mut RunningLab,
    scenario_name: &str,
    step: &ScenarioStep,
    undo: &mut Undo,
) -> Result<StepResult> {
    let mut action_results = Vec::new();

    for action in &step.actions {
        undo.note(lab, action);
        let result = execute_action(lab, action).await;
        if result.ok {
            undo.done(action);
        }
        action_results.push(result);
    }

    crate::events::record(
        lab.name(),
        crate::events::LifecycleKind::ScenarioStep {
            scenario: scenario_name.to_string(),
            time_ms: step.time_ms,
            ok: action_results.iter().all(|a| a.ok),
            actions: action_results.len(),
        },
    );

    Ok(StepResult {
        time_ms: step.time_ms,
        actions: action_results,
    })
}

async fn execute_action(lab: &mut RunningLab, action: &ScenarioAction) -> ActionResult {
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
            match lab.clear_impairment(endpoint).await {
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
        ScenarioAction::Impair {
            endpoint,
            impairment,
        } => {
            let desc = format!("impair {endpoint} {}", impairment_summary(impairment));
            match lab.set_impairment(endpoint, impairment).await {
                Ok(()) => ActionResult::ok(desc),
                Err(e) => ActionResult::failed(desc, e),
            }
        }
        ScenarioAction::Partition { endpoint, both } => {
            let desc = format!("partition {endpoint}{}", if *both { " both" } else { "" });
            match ends(lab, endpoint, *both) {
                Ok(eps) => {
                    for ep in &eps {
                        if let Err(e) = lab.partition(ep).await {
                            return ActionResult::failed(desc, format!("{ep}: {e}"));
                        }
                    }
                    ActionResult::ok(desc)
                }
                Err(e) => ActionResult::failed(desc, e),
            }
        }
        ScenarioAction::Heal { endpoint, both } => {
            let desc = format!("heal {endpoint}{}", if *both { " both" } else { "" });
            match ends(lab, endpoint, *both) {
                Ok(eps) => {
                    // healing what is not partitioned is fine here: the
                    // scenario says how the link should end up
                    let cut: Vec<String> = eps
                        .into_iter()
                        .filter(|ep| lab.is_partitioned(ep))
                        .collect();
                    for ep in &cut {
                        if let Err(e) = lab.heal(ep).await {
                            return ActionResult::failed(desc, format!("{ep}: {e}"));
                        }
                    }
                    ActionResult::ok(desc)
                }
                Err(e) => ActionResult::failed(desc, e),
            }
        }
        ScenarioAction::PairImpair {
            network,
            src,
            dst,
            impairment,
            rate_cap,
        } => {
            let desc = format!(
                "impair {network} {src} -- {dst} {}",
                impairment_summary(impairment)
            );
            let edit = crate::edit::Edit::SetPairImpairment {
                network: network.clone(),
                src: src.clone(),
                dst: dst.clone(),
                impairment: Some(impairment.clone()),
                rate_cap: rate_cap.clone(),
            };
            let applied = async {
                let edited = crate::edit::edited(lab.topology(), &[edit])?;
                crate::edit::apply_edited_with(lab, &edited, &crate::ops::OpCtx::default()).await
            }
            .await;
            match applied {
                Ok(_) => ActionResult::ok(desc),
                Err(e) => ActionResult::failed(desc, e),
            }
        }
    }
}

/// `endpoint`, and with `both` the other end of its link.
fn ends(lab: &RunningLab, endpoint: &str, both: bool) -> Result<Vec<String>> {
    let mut eps = vec![endpoint.to_string()];
    if both {
        eps.push(lab.peer_endpoint(endpoint)?);
    }
    Ok(eps)
}

/// An impairment's properties in one line (`delay 50ms loss 1%`), for the
/// report.
fn impairment_summary(imp: &crate::types::Impairment) -> String {
    crate::render::impairment_props(imp).unwrap_or_else(|_| format!("{imp:?}"))
}

/// Set interface up or down via netlink.
async fn set_link_state(lab: &RunningLab, endpoint: &str, up: bool) -> Result<()> {
    let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
        endpoint: endpoint.to_string(),
    })?;
    // a change to the lab, if not to its state: one at a time (#332)
    let _lock = crate::state::lock_wait(
        lab.name(),
        if up { "link-up" } else { "link-down" },
        crate::state::lock_timeout(),
    )
    .await?;
    // a container node too: `namespace_for` knew namespace nodes only
    let conn: Connection<Route> = lab
        .ns_resolver_of(&ep.node)
        .ok_or_else(|| Error::NodeNotFound {
            name: ep.node.clone(),
        })?
        .connection()
        .map_err(|e| Error::deploy_failed(format!("connection for '{}': {e}", ep.node)))?;
    if up {
        conn.set_link_up(&ep.iface)
            .await
            .map_err(|e| Error::nlink(format!("failed to bring up {endpoint}"), e))?;
    } else {
        conn.set_link_down(&ep.iface)
            .await
            .map_err(|e| Error::nlink(format!("failed to bring down {endpoint}"), e))?;
    }
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
            cleanup: Vec::new(),
        };
        assert!(!result.passed);
        assert_eq!(result.steps[0].actions.len(), 2);
        assert!(result.steps[0].actions[0].ok);
        assert!(!result.steps[0].actions[1].ok);
    }
}
