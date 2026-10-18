//! The daemon in-process on a temporary socket and state root, as the
//! user running the tests (no root, no lab): the handshake, the roles the
//! groups give, refusals, the reads of an empty host, a subscription.

use std::path::PathBuf;
use std::sync::OnceLock;

use nlink_lab::CancelToken;
use nlink_lab_client::{CallEvent, Client, ClientError, Endpoint};
use nlink_lab_shared::api::{
    ClientMsg, ErrorCode, Frame, Notification, Request, Response, Role, ServerMsg, StreamItem,
    Topic,
};
use nlink_labd::Config;

/// One state root for the binary: the variable is the process's.
fn state_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        // SAFETY: set once, before any daemon or client thread reads it.
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", &dir) };
        dir
    })
}

/// The name of the user's primary group.
fn my_group() -> String {
    // SAFETY: getgid cannot fail.
    let gid = unsafe { libc::getgid() };
    let out = std::process::Command::new("getent")
        .args(["group", &gid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .to_string()
}

struct Daemon {
    endpoint: Endpoint,
    shutdown: CancelToken,
    _dir: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// A daemon whose operators and viewers are these groups.
async fn daemon(operator_group: &str, viewer_group: &str) -> Daemon {
    let root = state_root();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("api.sock");
    let config = Config {
        socket: socket.clone(),
        operator_group: operator_group.into(),
        viewer_group: viewer_group.into(),
        rescan_ms: 100,
        audit_log: Some(root.join("audit.ndjson")),
        ..Config::default()
    };
    let listener = nlink_labd::listen::bind(&socket).unwrap();
    let shutdown = CancelToken::new();
    tokio::spawn(nlink_labd::serve(config, listener, shutdown.clone()));
    Daemon {
        endpoint: Endpoint::Unix(socket),
        shutdown,
        _dir: dir,
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[tokio::test]
async fn an_operator_reads_an_empty_host() {
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let client = Client::connect(&d.endpoint, "test/0").await.unwrap();
    let welcome = client.welcome();
    assert_eq!(welcome.role, Role::Operator);
    assert!(welcome.server.starts_with("nlink-labd/"));
    assert!(welcome.features.contains(&"exec".to_string()));
    client.ping().await.unwrap();
    assert!(client.list_labs().await.unwrap().is_empty());

    match client.call(Request::WhoAmI).await.unwrap() {
        Response::WhoAmI { role, uid, .. } => {
            assert_eq!(role, Role::Operator);
            // SAFETY: getuid cannot fail.
            assert_eq!(uid, unsafe { libc::getuid() });
        }
        other => panic!("{other:?}"),
    }
    match client.call(Request::DaemonStatus).await.unwrap() {
        Response::DaemonStatus { labs, clients, .. } => {
            assert_eq!(labs, 0);
            assert!(clients >= 1);
        }
        other => panic!("{other:?}"),
    }

    // a lab that is not there
    for request in [
        Request::GetLab { lab: "nope".into() },
        Request::ReadEvents {
            lab: "nope".into(),
            since: None,
            kinds: vec![],
            limit: None,
        },
        Request::ListSnapshots { lab: "nope".into() },
    ] {
        let err = client.call(request.clone()).await.unwrap_err();
        match err {
            ClientError::Api(e) => assert_eq!(e.code, ErrorCode::NotFound, "{request:?}: {e:?}"),
            other => panic!("{request:?}: {other:?}"),
        }
    }
    // a name that is not a lab's
    let err = client
        .call(Request::GetLab {
            lab: "../etc".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Api(e) if e.code == ErrorCode::InvalidArgument));
    // what is not served yet says so
    let err = client
        .call(Request::Shell {
            lab: "nope".into(),
            node: "a".into(),
            shell: None,
            rows: 24,
            cols: 80,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Api(e) if e.code == ErrorCode::Unsupported),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_viewer_reads_but_does_not_operate() {
    if is_root() {
        // root is an operator whatever its groups
        return;
    }
    let d = daemon("no-such-group-nlink", &my_group()).await;
    let client = Client::connect(&d.endpoint, "test/0").await.unwrap();
    assert_eq!(client.welcome().role, Role::Viewer);
    assert!(client.list_labs().await.unwrap().is_empty());
    for request in [
        Request::GetTopology {
            lab: "x".into(),
            redact: false,
        },
        Request::Exec {
            lab: "x".into(),
            node: "n".into(),
            argv: vec!["true".into()],
            env: vec![],
            workdir: None,
            stdin: false,
            tty: false,
            timeout_ms: None,
        },
        Request::Destroy { lab: "x".into() },
        Request::CancelOp { op: "op-1".into() },
    ] {
        match client.call(request).await.unwrap_err() {
            ClientError::Api(e) => {
                assert_eq!(e.code, ErrorCode::PermissionDenied);
                assert!(e.message.contains("is for operators"), "{}", e.message);
            }
            other => panic!("{other:?}"),
        }
    }
    // nothing an operator did: nothing audited
    let audit = std::fs::read_to_string(state_root().join("audit.ndjson")).unwrap_or_default();
    assert!(!audit.contains("\"uid\":65534"));
}

#[tokio::test]
async fn someone_in_neither_group_is_refused() {
    if is_root() {
        return;
    }
    let d = daemon("no-such-group-nlink", "no-such-group-nlink").await;
    match Client::connect(&d.endpoint, "test/0").await.unwrap_err() {
        ClientError::Api(e) => {
            assert_eq!(e.code, ErrorCode::PermissionDenied);
            assert!(e.message.contains("may not use"), "{}", e.message);
            assert!(e.hint.unwrap().contains("usermod"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn another_major_is_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let Endpoint::Unix(path) = &d.endpoint else {
        unreachable!()
    };
    let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let mut out = Vec::new();
    Frame::message(
        0,
        &ClientMsg::Hello {
            api: "9.0".into(),
            client: "future/9".into(),
            features: vec![],
        },
    )
    .encode(&mut out);
    stream.write_all(&out).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let (frame, _) = Frame::decode(&buf).unwrap().unwrap();
    let Frame::Message { json, .. } = frame else {
        panic!("{frame:?}")
    };
    match serde_json::from_slice::<ServerMsg>(&json).unwrap() {
        ServerMsg::End { err: Some(e), .. } => assert_eq!(e.code, ErrorCode::VersionMismatch),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_call_before_the_hello_ends_the_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let Endpoint::Unix(path) = &d.endpoint else {
        unreachable!()
    };
    let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let mut out = Vec::new();
    Frame::message(
        1,
        &ClientMsg::Call {
            request: Box::new(Request::ListLabs),
        },
    )
    .encode(&mut out);
    stream.write_all(&out).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let (Frame::Message { json, .. }, _) = Frame::decode(&buf).unwrap().unwrap() else {
        panic!()
    };
    assert!(String::from_utf8_lossy(&json).contains("starts with a hello"));
}

#[tokio::test]
async fn a_subscription_starts_with_the_labs_and_ends_on_cancel() {
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let client = Client::connect(&d.endpoint, "test/0").await.unwrap();
    let mut call = client
        .start(Request::Subscribe {
            lab: None,
            topics: vec![Topic::Labs],
            interval_ms: None,
        })
        .await
        .unwrap();
    match call.next().await.unwrap() {
        CallEvent::Item(StreamItem::Notification {
            notification: Notification::LabsChanged { added, .. },
        }) => assert!(added.is_empty()),
        other => panic!("{other:?}"),
    }
    call.cancel().unwrap();
    match call.next().await.unwrap() {
        CallEvent::End(Err(e)) => assert_eq!(e.code, ErrorCode::Cancelled),
        other => panic!("{other:?}"),
    }

    // metrics are a lab's
    let err = client
        .call(Request::Subscribe {
            lab: None,
            topics: vec![Topic::Metrics],
            interval_ms: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Api(e) if e.code == ErrorCode::InvalidArgument));
}

#[tokio::test]
async fn dial_stdio_relays_a_connection() {
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let Endpoint::Unix(path) = &d.endpoint else {
        unreachable!()
    };
    let endpoint = Endpoint::Command {
        program: env!("CARGO_BIN_EXE_nlink-labd").into(),
        args: vec![
            "dial-stdio".into(),
            "--socket".into(),
            path.display().to_string(),
        ],
    };
    let client = Client::connect(&endpoint, "test/0").await.unwrap();
    assert_eq!(client.welcome().role, Role::Operator);
    assert!(client.list_labs().await.unwrap().is_empty());
}

fn topology_json(text: &str) -> serde_json::Value {
    serde_json::to_value(nlink_lab::parser::parse(text).unwrap()).unwrap()
}

#[tokio::test]
async fn an_operator_plans_and_is_told_what_does_not_deploy() {
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let client = Client::connect(&d.endpoint, "test/0").await.unwrap();
    let simple =
        "lab \"planned\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n";

    // a plan reads nothing but the topology
    match client
        .call(Request::PlanDeploy {
            topology: topology_json(simple),
        })
        .await
        .unwrap()
    {
        Response::Report { kind, report } => {
            assert_eq!(kind, "plan");
            let ops = report.as_array().unwrap();
            assert!(ops.iter().any(|o| o["stage"] == "namespaces"), "{report}");
        }
        other => panic!("{other:?}"),
    }

    // a topology that does not validate is refused before it is queued
    let broken =
        "lab \"broken\"\nnode a\nlink a:eth0 -- ghost:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n";
    let topology = serde_json::to_value(
        nlink_lab::parser::parse(broken).unwrap_or_else(|_| nlink_lab::Topology::default()),
    )
    .unwrap();
    let err = client
        .call(Request::Deploy {
            topology,
            options: Default::default(),
            source: None,
        })
        .await
        .unwrap_err();
    match err {
        ClientError::Api(e) => assert_eq!(e.code, ErrorCode::Validation, "{e:?}"),
        other => panic!("{other:?}"),
    }

    // a source bundle that does not parse says where
    let err = client
        .call(Request::Deploy {
            topology: serde_json::Value::Null,
            options: Default::default(),
            source: Some(nlink_lab_shared::api::SourceBundle {
                path: "lab.nll".into(),
                text: "lab \"x\"\nnode\n".into(),
                imports: Default::default(),
                params: vec![],
            }),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Api(e) if e.code == ErrorCode::Validation),
        "{err:?}"
    );

    // no jobs ran: none is listed, and an unknown one is not found
    match client.call(Request::ListOps).await.unwrap() {
        Response::Ops { ops } => assert!(ops.is_empty(), "{ops:?}"),
        other => panic!("{other:?}"),
    }
    for request in [
        Request::AttachOp { op: "op-99".into() },
        Request::CancelOp { op: "op-99".into() },
    ] {
        match client.call(request).await.unwrap_err() {
            ClientError::Api(e) => assert_eq!(e.code, ErrorCode::NotFound),
            other => panic!("{other:?}"),
        }
    }
}
