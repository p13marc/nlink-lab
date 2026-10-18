//! Long operations on the daemon — a deploy, an apply, a scenario, a
//! benchmark, the assertions — as their progress comes (`OpEvent`s):
//! the stage, the steps, what is waited for, what happened.

use std::collections::VecDeque;

use nlink_lab::CancelToken;
use nlink_lab_shared::api::{OpEvent, OpOutcome, OpPhase, WaitKind};

use crate::live::Tone;

/// Lines a job keeps.
pub const LINES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    Deploy,
    Apply,
    Scenario(String),
    Benchmark(String),
    Assertions,
    /// Followed after the fact (`attach_op`).
    Attached(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done { ok: bool, text: String },
}

pub struct Job {
    pub id: u64,
    /// The daemon's id of the operation, once it says.
    pub op: Option<String>,
    pub lab: String,
    pub kind: JobKind,
    /// The stage, its place among them.
    pub stage: Option<(String, usize, usize)>,
    /// Steps done, of how many: the whole plan's.
    pub steps: (usize, usize),
    pub waiting: Option<String>,
    pub lines: VecDeque<(Tone, String)>,
    pub state: JobState,
    pub cancel: CancelToken,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("lab", &self.lab)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

fn wait_text(what: &WaitKind) -> String {
    match what {
        WaitKind::ImagePull { image } => format!("pulling {image}"),
        WaitKind::ContainerStart { node } => format!("starting {node}"),
        WaitKind::Healthcheck { node } => format!("{node}'s healthcheck"),
        WaitKind::FrrConvergence => "FRR converging".into(),
        WaitKind::WifiAssociation => "Wi-Fi associating".into(),
        WaitKind::AssertionRetries => "assertions, again".into(),
        WaitKind::Readiness { node } => format!("{node} getting ready"),
    }
}

impl Job {
    pub fn new(id: u64, lab: &str, kind: JobKind) -> Self {
        Self {
            id,
            op: None,
            lab: lab.to_string(),
            kind,
            stage: None,
            steps: (0, 0),
            waiting: None,
            lines: VecDeque::new(),
            state: JobState::Running,
            cancel: CancelToken::new(),
        }
    }

    pub fn title(&self) -> String {
        match &self.kind {
            JobKind::Deploy => format!("deploy {}", self.lab),
            JobKind::Apply => format!("apply to {}", self.lab),
            JobKind::Scenario(s) => format!("scenario {s}"),
            JobKind::Benchmark(b) => format!("benchmark {b}"),
            JobKind::Assertions => format!("assertions of {}", self.lab),
            JobKind::Attached(k) => format!("{k} of {}", self.lab),
        }
    }

    pub fn running(&self) -> bool {
        self.state == JobState::Running
    }

    fn line(&mut self, tone: Tone, text: String) {
        self.lines.push_back((tone, text));
        while self.lines.len() > LINES {
            self.lines.pop_front();
        }
    }

    /// How far it is, 0 to 1: the plan's steps done, else the stages.
    pub fn progress(&self) -> f32 {
        match (&self.state, &self.stage) {
            (JobState::Done { .. }, _) => 1.0,
            _ if self.steps.1 > 0 => (self.steps.0 as f32 / self.steps.1 as f32).clamp(0.0, 1.0),
            (_, Some((_, index, of))) if *of > 0 => (*index as f32 / *of as f32).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    /// Take an event in.
    pub fn event(&mut self, e: &OpEvent) {
        if self.op.is_none() && !e.op.is_empty() {
            self.op = Some(e.op.clone());
        }
        if !e.lab.is_empty() {
            self.lab = e.lab.clone();
        }
        match &e.phase {
            OpPhase::Queued { position } => {
                self.line(Tone::Info, format!("queued behind {position} other(s)"))
            }
            OpPhase::Started { .. } => self.line(Tone::Info, "started".into()),
            OpPhase::Planned { ops, removals, .. } => self.line(
                Tone::Info,
                if *removals > 0 {
                    format!("{ops} change(s), {removals} removal(s)")
                } else {
                    format!("{ops} change(s)")
                },
            ),
            OpPhase::Stage {
                stage, index, of, ..
            } => {
                // stages count from 1 for people
                self.stage = Some((stage.clone(), index.saturating_sub(1), *of));
                self.waiting = None;
            }
            OpPhase::Step { done, total, .. } => self.steps = (*done, *total),
            OpPhase::Waiting { what, detail, .. } => {
                self.waiting = Some(match detail {
                    Some(d) => format!("{} ({d})", wait_text(what)),
                    None => wait_text(what),
                });
            }
            OpPhase::WaitProgress {
                what, done, total, ..
            } => self.waiting = Some(format!("{} {done}/{total}", wait_text(what))),
            OpPhase::Log { level, message } => self.line(
                match level.as_str() {
                    "error" => Tone::Bad,
                    "warn" | "warning" => Tone::Warn,
                    _ => Tone::Info,
                },
                message.clone(),
            ),
            OpPhase::Assertion {
                description,
                passed,
                detail,
            } => self.line(
                if *passed { Tone::Good } else { Tone::Bad },
                format!(
                    "{} {description}{}",
                    if *passed { "pass" } else { "FAIL" },
                    detail
                        .as_ref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                ),
            ),
            OpPhase::ScenarioAction {
                time_ms,
                description,
                ok,
                detail,
            } => self.line(
                if *ok { Tone::Info } else { Tone::Bad },
                format!(
                    "{:>6.1}s {description}{}",
                    *time_ms as f64 / 1000.0,
                    detail
                        .as_ref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                ),
            ),
            OpPhase::RollingBack { reason, entries } => self.line(
                Tone::Warn,
                format!("rolling back {entries} change(s): {reason}"),
            ),
            OpPhase::Finished {
                outcome,
                elapsed_ms,
            } => {
                let secs = *elapsed_ms as f64 / 1000.0;
                let (ok, text) = match outcome {
                    OpOutcome::Succeeded => (true, format!("done in {secs:.1}s")),
                    OpOutcome::Cancelled => (false, "cancelled".to_string()),
                    OpOutcome::Failed { error } => (false, format!("failed: {}", error.message)),
                    OpOutcome::RolledBack { error } => {
                        (false, format!("rolled back: {}", error.message))
                    }
                };
                self.finish(ok, text);
            }
        }
    }

    /// It ended (its `Finished`, or the call's end, whichever first).
    pub fn finish(&mut self, ok: bool, text: String) {
        if self.running() {
            self.waiting = None;
            self.line(if ok { Tone::Good } else { Tone::Bad }, text.clone());
            self.state = JobState::Done { ok, text };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(phase: OpPhase) -> OpEvent {
        OpEvent {
            op: "op-1".into(),
            lab: "dc".into(),
            seq: 0,
            ts_ms: 0,
            phase,
        }
    }

    #[test]
    fn progress_follows_the_stages_and_steps() {
        let mut j = Job::new(1, "", JobKind::Deploy);
        j.event(&ev(OpPhase::Started {
            kind: "deploy".into(),
            actor: None,
        }));
        assert_eq!((j.op.as_deref(), j.lab.as_str()), (Some("op-1"), "dc"));
        j.event(&ev(OpPhase::Stage {
            stage: "links".into(),
            index: 2,
            of: 4,
            ops: 10,
        }));
        j.event(&ev(OpPhase::Step {
            done: 5,
            total: 10,
            describe: "veth".into(),
        }));
        assert!((j.progress() - 0.5).abs() < 1e-6, "{}", j.progress());
        j.event(&ev(OpPhase::Waiting {
            what: WaitKind::Healthcheck { node: "web".into() },
            detail: None,
            deadline_ms: None,
        }));
        assert_eq!(j.waiting.as_deref(), Some("web's healthcheck"));
        j.event(&ev(OpPhase::Assertion {
            description: "reach a b".into(),
            passed: false,
            detail: Some("timeout".into()),
        }));
        assert_eq!(j.lines.back().unwrap().0, Tone::Bad);
        j.event(&ev(OpPhase::Finished {
            outcome: OpOutcome::Succeeded,
            elapsed_ms: 1500,
        }));
        assert_eq!(
            j.state,
            JobState::Done {
                ok: true,
                text: "done in 1.5s".into()
            }
        );
        assert_eq!(j.progress(), 1.0);
        // what ends after the end changes nothing
        j.finish(false, "late".into());
        assert!(matches!(j.state, JobState::Done { ok: true, .. }));
    }
}
