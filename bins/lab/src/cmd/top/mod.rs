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
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Refresh interval (`1s`; 100ms at least).
    #[arg(short, long, value_name = "DURATION", default_value = "1s", value_parser = crate::args::interval)]
    pub interval: Duration,

    /// Print one text frame and exit: no terminal control, safe in pipes,
    /// CI and `watch`. With `--json`, prints the raw metrics snapshot.
    ///
    /// Collecting locally, this samples twice half a second apart — a
    /// rate is a delta between two samples, so a single collection would
    /// report every interface as idle.
    #[arg(long)]
    pub once: bool,

    /// Start with the node/interface filter set to this substring.
    #[arg(add = crate::ctx::node_completer(), long, value_name = "SUBSTR")]
    pub node: Option<String>,

    /// Read metrics from the nlink-lab daemon (`nlink-labd`) instead of
    /// collecting them: no root needed, and the write keys work for an
    /// operator. What `top` does without root, or with `NLINK_LAB_DAEMON`
    /// set (`ssh://HOST` for another host's labs).
    #[arg(long)]
    pub daemon: bool,
}

/// Where snapshots come from.
enum Source {
    /// Collected in-process with the same collector the daemon uses.
    /// Needs root: it calls `RunningLab::diagnose`, which opens a netlink
    /// socket in every namespace.
    Local {
        lab: nlink_lab::RunningLab,
        collector: nlink_lab::metrics::MetricsCollector,
    },
    /// A metrics subscription to the daemon; an operator also changes
    /// impairments through it.
    Daemon {
        /// The connection; `None` in tests.
        client: Option<nlink_lab_client::Client>,
        lab_name: String,
        /// The caller is an operator: the write keys work.
        operator: bool,
        /// The lab's impairments as the daemon says them.
        impairments: Vec<nlink_lab_shared::api::EndpointImpairment>,
        /// Owns the subscription; ends when `rx` is dropped.
        _task: tokio::task::JoinHandle<()>,
        /// A snapshot, or why the subscription ended.
        rx: tokio::sync::mpsc::Receiver<Result<MetricsSnapshot, String>>,
        /// Unprivileged `state.json` read, for the impairment column.
        /// `None` when the daemon is on another host.
        lab: Option<nlink_lab::RunningLab>,
        /// Set once the stream has ended. A closed channel returns
        /// `None` from `recv()` *immediately, every time*, so without
        /// this the event loop would spin — redrawing and re-reporting
        /// the error as fast as it can — instead of idling.
        ended: bool,
    },
}

/// What [`Source::due`] resolved to.
enum Due {
    /// Local: the interval elapsed, collect now — outside the `select!`,
    /// because the collector future is not cancel-safe.
    Collect,
    /// Daemon: the subscription delivered this.
    Snapshot(MetricsSnapshot),
    /// The subscription ended, and why.
    Closed(String),
}

impl Source {
    async fn open(args: &Args, lab: &str, interval: Duration) -> nlink_lab::Result<Self> {
        let from_env = std::env::var_os("NLINK_LAB_DAEMON").is_some_and(|v| !v.is_empty());
        if args.daemon || from_env || !crate::ctx::has_privileges() {
            return Self::open_daemon(lab, interval, args.daemon || from_env).await;
        }
        let lab = nlink_lab::RunningLab::load(lab)?;
        let collector = nlink_lab::metrics::MetricsCollector::new(&lab);
        Ok(Source::Local { lab, collector })
    }

    /// Subscribe to the lab's metrics on the daemon `NLINK_LAB_DAEMON`
    /// names (its socket by default). `asked`: the daemon was asked for,
    /// rather than tried for want of root.
    async fn open_daemon(lab: &str, interval: Duration, asked: bool) -> nlink_lab::Result<Self> {
        use nlink_lab_client::{CallEvent, Client, Endpoint};
        use nlink_lab_shared::api::{Notification, Request, StreamItem, Topic};
        let endpoint = Endpoint::from_env();
        let client = Client::connect(
            &endpoint,
            concat!("nlink-lab-top/", env!("CARGO_PKG_VERSION")),
        )
        .await
        .map_err(|e| match e {
            nlink_lab_client::ClientError::Io(_) if !asked => {
                nlink_lab::Error::Privileges(format!(
                    "top collects metrics itself as root, or reads them from the nlink-lab daemon, \
                     and no daemon answers at {endpoint}: run top with sudo, or start the daemon \
                     (`sudo systemctl enable --now nlink-labd.socket`)"
                ))
            }
            e => crate::ctx::daemon_error(e),
        })?;
        let operator = client.welcome().role == nlink_lab_shared::api::Role::Operator;
        let mut call = client
            .start(Request::Subscribe {
                lab: Some(lab.to_string()),
                topics: vec![Topic::Metrics],
                interval_ms: Some(interval.as_millis() as u64),
            })
            .await
            .map_err(crate::ctx::daemon_error)?;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(async move {
            while let Some(event) = call.next().await {
                let item = match event {
                    CallEvent::Item(StreamItem::Notification {
                        notification: Notification::Metrics { snapshot, .. },
                    }) => Ok(snapshot),
                    CallEvent::End(Err(e)) => Err(e.message),
                    CallEvent::End(Ok(_)) => Err("the lab is gone".into()),
                    _ => continue,
                };
                let last = item.is_err();
                if tx.send(item).await.is_err() || last {
                    return;
                }
            }
            let _ = tx
                .send(Err("the connection to the daemon closed".into()))
                .await;
        });
        let mut source = Source::Daemon {
            client: Some(client),
            lab_name: lab.to_string(),
            operator,
            impairments: Vec::new(),
            _task: task,
            rx,
            lab: nlink_lab::RunningLab::load(lab).ok(),
            ended: false,
        };
        source.fetch_impairments().await;
        Ok(source)
    }

    /// The daemon source's impairments, asked again. A failure keeps the
    /// last ones: the column is not worth an error.
    async fn fetch_impairments(&mut self) {
        let Source::Daemon {
            client: Some(client),
            lab_name,
            impairments,
            ..
        } = self
        else {
            return;
        };
        let request = nlink_lab_shared::api::Request::GetImpairments {
            lab: lab_name.clone(),
        };
        if let Ok(nlink_lab_shared::api::Response::Impairments { endpoints }) =
            client.call(request).await
        {
            *impairments = endpoints;
        }
    }

    /// The lab as its state file has it now. Loaded once, a container
    /// restart, an exited container or an `apply` left `top` reading stale
    /// namespaces (no metrics at all) and stale impairments (#218).
    fn refresh(&mut self) {
        let lab = match self {
            Source::Local { lab, .. } => Some(lab),
            Source::Daemon { lab, .. } => lab.as_mut(),
        };
        if let Some(lab) = lab
            && let Ok(fresh) = nlink_lab::RunningLab::load(lab.name())
        {
            *lab = fresh;
        }
    }

    fn lab(&self) -> Option<&nlink_lab::RunningLab> {
        match self {
            Source::Local { lab, .. } => Some(lab),
            Source::Daemon { lab, .. } => lab.as_ref(),
        }
    }

    /// Writes need root on the local source, an operator on the daemon's.
    fn can_mutate(&self) -> bool {
        match self {
            Source::Local { .. } => true,
            Source::Daemon { operator, .. } => *operator,
        }
    }

    /// Cancel-safe: sleeps on the ticker, or awaits the subscription's
    /// next snapshot.
    async fn due(&mut self, ticker: &mut tokio::time::Interval) -> Due {
        match self {
            Source::Local { .. } => {
                ticker.tick().await;
                Due::Collect
            }
            Source::Daemon { rx, ended, .. } => {
                if *ended {
                    // Park forever: the loop must still answer keys, but
                    // this source will never produce anything again.
                    std::future::pending::<()>().await;
                }
                match rx.recv().await {
                    Some(Ok(snap)) => Due::Snapshot(snap),
                    Some(Err(why)) => {
                        *ended = true;
                        Due::Closed(why)
                    }
                    None => {
                        *ended = true;
                        Due::Closed("the connection to the daemon closed".into())
                    }
                }
            }
        }
    }

    /// One local collection. Never called on the daemon source.
    async fn collect(&mut self) -> nlink_lab::Result<MetricsSnapshot> {
        self.refresh();
        match self {
            Source::Local { lab, collector } => {
                let (snap, _events) = collector.snapshot(lab).await?;
                Ok(snap)
            }
            Source::Daemon { .. } => Err(nlink_lab::Error::deploy_failed(
                "the daemon source is push-only",
            )),
        }
    }

    /// Perform a write. Errors come back as strings for the status line:
    /// a failed impairment must never take the TUI down.
    async fn act(&mut self, action: Action) -> Result<String, String> {
        self.refresh();
        let lab = match self {
            Source::Local { lab, .. } => lab,
            Source::Daemon {
                client: Some(client),
                lab_name,
                operator: true,
                ..
            } => {
                let result = act_on_daemon(client, lab_name, action).await;
                self.fetch_impairments().await;
                return result;
            }
            Source::Daemon { .. } => return Err("this source is read-only".to_string()),
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
    let lab = args.lab.resolve()?;
    // Fail before touching the terminal: the interactive view needs a tty,
    // and cli-smoke runs headless.
    if !args.once && !std::io::stdout().is_terminal() {
        return Err(nlink_lab::Error::deploy_failed(
            "top needs a terminal; use `top --once` for a single text frame (pipes, CI, watch)",
        ));
    }
    let interval = args.interval.max(Duration::from_millis(100));
    let mut source = Source::open(&args, &lab, interval).await?;
    let mut app = App::new(&lab, interval, deployed_at_unix(&lab), source.can_mutate());
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
    /// How long `--once` waits between its baseline collection and the
    /// sample it prints. A rate is a delta over an interval, so a single
    /// collection has nothing to report; long enough for a counter to
    /// move, short enough not to be felt in a pipeline or a `watch`.
    const RATE_WINDOW: Duration = Duration::from_millis(500);

    let snapshot = match source {
        Source::Local { .. } => {
            // Collect twice: the first pass is the collector's baseline
            // and carries no rates (#133), so a one-shot frame built from
            // it would report every interface as idle.
            let baseline = source.collect().await?;
            // the frame follows from it, as in the live view: printed
            // alone it said "need a second sample" under its flows (#218)
            app.apply_snapshot(baseline, &container_nodes(source));
            tokio::time::sleep(RATE_WINDOW).await;
            source.collect().await?
        }
        Source::Daemon { .. } => {
            // Bounded so a daemon that does not deliver cannot hang CI.
            let wait = (interval * 5).max(Duration::from_secs(10));
            // a fresh collector's first snapshot has no rates: as on the
            // local source, it is the baseline and the second is printed
            let baseline = next_snapshot(source, wait).await?;
            app.apply_snapshot(baseline, &container_nodes(source));
            next_snapshot(source, wait).await?
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

/// The daemon source's next snapshot, within `wait`.
async fn next_snapshot(source: &mut Source, wait: Duration) -> nlink_lab::Result<MetricsSnapshot> {
    let Source::Daemon { rx, .. } = source else {
        return source.collect().await;
    };
    match tokio::time::timeout(wait, rx.recv()).await {
        Ok(Some(Ok(snap))) => Ok(snap),
        Ok(Some(Err(why))) => Err(nlink_lab::Error::deploy_failed(why)),
        Ok(None) => Err(nlink_lab::Error::deploy_failed(
            "the connection to the daemon closed",
        )),
        // a timeout, exit 124 (#218)
        Err(_) => Err(nlink_lab::Error::TimedOut(format!(
            "the daemon sent no metrics within {}s",
            wait.as_secs()
        ))),
    }
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
                source.refresh();
                source.fetch_impairments().await;
                app.apply_snapshot(snapshot, &container_nodes(source));
                refresh_impairments(app, source);
            }
            Step::Due(Due::Closed(why)) => {
                app.set_error(format!("{why} — showing the last sample (q to quit)"));
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

/// A write key through the daemon: the request an operator may make.
async fn act_on_daemon(
    client: &nlink_lab_client::Client,
    lab: &str,
    action: Action,
) -> Result<String, String> {
    use nlink_lab_shared::api::{Direction, ImpairmentSpec, Request};
    let lab = lab.to_string();
    let (request, done) = match action {
        Action::Impair {
            endpoint,
            impairment,
        } => {
            let done = format!("{endpoint}: {}", impairment.summary());
            let request = Request::Impair {
                lab,
                endpoint,
                direction: Direction::Out,
                spec: ImpairmentSpec::from(&impairment),
            };
            (request, done)
        }
        Action::Clear { endpoint } => (
            Request::ClearImpairment {
                lab,
                endpoint: endpoint.clone(),
            },
            format!("{endpoint}: impairment cleared"),
        ),
        Action::Partition { endpoint } => (
            Request::Partition {
                lab,
                endpoint: endpoint.clone(),
            },
            format!("{endpoint}: partitioned (loss 100%)"),
        ),
        Action::Heal { endpoint } => (
            Request::Heal {
                lab,
                endpoint: endpoint.clone(),
            },
            format!("{endpoint}: healed"),
        ),
    };
    client
        .call(request)
        .await
        .map(|_| done)
        .map_err(|e| e.to_string())
}

fn refresh_impairments(app: &mut App, source: &Source) {
    if let Source::Daemon { impairments, .. } = source {
        let (mut declared, mut live, mut partitioned) = Default::default();
        daemon_impairments(impairments, &mut declared, &mut live, &mut partitioned);
        app.set_impairments(&declared, &live, &partitioned);
        return;
    }
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

type Impairments = std::collections::BTreeMap<String, nlink_lab::Impairment>;

/// The daemon's view of the impairments as the maps `App` reads: what the
/// topology declares, what `impair` set, what is partitioned.
fn daemon_impairments(
    endpoints: &[nlink_lab_shared::api::EndpointImpairment],
    declared: &mut Impairments,
    live: &mut Impairments,
    partitioned: &mut Impairments,
) {
    for e in endpoints {
        if let Some(d) = &e.declared {
            declared.insert(e.endpoint.clone(), d.into());
        }
        if let Some(l) = &e.live {
            live.insert(e.endpoint.clone(), l.into());
        }
        if e.partitioned {
            partitioned.insert(e.endpoint.clone(), nlink_lab::Impairment::default());
        }
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A daemon source whose subscription has already gone away.
    fn closed_daemon_source() -> Source {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<MetricsSnapshot, String>>(1);
        drop(tx);
        Source::Daemon {
            client: None,
            lab_name: "lab".into(),
            operator: false,
            impairments: Vec::new(),
            _task: tokio::spawn(async {}),
            rx,
            lab: None,
            ended: false,
        }
    }

    /// A closed channel yields `None` from `recv()` immediately and for
    /// ever. Reporting that once and then parking is what keeps the event
    /// loop from spinning at full CPU, redrawing on every iteration, when
    /// the daemon goes away.
    #[tokio::test]
    async fn a_closed_stream_reports_once_and_then_parks() {
        let mut source = closed_daemon_source();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));

        assert!(
            matches!(source.due(&mut ticker).await, Due::Closed(_)),
            "the first poll after the stream ends must report it"
        );
        // Every later poll must never resolve, so the `select!` falls
        // through to the key stream instead of looping on this arm.
        let parked =
            tokio::time::timeout(Duration::from_millis(200), source.due(&mut ticker)).await;
        assert!(parked.is_err(), "a second poll must not resolve");
    }

    #[tokio::test]
    async fn the_daemon_source_is_read_only_and_refuses_writes() {
        let mut source = closed_daemon_source();
        assert!(!source.can_mutate());
        let err = source
            .act(Action::Partition {
                endpoint: "a:eth0".into(),
            })
            .await
            .expect_err("the daemon source cannot write");
        assert!(err.contains("read-only"), "{err}");
    }

    #[tokio::test]
    async fn collecting_from_the_daemon_source_is_rejected() {
        let mut source = closed_daemon_source();
        assert!(
            source.collect().await.is_err(),
            "the daemon source is push-only"
        );
    }

    #[test]
    fn the_daemons_impairments_fill_the_column() {
        use nlink_lab_shared::api::{EndpointImpairment, ImpairmentSpec};
        let spec = |d: &str| ImpairmentSpec {
            delay: Some(d.into()),
            ..Default::default()
        };
        let endpoints = vec![
            EndpointImpairment {
                endpoint: "a:eth0".into(),
                declared: Some(spec("10ms")),
                live: Some(spec("50ms")),
                partitioned: false,
            },
            EndpointImpairment {
                endpoint: "b:eth0".into(),
                declared: None,
                live: None,
                partitioned: true,
            },
        ];
        let (mut declared, mut live, mut partitioned) = Default::default();
        daemon_impairments(&endpoints, &mut declared, &mut live, &mut partitioned);
        let shown = app::effective_impairments(&declared, &live, &partitioned);
        assert_eq!(shown["a:eth0"].origin, "live");
        assert!(shown["a:eth0"].summary.contains("50ms"));
        assert_eq!(shown["b:eth0"].origin, "partitioned");
    }

    #[test]
    fn an_unknown_lab_has_no_deploy_time() {
        assert_eq!(deployed_at_unix("no-such-lab-exists-here"), 0);
    }
}
