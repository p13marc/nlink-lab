//! The connection: the handshake, a writer task and a reader task that
//! routes every frame to its call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nlink_lab_shared::api::{
    API_VERSION, ClientMsg, Frame, LabSummary, Request, Response, Role, ServerMsg, api_major,
    api_version_string,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::call::{Call, Incoming};
use crate::{ClientError, Endpoint};

/// How long the daemon has to answer the handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// What the daemon said about itself, and about the caller.
#[derive(Debug, Clone)]
pub struct Welcome {
    pub api: String,
    pub server: String,
    pub host: String,
    pub uid: u32,
    pub user: String,
    pub role: Role,
    pub features: Vec<String>,
}

pub(crate) struct Inner {
    out: mpsc::UnboundedSender<Frame>,
    calls: Mutex<HashMap<u32, mpsc::UnboundedSender<Incoming>>>,
    next: AtomicU32,
    closed: AtomicBool,
    welcome: Mutex<Option<Welcome>>,
    _child: Mutex<Option<tokio::process::Child>>,
}

/// A connection to a daemon. Cheap to clone: the clones share it.
#[derive(Clone)]
pub struct Client(pub(crate) Arc<Inner>);

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("welcome", &self.0.welcome.lock().unwrap().clone())
            .finish()
    }
}

impl Client {
    /// Connect to `endpoint` and shake hands as `client` (`name/version`).
    pub async fn connect(endpoint: &Endpoint, client: &str) -> Result<Self, ClientError> {
        match endpoint {
            Endpoint::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path).await.map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("cannot reach the daemon at {}: {e}", path.display()),
                    )
                })?;
                let (r, w) = stream.into_split();
                Self::over(r, w, None, client).await
            }
            Endpoint::Command { program, args } => {
                let mut child = tokio::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn()?;
                let (Some(w), Some(r)) = (child.stdin.take(), child.stdout.take()) else {
                    return Err(ClientError::Protocol("no stdio from the command".into()));
                };
                Self::over(r, w, Some(child), client).await
            }
        }
    }

    /// A connection over any byte stream (a socket, a pipe, a test's
    /// duplex), with the handshake done.
    pub async fn over<R, W>(
        read: R,
        write: W,
        child: Option<tokio::process::Child>,
        client: &str,
    ) -> Result<Self, ClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, out_rx) = mpsc::unbounded_channel::<Frame>();
        let inner = Arc::new(Inner {
            out,
            calls: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            welcome: Mutex::new(None),
            _child: Mutex::new(child),
        });
        tokio::spawn(writer(write, out_rx));
        // call 0 is the connection: the handshake and pings
        let (tx0, mut rx0) = mpsc::unbounded_channel();
        inner.calls.lock().unwrap().insert(0, tx0);
        tokio::spawn(reader(read, Arc::downgrade(&inner)));
        let me = Client(inner);
        me.send(
            0,
            &ClientMsg::Hello {
                api: api_version_string(),
                client: client.to_string(),
                features: Vec::new(),
            },
        )?;
        let first = tokio::time::timeout(HANDSHAKE, rx0.recv())
            .await
            .map_err(|_| ClientError::Protocol("the daemon did not answer the handshake".into()))?;
        let welcome = match first.map(|i| match i {
            Incoming::Msg(m) => Ok(*m),
            other => Err(other),
        }) {
            Some(Ok(ServerMsg::Welcome {
                api,
                server,
                host,
                uid,
                user,
                role,
                features,
            })) => Welcome {
                api,
                server,
                host,
                uid,
                user,
                role,
                features,
            },
            Some(Ok(ServerMsg::End { err: Some(e), .. })) => {
                return Err(ClientError::Api(Box::new(e)));
            }
            Some(other) => {
                return Err(ClientError::Protocol(format!(
                    "expected a welcome, got {other:?}"
                )));
            }
            None => return Err(ClientError::Closed),
        };
        if api_major(&welcome.api) != Some(API_VERSION.0) {
            return Err(ClientError::VersionMismatch {
                server: welcome.api,
                client: api_version_string(),
            });
        }
        *me.0.welcome.lock().unwrap() = Some(welcome);
        Ok(me)
    }

    /// What the daemon said at the handshake.
    pub fn welcome(&self) -> Welcome {
        self.0
            .welcome
            .lock()
            .unwrap()
            .clone()
            .expect("a connected client has a welcome")
    }

    /// Whether the connection is gone.
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Relaxed)
    }

    pub(crate) fn send(&self, call: u32, msg: &ClientMsg) -> Result<(), ClientError> {
        self.send_frame(Frame::message(call, msg))
    }

    pub(crate) fn send_frame(&self, frame: Frame) -> Result<(), ClientError> {
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        self.0.out.send(frame).map_err(|_| ClientError::Closed)
    }

    pub(crate) fn forget(&self, call: u32) {
        self.0.calls.lock().unwrap().remove(&call);
    }

    /// Start a call: its events and end come through the [`Call`].
    pub async fn start(&self, request: Request) -> Result<Call, ClientError> {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.0.calls.lock().unwrap().insert(id, tx);
        if let Err(e) = self.send(
            id,
            &ClientMsg::Call {
                request: Box::new(request),
            },
        ) {
            self.forget(id);
            return Err(e);
        }
        Ok(Call::new(id, rx, self.clone()))
    }

    /// A call to its end; its events are dropped.
    pub async fn call(&self, request: Request) -> Result<Response, ClientError> {
        self.start(request).await?.finish().await
    }

    /// `ping`: the daemon is there.
    pub async fn ping(&self) -> Result<(), ClientError> {
        match self.call(Request::Ping).await? {
            Response::Pong | Response::Ok => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// `list_labs`.
    pub async fn list_labs(&self) -> Result<Vec<LabSummary>, ClientError> {
        match self.call(Request::ListLabs).await? {
            Response::Labs { labs } => Ok(labs),
            other => Err(unexpected(other)),
        }
    }
}

fn unexpected(r: Response) -> ClientError {
    ClientError::Protocol(format!("unexpected response {r:?}"))
}

async fn writer<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::UnboundedReceiver<Frame>) {
    let mut buf = Vec::new();
    while let Some(frame) = rx.recv().await {
        buf.clear();
        frame.encode(&mut buf);
        if w.write_all(&buf).await.is_err() || w.flush().await.is_err() {
            return;
        }
    }
}

async fn reader<R: AsyncRead + Unpin>(mut r: R, inner: std::sync::Weak<Inner>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    'read: loop {
        let n = match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        loop {
            let (frame, used) = match Frame::decode(&buf) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => break 'read,
            };
            buf.drain(..used);
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let call = frame.call();
            let incoming = match frame {
                Frame::Message { json, .. } => match serde_json::from_slice::<ServerMsg>(&json) {
                    Ok(msg) => Incoming::Msg(Box::new(msg)),
                    Err(_) => continue,
                },
                Frame::Data { stream, bytes, .. } => Incoming::Data(stream, bytes),
            };
            let tx = inner.calls.lock().unwrap().get(&call).cloned();
            if let Some(tx) = tx {
                let _ = tx.send(incoming);
            }
        }
    }
    if let Some(inner) = inner.upgrade() {
        inner.closed.store(true, Ordering::Relaxed);
        // every call sees its channel end
        inner.calls.lock().unwrap().clear();
    }
}
