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

/// Wrapper to carry an `Arc<Session>` through `Subscription::run_with`.
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
    let mut parts = split_command(&input).into_iter();
    let cmd = parts.next().ok_or("empty command")?;
    let args: Vec<String> = parts.collect();

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
            Err(e) => Err(format!("query error: {e}")),
        },
        Err(_) => Err("no reply received".to_string()),
    }
}

/// Ask the backend for a lab's current topology.
///
/// The topology is published once at daemon startup, so a viewer that
/// connects later never sees it on the subscription (issue #48). This
/// queries the same key, which the backend answers with a queryable.
pub async fn query_topology(session: Arc<zenoh::Session>, lab: String) -> Option<TopologyUpdate> {
    let topic = nlink_lab_shared::topics::topology(&lab);
    let replies = match session.get(&topic).await {
        Ok(replies) => replies,
        Err(e) => {
            eprintln!("Zenoh topology query failed: {e}");
            return None;
        }
    };
    let reply = replies.recv_async().await.ok()?;
    let sample = reply.result().ok()?;
    match serde_json::from_slice::<TopologyUpdate>(&sample.payload().to_bytes()) {
        Ok(update) => Some(update),
        Err(e) => {
            eprintln!("Zenoh topology query returned an undecodable reply: {e}");
            None
        }
    }
}

/// Split a command line into argv, honouring single and double quotes and
/// backslash escapes.
///
/// `split_whitespace` mangled every argument containing a space and left
/// the quote characters in place (issue #48). This is not a shell: no
/// expansion, no operators — just quoting, so `ip addr add "10.0.0.1/24"`
/// reaches the node as one argument.
pub fn split_command(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                started = true;
                // A backslash escapes the next character, except inside
                // single quotes where it is literal (as in sh).
                if quote == Some('\'') {
                    current.push(c);
                } else if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' | '"' => {
                started = true;
                match quote {
                    Some(q) if q == c => quote = None,
                    Some(_) => current.push(c),
                    None => quote = Some(c),
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }
    if started {
        out.push(current);
    }
    out
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
                    // `Noop`, not an empty map: an empty `MetricsReceived`
                    // wipes the metrics the user is looking at (issue #48).
                    Ok(sub) => Some((Message::Noop, State::Receiving(sub))),
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
                            // Keep the last good sample rather than clearing.
                            Message::Noop
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

#[cfg(test)]
mod tests {
    use super::split_command;

    #[test]
    fn splits_a_plain_command() {
        assert_eq!(split_command("ip addr"), vec!["ip", "addr"]);
    }

    #[test]
    fn keeps_a_quoted_argument_together_and_drops_the_quotes() {
        assert_eq!(
            split_command(r#"sh -c "ip addr add 10.0.0.1/24 dev eth0""#),
            vec!["sh", "-c", "ip addr add 10.0.0.1/24 dev eth0"]
        );
        assert_eq!(
            split_command("sh -c 'echo hello world'"),
            vec!["sh", "-c", "echo hello world"]
        );
    }

    #[test]
    fn a_quote_inside_the_other_quote_is_literal() {
        assert_eq!(
            split_command(r#"echo "it's fine""#),
            vec!["echo", "it's fine"]
        );
    }

    #[test]
    fn backslash_escapes_outside_single_quotes() {
        assert_eq!(split_command(r"echo a\ b"), vec!["echo", "a b"]);
        assert_eq!(split_command(r"echo a\'b"), vec!["echo", "a'b"]);
        // …but is literal inside them, as in sh.
        assert_eq!(split_command(r"echo 'a\b'"), vec!["echo", r"a\b"]);
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(split_command("  ip   addr  "), vec!["ip", "addr"]);
    }

    #[test]
    fn an_empty_quoted_argument_survives() {
        assert_eq!(split_command(r#"echo "" x"#), vec!["echo", "", "x"]);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(split_command("").is_empty());
        assert!(split_command("   ").is_empty());
    }
}
