//! `nlink-lab events` — the lab's lifecycle log, optionally followed and
//! merged with live drift (RTNETLINK / nftables) and runtime (process /
//! interface) events (issue #70).

use std::path::PathBuf;
use std::time::Duration;

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Keep running: tail new lifecycle events and merge live drift
    /// (`watch`) and runtime events (process exits, interface state).
    /// Drift and runtime sources need root; without it only the
    /// lifecycle log is followed.
    #[arg(long, short = 'f')]
    pub follow: bool,

    /// Only events newer than this (`10m`, `2h`, `30s`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,

    /// Only these event kinds (repeatable): lifecycle names such as
    /// `deployed`, `impaired`, `spawned`; drift names, `<action>_<object>`
    /// as `watch` prints them (`new_address`, `del_route`, `new_nft_rule`)
    /// or `ended`/`gone`; or the sources `lifecycle`, `drift`, `runtime`.
    #[arg(long, value_name = "KIND")]
    pub kind: Vec<String>,

    /// Runtime-event sampling interval in seconds (with --follow).
    #[arg(long, default_value_t = 2)]
    pub interval: u64,

    /// Also serve the stream on a unix socket: every client that connects
    /// receives one NDJSON record per line (with --follow).
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,
}

/// One record on the merged stream.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Event {
    /// Recorded by a CLI operation (`events.ndjson`).
    Lifecycle(nlink_lab::LifecycleEvent),
    /// RTNETLINK / nftables change inside a node (`nlink-lab watch`).
    Drift {
        ts: String,
        #[serde(flatten)]
        event: DriftEvent,
    },
    /// Process exit / interface state derived from sampling.
    Runtime {
        ts: String,
        #[serde(flatten)]
        event: serde_json::Value,
    },
}

/// `nlink_lab::WatchEvent` without the schemars dependency on the lib
/// type (flattened as JSON).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DriftEvent {
    pub node: String,
    #[serde(flatten)]
    pub detail: serde_json::Value,
}

/// A watch event as a drift record: its JSON without `node`, which
/// [`DriftEvent`] carries itself.
fn drift_record(ts: String, w: &nlink_lab::WatchEvent) -> Option<Event> {
    let mut detail = serde_json::to_value(w).ok()?;
    let node = detail
        .as_object_mut()
        .and_then(|m| m.remove("node"))
        .and_then(|n| n.as_str().map(str::to_string))
        .unwrap_or_default();
    Some(Event::Drift {
        ts,
        event: DriftEvent { node, detail },
    })
}

impl DriftEvent {
    /// The watch event the record carries: its `kind` and, for a kernel
    /// object, nlink's view under `event` (#195).
    fn kind(&self) -> Option<nlink_lab::watch::WatchEventKind> {
        serde_json::from_value(self.detail.clone()).ok()
    }
}

impl Event {
    fn name(&self) -> String {
        match self {
            Event::Lifecycle(e) => e.kind.name().to_string(),
            Event::Drift { event, .. } => match event.kind() {
                Some(kind) => kind.name(),
                None => "drift".to_string(),
            },
            Event::Runtime { event, .. } => event
                .get("kind")
                .and_then(|k| k.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("runtime")
                .to_string(),
        }
    }

    fn source(&self) -> &'static str {
        match self {
            Event::Lifecycle(_) => "lifecycle",
            Event::Drift { .. } => "drift",
            Event::Runtime { .. } => "runtime",
        }
    }

    fn render_line(&self) -> String {
        match self {
            Event::Lifecycle(e) => e.render_line(),
            Event::Drift { ts, event } => {
                let detail = event.kind().map(|k| k.detail()).unwrap_or_default();
                let line = format!("{ts} drift {}:{} {detail}", event.node, self.name());
                line.trim_end().to_string()
            }
            Event::Runtime { ts, event } => {
                let kind = self.name();
                let fields = event
                    .get("kind")
                    .and_then(|k| k.as_object())
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| *k != "type")
                            .map(|(k, v)| format!("{k}={}", compact(v)))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                format!("{ts} runtime {kind} {}", fields.join(" "))
            }
        }
    }
}

fn compact(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn now_iso8601() -> String {
    nlink_lab::events::now_iso8601()
}

/// `--since` cut-off as an RFC 3339 timestamp (events are compared as
/// strings; both are UTC RFC 3339, so lexical order is time order).
fn since_cutoff(since: &str) -> nlink_lab::Result<String> {
    let d = nlink_lab::helpers::parse_duration(since)?;
    Ok(nlink_lab::events::since_iso8601(d))
}

struct Filter {
    kinds: Vec<String>,
    since: Option<String>,
}

impl Filter {
    fn keep(&self, ev: &Event) -> bool {
        if let Some(cut) = &self.since {
            let ts = match ev {
                Event::Lifecycle(e) => e.ts.as_str(),
                Event::Drift { ts, .. } | Event::Runtime { ts, .. } => ts.as_str(),
            };
            if ts < cut.as_str() {
                return false;
            }
        }
        if self.kinds.is_empty() {
            return true;
        }
        let name = ev.name();
        self.kinds.iter().any(|k| k == ev.source() || *k == name)
    }
}

fn emit(ctx: &Ctx, ev: &Event) -> String {
    if ctx.json {
        serde_json::to_string(ev).unwrap_or_default()
    } else {
        ev.render_line()
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        follow,
        since,
        kind,
        interval,
        socket,
    } = args;
    if !nlink_lab::state::exists(&lab) {
        return Err(nlink_lab::Error::NotFound { name: lab });
    }
    let filter = Filter {
        kinds: kind,
        since: since.as_deref().map(since_cutoff).transpose()?,
    };

    // History first.
    let history = nlink_lab::events::read(&lab)?;
    for e in history {
        let ev = Event::Lifecycle(e);
        if filter.keep(&ev) {
            println!("{}", emit(ctx, &ev));
        }
    }
    if !follow {
        return Ok(());
    }

    // ── follow: merge three sources into one channel ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(1024);
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // 1. the lifecycle log, read through an open descriptor: a file that is
    //    rotated or deleted under us (destroy writes `destroyed` and
    //    removes the state directory at once) is still drained to its
    //    last line, and a file that is not the one we hold any more (a
    //    rotation, a redeploy under the same name) is read from its
    //    start. Polling by path missed `destroyed` and then polled a
    //    missing file forever, and a new file past the old offset lost its
    //    first lines (#210).
    {
        let tx = tx.clone();
        let path = nlink_lab::events::events_path(&lab);
        let start = std::fs::File::open(&path).ok().map(|f| {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            (f, len)
        });
        tasks.push(tokio::spawn(async move {
            use std::io::{Read as _, Seek as _};
            use std::os::unix::fs::MetadataExt as _;
            let mut held: Option<(std::fs::File, u64)> = start;
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if held.is_none() {
                    held = std::fs::File::open(&path).ok().map(|f| (f, 0));
                }
                let Some((file, offset)) = held.as_mut() else {
                    continue;
                };
                let mut new = Vec::new();
                if file.seek(std::io::SeekFrom::Start(*offset)).is_ok() {
                    let _ = file.read_to_end(&mut new);
                }
                // Only complete lines; keep a torn tail for the next poll.
                let end = new.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
                *offset += end as u64;
                let text = String::from_utf8_lossy(&new[..end]).into_owned();
                let mut destroyed = false;
                for e in nlink_lab::events::parse_lines(&text) {
                    destroyed |= matches!(e.kind, nlink_lab::events::LifecycleKind::Destroyed);
                    if tx.send(Event::Lifecycle(e)).await.is_err() {
                        return;
                    }
                }
                if destroyed {
                    return; // the printer ends the follow
                }
                // Still the file at that path? If not, it was drained above:
                // open the new one, from its start, on the next poll.
                let same = match (file.metadata(), std::fs::metadata(&path)) {
                    (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
                    _ => false,
                };
                if !same {
                    held = None;
                }
            }
        }));
    }

    let is_root = unsafe { libc::geteuid() } == 0;
    let running = nlink_lab::RunningLab::load(&lab)?;
    if is_root {
        // 2. drift: RTNETLINK + nftables subscriptions per node
        let opts = nlink_lab::WatchOpts::default();
        if let Some((mut drift_rx, subs)) = nlink_lab::watch_stream(&running, &opts)? {
            tasks.extend(subs);
            let tx = tx.clone();
            tasks.push(tokio::spawn(async move {
                while let Some(w) = drift_rx.recv().await {
                    let Some(ev) = drift_record(now_iso8601(), &w) else {
                        continue;
                    };
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }));
        }
        // 3. runtime: the backend's collector (process exits, link state)
        let tx = tx.clone();
        let interval = Duration::from_secs(interval.max(1));
        tasks.push(tokio::spawn(async move {
            let mut collector = nlink_lab::metrics::MetricsCollector::new(&running);
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                match collector.snapshot(&running).await {
                    Ok((_, events)) => {
                        for e in events {
                            let Ok(v) = serde_json::to_value(&e) else {
                                continue;
                            };
                            let ev = Event::Runtime {
                                ts: now_iso8601(),
                                event: v,
                            };
                            if tx.send(ev).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => tracing::warn!("runtime events: {e}"),
                }
            }
        }));
    } else {
        eprintln!("events: not root — following the lifecycle log only (drift/runtime need root)");
    }
    drop(tx);

    // Optional unix-socket fan-out.
    let (bcast, _) = tokio::sync::broadcast::channel::<String>(1024);
    if let Some(path) = &socket {
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path).map_err(|e| {
            nlink_lab::Error::deploy_failed(format!("bind {}: {e}", path.display()))
        })?;
        let bcast = bcast.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut rx = bcast.subscribe();
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let mut stream = stream;
                    while let Ok(line) = rx.recv().await {
                        if stream.write_all(line.as_bytes()).await.is_err()
                            || stream.write_all(b"\n").await.is_err()
                        {
                            break;
                        }
                    }
                });
            }
        }));
        if !ctx.quiet {
            eprintln!("events: serving NDJSON on {}", path.display());
        }
    }

    let json = ctx.json;
    let quiet = ctx.quiet;
    let printer = async {
        while let Some(ev) = rx.recv().await {
            // the lab is gone: nothing more will come for it
            let last = matches!(
                &ev,
                Event::Lifecycle(e) if matches!(e.kind, nlink_lab::events::LifecycleKind::Destroyed)
            );
            if !filter.keep(&ev) {
                if last {
                    break;
                }
                continue;
            }
            let line = if json || socket.is_some() {
                serde_json::to_string(&ev).unwrap_or_default()
            } else {
                String::new()
            };
            if socket.is_some() {
                let _ = bcast.send(line.clone());
            }
            if !quiet {
                if json {
                    println!("{line}");
                } else {
                    println!("{}", ev.render_line());
                }
            }
            if last {
                break;
            }
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = printer => {}
    }
    for t in tasks {
        t.abort();
    }
    if let Some(path) = &socket {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_by_kind_and_source() {
        let ev = Event::Lifecycle(nlink_lab::LifecycleEvent {
            ts: "2026-09-13T10:00:00Z".into(),
            lab: "t".into(),
            kind: nlink_lab::LifecycleKind::Deployed { nodes: 1, links: 0 },
        });
        let f = Filter {
            kinds: vec!["deployed".into()],
            since: None,
        };
        assert!(f.keep(&ev));
        let f = Filter {
            kinds: vec!["lifecycle".into()],
            since: None,
        };
        assert!(f.keep(&ev));
        let f = Filter {
            kinds: vec!["drift".into()],
            since: None,
        };
        assert!(!f.keep(&ev));
        let f = Filter {
            kinds: vec![],
            since: Some("2026-09-13T11:00:00Z".into()),
        };
        assert!(!f.keep(&ev));
    }

    #[test]
    fn json_shape_has_source_tag() {
        let ev = Event::Lifecycle(nlink_lab::LifecycleEvent {
            ts: "t".into(),
            lab: "l".into(),
            kind: nlink_lab::LifecycleKind::Destroyed,
        });
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"source\":\"lifecycle\""), "{json}");
        assert!(json.contains("\"event\":\"destroyed\""), "{json}");
        // The record `run` makes of a watch event (#195): named and
        // rendered from nlink's view, which `--kind` filters on.
        let watch = nlink_lab::WatchEvent {
            node: "a".into(),
            family: nlink_lab::WatchFamily::Route,
            kind: nlink_lab::watch::WatchEventKind::Event {
                event: nlink::netlink::events::view::EventView::Address {
                    action: nlink::netlink::events::view::Action::New,
                    ifindex: 2,
                    cidr: Some("10.0.0.9/32".into()),
                    scope: "global".into(),
                    label: None,
                },
            },
            from_snapshot: false,
        };
        let drift = drift_record("t".into(), &watch).unwrap();
        assert_eq!(drift.name(), "new_address");
        assert_eq!(
            drift.render_line(),
            "t drift a:new_address cidr=10.0.0.9/32 ifindex=2 scope=global"
        );
        let gone = nlink_lab::WatchEvent {
            kind: nlink_lab::watch::WatchEventKind::Gone,
            ..watch
        };
        let gone = drift_record("t".into(), &gone).unwrap();
        assert_eq!(
            (gone.name(), gone.render_line()),
            ("gone".into(), "t drift a:gone".into())
        );
        assert!(
            serde_json::to_string(&drift)
                .unwrap()
                .contains("\"source\":\"drift\"")
        );
    }
}
