//! The client against a fake daemon on a temporary unix socket: the
//! handshake, unary and streamed calls, data both ways, errors, a closed
//! connection, a version the client refuses.

use nlink_lab_client::{CallEvent, Client, ClientError, Endpoint};
use nlink_lab_shared::api::{
    ApiError, ClientMsg, DataStream, ErrorCode, Frame, Request, Response, Role, ServerMsg,
    StreamItem,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the fake daemon does.
#[derive(Clone, Copy)]
enum Mode {
    Normal,
    /// Claims API 2.0.
    Future,
    /// Hangs up after the welcome.
    HangUp,
}

async fn serve(stream: tokio::net::UnixStream, mode: Mode) {
    let (mut r, mut w) = stream.into_split();
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 4096];
    let send = |frame: Frame| {
        let mut out = Vec::new();
        frame.encode(&mut out);
        out
    };
    loop {
        let Ok(n) = r.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Ok(Some((frame, used))) = Frame::decode(&buf) {
            buf.drain(..used);
            let reply: Vec<Frame> = match frame {
                Frame::Message { call, json } => {
                    match serde_json::from_slice::<ClientMsg>(&json).unwrap() {
                        ClientMsg::Hello { .. } => vec![Frame::message(
                            call,
                            &ServerMsg::Welcome {
                                api: match mode {
                                    Mode::Future => "2.0".into(),
                                    _ => "1.0".into(),
                                },
                                server: "fake/0".into(),
                                host: "test".into(),
                                uid: 1000,
                                user: "tester".into(),
                                role: Role::Viewer,
                                features: vec![],
                            },
                        )],
                        ClientMsg::Call { request } => match *request {
                            Request::Ping => vec![Frame::message(
                                call,
                                &ServerMsg::End {
                                    ok: Some(Response::Pong),
                                    err: None,
                                },
                            )],
                            Request::ListLabs => vec![
                                Frame::message(
                                    call,
                                    &ServerMsg::Event {
                                        item: StreamItem::Started {
                                            op: Some("op-1".into()),
                                            pid: None,
                                        },
                                    },
                                ),
                                Frame::message(
                                    call,
                                    &ServerMsg::End {
                                        ok: Some(Response::Labs { labs: vec![] }),
                                        err: None,
                                    },
                                ),
                            ],
                            Request::Destroy { lab } => vec![Frame::message(
                                call,
                                &ServerMsg::End {
                                    ok: None,
                                    err: Some(ApiError::new(
                                        ErrorCode::NotFound,
                                        format!("lab not found: {lab}"),
                                    )),
                                },
                            )],
                            // anything else: an echo session, ended by Eof
                            _ => vec![],
                        },
                        ClientMsg::Eof => vec![Frame::message(
                            call,
                            &ServerMsg::End {
                                ok: Some(Response::Ok),
                                err: None,
                            },
                        )],
                        _ => vec![],
                    }
                }
                Frame::Data { call, bytes, .. } => vec![Frame::Data {
                    call,
                    stream: DataStream::Stdout,
                    bytes,
                }],
            };
            for f in reply {
                let welcome = matches!(&f, Frame::Message { call: 0, .. });
                if w.write_all(&send(f)).await.is_err() {
                    return;
                }
                if welcome && matches!(mode, Mode::HangUp) {
                    return;
                }
            }
        }
    }
}

async fn daemon(mode: Mode) -> (tempfile::TempDir, Endpoint) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, mode));
        }
    });
    (dir, Endpoint::Unix(path))
}

#[tokio::test]
async fn unary_and_streamed_calls() {
    let (_dir, ep) = daemon(Mode::Normal).await;
    let client = Client::connect(&ep, "test/0").await.unwrap();
    assert_eq!(client.welcome().user, "tester");
    assert_eq!(client.welcome().role, Role::Viewer);
    client.ping().await.unwrap();

    let mut call = client.start(Request::ListLabs).await.unwrap();
    let first = call.next().await.unwrap();
    assert!(
        matches!(first, CallEvent::Item(StreamItem::Started { .. })),
        "{first:?}"
    );
    let end = call.next().await.unwrap();
    assert_eq!(end, CallEvent::End(Ok(Response::Labs { labs: vec![] })));
    assert!(call.next().await.is_none());
    assert!(client.list_labs().await.unwrap().is_empty());
}

#[tokio::test]
async fn calls_interleave_on_one_connection() {
    let (_dir, ep) = daemon(Mode::Normal).await;
    let client = Client::connect(&ep, "test/0").await.unwrap();
    // an open echo session does not hold up other calls
    let mut echo = client.start(Request::WhoAmI).await.unwrap();
    client.ping().await.unwrap();
    echo.stdin(b"hello").unwrap();
    assert_eq!(
        echo.next().await.unwrap(),
        CallEvent::Data(DataStream::Stdout, b"hello".to_vec())
    );
    echo.eof().unwrap();
    assert_eq!(echo.next().await.unwrap(), CallEvent::End(Ok(Response::Ok)));
}

#[tokio::test]
async fn a_daemon_error_is_the_api_error() {
    let (_dir, ep) = daemon(Mode::Normal).await;
    let client = Client::connect(&ep, "test/0").await.unwrap();
    let err = client
        .call(Request::Destroy { lab: "wan".into() })
        .await
        .unwrap_err();
    match err {
        ClientError::Api(e) => {
            assert_eq!(e.code, ErrorCode::NotFound);
            assert_eq!(e.message, "lab not found: wan");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn another_major_is_refused() {
    let (_dir, ep) = daemon(Mode::Future).await;
    let err = Client::connect(&ep, "test/0").await.unwrap_err();
    assert!(
        matches!(err, ClientError::VersionMismatch { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_closed_connection_ends_every_call() {
    let (_dir, ep) = daemon(Mode::HangUp).await;
    let client = Client::connect(&ep, "test/0").await.unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(matches!(err, ClientError::Closed), "{err:?}");
}

#[tokio::test]
async fn no_daemon_says_where_it_looked() {
    let dir = tempfile::tempdir().unwrap();
    let ep = Endpoint::Unix(dir.path().join("nothing.sock"));
    let err = Client::connect(&ep, "test/0")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot reach the daemon at"), "{err}");
}
