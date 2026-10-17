//! The context an operation runs in (#236): its id, its lab, who asked,
//! where its progress goes, and whether it was cancelled.
//!
//! A deploy, apply, destroy, restore or scenario takes one ([`OpCtx`]) and
//! reports what it does as [`OpEvent`]s to the context's sink: the CLI
//! draws a progress line from them, the daemon streams them to its
//! clients. Cancelling the context's [`CancelToken`] stops the operation
//! at its next step, and a deploy or apply then rolls back what it did.
//! [`OpCtx::default`] reports nowhere and is never cancelled.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nlink_lab_shared::api::{OpEvent, OpOutcome, OpPhase, WaitKind};

use crate::error::{Error, Result};

/// Where an operation's progress goes. Called from whichever thread the
/// operation runs on; it must not block.
pub trait ProgressSink: Send + Sync + 'static {
    fn emit(&self, event: OpEvent);
}

impl<F> ProgressSink for F
where
    F: Fn(OpEvent) + Send + Sync + 'static,
{
    fn emit(&self, event: OpEvent) {
        self(event)
    }
}

/// Cancels an operation: cloned into it, set from anywhere.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel: the operation stops at its next step, or wakes from a wait.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once cancelled.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// The least time between two `Step` events: a 300-op deploy sends tens
/// of them, not hundreds. The first and the last step always go.
const STEP_EVERY: Duration = Duration::from_millis(100);

/// See the module documentation.
#[derive(Clone)]
pub struct OpCtx {
    op: Arc<str>,
    kind: Arc<str>,
    lab: Arc<Mutex<String>>,
    actor: Option<Arc<str>>,
    sink: Option<Arc<dyn ProgressSink>>,
    cancel: CancelToken,
    seq: Arc<AtomicU64>,
    last_step: Arc<Mutex<Option<Instant>>>,
    started: Instant,
    /// How many operations run in this one: a test's deploy and destroy
    /// report their stages, not a start and an end of their own.
    depth: Arc<AtomicUsize>,
    /// Whether a `RollingBack` went out: the end is then `RolledBack`.
    rolled_back: Arc<AtomicBool>,
}

impl Default for OpCtx {
    fn default() -> Self {
        Self::new("op", "")
    }
}

impl std::fmt::Debug for OpCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpCtx")
            .field("op", &self.op)
            .field("kind", &self.kind)
            .field("lab", &self.lab())
            .finish()
    }
}

impl OpCtx {
    /// A context for an operation of `kind` (`"deploy"`, `"apply"`, …) on
    /// `lab`, with a fresh id, no sink and its own token.
    pub fn new(kind: &str, lab: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            op: format!("op-{}-{n}", std::process::id()).into(),
            kind: kind.into(),
            lab: Arc::new(Mutex::new(lab.to_string())),
            actor: None,
            sink: None,
            cancel: CancelToken::new(),
            seq: Arc::new(AtomicU64::new(0)),
            last_step: Arc::new(Mutex::new(None)),
            started: Instant::now(),
            depth: Arc::new(AtomicUsize::new(0)),
            rolled_back: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The same, with `id` (the daemon names its operations).
    pub fn with_id(mut self, id: &str) -> Self {
        self.op = id.into();
        self
    }

    pub fn with_sink(mut self, sink: Arc<dyn ProgressSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Who asked, as audit records say it: `alice (uid 1000)`.
    pub fn with_actor(mut self, actor: &str) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn id(&self) -> &str {
        &self.op
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn lab(&self) -> String {
        self.lab.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Name the lab once it is known (a deploy's, after its suffix).
    pub fn set_lab(&self, lab: &str) {
        *self.lab.lock().unwrap_or_else(|p| p.into_inner()) = lab.to_string();
    }

    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Whether anything listens: reporting can skip building messages.
    pub fn reporting(&self) -> bool {
        self.sink.is_some()
    }

    /// Report `phase`.
    pub fn emit(&self, phase: OpPhase) {
        if matches!(phase, OpPhase::RollingBack { .. }) {
            self.rolled_back.store(true, Ordering::Relaxed);
        }
        let Some(sink) = &self.sink else {
            return;
        };
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        sink.emit(OpEvent {
            op: self.op.to_string(),
            lab: self.lab(),
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            ts_ms,
            phase,
        });
    }

    /// Report that the operation started, unless it runs inside another
    /// one. Each call is paired with a [`finished`](Self::finished).
    pub fn started(&self) {
        if self.depth.fetch_add(1, Ordering::Relaxed) > 0 {
            return;
        }
        self.emit(OpPhase::Started {
            kind: self.kind.to_string(),
            actor: self.actor.as_deref().map(str::to_string),
        });
    }

    /// Report step `done` of `total`, at most every 100 ms: the first and
    /// the last always. `describe` is only called for a step that is sent.
    pub fn step(&self, done: usize, total: usize, describe: impl FnOnce() -> String) {
        if self.sink.is_none() {
            return;
        }
        let now = Instant::now();
        {
            let mut last = self.last_step.lock().unwrap_or_else(|p| p.into_inner());
            let due = done <= 1
                || done >= total
                || last.is_none_or(|t| now.duration_since(t) >= STEP_EVERY);
            if !due {
                return;
            }
            *last = Some(now);
        }
        self.emit(OpPhase::Step {
            done,
            total,
            describe: describe(),
        });
    }

    /// Report a wait for `what`, of `budget` at most.
    pub fn waiting(&self, what: WaitKind, detail: Option<String>, budget: Option<Duration>) {
        self.emit(OpPhase::Waiting {
            what,
            detail,
            deadline_ms: budget.map(|d| d.as_millis() as u64),
        });
    }

    /// Report how a wait goes: `done` of `total` (adjacencies, attempts).
    pub fn wait_progress(&self, what: WaitKind, done: usize, total: usize, detail: Option<String>) {
        self.emit(OpPhase::WaitProgress {
            what,
            done,
            total,
            detail,
        });
    }

    pub fn log(&self, level: &str, message: impl Into<String>) {
        self.emit(OpPhase::Log {
            level: level.to_string(),
            message: message.into(),
        });
    }

    /// Report the end: succeeded, failed, cancelled, or rolled back after
    /// `rolled_back` failed.
    pub fn finished<T>(&self, result: &Result<T>, rolled_back: bool) {
        let depth = self
            .depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |d| {
                Some(d.saturating_sub(1))
            })
            .unwrap_or(0);
        if depth > 1 {
            return;
        }
        let rolled_back = rolled_back || self.rolled_back.load(Ordering::Relaxed);
        let outcome = match result {
            Ok(_) => OpOutcome::Succeeded,
            Err(Error::Cancelled(_)) => OpOutcome::Cancelled,
            Err(e) if rolled_back => OpOutcome::RolledBack { error: e.to_api() },
            Err(e) => OpOutcome::Failed { error: e.to_api() },
        };
        self.emit(OpPhase::Finished {
            outcome,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        });
    }

    /// `Err(Error::Cancelled)` once the operation is cancelled: called
    /// between its steps.
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(Error::Cancelled(format!(
                "{} of '{}'",
                self.kind,
                self.lab()
            )))
        } else {
            Ok(())
        }
    }

    /// Sleep `d`, or less if the operation is cancelled meanwhile.
    pub async fn sleep(&self, d: Duration) -> Result<()> {
        tokio::select! {
            () = tokio::time::sleep(d) => Ok(()),
            () = self.cancel.cancelled() => self.check(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording() -> (OpCtx, Arc<Mutex<Vec<OpEvent>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let seen = seen.clone();
            move |e: OpEvent| seen.lock().unwrap().push(e)
        };
        (OpCtx::new("deploy", "wan").with_sink(Arc::new(sink)), seen)
    }

    #[test]
    fn events_are_numbered_and_steps_coalesced() {
        let (ctx, seen) = recording();
        ctx.started();
        for done in 1..=1000 {
            ctx.step(done, 1000, || format!("op {done}"));
        }
        ctx.finished(&Ok::<(), Error>(()), false);
        let seen = seen.lock().unwrap();
        let seqs: Vec<u64> = seen.iter().map(|e| e.seq).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
        let steps: Vec<usize> = seen
            .iter()
            .filter_map(|e| match &e.phase {
                OpPhase::Step { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        // a tight loop: the first and the last, not a thousand
        assert_eq!(steps.first(), Some(&1));
        assert_eq!(steps.last(), Some(&1000));
        assert!(steps.len() < 50, "{} steps", steps.len());
        assert!(matches!(
            seen.last().unwrap().phase,
            OpPhase::Finished {
                outcome: OpOutcome::Succeeded,
                ..
            }
        ));
        assert!(seen.iter().all(|e| e.lab == "wan" && e.op == ctx.id()));
    }

    #[test]
    fn a_silent_context_builds_nothing() {
        let ctx = OpCtx::default();
        ctx.step(1, 1, || unreachable!("no sink: not described"));
        assert!(!ctx.reporting());
        assert!(ctx.check().is_ok());
    }

    #[tokio::test]
    async fn cancelling_stops_checks_and_wakes_sleeps() {
        let token = CancelToken::new();
        let ctx = OpCtx::new("apply", "lab").with_cancel(token.clone());
        let sleeper = {
            let ctx = ctx.clone();
            tokio::spawn(async move { ctx.sleep(Duration::from_secs(60)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        let woke = tokio::time::timeout(Duration::from_secs(2), sleeper)
            .await
            .expect("the sleep ignored the cancel")
            .unwrap();
        assert!(matches!(woke, Err(Error::Cancelled(_))));
        assert!(matches!(ctx.check(), Err(Error::Cancelled(_))));
        // a cancel before the wait is not missed either
        token.cancelled().await;
    }

    #[test]
    fn a_cancelled_or_rolled_back_end_says_so() {
        let (ctx, seen) = recording();
        ctx.finished(&Err::<(), _>(Error::Cancelled("x".into())), true);
        ctx.finished(&Err::<(), _>(Error::deploy_failed("boom")), true);
        ctx.finished(&Err::<(), _>(Error::deploy_failed("boom")), false);
        let outcomes: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| match &e.phase {
                OpPhase::Finished { outcome, .. } => format!("{outcome:?}"),
                _ => String::new(),
            })
            .collect();
        assert!(outcomes[0].starts_with("Cancelled"), "{outcomes:?}");
        assert!(outcomes[1].starts_with("RolledBack"), "{outcomes:?}");
        assert!(outcomes[2].starts_with("Failed"), "{outcomes:?}");
    }

    #[test]
    fn a_nested_operation_reports_its_stages_only() {
        let (ctx, seen) = recording();
        ctx.started(); // a test
        ctx.started(); // its deploy
        ctx.emit(OpPhase::RollingBack {
            reason: "boom".into(),
            entries: 3,
        });
        ctx.finished(&Err::<(), _>(Error::deploy_failed("boom")), false);
        ctx.finished(&Err::<(), _>(Error::deploy_failed("boom")), false);
        let phases: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| match &e.phase {
                OpPhase::Started { .. } => "started".into(),
                OpPhase::RollingBack { .. } => "rolling back".into(),
                OpPhase::Finished { outcome, .. } => format!("{outcome:?}"),
                p => format!("{p:?}"),
            })
            .collect();
        assert_eq!(phases.len(), 3, "{phases:?}");
        assert_eq!(phases[0], "started");
        // the deploy rolled back: so did the test
        assert!(phases[2].starts_with("RolledBack"), "{phases:?}");
    }
}
