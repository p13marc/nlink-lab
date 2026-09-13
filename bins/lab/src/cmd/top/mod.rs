//! `nlink-lab top` — live terminal UI over a running lab (issue #63).
//!
//! [`app`] holds the pure state (that is where the tests are), [`ui`] the
//! widgets and the plain-text `--once` frame, and this module the I/O:
//! where snapshots come from, terminal setup and teardown, and the event
//! loop that keeps keystrokes responsive while a collection is in flight.

mod app;
mod ui;

use std::io::IsTerminal;
use std::time::Duration;

use app::{Action, App};
use nlink_lab_shared::metrics::MetricsSnapshot;
use tokio_stream::StreamExt;

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Refresh interval in seconds.
    #[arg(short, long, default_value_t = 1, value_name = "SECS")]
    pub interval: u64,

    /// Print one text frame and exit: no terminal control, safe in pipes,
    /// CI and `watch`. With `--json`, prints the raw metrics snapshot.
    #[arg(long)]
    pub once: bool,

    /// Start with the node/interface filter set to this substring.
    #[arg(add = crate::ctx::node_completer(), short, long, value_name = "SUBSTR")]
    pub node: Option<String>,

    /// Read metrics from a running `daemon` over Zenoh instead of
    /// collecting them locally. No root needed; read-only.
    #[arg(long)]
    pub zenoh: bool,

    /// Zenoh connect endpoint (implies `--zenoh`).
    #[arg(long, value_name = "ENDPOINT")]
    pub zenoh_connect: Option<String>,
}

/// Where snapshots come from.
enum Source {
    /// Collected in-process with the same collector `daemon` uses. Needs
    /// root: it calls `RunningLab::diagnose`, which opens a netlink
    /// socket in every namespace.
    Local {
        lab: nlink_lab::RunningLab,
        collector: nlink_lab_backend::collector::MetricsCollector,
    },
    /// A running `daemon`'s `metrics/snapshot` topic. Read-only.
    Zenoh {
        /// Owns the session and subscriber; ends when `rx` is dropped.
        _task: tokio::task::JoinHandle<()>,
        rx: tokio::sync::mpsc::Receiver<MetricsSnapshot>,
        /// Unprivileged `state.json` read, for the impairment column.
        /// `None` when the daemon is on another host.
        lab: Option<nlink_lab::RunningLab>,
    },
}

/// What [`Source::due`] resolved to.
enum Due {
    /// Local: the interval elapsed, collect now — outside the `select!`,
    /// because the collector future is not cancel-safe.
    Collect,
    /// Zenoh: the daemon published this.
    Snapshot(MetricsSnapshot),
    /// The daemon's stream ended.
    Closed,
}

impl Source {
    async fn open(args: &Args) -> nlink_lab::Result<Self> {
        if args.zenoh || args.zenoh_connect.is_some() {
            return Self::open_zenoh(args).await;
        }
        if !crate::ctx::has_privileges() {
            return Err(nlink_lab::Error::deploy_failed(
                "top collects metrics locally, which needs root (same as `diagnose`): run it \
                 with sudo, or start `nlink-lab daemon <lab>` and use `top <lab> --zenoh`",
            ));
        }
        let lab = nlink_lab::RunningLab::load(&args.lab)?;
        let collector = nlink_lab_backend::collector::MetricsCollector::new(&lab);
        Ok(Source::Local { lab, collector })
    }

    async fn open_zenoh(args: &Args) -> nlink_lab::Result<Self> {
        // Same configuration path as `nlink-lab metrics`.
        let mut config = zenoh::Config::default();
        if let Some(connect) = &args.zenoh_connect {
            config
                .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}")))?;
        }
        let session = zenoh::open(config).await.map_err(|e| {
            nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
        })?;
        let topic = nlink_lab_shared::topics::metrics_snapshot(&args.lab);
        let subscriber = session
            .declare_subscriber(&topic)
            .await
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("subscribe to '{topic}': {e}")))?;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // The session and subscriber live in the task, so `Source` stays
        // free of zenoh handler generics.
        let task = tokio::spawn(async move {
            let _session = session;
            while let Ok(sample) = subscriber.recv_async().await {
                let payload = sample.payload().to_bytes();
                match serde_json::from_slice::<MetricsSnapshot>(&payload) {
                    Ok(snap) => {
                        if tx.send(snap).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => tracing::warn!("bad metrics snapshot: {e}"),
                }
            }
        });
        Ok(Source::Zenoh {
            _task: task,
            rx,
            lab: nlink_lab::RunningLab::load(&args.lab).ok(),
        })
    }

    fn lab(&self) -> Option<&nlink_lab::RunningLab> {
        match self {
            Source::Local { lab, .. } => Some(lab),
            Source::Zenoh { lab, .. } => lab.as_ref(),
        }
    }

    /// Writes are only possible on the local source: there is no
    /// `rpc/partition` topic, and `ImpairmentRequest` carries only six of
    /// the ten impairment knobs.
    fn can_mutate(&self) -> bool {
        matches!(self, Source::Local { .. })
    }

    /// Cancel-safe: sleeps on the ticker, or awaits the daemon's next
    /// publication.
    async fn due(&mut self, ticker: &mut tokio::time::Interval) -> Due {
        match self {
            Source::Local { .. } => {
                ticker.tick().await;
                Due::Collect
            }
            Source::Zenoh { rx, .. } => match rx.recv().await {
                Some(snap) => Due::Snapshot(snap),
                None => Due::Closed,
            },
        }
    }

    /// One local collection. Never called on the Zenoh source.
    async fn collect(&mut self) -> nlink_lab::Result<MetricsSnapshot> {
        match self {
            Source::Local { lab, collector } => {
                let (snap, _events) = collector.snapshot(lab).await?;
                Ok(snap)
            }
            Source::Zenoh { .. } => Err(nlink_lab::Error::deploy_failed(
                "the Zenoh source is push-only",
            )),
        }
    }

    /// Perform a write. Errors come back as strings for the status line:
    /// a failed impairment must never take the TUI down.
    async fn act(&mut self, action: Action) -> Result<String, String> {
        let Source::Local { lab, .. } = self else {
            return Err("this source is read-only".to_string());
        };
        match action {
            Action::Impair {
                endpoint,
                impairment,
            } => {
                let summary = impairment.summary();
                lab.set_impairment(&endpoint, &impairment)
                    .await
                    .map_err(|e| e.to_string())?;
                // `set_impairment` is partition-aware: on a partitioned
                // endpoint it records the value for `heal` and leaves the
                // 100% loss in place, which the user needs to be told.
                if lab.is_partitioned(&endpoint) {
                    Ok(format!(
                        "{endpoint}: {summary} recorded — takes effect on heal (partitioned)"
                    ))
                } else {
                    Ok(format!("{endpoint}: {summary}"))
                }
            }
            Action::Clear { endpoint } => {
                lab.clear_impairment(&endpoint)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("{endpoint}: impairment cleared"))
            }
            Action::Partition { endpoint } => {
                lab.partition(&endpoint).await.map_err(|e| e.to_string())?;
                Ok(format!("{endpoint}: partitioned (loss 100%)"))
            }
            Action::Heal { endpoint } => {
                lab.heal(&endpoint).await.map_err(|e| e.to_string())?;
                Ok(format!("{endpoint}: healed"))
            }
        }
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    // Fail before touching the terminal: the interactive view needs a tty,
    // and cli-smoke runs headless.
    if !args.once && !std::io::stdout().is_terminal() {
        return Err(nlink_lab::Error::deploy_failed(
            "top needs a terminal; use `top --once` for a single text frame (pipes, CI, watch)",
        ));
    }
    let interval = Duration::from_secs(args.interval.max(1));
    let mut source = Source::open(&args).await?;
    let mut app = App::new(
        &args.lab,
        interval,
        deployed_at_unix(&args.lab),
        source.can_mutate(),
    );
    if let Some(filter) = &args.node {
        app.filter = filter.to_lowercase();
    }
    refresh_impairments(&mut app, &source);

    if args.once {
        return run_once(ctx, &mut source, &mut app, interval).await;
    }

    // `try_init` enables raw mode, enters the alternate screen and
    // installs a panic hook that restores the terminal before the payload
    // is printed. Nothing between here and `restore()` may use `?`.
    let mut terminal = ratatui::try_init()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("terminal init: {e}")))?;
    let result = event_loop(&mut terminal, &mut source, &mut app).await;
    ratatui::restore();
    result
}

/// One frame, no terminal control at all.
async fn run_once(
    ctx: &Ctx,
    source: &mut Source,
    app: &mut App,
    interval: Duration,
) -> nlink_lab::Result<()> {
    let snapshot = match source {
        Source::Local { .. } => source.collect().await?,
        Source::Zenoh { rx, .. } => {
            // Bounded so a daemon that is not publishing cannot hang CI.
            let wait = (interval * 5).max(Duration::from_secs(10));
            match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(snap)) => snap,
                Ok(None) => {
                    return Err(nlink_lab::Error::deploy_failed(
                        "the daemon's metrics stream ended",
                    ));
                }
                Err(_) => {
                    crate::output::set_exit_code(crate::output::EXIT_TIMEOUT);
                    return Err(nlink_lab::Error::deploy_failed(format!(
                        "no metrics published within {}s — is `nlink-lab daemon` running?",
                        wait.as_secs()
                    )));
                }
            }
        }
    };
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    app.apply_snapshot(snapshot, &container_nodes(source));
    print!("{}", ui::render_text(app));
    Ok(())
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    source: &mut Source,
    app: &mut App,
) -> nlink_lab::Result<()> {
    use crossterm::event::{Event, KeyEventKind};

    // One stream for the whole loop: dropping and recreating it is what
    // loses keys. Dropping the `next()` future each iteration is fine.
    let mut keys = crossterm::event::EventStream::new();
    let mut ticker = tokio::time::interval(app.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Raw mode suppresses SIGINT (Ctrl-C arrives as a key), but an
    // external kill must still restore the terminal.
    let mut sigterm = signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = signal(tokio::signal::unix::SignalKind::hangup())?;

    enum Step {
        Due(Due),
        Input(Event),
        Warn(String),
        Quit,
    }

    terminal
        .draw(|f| ui::draw(f, app))
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("draw: {e}")))?;

    while !app.should_quit {
        let step = tokio::select! {
            due = source.due(&mut ticker) => Step::Due(due),
            event = keys.next() => match event {
                Some(Ok(event)) => Step::Input(event),
                Some(Err(e)) => Step::Warn(e.to_string()),
                None => Step::Quit,
            },
            _ = sigterm.recv() => Step::Quit,
            _ = sighup.recv() => Step::Quit,
        };
        match step {
            // Outside the `select!`: `snapshot` takes `&mut self` and is
            // not cancel-safe, so it must not be a select branch.
            Step::Due(Due::Collect) => match source.collect().await {
                Ok(snapshot) => {
                    app.apply_snapshot(snapshot, &container_nodes(source));
                    refresh_impairments(app, source);
                }
                Err(e) => app.set_error(e),
            },
            Step::Due(Due::Snapshot(snapshot)) => {
                app.apply_snapshot(snapshot, &container_nodes(source));
                refresh_impairments(app, source);
            }
            Step::Due(Due::Closed) => {
                app.set_error("the daemon's metrics stream ended");
            }
            Step::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if let Some(action) = app.on_key(key) {
                    match source.act(action).await {
                        Ok(msg) => app.set_status(msg),
                        Err(e) => app.set_error(e),
                    }
                    // Show the effect now rather than on the next tick.
                    refresh_impairments(app, source);
                }
            }
            // Resize and the key-release half of the kitty protocol: the
            // draw below is the whole response.
            Step::Input(_) => {}
            Step::Warn(e) => app.set_error(format!("input: {e}")),
            Step::Quit => break,
        }
        terminal
            .draw(|f| ui::draw(f, app))
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("draw: {e}")))?;
    }
    Ok(())
}

fn signal(kind: tokio::signal::unix::SignalKind) -> nlink_lab::Result<tokio::signal::unix::Signal> {
    tokio::signal::unix::signal(kind)
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("signal handler: {e}")))
}

fn container_nodes(source: &Source) -> Vec<String> {
    source
        .lab()
        .map(|lab| lab.containers().keys().cloned().collect())
        .unwrap_or_default()
}

fn refresh_impairments(app: &mut App, source: &Source) {
    let Some(lab) = source.lab() else {
        app.impairments.clear();
        return;
    };
    app.set_impairments(
        &lab.topology().impairments,
        lab.live_impairments(),
        lab.partitions(),
    );
}

/// The lab's `created_at` as unix seconds, for the header uptime; `0`
/// when the state file is unreadable or the timestamp does not parse.
fn deployed_at_unix(lab: &str) -> u64 {
    let Ok(labs) = nlink_lab::state::list() else {
        return 0;
    };
    let Some(info) = labs.iter().find(|l| l.name == lab) else {
        return 0;
    };
    time::OffsetDateTime::parse(
        &info.created_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|t| t.unix_timestamp().max(0) as u64)
    .unwrap_or(0)
}
