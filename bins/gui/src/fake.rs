//! The demo's labs (`--demo`): made up, plausibly. Two labs from the
//! built-in templates, with metrics that rise and fall and now and then
//! an error, a live impairment, a partition, processes and events — the
//! live app without a daemon, and what the tests draw.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iced::futures::stream::{self, BoxStream, StreamExt};
use nlink_lab::Topology;
use nlink_lab_shared::api::{
    EndpointImpairment, ImpairmentSpec, LabSummary, Notification, OpPhase, ProcessDto, Request,
    Response, Role, SnapshotDto, Topic,
};
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, NodeMetrics};

use crate::backend::{Feed, SessionItem};
use crate::model::Who;

#[derive(Clone)]
struct Lab {
    name: String,
    topology: Topology,
    endpoints: Vec<EndpointImpairment>,
    processes: Vec<ProcessDto>,
    events: Vec<serde_json::Value>,
    snapshots: Vec<SnapshotDto>,
}

/// The demo's labs, changed by what the user does to them.
pub struct Fake {
    labs: Mutex<Vec<Lab>>,
    start: Instant,
}

/// The demo's labs: a template each, under a name.
pub const LABS: &[(&str, &str)] = &[
    ("demo-fabric", "spine-leaf"),
    ("demo-wan", "wan"),
    ("demo-campus", "vlan-trunk"),
];

/// An assertion, said as in a `validate` block.
fn assertion_text(a: &nlink_lab::types::Assertion) -> String {
    use nlink_lab::types::Assertion;
    match a {
        Assertion::Reach { from, to } => format!("reach {from} {to}"),
        Assertion::NoReach { from, to } => format!("no-reach {from} {to}"),
        other => format!("{other:?}"),
    }
}

/// A scenario's action, said as in its `at` block.
fn action_text(a: &nlink_lab::types::ScenarioAction) -> String {
    use nlink_lab::types::ScenarioAction;
    match a {
        ScenarioAction::Down(ep) => format!("down {ep}"),
        ScenarioAction::Up(ep) => format!("up {ep}"),
        ScenarioAction::Clear(ep) => format!("clear {ep}"),
        ScenarioAction::Validate(v) => format!(
            "validate {}",
            v.iter().map(assertion_text).collect::<Vec<_>>().join(", ")
        ),
        ScenarioAction::Exec { node, cmd } => format!("exec {node} {}", cmd.join(" ")),
        ScenarioAction::Log(m) => format!("log {m}"),
        ScenarioAction::Impair { endpoint, .. } => format!("impair {endpoint}"),
        ScenarioAction::Partition { endpoint, both } => {
            format!("partition {endpoint}{}", if *both { " both" } else { "" })
        }
        ScenarioAction::Heal { endpoint, both } => {
            format!("heal {endpoint}{}", if *both { " both" } else { "" })
        }
        ScenarioAction::PairImpair {
            network, src, dst, ..
        } => format!("impair {network} {src} -- {dst}"),
        ScenarioAction::Replay { target, file, .. } => format!("replay {target} {file}"),
    }
}

/// What the WAN demo can run: an assertion, a scenario.
const WAN_RUNS: &str = r#"
validate { reach host-a host-b }
scenario "wan-flap" {
  at 0s { validate { reach host-a host-b } }
  at 2s { down router-a:wan0 }
  at 4s { validate { no-reach host-a host-b } }
  at 6s { up router-a:wan0 }
}
"#;

impl Default for Fake {
    fn default() -> Self {
        Self::new()
    }
}

impl Fake {
    pub fn new() -> Self {
        let labs = LABS
            .iter()
            .filter_map(|(name, template)| {
                let t = nlink_lab::templates::get(template)?;
                let mut text = nlink_lab::templates::render(t, Some(name));
                if *template == "wan" {
                    text.push_str(WAN_RUNS);
                }
                let topology = nlink_lab::parser::parse(&text).ok()?;
                Some(new_lab(name, topology))
            })
            .collect();
        Self {
            labs: Mutex::new(labs),
            start: Instant::now(),
        }
    }

    fn labs(&self) -> std::sync::MutexGuard<'_, Vec<Lab>> {
        self.labs.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn who(&self) -> Who {
        Who {
            server: format!("nlink-lab-gui {} (demo)", env!("CARGO_PKG_VERSION")),
            host: "demo".into(),
            user: "you".into(),
            // the demo is yours to change
            role: Role::Operator,
        }
    }

    fn lab(&self, name: &str) -> Result<Lab, String> {
        self.labs()
            .iter()
            .find(|l| l.name == name)
            .cloned()
            .ok_or_else(|| format!("lab '{name}' not found"))
    }

    /// Change lab `name` with `f`, recording `event`.
    fn change(
        &self,
        name: &str,
        event: serde_json::Value,
        f: impl FnOnce(&mut Lab),
    ) -> Result<Response, String> {
        let mut labs = self.labs();
        let l = labs
            .iter_mut()
            .find(|l| l.name == name)
            .ok_or_else(|| format!("lab '{name}' not found"))?;
        f(l);
        if let Some(o) = event.as_object() {
            let mut v = serde_json::json!({"ts": "2026-10-18T10:00:00Z", "lab": name});
            if let Some(m) = v.as_object_mut() {
                m.extend(o.clone());
            }
            l.events.push(v);
        }
        Ok(Response::Ok)
    }

    /// The endpoint's entry, made when there is none.
    fn endpoint<'a>(l: &'a mut Lab, endpoint: &str) -> &'a mut EndpointImpairment {
        if !l.endpoints.iter().any(|e| e.endpoint == endpoint) {
            l.endpoints.push(EndpointImpairment {
                endpoint: endpoint.to_string(),
                declared: None,
                live: None,
                partitioned: false,
            });
        }
        l.endpoints
            .iter_mut()
            .find(|e| e.endpoint == endpoint)
            .expect("just made")
    }

    pub fn summaries(&self) -> Vec<LabSummary> {
        self.labs()
            .iter()
            .map(|l| LabSummary {
                lab: l.name.clone(),
                nodes: l.topology.nodes.len(),
                containers: l
                    .topology
                    .nodes
                    .values()
                    .filter(|n| n.image.is_some())
                    .count(),
                links: l.topology.links.len(),
                created_at: "2026-10-18T09:00:00Z".into(),
                health: "running".into(),
                impaired: l.endpoints.iter().filter(|e| e.live.is_some()).count(),
                partitioned: l.endpoints.iter().filter(|e| e.partitioned).count(),
                processes: l.processes.len(),
                revision: revision(&l.name),
            })
            .collect()
    }

    pub fn call(&self, request: Request) -> Result<Response, String> {
        Ok(match request {
            Request::Ping => Response::Pong,
            Request::ListLabs => Response::Labs {
                labs: self.summaries(),
            },
            Request::GetTopology { lab, .. } => {
                let l = self.lab(&lab)?;
                Response::Topology {
                    lab: lab.clone(),
                    revision: revision(&lab),
                    topology: serde_json::to_value(&l.topology).map_err(|e| e.to_string())?,
                }
            }
            Request::GetImpairments { lab } => Response::Impairments {
                endpoints: self.lab(&lab)?.endpoints.clone(),
            },
            Request::ListProcesses { lab } => Response::Processes {
                processes: self.lab(&lab)?.processes.clone(),
            },
            Request::ReadEvents { lab, .. } => {
                let events = self.lab(&lab)?.events.clone();
                Response::Events {
                    next: events.len() as u64,
                    events,
                }
            }
            Request::GetMetrics { lab } => Response::Metrics {
                snapshot: self.metrics(&lab, self.start.elapsed().as_secs_f64())?,
            },
            Request::ListOps => Response::Ops { ops: Vec::new() },
            Request::PlanDeploy { topology } => {
                let t: Topology = serde_json::from_value(topology).map_err(|e| e.to_string())?;
                // as the daemon says it: the plan's ops, with their stage
                let mut ops: Vec<serde_json::Value> = t
                    .nodes
                    .keys()
                    .map(|n| serde_json::json!({"stage": "namespaces", "op": format!("create namespace {n}")}))
                    .collect();
                ops.extend(t.links.iter().map(|l| {
                    serde_json::json!({"stage": "links",
                        "op": format!("create veth {} -- {}", l.endpoints[0], l.endpoints[1])})
                }));
                Response::Report {
                    kind: "plan".into(),
                    report: serde_json::Value::Array(ops),
                }
            }
            Request::CheckApply { lab, topology, .. } => {
                let now = self.lab(&lab)?.topology;
                let t: Topology = serde_json::from_value(topology).map_err(|e| e.to_string())?;
                let added: Vec<&String> = t
                    .nodes
                    .keys()
                    .filter(|n| !now.nodes.contains_key(*n))
                    .collect();
                let removed: Vec<&String> = now
                    .nodes
                    .keys()
                    .filter(|n| !t.nodes.contains_key(*n))
                    .collect();
                Response::Report {
                    kind: "dry_run".into(),
                    report: serde_json::json!({
                        "lab": lab, "no_op": added.is_empty() && removed.is_empty(),
                        "change_count": added.len() + removed.len(),
                        "topology": {"nodes_added": added, "nodes_removed": removed},
                    }),
                }
            }
            Request::ListSnapshots { lab } => Response::Snapshots {
                snapshots: self.lab(&lab)?.snapshots,
            },
            Request::Impair {
                lab,
                endpoint,
                spec,
                ..
            } => {
                let event = serde_json::json!({"event": "impaired", "endpoint": endpoint,
                    "impairment": serde_json::to_value(&spec).unwrap_or_default()});
                return self.change(&lab, event, |l| {
                    Self::endpoint(l, &endpoint).live = Some(spec);
                });
            }
            Request::ClearImpairment { lab, endpoint } => {
                let event = serde_json::json!({"event": "impair_cleared", "endpoint": endpoint});
                // as the daemon does: an empty live impairment, so the
                // declared one does not come back
                return self.change(&lab, event, |l| {
                    Self::endpoint(l, &endpoint).live = Some(ImpairmentSpec::default());
                });
            }
            Request::Partition { lab, endpoint } => {
                let event = serde_json::json!({"event": "partitioned", "endpoint": endpoint});
                return self.change(&lab, event, |l| {
                    Self::endpoint(l, &endpoint).partitioned = true;
                });
            }
            Request::Heal { lab, endpoint } => {
                let event = serde_json::json!({"event": "healed", "endpoint": endpoint});
                return self.change(&lab, event, |l| {
                    Self::endpoint(l, &endpoint).partitioned = false;
                });
            }
            Request::SetPairImpairment { lab, .. } => {
                return self.change(&lab, serde_json::Value::Null, |_| {});
            }
            Request::Snapshot {
                lab,
                name,
                description,
            } => {
                let event = serde_json::json!({"event": "snapshot_taken", "name": name});
                return self.change(&lab, event, |l| {
                    l.snapshots.retain(|s| s.name != name);
                    l.snapshots.push(SnapshotDto {
                        name,
                        created_at: "2026-10-18T10:00:00Z".into(),
                        description,
                        nodes: l.topology.nodes.len(),
                        links: l.topology.links.len(),
                    });
                });
            }
            Request::DeleteSnapshot { lab, name } => {
                return self.change(&lab, serde_json::Value::Null, |l| {
                    l.snapshots.retain(|s| s.name != name);
                });
            }
            Request::Restore {
                lab,
                name,
                dry_run: true,
            } => {
                let l = self.lab(&lab)?;
                if !l.snapshots.iter().any(|s| s.name == name) {
                    return Err(format!("snapshot '{name}' not found"));
                }
                let changed: Vec<&str> = l
                    .endpoints
                    .iter()
                    .filter(|e| e.live.is_some() || e.partitioned)
                    .map(|e| e.endpoint.as_str())
                    .collect();
                Response::Report {
                    kind: "dry_run".into(),
                    report: serde_json::json!({
                        "schema_version": 3, "lab": lab, "no_op": changed.is_empty(),
                        "change_count": changed.len(), "topology": {"impairments": changed},
                    }),
                }
            }
            Request::Restore { lab, name, .. } => {
                let event = serde_json::json!({"event": "restored", "name": name});
                return self.change(&lab, event, |l| {
                    for e in &mut l.endpoints {
                        e.live = None;
                        e.partitioned = false;
                    }
                });
            }
            Request::Destroy { lab } => {
                let mut labs = self.labs();
                let before = labs.len();
                labs.retain(|l| l.name != lab);
                if labs.len() == before {
                    return Err(format!("lab '{lab}' not found"));
                }
                Response::Ok
            }
            // nothing to restart in a made-up lab
            Request::Restart { lab, .. } => {
                return self.change(&lab, serde_json::Value::Null, |_| {});
            }
            Request::Spawn {
                lab, node, argv, ..
            } => {
                let mut labs = self.labs();
                let l = labs
                    .iter_mut()
                    .find(|l| l.name == lab)
                    .ok_or_else(|| format!("lab '{lab}' not found"))?;
                let pid = 4100 + l.processes.len() as u32;
                l.processes.push(ProcessDto {
                    node: node.clone(),
                    pid,
                    alive: true,
                    ..Default::default()
                });
                l.events
                    .push(serde_json::json!({"ts": "2026-10-18T10:00:00Z", "lab": lab,
                    "event": "spawned", "node": node, "pid": pid, "cmd": argv.join(" ")}));
                Response::Spawned {
                    pid,
                    stdout: None,
                    stderr: None,
                }
            }
            Request::Kill { lab, pid, .. } => {
                let event = serde_json::json!({"event": "killed", "pid": pid});
                return self.change(&lab, event, |l| {
                    for p in l.processes.iter_mut().filter(|p| p.pid == pid) {
                        p.alive = false;
                        p.exit_code = Some(143);
                    }
                });
            }
            other => {
                return Err(format!(
                    "the demo does not do {}",
                    serde_json::to_value(&other)
                        .ok()
                        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
                        .unwrap_or_default()
                ));
            }
        })
    }

    /// The lab's metrics `t` seconds into the demo: every interface busy
    /// in its own rhythm, a partitioned one silent, one interface's errors
    /// growing every 20 s.
    pub fn metrics(&self, name: &str, t: f64) -> Result<MetricsSnapshot, String> {
        let l = self.lab(name)?;
        let mut nodes: HashMap<String, NodeMetrics> = HashMap::new();
        let partitioned: Vec<&str> = l
            .endpoints
            .iter()
            .filter(|e| e.partitioned)
            .map(|e| e.endpoint.as_str())
            .collect();
        for (i, ep) in endpoints(&l.topology).iter().enumerate() {
            let Some((node, iface)) = ep.split_once(':') else {
                continue;
            };
            let phase = i as f64 * 0.9;
            let base = 4.0e6 * (1.0 + (i % 3) as f64);
            let wave =
                |shift: f64| (base * (1.15 + (t * 0.5 + phase + shift).sin())).max(0.0) as u64;
            let silent = partitioned.contains(&ep.as_str());
            let (rx, tx) = if silent {
                (0, 0)
            } else {
                (wave(0.0), wave(1.3))
            };
            let errors = if i == 1 { (t / 20.0) as u64 } else { 0 };
            nodes
                .entry(node.to_string())
                .or_insert_with(|| NodeMetrics {
                    interfaces: Vec::new(),
                    issues: Vec::new(),
                    sockets: Vec::new(),
                });
            let m = nodes.get_mut(node).expect("just put");
            m.interfaces.push(InterfaceMetrics {
                name: iface.to_string(),
                state: "up".into(),
                rx_bps: rx,
                tx_bps: tx,
                rx_pps: rx / 8000,
                tx_pps: tx / 8000,
                rx_bytes: (t * rx as f64 / 8.0) as u64,
                tx_bytes: (t * tx as f64 / 8.0) as u64,
                rx_errors: errors,
                ..Default::default()
            });
        }
        Ok(MetricsSnapshot {
            wire_version: 1,
            lab_name: name.to_string(),
            timestamp: t as u64,
            nodes,
        })
    }

    /// A session in the demo: a command says what it would print, a log
    /// grows until stopped, a capture is refused.
    pub fn session(
        self: &std::sync::Arc<Self>,
        request: Request,
        cancel: nlink_lab::CancelToken,
    ) -> BoxStream<'static, SessionItem> {
        use nlink_lab_shared::api::DataStream;
        match request {
            Request::Exec { node, argv, .. } => {
                let text = format!(
                    "the demo runs nothing: `{}` on {node} would print here\n",
                    argv.join(" ")
                );
                stream::iter([
                    SessionItem::Data(DataStream::Stdout, text.into_bytes()),
                    SessionItem::End(Ok(Response::Exited {
                        code: Some(0),
                        signal: None,
                        duration_ms: 1,
                        timed_out: false,
                    })),
                ])
                .boxed()
            }
            Request::Logs { pid, .. } => stream::unfold(0u32, move |i| {
                let cancel = cancel.clone();
                async move {
                    if i > 0 {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                return (i < u32::MAX).then_some((
                                    SessionItem::End(Err("stopped".into())),
                                    u32::MAX,
                                ));
                            }
                            () = tokio::time::sleep(Duration::from_millis(700)) => {}
                        }
                    }
                    if i == u32::MAX {
                        return None;
                    }
                    let line = format!("[demo {pid}] line {i}: all is well\n");
                    Some((
                        SessionItem::Data(DataStream::Stdout, line.into_bytes()),
                        i + 1,
                    ))
                }
            })
            .boxed(),
            Request::Deploy {
                topology, options, ..
            } => {
                let t: Topology = match serde_json::from_value(topology) {
                    Ok(t) => t,
                    Err(e) => return stream::iter([SessionItem::End(Err(e.to_string()))]).boxed(),
                };
                let name = crate::model::deployed_name(&t.lab.name, options.suffix.as_deref());
                if self.lab(&name).is_ok() && !options.force {
                    return stream::iter([SessionItem::End(Err(format!(
                        "lab '{name}' already exists"
                    )))])
                    .boxed();
                }
                let stages = ["namespaces", "links", "addresses", "processes"];
                let fake = self.clone();
                let lab = name.clone();
                self.op(&name, "deploy", &stages, vec![], cancel, move || {
                    let mut labs = fake.labs();
                    labs.retain(|l| l.name != lab);
                    labs.push(new_lab(&lab, t));
                    Ok(Response::Done {
                        op: "demo-op".into(),
                        lab,
                        report: serde_json::Value::Null,
                    })
                })
            }
            Request::Apply { lab, topology, .. } => {
                let t: Topology = match serde_json::from_value(topology) {
                    Ok(t) => t,
                    Err(e) => return stream::iter([SessionItem::End(Err(e.to_string()))]).boxed(),
                };
                let fake = self.clone();
                let name = lab.clone();
                self.op(
                    &lab,
                    "apply",
                    &["links", "addresses"],
                    vec![],
                    cancel,
                    move || {
                        let mut labs = fake.labs();
                        let l = labs
                            .iter_mut()
                            .find(|l| l.name == name)
                            .ok_or_else(|| format!("lab '{name}' not found"))?;
                        l.topology = t;
                        Ok(Response::Done {
                            op: "demo-op".into(),
                            lab: name,
                            report: serde_json::Value::Null,
                        })
                    },
                )
            }
            Request::RunScenario { lab, name } | Request::RunBenchmark { lab, name } => {
                // a scenario's actions, each said when it happens
                let lines = self
                    .lab(&lab)
                    .ok()
                    .and_then(|l| l.topology.scenarios.into_iter().find(|s| s.name == name))
                    .map(|s| {
                        s.steps
                            .iter()
                            .flat_map(|step| {
                                step.actions.iter().map(|a| OpPhase::ScenarioAction {
                                    time_ms: step.time_ms,
                                    description: action_text(a),
                                    ok: true,
                                    detail: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let what = name.clone();
                self.op(&lab, "scenario", &["steps"], lines, cancel, move || {
                    Ok(Response::Report {
                        kind: "scenario".into(),
                        report: serde_json::json!({"name": what, "passed": true}),
                    })
                })
            }
            Request::RunAssertions { lab } => {
                let assertions = self
                    .lab(&lab)
                    .map(|l| l.topology.assertions)
                    .unwrap_or_default();
                let lines = assertions
                    .iter()
                    .map(|a| OpPhase::Assertion {
                        description: assertion_text(a),
                        passed: true,
                        detail: None,
                    })
                    .collect();
                let n = assertions.len();
                self.op(
                    &lab,
                    "assertions",
                    &["assertions"],
                    lines,
                    cancel,
                    move || {
                        Ok(Response::Report {
                            kind: "assertions".into(),
                            report: serde_json::json!({"passed": n, "failed": 0}),
                        })
                    },
                )
            }
            _ => stream::iter([SessionItem::End(Err("the demo does not do that".into()))]).boxed(),
        }
    }

    /// A made-up operation: its stages, a step each, a pause between them,
    /// then `done`; cancelled, it says so and does nothing.
    fn op(
        self: &std::sync::Arc<Self>,
        lab: &str,
        kind: &str,
        stages: &[&str],
        lines: Vec<nlink_lab_shared::api::OpPhase>,
        cancel: nlink_lab::CancelToken,
        done: impl FnOnce() -> Result<Response, String> + Send + 'static,
    ) -> BoxStream<'static, SessionItem> {
        use nlink_lab_shared::api::{OpEvent, OpOutcome};
        let ev = |seq: u64, phase: OpPhase| {
            SessionItem::Op(OpEvent {
                op: "demo-op".into(),
                lab: lab.to_string(),
                seq,
                ts_ms: 0,
                phase,
            })
        };
        let n = stages.len();
        let mut items = vec![ev(
            0,
            OpPhase::Started {
                kind: kind.to_string(),
                actor: Some("you".into()),
            },
        )];
        for (i, stage) in stages.iter().enumerate() {
            items.push(ev(
                1 + 2 * i as u64,
                OpPhase::Stage {
                    stage: stage.to_string(),
                    index: i + 1,
                    of: n,
                    ops: 1,
                },
            ));
            items.push(ev(
                2 + 2 * i as u64,
                OpPhase::Step {
                    done: i + 1,
                    total: n,
                    describe: format!("{stage} done"),
                },
            ));
        }
        for (i, phase) in lines.into_iter().enumerate() {
            items.push(ev(50 + i as u64, phase));
        }
        let finished = ev(
            99,
            OpPhase::Finished {
                outcome: OpOutcome::Succeeded,
                elapsed_ms: 150 * n as u64,
            },
        );
        let cancelled = ev(
            99,
            OpPhase::Finished {
                outcome: OpOutcome::Cancelled,
                elapsed_ms: 0,
            },
        );
        let state = (
            items.into_iter(),
            Some(done),
            Some(finished),
            cancelled,
            false,
        );
        stream::unfold(
            state,
            move |(mut items, mut done, mut finished, cancelled, over)| {
                let cancel = cancel.clone();
                async move {
                    if over {
                        return None;
                    }
                    if cancel.is_cancelled() {
                        return Some((
                            vec![cancelled.clone(), SessionItem::End(Err("cancelled".into()))],
                            (items, None, None, cancelled, true),
                        ));
                    }
                    if let Some(item) = items.next() {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        return Some((vec![item], (items, done, finished, cancelled, false)));
                    }
                    let end = done.take().map(|f| f()).unwrap_or(Ok(Response::Ok));
                    let mut out = Vec::new();
                    if end.is_ok() {
                        out.extend(finished.take());
                    }
                    out.push(SessionItem::End(end));
                    Some((out, (items, None, None, cancelled, true)))
                }
            },
        )
        .flat_map(stream::iter)
        .boxed()
    }

    pub fn subscribe(
        self: &std::sync::Arc<Self>,
        lab: Option<String>,
        topics: Vec<Topic>,
        interval_ms: u64,
    ) -> BoxStream<'static, Result<Feed, String>> {
        let Some(lab) = lab.filter(|_| topics.contains(&Topic::Metrics)) else {
            // the demo's labs never change
            return stream::pending().boxed();
        };
        let fake = self.clone();
        let every = Duration::from_millis(interval_ms.max(250));
        stream::unfold(true, move |first| {
            let (fake, lab) = (fake.clone(), lab.clone());
            async move {
                if !first {
                    tokio::time::sleep(every).await;
                }
                let t = fake.start.elapsed().as_secs_f64();
                let item = fake
                    .metrics(&lab, t)
                    .map(|snapshot| Feed::note(Notification::Metrics { lab, snapshot }));
                Some((item, false))
            }
        })
        .boxed()
    }
}

/// A lab's revision, shaped like the daemon's (hex).
pub fn revision(lab: &str) -> String {
    let h = lab.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}{:016x}", h.rotate_left(17))
}

/// Every interface the topology wires: its links' and its networks' ends.
fn endpoints(t: &Topology) -> Vec<String> {
    let mut out: Vec<String> = t.links.iter().flat_map(|l| l.endpoints.clone()).collect();
    out.extend(t.networks.values().flat_map(|n| n.members.clone()));
    out
}

fn new_lab(name: &str, topology: Topology) -> Lab {
    let eps = endpoints(&topology);
    let mut endpoints: Vec<EndpointImpairment> = topology
        .impairments
        .iter()
        .map(|(ep, imp)| EndpointImpairment {
            endpoint: ep.clone(),
            declared: Some(spec_of(imp)),
            live: None,
            partitioned: false,
        })
        .collect();
    // something set live on the first interface, the last one cut off
    if let Some(first) = eps.first() {
        let live = ImpairmentSpec {
            delay: Some("80ms".into()),
            loss: Some("2%".into()),
            ..Default::default()
        };
        match endpoints.iter_mut().find(|e| &e.endpoint == first) {
            Some(e) => e.live = Some(live),
            None => endpoints.push(EndpointImpairment {
                endpoint: first.clone(),
                declared: None,
                live: Some(live),
                partitioned: false,
            }),
        }
    }
    if eps.len() > 2
        && let Some(last) = eps.last()
    {
        endpoints.push(EndpointImpairment {
            endpoint: last.clone(),
            declared: None,
            live: None,
            partitioned: true,
        });
    }
    let hosts: Vec<&String> = topology
        .nodes
        .iter()
        .filter(|(_, n)| n.image.is_none())
        .map(|(k, _)| k)
        .collect();
    let processes = hosts
        .iter()
        .take(2)
        .enumerate()
        .map(|(i, node)| ProcessDto {
            node: node.to_string(),
            pid: 4100 + i as u32,
            alive: i == 0,
            exit_code: (i != 0).then_some(0),
            ..Default::default()
        })
        .collect();
    let ev = |ts: &str, rest: serde_json::Value| {
        let mut v = serde_json::json!({"ts": ts, "lab": name});
        if let (Some(o), Some(r)) = (v.as_object_mut(), rest.as_object()) {
            o.extend(r.clone());
        }
        v
    };
    let mut events = vec![ev(
        "2026-10-18T09:00:00Z",
        serde_json::json!({"event": "deployed", "nodes": topology.nodes.len(), "links": topology.links.len()}),
    )];
    if let Some(h) = hosts.first() {
        events.push(ev(
            "2026-10-18T09:00:05Z",
            serde_json::json!({"event": "spawned", "node": h, "pid": 4100, "cmd": "iperf3 -s"}),
        ));
    }
    for e in &endpoints {
        if e.live.is_some() {
            events.push(ev(
                "2026-10-18T09:01:10Z",
                serde_json::json!({"event": "impaired", "endpoint": e.endpoint,
                                   "impairment": {"delay": "80ms", "loss": "2%"}}),
            ));
        }
        if e.partitioned {
            events.push(ev(
                "2026-10-18T09:02:30Z",
                serde_json::json!({"event": "partitioned", "endpoint": e.endpoint}),
            ));
        }
    }
    Lab {
        name: name.to_string(),
        topology,
        endpoints,
        processes,
        events,
        snapshots: Vec::new(),
    }
}

fn spec_of(imp: &nlink_lab::Impairment) -> ImpairmentSpec {
    serde_json::to_value(imp)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_answers_like_a_daemon() {
        let f = Fake::new();
        let Response::Labs { labs } = f.call(Request::ListLabs).unwrap() else {
            panic!()
        };
        assert_eq!(labs.len(), LABS.len());
        let lab = labs[0].lab.clone();
        let Response::Topology { topology, .. } = f
            .call(Request::GetTopology {
                lab: lab.clone(),
                redact: true,
            })
            .unwrap()
        else {
            panic!()
        };
        let t: Topology = serde_json::from_value(topology).unwrap();
        assert!(!t.nodes.is_empty());
        let Response::Impairments { endpoints } = f
            .call(Request::GetImpairments { lab: lab.clone() })
            .unwrap()
        else {
            panic!()
        };
        assert!(endpoints.iter().any(|e| e.live.is_some()));
        assert!(endpoints.iter().any(|e| e.partitioned));
        let m = f.metrics(&lab, 3.0).unwrap();
        assert!(
            m.nodes
                .values()
                .flat_map(|n| &n.interfaces)
                .any(|i| i.rx_bps > 0)
        );
        let Response::Events { events, .. } = f
            .call(Request::ReadEvents {
                lab,
                since: None,
                kinds: vec![],
                limit: None,
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(
            events
                .iter()
                .all(|e| crate::live::lifecycle(e).time.len() == 8)
        );
        assert!(f.call(Request::ScanHost).is_err());
    }
}
