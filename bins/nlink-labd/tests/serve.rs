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
    daemon_with(operator_group, viewer_group, |_, _| {}).await
}

/// [`daemon`], its configuration changed by `tweak` (given the daemon's
/// directory) first.
async fn daemon_with(
    operator_group: &str,
    viewer_group: &str,
    tweak: impl FnOnce(&mut Config, &std::path::Path),
) -> Daemon {
    let root = state_root();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("api.sock");
    let mut config = Config {
        socket: socket.clone(),
        operator_group: operator_group.into(),
        viewer_group: viewer_group.into(),
        rescan_ms: 100,
        audit_log: Some(root.join("audit.ndjson")),
        zenoh: nlink_labd::config::ZenohConfig {
            enabled: false,
            ..Default::default()
        },
        ..Config::default()
    };
    tweak(&mut config, dir.path());
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

#[tokio::test]
async fn sessions_say_what_is_wrong() {
    use nlink_lab_shared::api::LogStream;
    let d = daemon(&my_group(), "no-such-group-nlink").await;
    let client = Client::connect(&d.endpoint, "test/0").await.unwrap();
    for (request, code) in [
        (
            Request::Shell {
                lab: "nope".into(),
                node: "a".into(),
                shell: None,
                rows: 24,
                cols: 80,
            },
            ErrorCode::NotFound,
        ),
        (
            Request::Logs {
                lab: "nope".into(),
                pid: 1,
                stream: LogStream::Both,
                tail: None,
                follow: false,
            },
            ErrorCode::NotFound,
        ),
        (Request::Export { lab: "nope".into() }, ErrorCode::NotFound),
        (
            Request::Subscribe {
                lab: None,
                topics: vec![Topic::Drift],
                interval_ms: None,
            },
            ErrorCode::InvalidArgument,
        ),
    ] {
        match client.call(request.clone()).await.unwrap_err() {
            ClientError::Api(e) => assert_eq!(e.code, code, "{request:?}: {e:?}"),
            other => panic!("{request:?}: {other:?}"),
        }
    }
}

/// `GET path` on `addr`: the status line and the body.
async fn http_get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[tokio::test]
async fn http_serves_the_labs_and_their_metrics() {
    // a free port, taken back by the daemon
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let _d = daemon_with(&my_group(), "no-such-group-nlink", |c, _| {
        c.http.listen = Some(addr);
    })
    .await;
    // it binds as the daemon starts
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (status, body) = http_get(addr, "/api/v2/labs").await;
    assert!(status.contains("200"), "{status}");
    let labs: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(labs["labs"], serde_json::json!([]));
    let (status, body) = http_get(addr, "/metrics").await;
    assert!(status.contains("200"), "{status}");
    assert!(body.ends_with("# EOF\n"), "{body}");
    let (status, _) = http_get(addr, "/api/v2/labs/nope").await;
    assert!(status.contains("404"), "{status}");
    let (status, _) = http_get(addr, "/nothing").await;
    assert!(status.contains("404"), "{status}");
}

// zenoh refuses tokio's current-thread scheduler
#[cfg(feature = "zenoh")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zenoh_listens_on_its_socket_only() {
    use std::os::unix::fs::PermissionsExt;
    let zsock = std::sync::Arc::new(std::sync::Mutex::new(std::path::PathBuf::new()));
    let seen = zsock.clone();
    let _d = daemon_with(&my_group(), "no-such-group-nlink", move |c, dir| {
        let path = dir.join("zenoh/zenoh.sock");
        c.zenoh = nlink_labd::config::ZenohConfig {
            enabled: true,
            listen: vec![format!("unixsock-stream/{}", path.display())],
            ..Default::default()
        };
        *seen.lock().unwrap() = path;
    })
    .await;
    let path = zsock.lock().unwrap().clone();
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).map_or(0, |m| m.permissions().mode() & 0o777);
    for _ in 0..100 {
        if mode(&path) == 0o660 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(mode(&path), 0o660);
    // closed to others before the socket existed
    assert_eq!(mode(path.parent().unwrap()), 0o750);
    // a client of the socket: no lab, so no topology answers
    let mut config = zenoh::Config::default();
    config.insert_json5("mode", "\"client\"").unwrap();
    config
        .insert_json5(
            "connect/endpoints",
            &serde_json::to_string(&[format!("unixsock-stream/{}", path.display())]).unwrap(),
        )
        .unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    let session = zenoh::open(config).await.unwrap();
    let replies = session.get("nlink-lab/*/topology").await.unwrap();
    let mut answers = 0;
    while let Ok(reply) = replies.recv_async().await {
        if reply.result().is_ok() {
            answers += 1;
        }
    }
    assert_eq!(answers, 0);
    session.close().await.unwrap();
}

/// A free port on loopback, given back for the daemon to take.
#[cfg(feature = "zenoh")]
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Wait for a path to exist.
#[cfg(feature = "zenoh")]
async fn exists(path: &std::path::Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

// zenoh refuses tokio's current-thread scheduler
#[cfg(feature = "zenoh")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_api_over_zenohs_unix_socket_is_a_viewers() {
    let zsock = std::sync::Arc::new(std::sync::Mutex::new(std::path::PathBuf::new()));
    let seen = zsock.clone();
    let _d = daemon_with(&my_group(), "no-such-group-nlink", move |c, dir| {
        let path = dir.join("zenoh/zenoh.sock");
        c.zenoh.enabled = true;
        c.zenoh.listen = vec![format!("unixsock-stream/{}", path.display())];
        *seen.lock().unwrap() = path;
    })
    .await;
    let path = zsock.lock().unwrap().clone();
    exists(&path).await;
    let endpoint = Endpoint::Zenoh {
        endpoint: format!("unixsock-stream/{}", path.display()),
        tls: None,
    };
    let client = Client::connect(&endpoint, "test/0").await.unwrap();
    // no TLS: no operator key, whoever connects
    assert_eq!(client.welcome().role, Role::Viewer);
    assert!(client.list_labs().await.unwrap().is_empty());
    match client.call(Request::ListOps).await.unwrap() {
        Response::Ops { ops } => assert!(ops.is_empty()),
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::Destroy { lab: "x".into() })
        .await
        .unwrap_err()
    {
        ClientError::Api(e) => assert_eq!(e.code, ErrorCode::PermissionDenied),
        other => panic!("{other:?}"),
    }
    // a viewer's session stays on the unix socket
    let err = client
        .call(Request::Logs {
            lab: "x".into(),
            pid: 1,
            stream: Default::default(),
            tail: None,
            follow: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Api(e) if e.code == ErrorCode::Unsupported),
        "{err:?}"
    );
}

/// A CA and the certificates it signs, written as PEM files under `dir`:
/// `ca.pem`, and `<name>.pem` + `<name>.key` for each name.
#[cfg(feature = "zenoh")]
fn certificates(dir: &std::path::Path, names: &[&str]) {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::CommonName, "nlink-lab test CA");
    let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    for name in names {
        let key = KeyPair::generate().unwrap();
        let mut leaf = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        leaf.distinguished_name.push(DnType::CommonName, *name);
        let cert = leaf.signed_by(&key, &ca).unwrap();
        std::fs::write(dir.join(format!("{name}.pem")), cert.pem()).unwrap();
        std::fs::write(dir.join(format!("{name}.key")), key.serialize_pem()).unwrap();
    }
}

#[cfg(feature = "zenoh")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutual_tls_and_the_access_control_decide_the_role() {
    let pki = tempfile::tempdir().unwrap();
    certificates(pki.path(), &["daemon", "op", "view", "stranger"]);
    let port = free_port();
    let p = pki.path().to_path_buf();
    let _d = daemon_with(&my_group(), "no-such-group-nlink", move |c, dir| {
        c.zenoh.enabled = true;
        c.zenoh.listen = vec![
            format!("unixsock-stream/{}", dir.join("zenoh.sock").display()),
            format!("tls/localhost:{port}"),
        ];
        c.zenoh.tls = Some(nlink_labd::config::TlsConfig {
            ca: p.join("ca.pem"),
            cert: p.join("daemon.pem"),
            key: p.join("daemon.key"),
        });
        c.zenoh.operators = vec!["op".into()];
        c.zenoh.viewers = vec!["view".into()];
    })
    .await;
    // it listens once its session is open
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let as_ = |name: &str| Endpoint::Zenoh {
        endpoint: format!("tls/localhost:{port}"),
        tls: Some(nlink_lab_client::TlsFiles {
            ca: pki.path().join("ca.pem"),
            cert: pki.path().join(format!("{name}.pem")),
            key: pki.path().join(format!("{name}.key")),
        }),
    };

    // an operator's certificate reaches the operator key
    let op = Client::connect(&as_("op"), "test/op").await.unwrap();
    assert_eq!(op.welcome().role, Role::Operator);
    let err = op
        .call(Request::DeleteSnapshot {
            lab: "nope".into(),
            name: "s".into(),
        })
        .await
        .unwrap_err();
    // the operation ran: the lab is not there
    assert!(
        matches!(&err, ClientError::Api(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );

    // a viewer's reads, and nothing more
    let viewer = Client::connect(&as_("view"), "test/view").await.unwrap();
    assert_eq!(viewer.welcome().role, Role::Viewer);
    assert!(viewer.list_labs().await.unwrap().is_empty());

    // the access control refuses it the operator key, the bridge aside
    let mut config = zenoh::Config::default();
    config.insert_json5("mode", "\"client\"").unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    config
        .insert_json5(
            "connect/endpoints",
            &serde_json::to_string(&[format!("tls/localhost:{port}")]).unwrap(),
        )
        .unwrap();
    let path = |f: &str| serde_json::to_string(&pki.path().join(f).display().to_string()).unwrap();
    config
        .insert_json5("transport/link/tls/root_ca_certificate", &path("ca.pem"))
        .unwrap();
    config
        .insert_json5("transport/link/tls/connect_certificate", &path("view.pem"))
        .unwrap();
    config
        .insert_json5("transport/link/tls/connect_private_key", &path("view.key"))
        .unwrap();
    config
        .insert_json5("transport/link/tls/enable_mtls", "true")
        .unwrap();
    let raw = zenoh::open(config).await.unwrap();
    let replies = raw
        .get("nlink-lab/@rpc/*/operator")
        .payload(serde_json::to_vec(&Request::Ping).unwrap())
        .timeout(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    let mut answered = false;
    while let Ok(reply) = replies.recv_async().await {
        answered |= reply.result().is_ok();
    }
    assert!(!answered, "a viewer's certificate reached the operator key");
    raw.close().await.unwrap();

    // a certificate the CA signed but no list names: no reads at all
    let err = Client::connect(&as_("stranger"), "test/x")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("no nlink-lab daemon answers"),
        "{err}"
    );

    // and without a certificate, no session
    let bare = Endpoint::Zenoh {
        endpoint: format!("tls/localhost:{port}"),
        tls: None,
    };
    assert!(Client::connect(&bare, "test/bare").await.is_err());
}
