//! The labs of the host as they change, and the subscriptions that
//! follow them.
//!
//! The registry scans the state root every `rescan_ms`: a lab deployed or
//! destroyed by the CLI, the daemon or a test is seen within one scan,
//! whoever deployed it. Each lab's `events.ndjson` is tailed from where
//! the last scan left it, and a changed topology revision (an `apply`, a
//! `restore`) is announced. Metrics are collected only for labs someone
//! subscribed to, on a thread per lab: nlink's namespace connections are
//! not `Send`.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nlink_lab::CancelToken;
use nlink_lab::ops::view;
use nlink_lab_shared::api::{
    ApiError, ErrorCode, LabSummary, Notification, Response, StreamItem, Topic,
};
use nlink_lab_shared::metrics::MetricsSnapshot;
use tokio::sync::broadcast;

use crate::calls::cancelled;
use crate::config::Config;
use crate::server::CallCtx;

/// The shortest metrics interval a subscription gets.
const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// A notification, with what it is about.
pub(crate) struct Note {
    /// The lab, or `None` for the host's (`labs_changed`).
    pub lab: Option<String>,
    pub topic: Topic,
    pub notification: Notification,
}

/// Where an events file was read up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    ino: u64,
    offset: u64,
}

/// A lab's metrics collector and who wants it, at which interval.
struct Collector {
    wants: HashMap<u64, Duration>,
    interval: Arc<Mutex<Duration>>,
    stop: Arc<AtomicBool>,
}

impl Collector {
    fn retime(&self) {
        if let Some(min) = self.wants.values().min() {
            *self.interval.lock().unwrap() = *min;
        }
    }
}

/// A lab's drift watch and who wants it.
struct DriftWatch {
    wants: std::collections::HashSet<u64>,
    stop: Arc<AtomicBool>,
}

pub(crate) struct Hub {
    tx: broadcast::Sender<Arc<Note>>,
    labs: Mutex<BTreeMap<String, LabSummary>>,
    latest: Arc<Mutex<HashMap<String, MetricsSnapshot>>>,
    collectors: Mutex<HashMap<String, Collector>>,
    drift: Mutex<HashMap<String, DriftWatch>>,
    next_want: AtomicU64,
    metrics_interval: Duration,
    rescan: Duration,
}

impl Hub {
    pub fn new(config: &Config) -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(1024).0,
            labs: Mutex::default(),
            latest: Arc::default(),
            collectors: Mutex::default(),
            drift: Mutex::default(),
            next_want: AtomicU64::new(1),
            metrics_interval: config.metrics_interval(),
            rescan: config.rescan(),
        })
    }

    pub fn lab_count(&self) -> usize {
        self.labs.lock().unwrap().len()
    }

    /// The latest metrics of the lab, when a subscriber's collector runs.
    pub fn latest_metrics(&self, lab: &str) -> Option<MetricsSnapshot> {
        self.latest.lock().unwrap().get(lab).cloned()
    }

    /// An operation's progress, for the `ops` topic.
    pub fn publish_op(&self, event: &nlink_lab_shared::api::OpEvent) {
        let lab = Some(event.lab.as_str()).filter(|l| !l.is_empty());
        self.publish(
            lab,
            Topic::Ops,
            Notification::OpProgress {
                event: event.clone(),
            },
        );
    }

    fn publish(&self, lab: Option<&str>, topic: Topic, notification: Notification) {
        // no subscriber is not an error
        let _ = self.tx.send(Arc::new(Note {
            lab: lab.map(str::to_string),
            topic,
            notification,
        }));
    }

    /// The registry: scan the state root until `shutdown`.
    pub async fn run(self: Arc<Self>, shutdown: CancelToken) {
        let mut cursors: HashMap<String, Cursor> = HashMap::new();
        let mut first = true;
        loop {
            let (mut scanned_cursors, from_end) = (std::mem::take(&mut cursors), first);
            let scanned = tokio::task::spawn_blocking(move || {
                let result = scan(&mut scanned_cursors, from_end);
                (scanned_cursors, result)
            })
            .await;
            match scanned {
                Ok((c, (summaries, events))) => {
                    cursors = c;
                    first = false;
                    self.update(summaries, events);
                }
                Err(e) => tracing::warn!("scanning the state root: {e}"),
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.rescan) => {}
            }
        }
    }

    /// Take a scan in: announce what changed.
    fn update(&self, summaries: Vec<LabSummary>, events: Vec<(String, Vec<serde_json::Value>)>) {
        let new: BTreeMap<String, LabSummary> =
            summaries.into_iter().map(|s| (s.lab.clone(), s)).collect();
        let (added, updated, removed, moved) = {
            let mut labs = self.labs.lock().unwrap();
            let added: Vec<LabSummary> = new
                .values()
                .filter(|s| !labs.contains_key(&s.lab))
                .cloned()
                .collect();
            let updated: Vec<LabSummary> = new
                .values()
                .filter(|s| labs.get(&s.lab).is_some_and(|old| old != *s))
                .cloned()
                .collect();
            let moved: Vec<(String, String)> = updated
                .iter()
                .filter(|s| {
                    labs.get(&s.lab)
                        .is_some_and(|old| old.revision != s.revision)
                })
                .map(|s| (s.lab.clone(), s.revision.clone()))
                .collect();
            let removed: Vec<String> = labs
                .keys()
                .filter(|name| !new.contains_key(*name))
                .cloned()
                .collect();
            *labs = new;
            (added, updated, removed, moved)
        };
        for lab in &removed {
            self.latest.lock().unwrap().remove(lab);
        }
        if !(added.is_empty() && updated.is_empty() && removed.is_empty()) {
            self.publish(
                None,
                Topic::Labs,
                Notification::LabsChanged {
                    added,
                    updated,
                    removed,
                },
            );
        }
        for (lab, revision) in moved {
            self.publish(
                Some(&lab),
                Topic::Topology,
                Notification::TopologyChanged {
                    lab: lab.clone(),
                    revision,
                },
            );
        }
        for (lab, events) in events {
            for event in events {
                self.publish(
                    Some(&lab),
                    Topic::Lifecycle,
                    Notification::Lifecycle {
                        lab: lab.clone(),
                        event,
                    },
                );
            }
        }
    }

    /// Follow `topics` of `lab` (of the host with `None`) until the
    /// client cancels or goes, or the lab goes away.
    pub async fn subscribe(
        self: &Arc<Self>,
        ctx: &CallCtx,
        lab: Option<String>,
        topics: Vec<Topic>,
        interval_ms: Option<u64>,
    ) -> Result<Response, ApiError> {
        if topics.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "subscribe to one topic at least",
            ));
        }
        if lab.is_none() && topics.contains(&Topic::Drift) {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "drift notifications are a lab's: give `lab`",
            ));
        }
        if let Some(lab) = &lab {
            nlink_lab::state::validate_lab_name(lab).map_err(|e| e.to_api())?;
            if !nlink_lab::state::exists(lab) {
                return Err(nlink_lab::Error::NotFound { name: lab.clone() }.to_api());
            }
        }
        let collect = topics.contains(&Topic::Metrics) || topics.contains(&Topic::Runtime);
        if collect && lab.is_none() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "metrics and runtime notifications are a lab's: give `lab`",
            ));
        }
        let every = interval_ms
            .map(Duration::from_millis)
            .unwrap_or(self.metrics_interval)
            .max(MIN_INTERVAL);
        // subscribed before the first state is read: nothing falls between
        let mut rx = self.tx.subscribe();
        let _want = match (&lab, collect) {
            (Some(lab), true) => Some(self.want_metrics(lab, every)),
            _ => None,
        };
        let _drift = match &lab {
            Some(lab) if topics.contains(&Topic::Drift) => Some(self.want_drift(lab)),
            _ => None,
        };
        let notify = async |notification| ctx.item(StreamItem::Notification { notification }).await;
        if topics.contains(&Topic::Labs) {
            let added: Vec<LabSummary> = self
                .labs
                .lock()
                .unwrap()
                .values()
                .filter(|s| lab.as_ref().is_none_or(|l| *l == s.lab))
                .cloned()
                .collect();
            let first = Notification::LabsChanged {
                added,
                updated: vec![],
                removed: vec![],
            };
            if !notify(first).await {
                return Err(cancelled());
            }
        }
        if let Some(lab) = lab.as_deref().filter(|_| topics.contains(&Topic::Metrics))
            && let Some(snapshot) = self.latest_metrics(lab)
            && !notify(Notification::Metrics {
                lab: lab.to_string(),
                snapshot,
            })
            .await
        {
            return Err(cancelled());
        }
        let mut last_metrics: Option<Instant> = None;
        loop {
            let note = tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(cancelled()),
                note = rx.recv() => note,
            };
            let note = match note {
                Ok(note) => note,
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    if !ctx.item(StreamItem::Lagged { dropped }).await {
                        return Err(cancelled());
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(Response::Ok),
            };
            let notification = match (&lab, &note.notification) {
                (
                    Some(want),
                    Notification::LabsChanged {
                        added,
                        updated,
                        removed,
                    },
                ) => {
                    let gone = removed.contains(want);
                    let mine = |s: &&LabSummary| s.lab == *want;
                    let scoped = Notification::LabsChanged {
                        added: added.iter().filter(mine).cloned().collect(),
                        updated: updated.iter().filter(mine).cloned().collect(),
                        removed: removed.iter().filter(|l| *l == want).cloned().collect(),
                    };
                    let empty = matches!(&scoped, Notification::LabsChanged { added, updated, removed }
                        if added.is_empty() && updated.is_empty() && removed.is_empty());
                    if topics.contains(&Topic::Labs) && !empty && !notify(scoped).await {
                        return Err(cancelled());
                    }
                    // the lab is gone, and with it what there was to follow
                    if gone {
                        return Ok(Response::Ok);
                    }
                    continue;
                }
                (Some(want), _) if note.lab.as_ref().is_some_and(|l| l != want) => continue,
                _ if !topics.contains(&note.topic) => continue,
                (_, n @ Notification::Metrics { .. }) => {
                    // the collector ticks at the fastest subscriber's pace
                    let now = Instant::now();
                    if last_metrics.is_some_and(|t| now - t + MIN_INTERVAL / 2 < every) {
                        continue;
                    }
                    last_metrics = Some(now);
                    n.clone()
                }
                (_, n) => n.clone(),
            };
            if !notify(notification).await {
                return Err(cancelled());
            }
        }
    }

    /// Someone wants `lab`'s metrics every `every`: its collector runs
    /// while the returned guard lives.
    fn want_metrics(self: &Arc<Self>, lab: &str, every: Duration) -> Want {
        let id = self.next_want.fetch_add(1, Ordering::Relaxed);
        let mut collectors = self.collectors.lock().unwrap();
        let collector = collectors
            .entry(lab.to_string())
            .or_insert_with(|| self.start_collector(lab, every));
        collector.wants.insert(id, every);
        collector.retime();
        Want {
            hub: self.clone(),
            lab: lab.to_string(),
            id,
        }
    }

    /// Someone wants `lab`'s drift: its watch runs while the guard lives.
    fn want_drift(self: &Arc<Self>, lab: &str) -> DriftWant {
        let id = self.next_want.fetch_add(1, Ordering::Relaxed);
        let mut drift = self.drift.lock().unwrap();
        let watch = drift.entry(lab.to_string()).or_insert_with(|| {
            let stop = Arc::new(AtomicBool::new(false));
            let (tx, thread_stop, name) = (self.tx.clone(), stop.clone(), lab.to_string());
            let spawned = std::thread::Builder::new()
                .name(format!("drift {lab}"))
                .spawn(move || watch_drift(name, tx, thread_stop));
            if let Err(e) = spawned {
                tracing::warn!("cannot watch the drift of '{lab}': {e}");
            }
            DriftWatch {
                wants: Default::default(),
                stop,
            }
        });
        watch.wants.insert(id);
        DriftWant {
            hub: self.clone(),
            lab: lab.to_string(),
            id,
        }
    }

    fn start_collector(&self, lab: &str, every: Duration) -> Collector {
        let interval = Arc::new(Mutex::new(every));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, latest) = (self.tx.clone(), self.latest.clone());
        let (thread_interval, thread_stop, name) =
            (interval.clone(), stop.clone(), lab.to_string());
        let spawned = std::thread::Builder::new()
            .name(format!("metrics {lab}"))
            .spawn(move || collect(name, tx, latest, thread_interval, thread_stop));
        if let Err(e) = spawned {
            tracing::warn!("cannot start the metrics collector of '{lab}': {e}");
        }
        Collector {
            wants: HashMap::new(),
            interval,
            stop,
        }
    }
}

/// A subscriber's want of a lab's metrics; dropping it lets the collector
/// stop when no one else wants them.
struct Want {
    hub: Arc<Hub>,
    lab: String,
    id: u64,
}

impl Drop for Want {
    fn drop(&mut self) {
        let mut collectors = self.hub.collectors.lock().unwrap();
        if let Some(c) = collectors.get_mut(&self.lab) {
            c.wants.remove(&self.id);
            if c.wants.is_empty() {
                c.stop.store(true, Ordering::Relaxed);
                collectors.remove(&self.lab);
                self.hub.latest.lock().unwrap().remove(&self.lab);
            } else {
                c.retime();
            }
        }
    }
}

/// A subscriber's want of a lab's drift.
struct DriftWant {
    hub: Arc<Hub>,
    lab: String,
    id: u64,
}

impl Drop for DriftWant {
    fn drop(&mut self) {
        let mut drift = self.hub.drift.lock().unwrap();
        if let Some(w) = drift.get_mut(&self.lab) {
            w.wants.remove(&self.id);
            if w.wants.is_empty() {
                w.stop.store(true, Ordering::Relaxed);
                drift.remove(&self.lab);
            }
        }
    }
}

/// A lab's drift: every node's RTNETLINK and nftables changes, as
/// notifications, until stopped or the lab is gone.
fn watch_drift(lab: String, tx: broadcast::Sender<Arc<Note>>, stop: Arc<AtomicBool>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("the drift watch of '{lab}': {e}");
            return;
        }
    };
    rt.block_on(async {
        let Ok(running) = nlink_lab::RunningLab::load(&lab) else {
            return;
        };
        let opts = nlink_lab::WatchOpts::default();
        let (mut rx, tasks) = match nlink_lab::watch_stream(&running, &opts) {
            Ok(Some(stream)) => stream,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("the drift watch of '{lab}': {e}");
                return;
            }
        };
        while !stop.load(Ordering::Relaxed) {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    let Ok(detail) = serde_json::to_value(&event) else {
                        continue;
                    };
                    let _ = tx.send(Arc::new(Note {
                        lab: Some(lab.clone()),
                        topic: Topic::Drift,
                        notification: Notification::Drift {
                            lab: lab.clone(),
                            node: event.node.clone(),
                            detail,
                        },
                    }));
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        for task in tasks {
            task.abort();
        }
    });
}

/// A lab's metrics collector: a snapshot every interval until stopped or
/// the lab is gone. The lab is loaded again each time, so processes
/// spawned and topologies applied since are seen.
fn collect(
    lab: String,
    tx: broadcast::Sender<Arc<Note>>,
    latest: Arc<Mutex<HashMap<String, MetricsSnapshot>>>,
    interval: Arc<Mutex<Duration>>,
    stop: Arc<AtomicBool>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("the metrics collector of '{lab}': {e}");
            return;
        }
    };
    rt.block_on(async {
        let mut collector = None;
        while !stop.load(Ordering::Relaxed) {
            let tick = Instant::now();
            match nlink_lab::RunningLab::load(&lab) {
                Ok(running) => {
                    let c = collector
                        .get_or_insert_with(|| nlink_lab::metrics::MetricsCollector::new(&running));
                    match c.snapshot(&running).await {
                        Ok((snapshot, events)) => {
                            latest.lock().unwrap().insert(lab.clone(), snapshot.clone());
                            let _ = tx.send(Arc::new(Note {
                                lab: Some(lab.clone()),
                                topic: Topic::Metrics,
                                notification: Notification::Metrics {
                                    lab: lab.clone(),
                                    snapshot,
                                },
                            }));
                            for event in events {
                                let Ok(event) = serde_json::to_value(&event) else {
                                    continue;
                                };
                                let _ = tx.send(Arc::new(Note {
                                    lab: Some(lab.clone()),
                                    topic: Topic::Runtime,
                                    notification: Notification::Runtime {
                                        lab: lab.clone(),
                                        event,
                                    },
                                }));
                            }
                        }
                        Err(e) => tracing::debug!("metrics of '{lab}': {e}"),
                    }
                }
                Err(_) if !nlink_lab::state::exists(&lab) => return,
                Err(e) => tracing::debug!("metrics of '{lab}': {e}"),
            }
            let every = *interval.lock().unwrap();
            while !stop.load(Ordering::Relaxed) && tick.elapsed() < every {
                let left = every.saturating_sub(tick.elapsed());
                tokio::time::sleep(left.min(Duration::from_millis(100))).await;
            }
        }
    });
}

/// Every lab of the state root and the events each recorded since the
/// last scan. On the first scan the events files are read from their
/// end (what happened before the daemon started is history: a client
/// reads it with `read_events`); a lab that appears later is read from
/// its start, so its `deployed` is seen.
fn scan(
    cursors: &mut HashMap<String, Cursor>,
    from_end: bool,
) -> (Vec<LabSummary>, Vec<(String, Vec<serde_json::Value>)>) {
    let summaries = match view::lab_summaries() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("listing labs: {e}");
            return (Vec::new(), Vec::new());
        }
    };
    cursors.retain(|lab, _| summaries.iter().any(|s| s.lab == *lab));
    let mut events = Vec::new();
    for s in &summaries {
        let meta = std::fs::metadata(nlink_lab::events::events_path(&s.lab)).ok();
        let (ino, len) = meta.map_or((0, 0), |m| (m.ino(), m.len()));
        let cursor = cursors.entry(s.lab.clone()).or_insert(Cursor {
            ino,
            offset: if from_end { len } else { 0 },
        });
        if cursor.ino != ino {
            // rotated, or the lab was deployed again
            *cursor = Cursor { ino, offset: 0 };
        }
        match view::read_events(&s.lab, Some(cursor.offset), &[], None) {
            Ok((new, next)) => {
                cursor.offset = next;
                if !new.is_empty() {
                    events.push((s.lab.clone(), new));
                }
            }
            Err(e) => tracing::debug!("events of '{}': {e}", s.lab),
        }
    }
    (summaries, events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(lab: &str, revision: &str) -> LabSummary {
        LabSummary {
            lab: lab.into(),
            revision: revision.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn updates_announce_what_changed() {
        let hub = Hub::new(&Config::default());
        let mut rx = hub.tx.subscribe();
        hub.update(vec![summary("a", "1"), summary("b", "1")], vec![]);
        let n = rx.recv().await.unwrap();
        assert!(
            matches!(&n.notification, Notification::LabsChanged { added, .. } if added.len() == 2)
        );

        // b's topology changed, a went away, c came with an event
        hub.update(
            vec![summary("b", "2"), summary("c", "1")],
            vec![("c".into(), vec![serde_json::json!({"event": "deployed"})])],
        );
        let n = rx.recv().await.unwrap();
        match &n.notification {
            Notification::LabsChanged {
                added,
                updated,
                removed,
            } => {
                assert_eq!(added[0].lab, "c");
                assert_eq!(updated[0].lab, "b");
                assert_eq!(removed, &["a".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        let n = rx.recv().await.unwrap();
        assert!(matches!(
            &n.notification,
            Notification::TopologyChanged { lab, revision } if lab == "b" && revision == "2"
        ));
        assert_eq!(n.topic, Topic::Topology);
        let n = rx.recv().await.unwrap();
        assert_eq!(n.lab.as_deref(), Some("c"));
        assert_eq!(n.topic, Topic::Lifecycle);

        // nothing changed: nothing said
        hub.update(vec![summary("b", "2"), summary("c", "1")], vec![]);
        assert!(rx.try_recv().is_err());
        assert_eq!(hub.lab_count(), 2);
    }

    #[test]
    fn a_collector_keeps_the_fastest_interval() {
        let hub = Hub::new(&Config::default());
        // a lab that does not exist: its collector thread ends at once
        let slow = hub.want_metrics("nolab", Duration::from_secs(5));
        let fast = hub.want_metrics("nolab", Duration::from_secs(1));
        let interval = |hub: &Hub| {
            *hub.collectors.lock().unwrap()["nolab"]
                .interval
                .lock()
                .unwrap()
        };
        assert_eq!(interval(&hub), Duration::from_secs(1));
        drop(fast);
        assert_eq!(interval(&hub), Duration::from_secs(5));
        drop(slow);
        assert!(hub.collectors.lock().unwrap().is_empty());
    }
}
