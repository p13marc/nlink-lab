//! Where running labs come from: the daemon (`nlink-labd`), through its
//! client, or the demo's stand-in. One interface for both, so the rest of
//! the app never knows which.

use std::sync::Arc;
use std::time::Duration;

use iced::futures::stream::{self, BoxStream, StreamExt};
use nlink_lab::CancelToken;
use nlink_lab_client::{CallEvent, Client, Endpoint};
use nlink_lab_shared::api::{
    DataStream, ErrorCode, LabSummary, Notification, Request, Response, Role, StreamItem, Topic,
};

use crate::fake::Fake;
use crate::model::Who;

/// What to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Daemon(Endpoint),
    /// Made-up labs, with made-up metrics: `--demo`.
    Demo,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Daemon(e) => e.fmt(f),
            Target::Demo => f.write_str("the demo"),
        }
    }
}

/// What a subscription brings.
#[derive(Debug, Clone, PartialEq)]
pub enum Feed {
    Note(Box<Notification>),
    /// Every lab there is: asked for where the daemon streams nothing.
    Labs(Vec<LabSummary>),
}

/// What a session (`exec`, `logs`, `capture`) brings.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionItem {
    Data(DataStream, Vec<u8>),
    /// The end: the answer, or why not.
    End(Result<Response, String>),
}

impl Feed {
    pub fn note(n: Notification) -> Self {
        Feed::Note(Box::new(n))
    }
}

#[derive(Clone)]
pub enum Backend {
    Daemon(Client),
    Fake(Arc<Fake>),
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Backend::Daemon(_) => "Backend::Daemon",
            Backend::Fake(_) => "Backend::Fake",
        })
    }
}

/// How often a lab's data is asked for when the daemon cannot stream it
/// (over Zenoh, it answers requests but streams nothing).
const POLL: Duration = Duration::from_secs(1);

impl Backend {
    pub async fn connect(target: Target) -> Result<(Backend, Who), String> {
        match target {
            Target::Demo => {
                let fake = Fake::new();
                let who = fake.who();
                Ok((Backend::Fake(Arc::new(fake)), who))
            }
            Target::Daemon(endpoint) => {
                let client = Client::connect(
                    &endpoint,
                    concat!("nlink-lab-gui/", env!("CARGO_PKG_VERSION")),
                )
                .await
                .map_err(|e| e.to_string())?;
                let w = client.welcome();
                let who = Who {
                    server: w.server,
                    host: w.host,
                    user: w.user,
                    role: w.role,
                };
                Ok((Backend::Daemon(client), who))
            }
        }
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Backend::Fake(_))
    }

    /// One request, its answer.
    pub async fn call(&self, request: Request) -> Result<Response, String> {
        match self {
            Backend::Daemon(c) => c.call(request).await.map_err(|e| e.to_string()),
            Backend::Fake(f) => f.call(request),
        }
    }

    /// A session's data until it ends. `cancel` stops it: a command is
    /// sent `SIGINT`, anything else is cancelled.
    pub fn session(self, request: Request, cancel: CancelToken) -> BoxStream<'static, SessionItem> {
        match self {
            Backend::Fake(f) => f.session(request, cancel),
            Backend::Daemon(client) => daemon_session(client, request, cancel),
        }
    }

    /// The notifications of `topics` (of `lab`, or of every lab), until
    /// the subscription ends: an `Err` says why, and is the last item.
    /// Where the daemon streams nothing, the labs and the metrics are
    /// asked for every second instead.
    pub fn subscribe(
        self,
        lab: Option<String>,
        topics: Vec<Topic>,
        interval_ms: u64,
    ) -> BoxStream<'static, Result<Feed, String>> {
        match self {
            Backend::Fake(f) => f.subscribe(lab, topics, interval_ms),
            Backend::Daemon(client) => daemon_stream(client, lab, topics, interval_ms),
        }
    }
}

fn daemon_stream(
    client: Client,
    lab: Option<String>,
    topics: Vec<Topic>,
    interval_ms: u64,
) -> BoxStream<'static, Result<Feed, String>> {
    enum State {
        Start,
        Streaming(nlink_lab_client::Call),
        Polling,
        Done,
    }
    let request = Request::Subscribe {
        lab: lab.clone(),
        topics: topics.clone(),
        interval_ms: Some(interval_ms),
    };
    stream::unfold(State::Start, move |state| {
        let (client, request, lab, topics) =
            (client.clone(), request.clone(), lab.clone(), topics.clone());
        async move {
            match state {
                State::Done => None,
                State::Start => match client.start(request).await {
                    Ok(call) => Some((None, State::Streaming(call))),
                    Err(e) => Some((Some(Err(e.to_string())), State::Done)),
                },
                State::Streaming(mut call) => loop {
                    match call.next().await {
                        Some(CallEvent::Item(StreamItem::Notification { notification })) => {
                            return Some((
                                Some(Ok(Feed::note(notification))),
                                State::Streaming(call),
                            ));
                        }
                        Some(CallEvent::End(Err(e))) if e.code == ErrorCode::Unsupported => {
                            return Some((None, State::Polling));
                        }
                        Some(CallEvent::End(Err(e))) => {
                            return Some((Some(Err(e.message)), State::Done));
                        }
                        Some(CallEvent::End(Ok(_))) => {
                            return Some((Some(Err("the subscription ended".into())), State::Done));
                        }
                        Some(_) => continue,
                        None => {
                            return Some((
                                Some(Err("the connection to the daemon closed".into())),
                                State::Done,
                            ));
                        }
                    }
                },
                State::Polling => {
                    tokio::time::sleep(POLL).await;
                    let polled = poll(&client, lab.as_deref(), &topics).await;
                    let next = if polled.is_err() {
                        State::Done
                    } else {
                        State::Polling
                    };
                    Some((Some(polled), next))
                }
            }
        }
    })
    .filter_map(|item| async move { item })
    .boxed()
}

fn daemon_session(
    client: Client,
    request: Request,
    cancel: CancelToken,
) -> BoxStream<'static, SessionItem> {
    enum State {
        Start(Box<Request>),
        Running {
            call: nlink_lab_client::Call,
            stopping: bool,
        },
        Done,
    }
    let signal = matches!(request, Request::Exec { .. });
    stream::unfold(State::Start(Box::new(request)), move |state| {
        let (client, cancel) = (client.clone(), cancel.clone());
        async move {
            match state {
                State::Done => None,
                State::Start(request) => match client.start(*request).await {
                    Ok(call) => Some((
                        None,
                        State::Running {
                            call,
                            stopping: false,
                        },
                    )),
                    Err(e) => Some((Some(SessionItem::End(Err(e.to_string()))), State::Done)),
                },
                State::Running { mut call, stopping } => loop {
                    let event = tokio::select! {
                        () = cancel.cancelled(), if !stopping => {
                            let sent = if signal { call.signal("INT") } else { call.cancel() };
                            if sent.is_err() {
                                return Some((Some(SessionItem::End(Err("stopped".into()))), State::Done));
                            }
                            return Some((None, State::Running { call, stopping: true }));
                        }
                        event = call.next() => event,
                    };
                    match event {
                        Some(CallEvent::Data(stream, bytes)) => {
                            return Some((
                                Some(SessionItem::Data(stream, bytes)),
                                State::Running { call, stopping },
                            ));
                        }
                        Some(CallEvent::End(r)) => {
                            return Some((Some(SessionItem::End(r.map_err(|e| e.message))), State::Done));
                        }
                        Some(CallEvent::Item(_)) => continue,
                        None => {
                            return Some((
                                Some(SessionItem::End(Err("the connection to the daemon closed".into()))),
                                State::Done,
                            ));
                        }
                    }
                },
            }
        }
    })
    .filter_map(|item| async move { item })
    .boxed()
}

/// A subscription's notification, asked for: the labs, or a lab's
/// metrics.
async fn poll(client: &Client, lab: Option<&str>, topics: &[Topic]) -> Result<Feed, String> {
    match lab {
        Some(lab) if topics.contains(&Topic::Metrics) => {
            match client.call(Request::GetMetrics { lab: lab.into() }).await {
                Ok(Response::Metrics { snapshot }) => Ok(Feed::note(Notification::Metrics {
                    lab: lab.into(),
                    snapshot,
                })),
                Ok(other) => Err(format!("unexpected answer {other:?}")),
                Err(e) => Err(e.to_string()),
            }
        }
        _ => match client.call(Request::ListLabs).await {
            Ok(Response::Labs { labs }) => Ok(Feed::Labs(labs)),
            Ok(other) => Err(format!("unexpected answer {other:?}")),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// What a role may do, said plainly.
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Operator => "operator",
        Role::Viewer => "viewer",
    }
}
