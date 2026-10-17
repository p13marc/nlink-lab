//! Plan 159b — `nlink-lab watch` event tail.
//!
//! Subscribes to nftables and/or RTNETLINK multicast on every
//! node in a running lab and prints typed events. Powered by
//! nlink 0.19's `Connection<Route>::subscribe_all_with_resync`
//! and `Connection<Nftables>::subscribe_all_with_resync`.
//!
//! The implementation prints directly to stdout so the CLI
//! doesn't need to wire its own event loop. Per-node tasks
//! forward events through an mpsc; the main task drains and
//! prints.
//!
//! Plan 160 (nlink 0.25) — nftables `NewRule` drift lines carry the
//! typed per-rule `(packets, bytes)` counter via `RuleInfo::counter()`.

use std::sync::Arc;

use nlink::netlink::events::view::EventView;
use nlink::netlink::nftables::events::NftablesEvent;
use nlink::netlink::resync::ResyncedEvent;
use nlink::{Connection, Nftables, Route};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use crate::error::{Error, Result};
use crate::running::RunningLab;

/// Where a node's namespace lives — re-exported from the deployer so
/// `watch` and `RunningLab` share one type.
pub use crate::deploy::NsRef as NsResolver;

/// Which event families to subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchFamily {
    /// RTNETLINK only (link/addr/route/neighbor/qdisc/filter/...).
    Route,
    /// nftables only (table/chain/rule/set/flowtable changes).
    Nftables,
    /// Both families on the same stream.
    Both,
}

impl WatchFamily {
    fn wants_route(self) -> bool {
        matches!(self, WatchFamily::Route | WatchFamily::Both)
    }
    fn wants_nftables(self) -> bool {
        matches!(self, WatchFamily::Nftables | WatchFamily::Both)
    }
}

/// One emitted event line.
///
/// As JSON (`watch --json`, one per line):
///
/// ```text
/// {"node":"r1","family":"route","kind":"event","event":{"object":"link","action":"new","ifindex":7,…}}
/// {"node":"r1","family":"route","kind":"ended","reason":"…","retry_in_ms":200}
/// {"node":"r1","family":"route","kind":"gone"}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEvent {
    /// Lab node the event came from.
    pub node: String,
    /// Family the event came from. `Both` is never set on an
    /// individual `WatchEvent` — only on the subscription
    /// request — because each event has exactly one source.
    pub family: WatchFamily,
    /// What happened: a kernel object changed, or the node's
    /// subscription ended.
    #[serde(flatten)]
    pub kind: WatchEventKind,
    /// True when this frame came from an ENOBUFS resync replay
    /// (rather than live multicast). Defaults false on the live
    /// path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_snapshot: bool,
}

/// What a [`WatchEvent`] reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEventKind {
    /// A kernel object came, changed or went: nlink's stable projection
    /// of the RTNETLINK or nftables message, tagged by `object` with an
    /// `action` (#195). Under its own key because it has a `family` of
    /// its own (`inet`, `inet6`, …).
    Event { event: EventView },
    /// The node's subscription ended — a stream error its resync does not
    /// recover from — and is subscribed again in `retry_in_ms` (#175).
    /// Changes made in between are not seen.
    Ended { reason: String, retry_in_ms: u64 },
    /// The node's namespace is gone (the lab destroyed, the node removed or
    /// its container stopped): its watch stops, and its socket no longer
    /// keeps the namespace alive (#175).
    Gone,
}

impl WatchEventKind {
    /// The event's name: `<action>_<object>` for a kernel object
    /// (`new_link`, `del_route`, `new_nft_rule`), `ended` or `gone`
    /// for the subscription. `nlink-lab events --kind` filters on it.
    pub fn name(&self) -> String {
        match self {
            WatchEventKind::Event { event } => view_name(event),
            WatchEventKind::Ended { .. } => "ended".into(),
            WatchEventKind::Gone => "gone".into(),
        }
    }

    /// Its fields as `key=value`, in key order: a view's own fields
    /// (not its tags), or why a subscription ended.
    pub fn detail(&self) -> String {
        match self {
            WatchEventKind::Event { event } => view_fields(event),
            WatchEventKind::Ended {
                reason,
                retry_in_ms,
            } => format!("reason={reason} retry_in_ms={retry_in_ms}"),
            WatchEventKind::Gone => String::new(),
        }
    }
}

/// `<action>_<object>` from a view's own JSON tags, so a variant nlink
/// adds is named without a change here.
fn view_name(view: &EventView) -> String {
    let v = serde_json::to_value(view).unwrap_or_default();
    let tag = |k: &str| v.get(k).and_then(|t| t.as_str()).map(str::to_string);
    match (tag("action"), tag("object")) {
        (Some(action), Some(object)) => format!("{action}_{object}"),
        (None, Some(object)) => object,
        _ => "event".into(),
    }
}

/// The view's fields other than its tags, as `key=value`, in key order.
fn view_fields(view: &EventView) -> String {
    let v = serde_json::to_value(view).unwrap_or_default();
    v.as_object()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| *k != "object" && *k != "action")
                .map(|(k, v)| match v {
                    serde_json::Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

impl WatchEvent {
    /// Render a one-line human-readable form:
    /// `[r1/route] new_link ifindex=7 mtu=1500 name=d0 …`.
    pub fn render_line(&self) -> String {
        let snap = if self.from_snapshot {
            " [snapshot]"
        } else {
            ""
        };
        let family = match self.family {
            WatchFamily::Route => "route",
            WatchFamily::Nftables => "nftables",
            WatchFamily::Both => "both",
        };
        let what = match &self.kind {
            WatchEventKind::Event { .. } => {
                format!("{} {}", self.kind.name(), self.kind.detail())
            }
            WatchEventKind::Ended {
                reason,
                retry_in_ms,
            } => format!("watch ended: {reason}; resubscribing in {retry_in_ms} ms"),
            WatchEventKind::Gone => "namespace gone; watch stopped".to_string(),
        };
        format!("[{}/{family}]{snap} {}", self.node, what.trim_end())
    }
}

/// The view of an nftables event, or `None` for a generation event:
/// nlink delivers one after every committed batch, nlink-lab's own
/// applies included, so it is not drift.
fn nftables_view(ev: &NftablesEvent) -> Option<EventView> {
    match EventView::from(ev) {
        EventView::NftGen { .. } => None,
        view => Some(view),
    }
}

/// Options for [`watch_loop`].
#[derive(Debug, Clone)]
pub struct WatchOpts {
    pub family: WatchFamily,
    pub json: bool,
    /// Restrict subscription to a single node. `None` =
    /// subscribe to every node in the lab. Plan 159b Phase 3 —
    /// filters PRE-subscription so we don't open connections we
    /// don't need.
    pub node: Option<String>,
    /// Show resync replay frames after ENOBUFS recoveries. By
    /// default they're silenced — the user only sees live
    /// multicast deltas. Plan 159b Phase 3.
    pub include_snapshot: bool,
}

impl Default for WatchOpts {
    fn default() -> Self {
        Self {
            family: WatchFamily::Both,
            json: false,
            node: None,
            include_snapshot: false,
        }
    }
}

/// Run the event tail until Ctrl-C or all per-node tasks exit.
///
/// Subscribes to every node in the running lab on the requested
/// families and prints one line per event to stdout. JSON mode
/// emits one NDJSON record per line for piping to `jq`.
///
/// On per-node connection failure, the offending task exits but
/// the rest of the lab keeps tailing. Errors are written to
/// stderr.
pub async fn watch_loop(lab: &RunningLab, opts: WatchOpts) -> Result<()> {
    let print_json = opts.json;
    let Some((mut rx, tasks)) = watch_stream(lab, &opts)? else {
        return Ok(());
    };

    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if print_json {
                match serde_json::to_string(&ev) {
                    Ok(line) => println!("{line}"),
                    Err(e) => eprintln!("[watch] json encode failed: {e}"),
                }
            } else {
                println!("{}", ev.render_line());
            }
        }
    });

    // Wait for Ctrl-C OR every subscription task to finish (the
    // latter happens if every node fails to subscribe).
    let all_subs = futures_join_all(tasks);
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("watch: Ctrl-C — shutting down");
        }
        _ = all_subs => {
            tracing::info!("watch: all subscriptions ended");
        }
    }
    // Drop the printer task (channel closes when tasks exit).
    drop(printer);
    Ok(())
}

/// The drift channel and the per-node subscription tasks feeding it.
pub type WatchStream = (
    tokio::sync::mpsc::Receiver<WatchEvent>,
    Vec<tokio::task::JoinHandle<()>>,
);

/// Subscribe to the lab's drift events and hand back the stream.
///
/// Returns `None` when no node matches `opts.node`. The channel closes
/// once every subscription task has exited; drop the receiver (or abort
/// the tasks) to stop. `opts.json` is ignored — rendering is the
/// caller's business. Used by [`watch_loop`] and `nlink-lab events`.
pub fn watch_stream(lab: &RunningLab, opts: &WatchOpts) -> Result<Option<WatchStream>> {
    let node_names: Vec<String> = lab
        .topology()
        .nodes
        .keys()
        .filter(|n| match &opts.node {
            Some(target) => target == *n,
            None => true,
        })
        .cloned()
        .collect();
    if node_names.is_empty() {
        if opts.node.is_some() {
            eprintln!(
                "watch: no nodes match the --node filter (target: {:?})",
                opts.node
            );
        }
        return Ok(None);
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<WatchEvent>(1024);
    let mut tasks = Vec::new();
    let include_snapshot = opts.include_snapshot;

    for node in &node_names {
        // Plan 159b Phase 4 — `NsResolver` handles both bare
        // namespaces (name-based) and container nodes (pid-based).
        let resolver = match lab.ns_resolver_of(node) {
            Some(r) => r,
            None => {
                tracing::warn!(
                    node = %node,
                    "skipping watch — no namespace handle (node not running?)"
                );
                continue;
            }
        };

        if opts.family.wants_route() {
            let tx = tx.clone();
            let node = node.clone();
            let r = resolver.clone();
            tasks.push(tokio::spawn(async move {
                supervise(&node, WatchFamily::Route, &r, &tx, || {
                    run_route_subscription(&node, r.clone(), tx.clone(), include_snapshot)
                })
                .await
            }));
        }
        if opts.family.wants_nftables() {
            let tx = tx.clone();
            let node = node.clone();
            let r = resolver.clone();
            tasks.push(tokio::spawn(async move {
                supervise(&node, WatchFamily::Nftables, &r, &tx, || {
                    run_nftables_subscription(&node, r.clone(), tx.clone(), include_snapshot)
                })
                .await
            }));
        }
    }

    // Drop the local sender so the channel closes once all
    // tasks exit.
    drop(tx);
    Ok(Some((rx, tasks)))
}

/// Join a Vec of `JoinHandle<()>` futures, awaiting them all
/// sequentially. We don't pull in `futures_util` just for this.
async fn futures_join_all(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

/// Why a node's subscription returned.
enum Stop {
    /// The receiver is gone: the watch is over.
    Closed,
    /// The stream ended, with the last error it reported.
    Ended(String),
    /// The node's namespace no longer exists.
    Gone,
}

/// How often a subscription checks that its node's namespace still
/// exists. A socket keeps a namespace alive after its name is removed,
/// so the stream itself would never end (#175).
const NAMESPACE_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// Keep a node's subscription running (#175). nlink's resync stream ends
/// after an error other than ENOBUFS; the node's watch used to stop for
/// good then, silently. Now its end is an event, and it is subscribed
/// again after a backoff (200 ms, doubling to 5 s), until the namespace is
/// gone or the watch is over.
async fn supervise<F, Fut>(
    node: &str,
    family: WatchFamily,
    resolver: &NsResolver,
    tx: &tokio::sync::mpsc::Sender<WatchEvent>,
    mut subscribe: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Stop>>,
{
    let mut backoff = std::time::Duration::from_millis(200);
    loop {
        let reason = match subscribe().await {
            Ok(Stop::Closed) => return,
            Ok(Stop::Gone) => None,
            Ok(Stop::Ended(reason)) => Some(reason),
            Err(e) => Some(e.to_string()),
        };
        let event = |kind| WatchEvent {
            node: node.to_owned(),
            family,
            kind,
            from_snapshot: false,
        };
        let Some(reason) = reason.filter(|_| resolver.netns().is_ok()) else {
            let _ = tx.send(event(WatchEventKind::Gone)).await;
            return;
        };
        let ended = WatchEventKind::Ended {
            reason,
            retry_in_ms: u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
        };
        if tx.send(event(ended)).await.is_err() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
    }
}

async fn run_route_subscription(
    node: &str,
    resolver: NsResolver,
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    include_snapshot: bool,
) -> Result<Stop> {
    let conn: Connection<Route> = resolver
        .connection::<Route>()
        .map_err(|e| Error::nlink(format!("watch: route connection for '{node}'"), e))?;

    let resolver_for_factory = resolver.clone();
    let factory: nlink::ConnectionFactory<Route> = Arc::new(move || {
        let r = resolver_for_factory.clone();
        Box::pin(async move { r.connection::<Route>() })
    });

    let mut stream = conn
        .into_events_with_resync(factory)
        .await
        .map_err(|e| Error::nlink(format!("watch: route subscribe for '{node}'"), e))?;

    let mut last_error = String::from("the stream ended");
    let mut check = tokio::time::interval(NAMESPACE_CHECK);
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            _ = check.tick() => {
                if resolver.netns().is_err() {
                    return Ok(Stop::Gone);
                }
                continue;
            }
        };
        let Some(item) = item else {
            return Ok(Stop::Ended(last_error));
        };
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(node = %node, "watch: route stream error: {e}");
                last_error = e.to_string();
                continue;
            }
        };
        let (event, from_snapshot) = match item {
            ResyncedEvent::Event(ev) => (EventView::from(&ev), false),
            ResyncedEvent::Resynced(ev) if include_snapshot => (EventView::from(&ev), true),
            ResyncedEvent::Resynced(_) => continue,
            ResyncedEvent::Marker(_) => continue,
            _ => continue,
        };
        let kind = WatchEventKind::Event { event };
        let watch_ev = WatchEvent {
            node: node.to_owned(),
            family: WatchFamily::Route,
            kind,
            from_snapshot,
        };
        if tx.send(watch_ev).await.is_err() {
            return Ok(Stop::Closed);
        }
    }
}

async fn run_nftables_subscription(
    node: &str,
    resolver: NsResolver,
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    include_snapshot: bool,
) -> Result<Stop> {
    let conn: Connection<Nftables> = resolver
        .connection::<Nftables>()
        .map_err(|e| Error::nlink(format!("watch: nftables connection for '{node}'"), e))?;

    let resolver_for_factory = resolver.clone();
    let factory: nlink::ConnectionFactory<Nftables> = Arc::new(move || {
        let r = resolver_for_factory.clone();
        Box::pin(async move { r.connection::<Nftables>() })
    });

    let mut stream = conn
        .into_events_with_resync(factory)
        .await
        .map_err(|e| Error::nlink(format!("watch: nftables subscribe for '{node}'"), e))?;

    let mut last_error = String::from("the stream ended");
    let mut check = tokio::time::interval(NAMESPACE_CHECK);
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            _ = check.tick() => {
                if resolver.netns().is_err() {
                    return Ok(Stop::Gone);
                }
                continue;
            }
        };
        let Some(item) = item else {
            return Ok(Stop::Ended(last_error));
        };
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(node = %node, "watch: nftables stream error: {e}");
                last_error = e.to_string();
                continue;
            }
        };
        let (event, from_snapshot) = match item {
            ResyncedEvent::Event(ev) => (nftables_view(&ev), false),
            ResyncedEvent::Resynced(ev) if include_snapshot => (nftables_view(&ev), true),
            ResyncedEvent::Resynced(_) => continue,
            ResyncedEvent::Marker(_) => continue,
            _ => continue,
        };
        let Some(event) = event else {
            continue;
        };
        let kind = WatchEventKind::Event { event };
        let watch_ev = WatchEvent {
            node: node.to_owned(),
            family: WatchFamily::Nftables,
            kind,
            from_snapshot,
        };
        if tx.send(watch_ev).await.is_err() {
            return Ok(Stop::Closed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that ends is reported, with its error, and subscribed
    /// again after the backoff (#175); the watch stops when the receiver
    /// is gone.
    #[tokio::test(start_paused = true)]
    async fn a_subscription_that_ends_is_reported_and_restarted() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        supervise("n", WatchFamily::Route, &NsResolver::Root, &tx, || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                match n {
                    0 => Ok(Stop::Ended("EBADF".into())),
                    1 => Err(Error::deploy_failed("subscribe refused")),
                    _ => Ok(Stop::Closed),
                }
            }
        })
        .await;
        assert_eq!(calls.into_inner(), 3);
        let kinds: Vec<WatchEventKind> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| e.kind)
            .collect();
        assert!(
            matches!(
                kinds.as_slice(),
                [
                    WatchEventKind::Ended { reason: a, retry_in_ms: 200 },
                    WatchEventKind::Ended { reason: b, retry_in_ms: 400 },
                ] if a == "EBADF" && b.contains("subscribe refused")
            ),
            "{kinds:?}"
        );
    }

    #[test]
    fn watch_family_subscription_flags() {
        assert!(WatchFamily::Route.wants_route());
        assert!(!WatchFamily::Route.wants_nftables());
        assert!(WatchFamily::Nftables.wants_nftables());
        assert!(!WatchFamily::Nftables.wants_route());
        assert!(WatchFamily::Both.wants_route());
        assert!(WatchFamily::Both.wants_nftables());
    }

    #[test]
    fn watch_opts_defaults() {
        let opts = WatchOpts::default();
        assert_eq!(opts.family, WatchFamily::Both);
        assert!(!opts.json);
        assert!(opts.node.is_none());
        assert!(!opts.include_snapshot);
    }

    /// Plan 159b Phase 3 — `WatchOpts` is `Clone` (the watch
    /// loop clones the node filter into per-task closures).
    #[test]
    fn watch_opts_is_clone() {
        let opts = WatchOpts {
            family: WatchFamily::Route,
            json: true,
            node: Some("router".into()),
            include_snapshot: true,
        };
        let cloned = opts.clone();
        assert_eq!(opts.family, cloned.family);
        assert_eq!(opts.node, cloned.node);
    }

    /// Plan 159b Phase 4 — `NsResolver` distinguishes
    /// name-based (bare namespace) from pid-based (container)
    /// resolution. The watch loop branches on the variant to
    /// build the right `Connection<P>`.
    #[test]
    fn ns_resolver_variants_are_distinguishable() {
        let by_name = NsResolver::Named {
            name: "router".into(),
        };
        let by_pid = NsResolver::Container {
            id: "c1".into(),
            pid: 42,
        };
        assert!(matches!(by_name, NsResolver::Named { ref name } if name == "router"));
        assert!(matches!(by_pid, NsResolver::Container { pid: 42, .. }));
        // Clone — required by the watch loop which spawns one
        // tokio task per (node, family) and gives each task its
        // own owned copy.
        let _: NsResolver = by_name.clone();
        let _: NsResolver = by_pid.clone();
    }

    use nlink::netlink::events::view::Action;

    fn link(name: &str) -> EventView {
        EventView::Link {
            action: Action::New,
            ifindex: 7,
            name: Some(name.into()),
            link_kind: Some("veth".into()),
            mtu: Some(1500),
            operstate: Some("up".into()),
            up: true,
            master: None,
            mac: None,
        }
    }

    fn event(node: &str, family: WatchFamily, kind: WatchEventKind) -> WatchEvent {
        WatchEvent {
            node: node.into(),
            family,
            kind,
            from_snapshot: false,
        }
    }

    /// One flat `kind` beside `node` and `family`, and nlink's view under
    /// `event` with its own `family` (#195). It reads back.
    #[test]
    fn events_serialize_flat_with_the_view_under_event() {
        let ev = event(
            "r1",
            WatchFamily::Route,
            WatchEventKind::Event {
                event: EventView::Route {
                    action: Action::Del,
                    family: "inet".into(),
                    table: 254,
                    dst: "default".into(),
                    gateway: Some("10.0.0.1".into()),
                    oif: Some(2),
                    metric: None,
                    protocol: "static".into(),
                    route_type: "unicast".into(),
                    nhid: None,
                },
            },
        );
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["node"], "r1");
        assert_eq!(v["family"], "route");
        assert_eq!(v["kind"], "event");
        assert_eq!(v["event"]["object"], "route");
        assert_eq!(v["event"]["action"], "del");
        assert_eq!(v["event"]["family"], "inet");
        assert_eq!(v["event"]["gateway"], "10.0.0.1");
        assert!(v.get("from_snapshot").is_none(), "{v}");
        let back: WatchEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, ev.kind);
        assert_eq!(ev.kind.name(), "del_route");

        let ended = event(
            "r1",
            WatchFamily::Nftables,
            WatchEventKind::Ended {
                reason: "EBADF".into(),
                retry_in_ms: 200,
            },
        );
        assert_eq!(
            serde_json::to_string(&ended).unwrap(),
            r#"{"node":"r1","family":"nftables","kind":"ended","reason":"EBADF","retry_in_ms":200}"#
        );
        let gone = event("r1", WatchFamily::Route, WatchEventKind::Gone);
        assert_eq!(
            serde_json::to_string(&gone).unwrap(),
            r#"{"node":"r1","family":"route","kind":"gone"}"#
        );
        assert_eq!(
            (ended.kind.name(), gone.kind.name()),
            ("ended".into(), "gone".into())
        );
    }

    /// The text line: node and family, the event's name, its fields.
    #[test]
    fn events_render_their_name_and_fields() {
        let mut ev = event(
            "router",
            WatchFamily::Route,
            WatchEventKind::Event {
                event: link("eth1"),
            },
        );
        assert_eq!(
            ev.render_line(),
            "[router/route] new_link ifindex=7 link_kind=veth mtu=1500 name=eth1 operstate=up up=true"
        );
        ev.from_snapshot = true;
        assert!(
            ev.render_line()
                .starts_with("[router/route] [snapshot] new_link ")
        );

        let rule = event(
            "fw",
            WatchFamily::Nftables,
            WatchEventKind::Event {
                event: EventView::NftRule {
                    action: Action::New,
                    family: "inet".into(),
                    table: "nlink-lab".into(),
                    chain: "input".into(),
                    handle: 4,
                    counter: Some((3, 180)),
                },
            },
        );
        assert_eq!(
            rule.render_line(),
            "[fw/nftables] new_nft_rule chain=input counter=[3,180] family=inet handle=4 table=nlink-lab"
        );
        assert_eq!(
            event("x", WatchFamily::Route, WatchEventKind::Gone).render_line(),
            "[x/route] namespace gone; watch stopped"
        );
    }
}
