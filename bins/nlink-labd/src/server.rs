//! Connections and calls: the handshake, the role check, one task per
//! call, what a call receives after it starts, and the end of every call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use nlink_lab::CancelToken;
use nlink_lab_shared::api::{
    API_VERSION, ApiError, ClientMsg, DataStream, ErrorCode, Frame, OnDisconnect, Request,
    Response, Role, ServerMsg, StreamItem, api_major, api_version_string,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::audit::Audit;
use crate::authz::{self, Groups, Peer};
use crate::hub::Hub;

/// What every connection shares.
pub(crate) struct Shared {
    pub groups: Groups,
    pub hub: Arc<Hub>,
    pub jobs: Arc<crate::jobs::Jobs>,
    pub audit: Audit,
    pub started: Instant,
    pub clients: AtomicUsize,
    pub host: String,
}

/// The daemon's name and version, as the handshake says it.
pub(crate) fn server_name() -> String {
    format!("nlink-labd/{}", env!("CARGO_PKG_VERSION"))
}

/// What the daemon serves beyond the API's version.
const FEATURES: &[&str] = &[
    "read",
    "subscribe",
    "exec",
    "operations",
    "jobs",
    "sessions",
    // `conntrack`, `flush_conntrack` (API 1.1, #303)
    "conntrack",
    // a deploy's `seed`, an impairment's `seed` (API 1.2, #337)
    "seed",
];

/// What a running call receives from its client.
#[derive(Debug)]
pub(crate) enum Input {
    Stdin(Vec<u8>),
    Eof,
    #[allow(dead_code)] // the pty sessions read it
    Resize {
        rows: u16,
        cols: u16,
    },
    Signal(String),
}

struct CallHandle {
    cancel: CancelToken,
    detach: CancelToken,
    on_disconnect: OnDisconnect,
    input: mpsc::UnboundedSender<Input>,
}

impl CallHandle {
    /// The client leaves the call (`detach`, or it went away): a long
    /// operation goes on, anything else stops.
    fn leave(&self) {
        match self.on_disconnect {
            OnDisconnect::Continue => self.detach.cancel(),
            OnDisconnect::Cancel => self.cancel.cancel(),
        }
    }
}

type Calls = Arc<Mutex<HashMap<u32, CallHandle>>>;

/// A running call: where its items go, what its client sends it, whether
/// it was cancelled, and who asked.
pub(crate) struct CallCtx {
    pub id: u32,
    out: mpsc::Sender<Frame>,
    pub cancel: CancelToken,
    /// Set when the client leaves a long operation running.
    pub detached: CancelToken,
    pub input: mpsc::UnboundedReceiver<Input>,
    pub peer: Arc<Peer>,
    pub role: Role,
    pub shared: Arc<Shared>,
}

impl CallCtx {
    /// A call that comes from no connection (a Zenoh query): its frames go
    /// to the returned receiver, and no input ever comes.
    #[cfg_attr(not(feature = "zenoh"), allow(dead_code))]
    pub fn standalone(
        shared: Arc<Shared>,
        peer: Arc<Peer>,
        role: Role,
    ) -> (Self, mpsc::Receiver<Frame>) {
        let (out, frames) = mpsc::channel(256);
        let (_, input) = mpsc::unbounded_channel();
        let ctx = Self {
            id: 1,
            out,
            cancel: CancelToken::new(),
            detached: CancelToken::new(),
            input,
            peer,
            role,
            shared,
        };
        (ctx, frames)
    }

    /// End the call with `result`.
    #[cfg_attr(not(feature = "zenoh"), allow(dead_code))]
    pub async fn end(&self, result: Result<Response, ApiError>) {
        let _ = self.out.send(end_frame(self.id, result)).await;
    }

    /// Stream an item; `false` once the client is gone.
    pub async fn item(&self, item: StreamItem) -> bool {
        self.out
            .send(Frame::message(self.id, &ServerMsg::Event { item }))
            .await
            .is_ok()
    }

    /// What streams the call's bytes, from a task of its own.
    pub fn data_sender(&self) -> DataSender {
        DataSender {
            call: self.id,
            out: self.out.clone(),
        }
    }
}

/// Streams a call's bytes.
pub(crate) struct DataSender {
    call: u32,
    out: mpsc::Sender<Frame>,
}

impl DataSender {
    /// `false` once the client is gone.
    pub async fn send(&self, stream: DataStream, bytes: Vec<u8>) -> bool {
        self.out
            .send(Frame::Data {
                call: self.call,
                stream,
                bytes,
            })
            .await
            .is_ok()
    }

    /// [`send`](Self::send) from a thread that may block (not the event
    /// loop's).
    pub fn blocking_send(&self, stream: DataStream, bytes: Vec<u8>) -> bool {
        self.out
            .blocking_send(Frame::Data {
                call: self.call,
                stream,
                bytes,
            })
            .is_ok()
    }

    /// A writer of `stream`, for a blocking producer (a log follower, a
    /// capture): a write fails once the client is gone, which ends it.
    pub fn writer(&self, stream: DataStream) -> DataWriter {
        DataWriter {
            sender: self.clone(),
            stream,
        }
    }
}

impl Clone for DataSender {
    fn clone(&self) -> Self {
        Self {
            call: self.call,
            out: self.out.clone(),
        }
    }
}

/// A call's byte stream as a `std::io::Write`.
pub(crate) struct DataWriter {
    sender: DataSender,
    stream: DataStream,
}

impl std::io::Write for DataWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // frames well under the limit
        let n = buf.len().min(256 * 1024);
        if self.sender.blocking_send(self.stream, buf[..n].to_vec()) {
            Ok(n)
        } else {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A call's end as a frame.
fn end_frame(call: u32, result: Result<Response, ApiError>) -> Frame {
    let msg = match result {
        Ok(ok) => ServerMsg::End {
            ok: Some(ok),
            err: None,
        },
        Err(err) => ServerMsg::End {
            ok: None,
            err: Some(err),
        },
    };
    Frame::message(call, &msg)
}

/// Accept connections until `shutdown`.
pub(crate) async fn serve(
    listener: tokio::net::UnixListener,
    shared: Arc<Shared>,
    shutdown: CancelToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(connection(stream, shared.clone(), shutdown.clone()));
                }
                Err(e) => {
                    // out of descriptors: let some calls end
                    tracing::warn!("accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// The answer to a client's hello: a welcome, or why not.
fn welcome(
    shared: &Shared,
    peer: &Peer,
    role: Option<Role>,
    api: &str,
) -> Result<ServerMsg, ApiError> {
    if api_major(api) != Some(API_VERSION.0) {
        return Err(ApiError::new(
            ErrorCode::VersionMismatch,
            format!(
                "this daemon speaks API {}, the client {api}",
                api_version_string()
            ),
        )
        .with_hint("use a client of the daemon's version"));
    }
    let Some(role) = role else {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!(
                "{} may not use the nlink-lab daemon: not root, and in neither group {} nor {}",
                peer.actor(),
                shared.groups.operator_name,
                shared.groups.viewer_name
            ),
        )
        .with_hint(format!(
            "an administrator adds you to {} to read labs, or to {} to change them \
             (`usermod -aG GROUP USER`, then log in again)",
            shared.groups.viewer_name, shared.groups.operator_name
        )));
    };
    Ok(ServerMsg::Welcome {
        api: api_version_string(),
        server: server_name(),
        host: shared.host.clone(),
        uid: peer.uid,
        user: peer.user.clone(),
        role,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    })
}

async fn connection(stream: tokio::net::UnixStream, shared: Arc<Shared>, shutdown: CancelToken) {
    let peer = match Peer::of(&stream) {
        Ok(peer) => Arc::new(peer),
        Err(e) => {
            tracing::warn!("a connection without credentials: {e}");
            return;
        }
    };
    let role = authz::role(&peer, &shared.groups);
    let (mut r, mut w) = stream.into_split();
    let (out, mut out_rx) = mpsc::channel::<Frame>(256);
    tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Some(frame) = out_rx.recv().await {
            buf.clear();
            frame.encode(&mut buf);
            // whatever else is ready goes in the same write
            while buf.len() < 1 << 20
                && let Ok(frame) = out_rx.try_recv()
            {
                frame.encode(&mut buf);
            }
            if w.write_all(&buf).await.is_err() {
                return;
            }
        }
        let _ = w.shutdown().await;
    });
    shared.clients.fetch_add(1, Ordering::Relaxed);
    let calls: Calls = Arc::default();
    let mut welcomed = false;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    'conn: loop {
        let n = tokio::select! {
            _ = shutdown.cancelled() => break,
            read = r.read(&mut chunk) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            },
        };
        buf.extend_from_slice(&chunk[..n]);
        loop {
            let (frame, used) = match Frame::decode(&buf) {
                Ok(Some(decoded)) => decoded,
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("bad frame from {}: {e}", peer.actor());
                    break 'conn;
                }
            };
            buf.drain(..used);
            let (call, json) = match frame {
                Frame::Message { call, json } => (call, json),
                Frame::Data {
                    call,
                    stream: DataStream::Stdin,
                    bytes,
                } => {
                    send_input(&calls, call, Input::Stdin(bytes));
                    continue;
                }
                Frame::Data { .. } => continue,
            };
            let msg = match serde_json::from_slice::<ClientMsg>(&json) {
                Ok(msg) => msg,
                Err(e) => {
                    let err = ApiError::new(
                        ErrorCode::InvalidArgument,
                        format!("not a message of API {}: {e}", api_version_string()),
                    );
                    if out.send(end_frame(call, Err(err))).await.is_err() || call == 0 {
                        break 'conn;
                    }
                    continue;
                }
            };
            if call == 0 {
                match msg {
                    ClientMsg::Hello { api, client, .. } if !welcomed => {
                        match welcome(&shared, &peer, role, &api) {
                            Ok(w) => {
                                welcomed = true;
                                tracing::info!(
                                    "{} connected with {client} as {:?}",
                                    peer.actor(),
                                    role.unwrap_or(Role::Viewer)
                                );
                                let _ = out.send(Frame::message(0, &w)).await;
                            }
                            Err(e) => {
                                tracing::info!("{} refused: {}", peer.actor(), e.message);
                                let _ = out.send(end_frame(0, Err(e))).await;
                                break 'conn;
                            }
                        }
                    }
                    ClientMsg::Ping => {
                        let _ = out.send(Frame::message(0, &ServerMsg::Pong)).await;
                    }
                    _ => {}
                }
                continue;
            }
            let Some(role) = role.filter(|_| welcomed) else {
                let err = ApiError::new(
                    ErrorCode::InvalidArgument,
                    "a connection starts with a hello on call 0",
                );
                let _ = out.send(end_frame(call, Err(err))).await;
                break 'conn;
            };
            match msg {
                ClientMsg::Call { request } => {
                    start(call, *request, role, peer.clone(), &shared, &calls, &out).await;
                }
                ClientMsg::Cancel => {
                    if let Some(h) = calls.lock().unwrap().get(&call) {
                        h.cancel.cancel();
                    }
                }
                ClientMsg::Detach => {
                    if let Some(h) = calls.lock().unwrap().get(&call) {
                        h.leave();
                    }
                }
                ClientMsg::Eof => send_input(&calls, call, Input::Eof),
                ClientMsg::Resize { rows, cols } => {
                    send_input(&calls, call, Input::Resize { rows, cols })
                }
                ClientMsg::Signal { signal } => send_input(&calls, call, Input::Signal(signal)),
                ClientMsg::Hello { .. } | ClientMsg::Ping => {}
            }
        }
    }
    // the client is gone: its calls stop, its long operations go on
    for (_, h) in calls.lock().unwrap().drain() {
        h.leave();
    }
    shared.clients.fetch_sub(1, Ordering::Relaxed);
    if welcomed {
        tracing::debug!("{} disconnected", peer.actor());
    }
}

fn send_input(calls: &Calls, call: u32, input: Input) {
    if let Some(h) = calls.lock().unwrap().get(&call) {
        let _ = h.input.send(input);
    }
}

/// Start a call: check the caller may make it, then run it on a task of
/// its own, which ends it.
async fn start(
    call: u32,
    request: Request,
    role: Role,
    peer: Arc<Peer>,
    shared: &Arc<Shared>,
    calls: &Calls,
    out: &mpsc::Sender<Frame>,
) {
    let meta = request.meta();
    if meta.role > role {
        let err = ApiError::new(
            ErrorCode::PermissionDenied,
            format!(
                "`{}` is for operators: {} is a viewer",
                request.method(),
                peer.actor()
            ),
        )
        .with_hint(format!(
            "an administrator adds you to {}",
            shared.groups.operator_name
        ));
        let _ = out.send(end_frame(call, Err(err))).await;
        return;
    }
    let (input_tx, input) = mpsc::unbounded_channel();
    let cancel = CancelToken::new();
    let detached = CancelToken::new();
    let in_use = {
        let mut calls = calls.lock().unwrap();
        let in_use = calls.contains_key(&call);
        if !in_use {
            calls.insert(
                call,
                CallHandle {
                    cancel: cancel.clone(),
                    detach: detached.clone(),
                    on_disconnect: meta.on_disconnect,
                    input: input_tx,
                },
            );
        }
        in_use
    };
    if in_use {
        let err = ApiError::new(
            ErrorCode::InvalidArgument,
            format!("call {call} is already running"),
        );
        let _ = out.send(end_frame(call, Err(err))).await;
        return;
    }
    let audit = (meta.role == Role::Operator).then(|| shared.audit.entry(&peer, &request));
    let mut ctx = CallCtx {
        id: call,
        out: out.clone(),
        cancel,
        detached,
        input,
        peer,
        role,
        shared: shared.clone(),
    };
    let calls = calls.clone();
    tokio::spawn(async move {
        let result = crate::calls::dispatch(&mut ctx, request).await;
        if let Some(entry) = audit {
            ctx.shared.audit.record(entry, &result);
        }
        // gone before its end goes out: the client may reuse the id then
        calls.lock().unwrap().remove(&call);
        let _ = ctx.out.send(end_frame(call, result)).await;
    });
}
