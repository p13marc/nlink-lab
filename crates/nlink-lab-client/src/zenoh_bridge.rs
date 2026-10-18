//! The daemon's API over Zenoh (#271), behind the `zenoh` feature.
//!
//! A bridge task speaks the unix socket's framing on one side of an
//! in-process pipe and Zenoh on the other, so a [`Client`] over Zenoh is
//! the same client: each call is a query on the daemon's key for its role
//! (`nlink-lab/@rpc/<host>/viewer` or `…/operator`), each reply one of the
//! call's messages. A call has no input: a `cancel` of a job becomes a
//! `cancel_op`, and a `detach` stops following it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nlink_lab_shared::api::{
    ApiError, ClientMsg, ErrorCode, Frame, Request, Response, Role, ServerMsg, StreamItem,
    api_version_string,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::{Client, ClientError, TlsFiles};

/// How long the daemon has to answer the first query.
const FIND: Duration = Duration::from_secs(5);
/// How long a call may last: a job longer than this is followed with
/// `attach_op` again.
const CALL: Duration = Duration::from_secs(24 * 3600);

fn key(host: &str, role: Role) -> String {
    match role {
        Role::Viewer => format!("nlink-lab/@rpc/{host}/viewer"),
        Role::Operator => format!("nlink-lab/@rpc/{host}/operator"),
    }
}

fn config(endpoint: &str, tls: Option<&TlsFiles>) -> Result<zenoh::Config, ClientError> {
    let mut z = zenoh::Config::default();
    let mut set = |key: &str, json: String| {
        z.insert_json5(key, &json)
            .map_err(|e| ClientError::Protocol(format!("zenoh {key}: {e}")))
    };
    set("mode", "\"client\"".into())?;
    set("scouting/multicast/enabled", "false".into())?;
    set(
        "connect/endpoints",
        serde_json::to_string(&[endpoint]).unwrap_or_default(),
    )?;
    if let Some(tls) = tls {
        let path = |p: &std::path::Path| {
            serde_json::to_string(&p.display().to_string()).unwrap_or_default()
        };
        set("transport/link/tls/root_ca_certificate", path(&tls.ca))?;
        set("transport/link/tls/connect_certificate", path(&tls.cert))?;
        set("transport/link/tls/connect_private_key", path(&tls.key))?;
        set("transport/link/tls/enable_mtls", "true".into())?;
    }
    Ok(z)
}

/// The messages a query on `key` answers `request` with, until its end.
async fn ask(
    session: &zenoh::Session,
    key: &str,
    request: &Request,
    timeout: Duration,
) -> Result<zenoh::handlers::FifoChannelHandler<zenoh::query::Reply>, ClientError> {
    let payload = serde_json::to_vec(request).map_err(|e| ClientError::Protocol(e.to_string()))?;
    // every reply: the default consolidation keeps the latest per key,
    // which would drop all of a call's messages but its end
    session
        .get(key)
        .payload(payload)
        .consolidation(zenoh::query::ConsolidationMode::None)
        .timeout(timeout)
        .await
        .map_err(|e| ClientError::Protocol(format!("zenoh query: {e}")))
}

/// The first message a reply stream carries, and the key it came on.
async fn first(
    replies: &zenoh::handlers::FifoChannelHandler<zenoh::query::Reply>,
) -> Option<(String, ServerMsg)> {
    while let Ok(reply) = replies.recv_async().await {
        if let Ok(sample) = reply.result()
            && let Ok(msg) = serde_json::from_slice::<ServerMsg>(&sample.payload().to_bytes())
        {
            return Some((sample.key_expr().as_str().to_string(), msg));
        }
    }
    None
}

/// Connect to the daemon behind `endpoint` and shake hands as `client`.
pub(crate) async fn connect(
    endpoint: &str,
    tls: Option<&TlsFiles>,
    client: &str,
) -> Result<Client, ClientError> {
    let session = zenoh::open(config(endpoint, tls)?)
        .await
        .map_err(|e| ClientError::Protocol(format!("cannot reach zenoh at {endpoint}: {e}")))?;
    // the daemon is whoever answers the viewer key; one that is starting
    // declares it a moment after its socket is there
    let deadline = tokio::time::Instant::now() + FIND;
    let (found, msg) = loop {
        let replies = ask(
            &session,
            "nlink-lab/@rpc/*/viewer",
            &Request::WhoAmI,
            Duration::from_secs(1),
        )
        .await?;
        if let Some(answer) = first(&replies).await {
            break answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ClientError::Protocol(format!(
                "no nlink-lab daemon answers at zenoh:{endpoint}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let host = found.split('/').nth(2).unwrap_or_default().to_string();
    let ServerMsg::End {
        ok:
            Some(Response::WhoAmI {
                server,
                host: daemon_host,
                uid,
                user,
                features,
                ..
            }),
        ..
    } = msg
    else {
        return Err(ClientError::Protocol(format!("unexpected answer {msg:?}")));
    };
    // an operator is who the operator key answers
    let replies = ask(&session, &key(&host, Role::Operator), &Request::Ping, FIND).await?;
    let role = if first(&replies).await.is_some() {
        Role::Operator
    } else {
        Role::Viewer
    };
    let welcome = ServerMsg::Welcome {
        api: api_version_string(),
        server,
        host: daemon_host,
        uid,
        user,
        role,
        features,
    };
    let (near, far) = tokio::io::duplex(1 << 20);
    tokio::spawn(bridge(Arc::new(session), host, role, welcome, far));
    let (r, w) = tokio::io::split(near);
    Client::over(r, w, None, client).await
}

/// One side of the pipe is the client; this is the other.
async fn bridge(
    session: Arc<zenoh::Session>,
    host: String,
    role: Role,
    welcome: ServerMsg,
    pipe: tokio::io::DuplexStream,
) {
    let (mut r, mut w) = tokio::io::split(pipe);
    let (out, mut out_rx) = mpsc::unbounded_channel::<Frame>();
    tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Some(frame) = out_rx.recv().await {
            buf.clear();
            frame.encode(&mut buf);
            if w.write_all(&buf).await.is_err() {
                return;
            }
        }
    });
    // the job each call follows, for a cancel; and the calls followed
    let ops: Arc<Mutex<HashMap<u32, String>>> = Arc::default();
    let following: Arc<Mutex<HashMap<u32, tokio::task::JoinHandle<()>>>> = Arc::default();
    let end = |call: u32, result: Result<Response, ApiError>| {
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
    };
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    'pipe: loop {
        let n = match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        loop {
            let (frame, used) = match Frame::decode(&buf) {
                Ok(Some(decoded)) => decoded,
                Ok(None) => break,
                Err(_) => break 'pipe,
            };
            buf.drain(..used);
            let Frame::Message { call, json } = frame else {
                let call = frame.call();
                let err = ApiError::new(ErrorCode::Unsupported, "no input over Zenoh");
                let _ = out.send(end(call, Err(err)));
                continue;
            };
            let Ok(msg) = serde_json::from_slice::<ClientMsg>(&json) else {
                continue;
            };
            match msg {
                ClientMsg::Hello { .. } => {
                    let _ = out.send(Frame::message(0, &welcome));
                }
                ClientMsg::Ping => {
                    let _ = out.send(Frame::message(0, &ServerMsg::Pong));
                }
                ClientMsg::Call { request } => {
                    let request = *request;
                    let meta = request.meta();
                    if meta.role > role {
                        let err = ApiError::new(
                            ErrorCode::PermissionDenied,
                            format!(
                                "`{}` is for operators, and the daemon's operator key does not \
                                 answer this certificate (or there is none)",
                                request.method()
                            ),
                        );
                        let _ = out.send(end(call, Err(err)));
                        continue;
                    }
                    let key = key(&host, meta.role);
                    let (session, out, ops) = (session.clone(), out.clone(), ops.clone());
                    let task = tokio::spawn(async move {
                        let replies = match ask(&session, &key, &request, CALL).await {
                            Ok(replies) => replies,
                            Err(e) => {
                                let err = ApiError::new(ErrorCode::Io, e.to_string());
                                let _ = out.send(end(call, Err(err)));
                                return;
                            }
                        };
                        let mut ended = false;
                        while let Ok(reply) = replies.recv_async().await {
                            let Ok(sample) = reply.result() else { continue };
                            let bytes = sample.payload().to_bytes();
                            let Ok(msg) = serde_json::from_slice::<ServerMsg>(&bytes) else {
                                continue;
                            };
                            if let ServerMsg::Event {
                                item: StreamItem::Started { op: Some(op), .. },
                            } = &msg
                            {
                                ops.lock().unwrap().insert(call, op.clone());
                            }
                            ended |= matches!(msg, ServerMsg::End { .. });
                            let _ = out.send(Frame::message(call, &msg));
                        }
                        if !ended {
                            let err = ApiError::new(
                                ErrorCode::Io,
                                "the daemon's answer stopped (its access control, a timeout, \
                                 or it went away)",
                            );
                            let _ = out.send(end(call, Err(err)));
                        }
                        ops.lock().unwrap().remove(&call);
                    });
                    following.lock().unwrap().insert(call, task);
                }
                ClientMsg::Cancel => {
                    if let Some(op) = ops.lock().unwrap().get(&call).cloned() {
                        let session = session.clone();
                        let key = key(&host, Role::Operator);
                        tokio::spawn(async move {
                            let _ = ask(&session, &key, &Request::CancelOp { op }, FIND).await;
                        });
                    }
                }
                ClientMsg::Detach => {
                    if let Some(task) = following.lock().unwrap().remove(&call) {
                        task.abort();
                    }
                    let result = match ops.lock().unwrap().remove(&call) {
                        Some(op) => Ok(Response::Detached { op }),
                        None => Err(ApiError::new(ErrorCode::Cancelled, "cancelled")),
                    };
                    let _ = out.send(end(call, result));
                }
                ClientMsg::Eof | ClientMsg::Resize { .. } | ClientMsg::Signal { .. } => {}
            }
        }
    }
    for (_, task) in following.lock().unwrap().drain() {
        task.abort();
    }
}
