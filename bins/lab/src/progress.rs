//! Live progress of a long operation — deploy, apply, destroy, restore,
//! test, waits, pulls: the [`OpEvent`]s the library reports through an
//! [`OpCtx`], rendered for where they go.
//!
//! - **A terminal** (stderr): one live line — spinner, stage, the op being
//!   done, elapsed time — and a kept line per finished stage.
//! - **A pipe or a file**: one line per stage, wait and warning, each
//!   after the time since the start, and a heartbeat every 10 s while
//!   nothing else is said.
//! - **`--format ndjson`**: every event as one JSON line on stdout, before
//!   the result's.
//! - **`--json`** (one document) and **`--quiet`**: nothing.
//!
//! Ctrl-C cancels the operation, which rolls back what it did where it
//! can; a second Ctrl-C exits at once (130).

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nlink_lab::api::{OpEvent, OpOutcome, OpPhase, WaitKind};
use nlink_lab::{CancelToken, OpCtx, ProgressSink};

use crate::ctx::Ctx;
use crate::output::Format;
use crate::output::style::{green, red, yellow};

/// How often the live line is redrawn, and how long a pipe goes without a
/// line before a heartbeat.
const TICK: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(10);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Lines,
    Ndjson,
    Off,
}

/// What the renderer knows of the operation so far.
#[derive(Debug, Default)]
struct State {
    /// The stage under way: name, ops, when it began.
    stage: Option<(String, usize, Duration)>,
    /// What the live line says after the stage.
    detail: String,
    /// A wait under way: what, since when.
    wait: Option<(String, Duration)>,
    /// When a pipe last got a wait's progress: one line every 5 s, not
    /// one per probe.
    last_wait_line: Option<Duration>,
}

fn secs(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

fn wait_name(what: &WaitKind) -> String {
    match what {
        WaitKind::ImagePull { image } => format!("image pull {image}"),
        WaitKind::ContainerStart { node } => format!("container {node}"),
        WaitKind::Healthcheck { node } => format!("healthcheck on {node}"),
        WaitKind::FrrConvergence => "FRR adjacencies".into(),
        WaitKind::WifiAssociation => "Wi-Fi association".into(),
        WaitKind::AssertionRetries => "assertion retries".into(),
        WaitKind::Readiness { node } => node.clone(),
    }
}

impl State {
    /// The finished stage's kept line, if one was under way.
    fn close_stage(&mut self, at: Duration, ok: bool) -> Option<String> {
        let (name, ops, began) = self.stage.take()?;
        let mark = if ok { green("✓") } else { red("✗") };
        Some(format!(
            "  {mark} {name} ({ops} op{}, {})",
            if ops == 1 { "" } else { "s" },
            secs(at.saturating_sub(began))
        ))
    }

    /// The lines `event` prints (kept, not live), at `at` since the start,
    /// and the live line's new text. `lines` is the pipe form: a line per
    /// stage and wait as well.
    fn apply(&mut self, event: &OpEvent, at: Duration, lines: bool) -> Vec<String> {
        let mut out = Vec::new();
        match &event.phase {
            OpPhase::Queued { position } => {
                self.detail = format!("queued behind {position} operation(s)");
                if lines {
                    out.push(format!("queued behind {position} operation(s)"));
                }
            }
            OpPhase::Started { kind, .. } => {
                self.detail = format!("{kind} {}", event.lab);
            }
            OpPhase::Planned { stages, ops, .. } => {
                self.detail = format!("{ops} op(s) planned");
                if lines {
                    out.push(format!("plan: {ops} op(s) in {} stage(s)", stages.len()));
                }
            }
            OpPhase::Stage {
                stage,
                index,
                of,
                ops,
            } => {
                out.extend(self.close_stage(at, true));
                self.stage = Some((stage.clone(), *ops, at));
                self.detail = format!("stage {index}/{of}");
                self.wait = None;
            }
            OpPhase::Step {
                done,
                total,
                describe,
            } => {
                self.detail = format!("{done}/{total} {describe}");
            }
            OpPhase::Waiting { what, detail, .. } => {
                let what = match detail {
                    Some(d) => format!("{}: {d}", wait_name(what)),
                    None => wait_name(what),
                };
                self.detail = format!("waiting for {what}");
                self.wait = Some((what.clone(), at));
                self.last_wait_line = Some(at);
                if lines {
                    out.push(format!("waiting for {what}"));
                }
            }
            OpPhase::WaitProgress {
                what,
                done,
                total,
                detail,
            } => {
                let of = if *total > 0 {
                    format!("{done}/{total}")
                } else {
                    done.to_string()
                };
                let detail = detail
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                self.detail = format!("waiting for {}: {of}{detail}", wait_name(what));
                let due = self
                    .last_wait_line
                    .is_none_or(|last| at.saturating_sub(last) >= Duration::from_secs(5));
                if lines && due {
                    out.push(format!("waiting for {}: {of}{detail}", wait_name(what)));
                    self.last_wait_line = Some(at);
                }
            }
            OpPhase::Log { level, message } => {
                let tag = if level == "warn" {
                    yellow("WARN")
                } else {
                    "info".to_string()
                };
                out.push(format!("  {tag} {message}"));
            }
            // the command prints every result at the end
            OpPhase::Assertion { .. } => {}
            OpPhase::ScenarioAction {
                time_ms,
                description,
                ok,
                detail,
            } => {
                let mark = if *ok { green("ok") } else { red("FAIL") };
                let detail = detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                out.push(format!(
                    "  [{}] {mark} {description}{detail}",
                    secs(Duration::from_millis(*time_ms))
                ));
            }
            OpPhase::RollingBack { reason, entries } => {
                out.extend(self.close_stage(at, false));
                out.push(format!(
                    "  {} rolling back {entries} change(s): {reason}",
                    yellow("↺")
                ));
                self.detail = "rolling back".into();
            }
            OpPhase::Finished { outcome, .. } => {
                let ok = matches!(outcome, OpOutcome::Succeeded);
                out.extend(self.close_stage(at, ok));
                self.detail.clear();
                self.wait = None;
            }
        }
        out
    }

    /// The live line: what is going on now.
    fn live(&self, at: Duration, frame: usize) -> String {
        let spin = SPINNER[frame % SPINNER.len()];
        let what = match &self.stage {
            Some((name, _, _)) if self.detail.is_empty() => name.clone(),
            Some((name, _, _)) => format!("{name}: {}", self.detail),
            None => self.detail.clone(),
        };
        format!("  {spin} {what}  {}", secs(at))
    }
}

/// The renderer of one command's operations. Cheap to clone into a
/// sink: it is an `Arc`.
pub struct Progress {
    mode: Mode,
    color: bool,
    start: Instant,
    state: Mutex<(State, Instant, usize, bool)>,
    stop: AtomicBool,
}

impl Progress {
    /// A renderer for `ctx`'s output: live on a terminal, lines on a pipe,
    /// NDJSON events with `--format ndjson`, nothing with `--json` or
    /// `--quiet`. The ticker (the live line's spinner, the pipe's
    /// heartbeat) runs until [`finish`](Self::finish).
    pub fn new(ctx: &Ctx) -> Arc<Self> {
        let err = std::io::stderr();
        let mode = match ctx.format {
            Format::Ndjson => Mode::Ndjson,
            Format::Json => Mode::Off,
            Format::Text if ctx.quiet => Mode::Off,
            Format::Text if err.is_terminal() => Mode::Live,
            Format::Text => Mode::Lines,
        };
        let color = anstream::stderr().current_choice() != anstream::ColorChoice::Never;
        let p = Arc::new(Self {
            mode,
            color,
            start: Instant::now(),
            state: Mutex::new((State::default(), Instant::now(), 0, false)),
            stop: AtomicBool::new(false),
        });
        if matches!(mode, Mode::Live | Mode::Lines) {
            let ticker = Arc::clone(&p);
            std::thread::spawn(move || {
                while !ticker.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(TICK);
                    ticker.tick();
                }
            });
        }
        p
    }

    /// An operation context reporting here, cancelled by Ctrl-C.
    pub fn op(self: &Arc<Self>, kind: &str, lab: &str) -> OpCtx {
        let cancel = CancelToken::new();
        self.cancel_on_ctrl_c(cancel.clone(), true);
        OpCtx::new(kind, lab)
            .with_sink(Arc::clone(self) as Arc<dyn ProgressSink>)
            .with_cancel(cancel)
    }

    /// The same for an operation that must not stop halfway (destroy):
    /// Ctrl-C only says so, a second one exits.
    pub fn op_uncancellable(self: &Arc<Self>, kind: &str, lab: &str) -> OpCtx {
        self.cancel_on_ctrl_c(CancelToken::new(), false);
        OpCtx::new(kind, lab).with_sink(Arc::clone(self) as Arc<dyn ProgressSink>)
    }

    fn cancel_on_ctrl_c(self: &Arc<Self>, token: CancelToken, cancellable: bool) {
        let me = Arc::clone(self);
        // outside a runtime (a sync command) Ctrl-C keeps its default
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        rt.spawn(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            let first = std::time::Instant::now();
            if cancellable {
                me.say(&format!(
                    "  {} cancelling: rolling back what was done (Ctrl-C again to quit now)",
                    yellow("^C")
                ));
                token.cancel();
            } else {
                me.say(&format!(
                    "  {} this cannot stop halfway (Ctrl-C again to quit anyway)",
                    yellow("^C")
                ));
            }
            // the same Ctrl-C delivered twice (a relaying `sudo`) is not a
            // second one: that would quit before the rollback
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                if first.elapsed() >= Duration::from_secs(1) {
                    me.finish();
                    std::process::exit(130);
                }
            }
        });
    }

    fn write_err(&self, text: &str) {
        let text = if self.color {
            text.to_string()
        } else {
            anstream::adapter::strip_str(text).to_string()
        };
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }

    /// A kept line (a note, a warning), above the live line.
    pub fn say(&self, line: &str) {
        if matches!(self.mode, Mode::Off | Mode::Ndjson) {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let at = self.start.elapsed();
        self.print_kept(&mut st, &[line.to_string()], at);
    }

    /// Print `lines` (clearing the live line first), and redraw it.
    fn print_kept(&self, st: &mut (State, Instant, usize, bool), lines: &[String], at: Duration) {
        let mut text = String::new();
        if self.mode == Mode::Live && st.3 {
            text.push_str("\r\x1b[2K");
            st.3 = false;
        }
        for l in lines {
            if self.mode == Mode::Lines {
                text.push_str(&format!("[{:>7}] {}\n", secs(at), l.trim_start()));
            } else {
                text.push_str(l);
                text.push('\n');
            }
        }
        if !lines.is_empty() {
            st.1 = Instant::now();
        }
        self.write_err(&text);
    }

    fn redraw(&self, st: &mut (State, Instant, usize, bool)) {
        if self.mode != Mode::Live
            || self.stop.load(Ordering::Relaxed)
            || (st.0.stage.is_none() && st.0.detail.is_empty())
        {
            return;
        }
        let at = self.start.elapsed();
        let line = st.0.live(at, st.2);
        let width = crate::output::table::terminal_width()
            .unwrap_or(80)
            .saturating_sub(1);
        let line = cut(&line, width);
        st.3 = true;
        self.write_err(&format!("\r\x1b[2K{line}"));
    }

    fn tick(&self) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.2 += 1;
        match self.mode {
            Mode::Live => self.redraw(&mut st),
            Mode::Lines if st.1.elapsed() >= HEARTBEAT => {
                let at = self.start.elapsed();
                let what = match (&st.0.wait, &st.0.stage) {
                    (Some((w, since)), _) => {
                        format!(
                            "still waiting for {w} ({})",
                            secs(at.saturating_sub(*since))
                        )
                    }
                    (None, Some((name, _, since))) => {
                        format!("still in {name} ({})", secs(at.saturating_sub(*since)))
                    }
                    (None, None) => return,
                };
                self.print_kept(&mut st, &[what], at);
            }
            _ => {}
        }
    }

    /// Stop the ticker and clear the live line: the command's own output
    /// follows.
    pub fn finish(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if self.mode == Mode::Live && st.3 {
            st.3 = false;
            self.write_err("\r\x1b[2K");
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// `s` cut to `max` columns (it is one line, with escapes).
fn cut(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let plain = anstream::adapter::strip_str(s).to_string();
    if plain.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in plain.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

impl ProgressSink for Progress {
    fn emit(&self, event: OpEvent) {
        match self.mode {
            Mode::Off => {}
            Mode::Ndjson => {
                if let Ok(line) = serde_json::to_string(&serde_json::json!({
                    "schema_version": 1,
                    "item": "op",
                    "event": event,
                })) {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
            }
            Mode::Live | Mode::Lines => {
                let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let at = self.start.elapsed();
                let lines = st.0.apply(&event, at, self.mode == Mode::Lines);
                self.print_kept(&mut st, &lines, at);
                // the end: the command's own output follows
                if !matches!(event.phase, OpPhase::Finished { .. }) {
                    self.redraw(&mut st);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(phase: OpPhase) -> OpEvent {
        OpEvent {
            op: "op-1".into(),
            lab: "wan".into(),
            seq: 0,
            ts_ms: 0,
            phase,
        }
    }

    fn render(events: &[OpPhase], lines: bool) -> Vec<String> {
        let mut st = State::default();
        let mut out = Vec::new();
        for (i, p) in events.iter().enumerate() {
            let at = Duration::from_millis(100 * i as u64);
            for l in st.apply(&ev(p.clone()), at, lines) {
                out.push(anstream::adapter::strip_str(&l).to_string());
            }
        }
        out
    }

    fn deploy() -> Vec<OpPhase> {
        vec![
            OpPhase::Started {
                kind: "deploy".into(),
                actor: None,
            },
            OpPhase::Planned {
                stages: vec![("namespaces".into(), 2), ("links".into(), 1)],
                ops: 3,
                removals: 0,
            },
            OpPhase::Stage {
                stage: "namespaces".into(),
                index: 1,
                of: 2,
                ops: 2,
            },
            OpPhase::Step {
                done: 2,
                total: 3,
                describe: "netns wan-b".into(),
            },
            OpPhase::Stage {
                stage: "links".into(),
                index: 2,
                of: 2,
                ops: 1,
            },
            OpPhase::Waiting {
                what: WaitKind::Healthcheck { node: "web".into() },
                detail: Some("curl -sf localhost".into()),
                deadline_ms: Some(30_000),
            },
            OpPhase::Finished {
                outcome: OpOutcome::Succeeded,
                elapsed_ms: 600,
            },
        ]
    }

    #[test]
    fn a_terminal_keeps_one_line_per_finished_stage() {
        assert_eq!(
            render(&deploy(), false),
            ["  ✓ namespaces (2 ops, 0.2s)", "  ✓ links (1 op, 0.2s)",]
        );
    }

    #[test]
    fn a_pipe_gets_stages_and_waits_too() {
        assert_eq!(
            render(&deploy(), true),
            [
                "plan: 3 op(s) in 2 stage(s)",
                "  ✓ namespaces (2 ops, 0.2s)",
                "waiting for healthcheck on web: curl -sf localhost",
                "  ✓ links (1 op, 0.2s)",
            ]
        );
    }

    #[test]
    fn a_rollback_marks_the_stage_failed() {
        let events = [
            OpPhase::Stage {
                stage: "stack".into(),
                index: 1,
                of: 1,
                ops: 4,
            },
            OpPhase::RollingBack {
                reason: "cancelled".into(),
                entries: 9,
            },
            OpPhase::Finished {
                outcome: OpOutcome::Cancelled,
                elapsed_ms: 200,
            },
        ];
        assert_eq!(
            render(&events, false),
            [
                "  ✗ stack (4 ops, 0.1s)",
                "  ↺ rolling back 9 change(s): cancelled",
            ]
        );
    }

    #[test]
    fn the_live_line_says_what_is_going_on() {
        let mut st = State::default();
        for p in &deploy()[..4] {
            st.apply(&ev(p.clone()), Duration::ZERO, false);
        }
        let live = st.live(Duration::from_millis(1500), 0);
        assert_eq!(live, "  ⠋ namespaces: 2/3 netns wan-b  1.5s");
        assert_eq!(cut(&live, 20).chars().count(), 20);
    }
}
