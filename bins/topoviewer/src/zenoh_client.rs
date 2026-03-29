//! Zenoh client — session management, subscriptions, and queries.

use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;

use iced::Subscription;
use iced::futures::Stream;
use iced::futures::stream::unfold;

use nlink_lab_shared::messages::{ExecRequest, ExecResponse, HealthStatus, TopologyUpdate};
use nlink_lab_shared::metrics::MetricsSnapshot;

use crate::app::Message;

// ─── Session management ──────────────────────────────────

/// Open a Zenoh session with the given config.
pub async fn open_session(config: zenoh::Config) -> Option<Arc<zenoh::Session>> {
    match zenoh::open(config).await {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!("Zenoh connect failed: {e}");
            None
        }
    }
}

// ─── Subscription identity ───────────────────────────────

/// Wrapper to carry an Arc<Session> through Subscription::run_with.
/// Hashes by the string key only (session identity is irrelevant for dedup).
struct SubKey {
    key: String,
    session: Arc<zenoh::Session>,
}

impl Hash for SubKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

struct MetricsSubKey {
    lab: String,
    session: Arc<zenoh::Session>,
}

impl Hash for MetricsSubKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "metrics".hash(state);
        self.lab.hash(state);
    }
}

// ─── Subscriptions ───────────────────────────────────────

/// Subscribe to live metrics for a specific lab.
pub fn metrics_subscription(
    session: Arc<zenoh::Session>,
    lab_name: String,
) -> Subscription<Message> {
    Subscription::run_with(
        MetricsSubKey {
            lab: lab_name,
            session,
        },
        create_metrics_stream,
    )
}

/// Subscribe to health status from all labs (wildcard).
pub fn health_subscription(session: Arc<zenoh::Session>) -> Subscription<Message> {
    Subscription::run_with(
        SubKey {
            key: "health".into(),
            session,
        },
        create_health_stream,
    )
}

/// Subscribe to topology updates from all labs (wildcard).
pub fn topology_subscription(session: Arc<zenoh::Session>) -> Subscription<Message> {
    Subscription::run_with(
        SubKey {
            key: "topology".into(),
            session,
        },
        create_topology_stream,
    )
}

// ─── Queries ─────────────────────────────────────────────

/// Execute a command in a lab node via Zenoh RPC.
pub async fn exec_command(
    session: Arc<zenoh::Session>,
    lab: String,
    node: String,
    input: String,
) -> Result<ExecResponse, String> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.first().ok_or("empty command")?.to_string();
    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    let request = ExecRequest { node, cmd, args };
    let payload = serde_json::to_string(&request).map_err(|e| e.to_string())?;

    let topic = nlink_lab_shared::topics::rpc_exec(&lab);
    let replies = session
        .get(&topic)
        .payload(payload)
        .await
        .map_err(|e| format!("zenoh get: {e}"))?;

    match replies.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(sample) => {
                let bytes = sample.payload().to_bytes();
                serde_json::from_slice::<ExecResponse>(&bytes)
                    .map_err(|e| format!("deserialize: {e}"))
            }
            Err(e) => Err(format!("query error: {e:?}")),
        },
        Err(_) => Err("no reply received".to_string()),
    }
}

// ─── Stream helpers ──────────────────────────────────────

type Subscriber =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

fn create_metrics_stream(key: &MetricsSubKey) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let session = key.session.clone();
    let lab = key.lab.clone();

    enum State {
        Starting(Arc<zenoh::Session>, String),
        Receiving(Subscriber),
    }

    Box::pin(unfold(State::Starting(session, lab), |state| async move {
        match state {
            State::Starting(session, lab) => {
                let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
                match session.declare_subscriber(&topic).await {
                    Ok(sub) => Some((
                        Message::MetricsReceived(Default::default()),
                        State::Receiving(sub),
                    )),
                    Err(e) => {
                        eprintln!("Zenoh metrics subscribe failed: {e}");
                        None
                    }
                }
            }
            State::Receiving(sub) => match sub.recv_async().await {
                Ok(sample) => {
                    let payload = sample.payload().to_bytes();
                    let msg =
                        if let Ok(snapshot) = serde_json::from_slice::<MetricsSnapshot>(&payload) {
                            Message::MetricsReceived(snapshot.nodes)
                        } else {
                            Message::MetricsReceived(Default::default())
                        };
                    Some((msg, State::Receiving(sub)))
                }
                Err(_) => None,
            },
        }
    }))
}

fn create_health_stream(key: &SubKey) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let session = key.session.clone();

    enum State {
        Starting(Arc<zenoh::Session>),
        Receiving(Subscriber),
    }

    Box::pin(unfold(State::Starting(session), |state| async move {
        match state {
            State::Starting(session) => {
                let topic = nlink_lab_shared::topics::all_health();
                match session.declare_subscriber(topic).await {
                    Ok(sub) => Some((Message::Noop, State::Receiving(sub))),
                    Err(e) => {
                        eprintln!("Zenoh health subscribe failed: {e}");
                        None
                    }
                }
            }
            State::Receiving(sub) => match sub.recv_async().await {
                Ok(sample) => {
                    let payload = sample.payload().to_bytes();
                    let msg = if let Ok(status) = serde_json::from_slice::<HealthStatus>(&payload) {
                        Message::HealthReceived(status)
                    } else {
                        Message::Noop
                    };
                    Some((msg, State::Receiving(sub)))
                }
                Err(_) => None,
            },
        }
    }))
}

fn create_topology_stream(key: &SubKey) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let session = key.session.clone();

    enum State {
        Starting(Arc<zenoh::Session>),
        Receiving(Subscriber),
    }

    Box::pin(unfold(State::Starting(session), |state| async move {
        match state {
            State::Starting(session) => {
                let topic = nlink_lab_shared::topics::all_topologies();
                match session.declare_subscriber(topic).await {
                    Ok(sub) => Some((Message::Noop, State::Receiving(sub))),
                    Err(e) => {
                        eprintln!("Zenoh topology subscribe failed: {e}");
                        None
                    }
                }
            }
            State::Receiving(sub) => match sub.recv_async().await {
                Ok(sample) => {
                    let payload = sample.payload().to_bytes();
                    let msg = if let Ok(update) = serde_json::from_slice::<TopologyUpdate>(&payload)
                    {
                        Message::TopologyReceived(update)
                    } else {
                        Message::Noop
                    };
                    Some((msg, State::Receiving(sub)))
                }
                Err(_) => None,
            },
        }
    }))
}
