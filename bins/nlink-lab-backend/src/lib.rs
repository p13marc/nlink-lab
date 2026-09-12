//! nlink-lab-backend: Zenoh backend daemon for live lab monitoring.
//!
//! Runs as root (or CAP_NET_ADMIN), collects metrics from a deployed lab,
//! and exposes them via Zenoh pub/sub and query/reply. The crate is a
//! library so the `nlink-lab daemon` CLI arm and the standalone
//! `nlink-lab-backend` binary share one implementation:
//!
//! ```no_run
//! # async fn demo() -> Result<(), nlink_lab_backend::Error> {
//! let lab = nlink_lab::RunningLab::load("simple")?;
//! let opts = nlink_lab_backend::BackendOpts::default();
//! nlink_lab_backend::run(lab, opts).await // returns on Ctrl-C
//! # }
//! ```
//!
//! What the daemon does, per lab:
//!
//! - publishes the topology once on the `topology` topic and holds a
//!   liveliness token on the `health` key;
//! - publishes `HealthStatus`
//!   every [`HEALTH_INTERVAL`] and a
//!   [`MetricsSnapshot`](nlink_lab_shared::metrics::MetricsSnapshot) (plus
//!   one per-interface sample) every [`BackendOpts::interval`];
//! - emits [`LabEvent`](nlink_lab_shared::messages::LabEvent)s on the
//!   `events` topic when an interface changes state or a tracked
//!   background process exits;
//! - serves the `rpc/exec`, `rpc/impairment` and `rpc/status` queryables.

// `nlink_lab::Error` is deliberately unboxed (see the Plan 159f note in
// `nlink-lab/src/error.rs`); match the allow the library and CLI already carry.
#![allow(clippy::result_large_err)]

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use nlink_lab::RunningLab;
use nlink_lab_shared::messages::{HealthStatus, TopologyUpdate};
use nlink_lab_shared::topics;
use serde::Serialize;
use tracing::{info, warn};
use zenoh::config::EndPoint;

pub mod collector;
pub mod handlers;

/// How often [`HealthStatus`] is published, independent of the metrics
/// interval.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

/// Errors the backend can fail with.
///
/// Converts into [`nlink_lab::Error`] (as `DeployFailed`, or the wrapped
/// lab error itself) so callers on the library's error path can `?` it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `zenoh_mode` was neither `peer` nor `client`.
    #[error("unknown zenoh mode '{0}' (expected 'peer' or 'client')")]
    InvalidMode(String),

    /// A listen/connect endpoint did not parse (e.g. `tcp/0.0.0.0:7447`).
    #[error("invalid zenoh {kind} endpoint '{endpoint}': {reason}")]
    InvalidEndpoint {
        kind: &'static str,
        endpoint: String,
        reason: String,
    },

    /// The metrics interval must be non-zero (tokio's interval panics on
    /// a zero period).
    #[error("metrics interval must be greater than zero")]
    ZeroInterval,

    /// zenoh rejected a config key we inserted.
    #[error("bad zenoh config ({key}): {reason}")]
    Config { key: &'static str, reason: String },

    /// `zenoh::open` failed.
    #[error("failed to open zenoh session: {0}")]
    Session(String),

    /// Declaring a publisher/queryable/liveliness token failed.
    #[error("failed to declare zenoh {what}: {reason}")]
    Declare { what: &'static str, reason: String },

    /// A publish that must succeed (the initial topology) failed.
    #[error("failed to publish {what}: {reason}")]
    Publish { what: &'static str, reason: String },

    /// A wire message failed to serialize.
    #[error("failed to serialize {what}: {source}")]
    Serialize {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },

    /// An error from the lab itself (loading state, diagnosing, ...).
    #[error(transparent)]
    Lab(#[from] nlink_lab::Error),
}

impl From<Error> for nlink_lab::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Lab(inner) => inner,
            other => nlink_lab::Error::deploy_failed(other.to_string()),
        }
    }
}

/// Zenoh session mode the backend opens with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZenohMode {
    /// Peer-to-peer (default): discovers other peers via multicast scouting
    /// and accepts connections on `listen` endpoints.
    #[default]
    Peer,
    /// Client: only talks to the routers/peers in `connect`.
    Client,
}

impl ZenohMode {
    /// The mode name as zenoh's config spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peer => "peer",
            Self::Client => "client",
        }
    }
}

impl fmt::Display for ZenohMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ZenohMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        match s.trim().to_ascii_lowercase().as_str() {
            "peer" => Ok(Self::Peer),
            "client" => Ok(Self::Client),
            _ => Err(Error::InvalidMode(s.to_string())),
        }
    }
}

/// Daemon options — mirrors the `nlink-lab-backend` / `nlink-lab daemon`
/// CLI flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendOpts {
    /// Metrics collection/publish interval (default 2 s). Must be non-zero.
    pub interval: Duration,
    /// Zenoh session mode (default peer).
    pub zenoh_mode: ZenohMode,
    /// Zenoh listen endpoints, e.g. `tcp/0.0.0.0:7447` (default: zenoh's own).
    pub zenoh_listen: Vec<String>,
    /// Zenoh connect endpoints, e.g. `tcp/127.0.0.1:7447` (default: none).
    pub zenoh_connect: Vec<String>,
}

impl Default for BackendOpts {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            zenoh_mode: ZenohMode::Peer,
            zenoh_listen: Vec::new(),
            zenoh_connect: Vec::new(),
        }
    }
}

/// Build the zenoh [`Config`](zenoh::Config) for `opts`.
///
/// Pure: validates every endpoint up front and never touches the network.
/// Empty endpoint lists leave zenoh's defaults in place.
pub fn zenoh_config(opts: &BackendOpts) -> Result<zenoh::Config, Error> {
    let mut config = zenoh::Config::default();

    config
        .insert_json5("mode", &format!("\"{}\"", opts.zenoh_mode))
        .map_err(|e| Error::Config {
            key: "mode",
            reason: e.to_string(),
        })?;

    insert_endpoints(
        &mut config,
        "listen",
        "listen/endpoints",
        &opts.zenoh_listen,
    )?;
    insert_endpoints(
        &mut config,
        "connect",
        "connect/endpoints",
        &opts.zenoh_connect,
    )?;

    Ok(config)
}

fn insert_endpoints(
    config: &mut zenoh::Config,
    kind: &'static str,
    key: &'static str,
    endpoints: &[String],
) -> Result<(), Error> {
    if endpoints.is_empty() {
        return Ok(());
    }
    for ep in endpoints {
        EndPoint::from_str(ep).map_err(|e| Error::InvalidEndpoint {
            kind,
            endpoint: ep.clone(),
            reason: e.to_string(),
        })?;
    }
    // Serialize through serde_json so quotes/escapes inside an endpoint
    // can never break the JSON5 literal.
    let json = serde_json::to_string(endpoints).map_err(|e| Error::Serialize {
        what: "zenoh endpoints",
        source: e,
    })?;
    config.insert_json5(key, &json).map_err(|e| Error::Config {
        key,
        reason: e.to_string(),
    })
}

/// Run the backend for `lab` until Ctrl-C.
///
/// Opens its own zenoh session from [`zenoh_config`]. Errors during setup
/// are returned; publish failures inside the loop are `warn!`-logged and
/// the loop carries on.
pub async fn run(lab: RunningLab, opts: BackendOpts) -> Result<(), Error> {
    run_until(lab, opts, async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!("ctrl-c handler unavailable: {e}; running until dropped");
            std::future::pending::<()>().await;
        }
    })
    .await
}

/// Run the backend for `lab` until `shutdown` resolves (or the returned
/// future is dropped).
///
/// Opens its own zenoh session; see [`serve`] to supply one.
pub async fn run_until(
    lab: RunningLab,
    opts: BackendOpts,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let config = zenoh_config(&opts)?;
    let session = zenoh::open(config)
        .await
        .map_err(|e| Error::Session(e.to_string()))?;
    info!(mode = %opts.zenoh_mode, "Zenoh session opened");

    let result = serve(&session, lab, &opts, shutdown).await;

    if let Err(e) = session.close().await {
        warn!("closing zenoh session: {e}");
    }
    result
}

/// Serve `lab` on an already-open zenoh `session` until `shutdown`
/// resolves. Declares publishers/queryables on the session; they are
/// undeclared when this future completes.
pub async fn serve(
    session: &zenoh::Session,
    mut lab: RunningLab,
    opts: &BackendOpts,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    if opts.interval.is_zero() {
        return Err(Error::ZeroInterval);
    }

    let lab_name = lab.name().to_string();
    let start_time = Instant::now();

    // ── Publishers ──────────────────────────────────────────
    let topo_publisher =
        declare_publisher(session, "topology publisher", topics::topology(&lab_name)).await?;
    let health_publisher =
        declare_publisher(session, "health publisher", topics::health(&lab_name)).await?;
    let snapshot_publisher = declare_publisher(
        session,
        "snapshot publisher",
        topics::metrics_snapshot(&lab_name),
    )
    .await?;
    let events_publisher =
        declare_publisher(session, "events publisher", topics::events(&lab_name)).await?;

    // ── Queryables ─────────────────────────────────────────
    let exec_queryable =
        declare_queryable(session, "exec queryable", topics::rpc_exec(&lab_name)).await?;
    let impair_queryable = declare_queryable(
        session,
        "impairment queryable",
        topics::rpc_impairment(&lab_name),
    )
    .await?;
    let status_queryable =
        declare_queryable(session, "status queryable", topics::rpc_status(&lab_name)).await?;

    // ── Publish initial topology ───────────────────────────
    let topo = lab.topology();
    let topo_update = TopologyUpdate {
        wire_version: nlink_lab_shared::WIRE_VERSION,
        lab_name: lab_name.clone(),
        timestamp: now_unix(),
        node_count: topo.nodes.len(),
        link_count: topo.links.len(),
        topology_json: serde_json::to_string(topo).map_err(|e| Error::Serialize {
            what: "topology",
            source: e,
        })?,
    };
    let topo_bytes = serde_json::to_vec(&topo_update).map_err(|e| Error::Serialize {
        what: "topology update",
        source: e,
    })?;
    topo_publisher
        .put(topo_bytes)
        .await
        .map_err(|e| Error::Publish {
            what: "topology",
            reason: e.to_string(),
        })?;
    info!("published initial topology");

    // ── Liveliness token ───────────────────────────────────
    let _token = session
        .liveliness()
        .declare_token(topics::health(&lab_name))
        .await
        .map_err(|e| Error::Declare {
            what: "liveliness token",
            reason: e.to_string(),
        })?;

    // ── Main event loop ────────────────────────────────────
    let mut collector = collector::MetricsCollector::new(&lab);
    let mut health_interval = tokio::time::interval(HEALTH_INTERVAL);
    let mut metrics_interval = tokio::time::interval(opts.interval);
    // A slow `diagnose` must not be followed by a burst of catch-up ticks.
    metrics_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut shutdown = std::pin::pin!(shutdown);

    info!(
        lab = lab_name,
        interval_ms = opts.interval.as_millis() as u64,
        "backend daemon running"
    );

    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                match collector.snapshot(&lab).await {
                    Ok((snapshot, events)) => {
                        for event in &events {
                            if let Some(bytes) = encode("event", event)
                                && let Err(e) = events_publisher.put(bytes).await
                            {
                                warn!("publish event: {e}");
                            }
                        }
                        for (node_name, node_metrics) in &snapshot.nodes {
                            for iface in &node_metrics.interfaces {
                                let topic = topics::metrics_iface(&lab_name, node_name, &iface.name);
                                if let Some(bytes) = encode("interface metrics", iface)
                                    && let Err(e) = session.put(&topic, bytes).await
                                {
                                    warn!("publish iface metrics {topic}: {e}");
                                }
                            }
                        }
                        if let Some(bytes) = encode("metrics snapshot", &snapshot)
                            && let Err(e) = snapshot_publisher.put(bytes).await
                        {
                            warn!("publish metrics snapshot: {e}");
                        }
                    }
                    Err(e) => warn!("metrics collection: {e}"),
                }
            }

            _ = health_interval.tick() => {
                let status = HealthStatus {
                    wire_version: nlink_lab_shared::WIRE_VERSION,
                    lab_name: lab_name.clone(),
                    timestamp: now_unix(),
                    node_count: lab.topology().nodes.len(),
                    namespace_count: lab.namespace_count(),
                    container_count: lab.containers().len(),
                    pid_count: lab.process_status().len(),
                    uptime_secs: start_time.elapsed().as_secs(),
                };
                if let Some(bytes) = encode("health status", &status)
                    && let Err(e) = health_publisher.put(bytes).await
                {
                    warn!("publish health: {e}");
                }
            }

            Ok(query) = exec_queryable.recv_async() => {
                handlers::handle_exec(&lab, query).await;
            }

            Ok(query) = impair_queryable.recv_async() => {
                handlers::handle_impairment(&mut lab, query).await;
            }

            Ok(query) = status_queryable.recv_async() => {
                handlers::handle_status(&lab, start_time, query).await;
            }

            () = &mut shutdown => {
                info!(lab = lab_name, "shutting down");
                break;
            }
        }
    }

    Ok(())
}

async fn declare_publisher<'a>(
    session: &'a zenoh::Session,
    what: &'static str,
    key: String,
) -> Result<zenoh::pubsub::Publisher<'a>, Error> {
    session
        .declare_publisher(key)
        .await
        .map_err(|e| Error::Declare {
            what,
            reason: e.to_string(),
        })
}

async fn declare_queryable(
    session: &zenoh::Session,
    what: &'static str,
    key: String,
) -> Result<zenoh::query::Queryable<zenoh::handlers::FifoChannelHandler<zenoh::query::Query>>, Error>
{
    session
        .declare_queryable(key)
        .await
        .map_err(|e| Error::Declare {
            what,
            reason: e.to_string(),
        })
}

/// Serialize a wire message, logging (not swallowing) a failure.
fn encode<T: Serialize>(what: &str, value: &T) -> Option<Vec<u8>> {
    match serde_json::to_vec(value) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            warn!("serialize {what}: {e}");
            None
        }
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_at(config: &zenoh::Config, key: &str) -> serde_json::Value {
        let raw = config.get_json(key).expect("key exists");
        serde_json::from_str(&raw).expect("valid json")
    }

    #[test]
    fn defaults_are_two_seconds_peer_no_endpoints() {
        let opts = BackendOpts::default();
        assert_eq!(opts.interval, Duration::from_secs(2));
        assert_eq!(opts.zenoh_mode, ZenohMode::Peer);
        assert!(opts.zenoh_listen.is_empty());
        assert!(opts.zenoh_connect.is_empty());
    }

    #[test]
    fn mode_parses_case_insensitively_and_rejects_unknown() {
        assert_eq!("peer".parse::<ZenohMode>().unwrap(), ZenohMode::Peer);
        assert_eq!("Client".parse::<ZenohMode>().unwrap(), ZenohMode::Client);
        assert!(matches!(
            "router".parse::<ZenohMode>(),
            Err(Error::InvalidMode(m)) if m == "router"
        ));
    }

    #[test]
    fn config_peer_mode() {
        let config = zenoh_config(&BackendOpts::default()).unwrap();
        assert_eq!(json_at(&config, "mode"), serde_json::json!("peer"));
    }

    #[test]
    fn config_client_mode() {
        let opts = BackendOpts {
            zenoh_mode: ZenohMode::Client,
            ..Default::default()
        };
        let config = zenoh_config(&opts).unwrap();
        assert_eq!(json_at(&config, "mode"), serde_json::json!("client"));
    }

    #[test]
    fn config_listen_and_connect_endpoints() {
        let opts = BackendOpts {
            zenoh_listen: vec!["tcp/0.0.0.0:7447".into(), "tcp/[::]:7448".into()],
            zenoh_connect: vec!["tcp/127.0.0.1:7447".into()],
            ..Default::default()
        };
        let config = zenoh_config(&opts).unwrap();
        let listen = config.get_json("listen/endpoints").unwrap();
        assert!(listen.contains("tcp/0.0.0.0:7447"), "listen: {listen}");
        assert!(listen.contains("tcp/[::]:7448"), "listen: {listen}");
        let connect = config.get_json("connect/endpoints").unwrap();
        assert!(connect.contains("tcp/127.0.0.1:7447"), "connect: {connect}");
    }

    #[test]
    fn config_rejects_bad_endpoint() {
        let opts = BackendOpts {
            zenoh_listen: vec!["not an endpoint".into()],
            ..Default::default()
        };
        match zenoh_config(&opts) {
            Err(Error::InvalidEndpoint { kind, endpoint, .. }) => {
                assert_eq!(kind, "listen");
                assert_eq!(endpoint, "not an endpoint");
            }
            other => panic!("expected InvalidEndpoint, got {other:?}"),
        }

        let opts = BackendOpts {
            zenoh_connect: vec![String::new()],
            ..Default::default()
        };
        assert!(matches!(
            zenoh_config(&opts),
            Err(Error::InvalidEndpoint {
                kind: "connect",
                ..
            })
        ));
    }

    #[test]
    fn error_converts_into_lab_error() {
        let e: nlink_lab::Error = Error::ZeroInterval.into();
        assert!(e.to_string().contains("interval"));

        let inner = nlink_lab::Error::deploy_failed("boom");
        let e: nlink_lab::Error = Error::Lab(inner).into();
        assert!(e.to_string().contains("boom"));
    }
}
