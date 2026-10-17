//! The daemon against real labs (root): reads, a metrics subscription,
//! exec with output, exit code, stdin, cancel and a client that goes
//! away, a lab deployed behind the daemon's back adopted within a scan,
//! and uid 65534 refused through `dial-stdio`.
//!
//! Run with: `sudo -E env PATH=$PATH:/usr/sbin:/sbin target/debug/deps/e2e-…`

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use nlink_lab::CancelToken;
use nlink_lab_client::{CallEvent, Client, ClientError, Endpoint};
use nlink_lab_shared::api::{
    DataStream, ErrorCode, Notification, Request, Response, Role, StreamItem, Topic,
};
use nlink_labd::Config;

const LAB: &str = "labd-e2e";
const LATE: &str = "labd-e2e-late";

fn topology(name: &str, subnet: u8) -> nlink_lab::Topology {
    nlink_lab::parser::parse(&format!(
        r#"lab "{name}"
node a
node b
link a:eth0 -- b:eth0 {{
  10.{subnet}.0.1/24 -- 10.{subnet}.0.2/24
  delay 5ms
}}
"#
    ))
    .unwrap()
}

/// Destroys the labs a failed test leaves.
struct Cleanup(&'static [&'static str]);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let labs = self.0;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for lab in labs {
                rt.block_on(async {
                    match nlink_lab::RunningLab::load(lab) {
                        Ok(running) => {
                            let _ = running.destroy().await;
                        }
                        Err(_) => {
                            nlink_lab::host::force_cleanup(lab, lab).await;
                        }
                    }
                });
            }
        })
        .join()
        .unwrap();
    }
}

async fn next_notification(call: &mut nlink_lab_client::Call, within: Duration) -> Notification {
    loop {
        match tokio::time::timeout(within, call.next()).await {
            Ok(Some(CallEvent::Item(StreamItem::Notification { notification }))) => {
                return notification;
            }
            Ok(Some(CallEvent::Item(_))) => continue,
            Ok(other) => panic!("the subscription ended: {other:?}"),
            Err(_) => panic!("no notification within {within:?}"),
        }
    }
}

/// An exec to its end: stdout, stderr and the response.
async fn exec(
    call: &mut nlink_lab_client::Call,
) -> (Option<u32>, String, String, Result<Response, ClientError>) {
    let (mut pid, mut out, mut err) = (None, String::new(), String::new());
    while let Some(event) = call.next().await {
        match event {
            CallEvent::Item(StreamItem::Started { pid: p, .. }) => pid = p,
            CallEvent::Data(DataStream::Stdout, b) => out.push_str(&String::from_utf8_lossy(&b)),
            CallEvent::Data(DataStream::Stderr, b) => err.push_str(&String::from_utf8_lossy(&b)),
            CallEvent::End(r) => {
                return (pid, out, err, r.map_err(|e| ClientError::Api(Box::new(e))));
            }
            _ => {}
        }
    }
    panic!("no end")
}

fn exec_request(argv: &[&str], stdin: bool, timeout_ms: Option<u64>) -> Request {
    Request::Exec {
        lab: LAB.into(),
        node: "a".into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: vec![("GREETING".into(), "hi".into())],
        workdir: None,
        stdin,
        tty: false,
        timeout_ms,
    }
}

fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
        && !std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .unwrap_or_default()
            .contains(") Z ")
}

async fn gone_within(pid: u32, within: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < within {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_serves_real_labs() {
    // `just ci` runs it rootless; the privileged CI lane runs it as root
    if !nlink_lab::privileges::has_privileges() {
        eprintln!("skipping the_daemon_serves_real_labs: requires root");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    // SAFETY: set before the daemon, the deploys and their threads run.
    unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", root.path()) };
    let _cleanup = Cleanup(&[LAB, LATE]);
    let running = topology(LAB, 231).deploy().await.expect("deploy");

    // a socket uid 65534 can reach
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = dir.path().join("api.sock");
    let config = Config {
        socket: socket.clone(),
        rescan_ms: 200,
        metrics_interval_ms: 500,
        ..Config::default()
    };
    let listener = nlink_labd::listen::bind(&socket).unwrap();
    let shutdown = CancelToken::new();
    tokio::spawn(nlink_labd::serve(config, listener, shutdown.clone()));
    let endpoint = Endpoint::Unix(socket.clone());
    let client = Client::connect(&endpoint, "e2e/0").await.unwrap();
    assert_eq!(client.welcome().role, Role::Operator);

    // ── reads ──
    let labs = client.list_labs().await.unwrap();
    let summary = labs
        .iter()
        .find(|l| l.lab == LAB)
        .expect("the lab is listed");
    assert_eq!((summary.nodes, summary.links), (2, 1));
    assert_eq!(summary.health, "running");
    assert_eq!(summary.impaired, 2, "{summary:?}");
    assert_eq!(summary.revision.len(), 16);
    match client
        .call(Request::GetLab { lab: LAB.into() })
        .await
        .unwrap()
    {
        Response::Lab { detail, .. } => {
            assert_eq!(detail["nodes"].as_array().unwrap().len(), 2);
            assert_eq!(detail["nodes"][0]["addresses"]["eth0"][0], "10.231.0.1/24");
        }
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::GetTopology {
            lab: LAB.into(),
            redact: true,
        })
        .await
        .unwrap()
    {
        Response::Topology {
            revision, topology, ..
        } => {
            assert_eq!(revision, summary.revision);
            assert!(topology["nodes"]["a"].is_object());
        }
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::GetImpairments { lab: LAB.into() })
        .await
        .unwrap()
    {
        Response::Impairments { endpoints } => {
            assert_eq!(endpoints.len(), 2);
            assert_eq!(
                endpoints[0].declared.as_ref().unwrap().delay.as_deref(),
                Some("5ms")
            );
            assert!(endpoints[0].live.is_none());
        }
        other => panic!("{other:?}"),
    }
    let (events, cursor) = match client
        .call(Request::ReadEvents {
            lab: LAB.into(),
            since: None,
            kinds: vec![],
            limit: None,
        })
        .await
        .unwrap()
    {
        Response::Events { events, next } => (events, next),
        other => panic!("{other:?}"),
    };
    assert_eq!(events[0]["event"], "deployed");
    match client
        .call(Request::ReadEvents {
            lab: LAB.into(),
            since: Some(cursor),
            kinds: vec![],
            limit: None,
        })
        .await
        .unwrap()
    {
        Response::Events { events, next } => {
            assert!(events.is_empty());
            assert_eq!(next, cursor);
        }
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::Diagnose {
            lab: LAB.into(),
            node: Some("a".into()),
        })
        .await
        .unwrap()
    {
        Response::Report { kind, report } => {
            assert_eq!(kind, "diagnose");
            assert_eq!(report[0]["node"], "a");
        }
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::NodeAddresses {
            lab: LAB.into(),
            node: "b".into(),
        })
        .await
        .unwrap()
    {
        Response::Report { report, .. } => assert_eq!(report["eth0"][0], "10.231.0.2/24"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        client
            .call(Request::GetMetrics { lab: LAB.into() })
            .await
            .unwrap(),
        Response::Metrics { .. }
    ));

    // ── a metrics subscription ──
    let mut metrics = client
        .start(Request::Subscribe {
            lab: Some(LAB.into()),
            topics: vec![Topic::Metrics, Topic::Lifecycle],
            interval_ms: Some(500),
        })
        .await
        .unwrap();
    match next_notification(&mut metrics, Duration::from_secs(5)).await {
        Notification::Metrics { lab, snapshot } => {
            assert_eq!(lab, LAB);
            assert!(snapshot.nodes.contains_key("a"), "{snapshot:?}");
        }
        other => panic!("{other:?}"),
    }
    // the collector's latest now answers get_metrics
    assert!(matches!(
        client
            .call(Request::GetMetrics { lab: LAB.into() })
            .await
            .unwrap(),
        Response::Metrics { .. }
    ));
    // an event the library records comes as a lifecycle notification
    nlink_lab::events::record(
        LAB,
        nlink_lab::LifecycleKind::SnapshotTaken { name: "e2e".into() },
    );
    let start = Instant::now();
    loop {
        match next_notification(&mut metrics, Duration::from_secs(5)).await {
            Notification::Lifecycle { event, .. } => {
                assert_eq!(event["event"], "snapshot_taken");
                break;
            }
            Notification::Metrics { .. } if start.elapsed() < Duration::from_secs(5) => continue,
            other => panic!("{other:?}"),
        }
    }
    metrics.cancel().unwrap();

    // ── exec ──
    let mut call = client
        .start(exec_request(
            &[
                "sh",
                "-c",
                "echo $GREETING; echo oops >&2; test -z \"$NLINK_LAB_STATE_DIR\"; exit 3",
            ],
            false,
            None,
        ))
        .await
        .unwrap();
    let (pid, out, err, end) = exec(&mut call).await;
    assert!(pid.is_some());
    assert_eq!(out, "hi\n");
    assert_eq!(err, "oops\n");
    match end.unwrap() {
        Response::Exited {
            code, timed_out, ..
        } => {
            // 3, not 1: the daemon's environment did not leak in
            assert_eq!(code, Some(3));
            assert!(!timed_out);
        }
        other => panic!("{other:?}"),
    }

    let mut call = client
        .start(exec_request(&["cat"], true, None))
        .await
        .unwrap();
    call.stdin(b"through stdin").unwrap();
    call.eof().unwrap();
    let (_, out, _, end) = exec(&mut call).await;
    assert_eq!(out, "through stdin");
    assert!(matches!(
        end.unwrap(),
        Response::Exited { code: Some(0), .. }
    ));

    // a timeout, then a cancel
    let mut call = client
        .start(exec_request(&["sleep", "30"], false, Some(300)))
        .await
        .unwrap();
    let (_, _, _, end) = exec(&mut call).await;
    assert!(matches!(
        end.unwrap(),
        Response::Exited {
            timed_out: true,
            ..
        }
    ));

    let mut call = client
        .start(exec_request(&["sleep", "30"], false, None))
        .await
        .unwrap();
    let pid = match call.next().await.unwrap() {
        CallEvent::Item(StreamItem::Started { pid: Some(pid), .. }) => pid,
        other => panic!("{other:?}"),
    };
    call.cancel().unwrap();
    let (_, _, _, end) = exec(&mut call).await;
    assert!(matches!(end, Err(ClientError::Api(e)) if e.code == ErrorCode::Cancelled));
    assert!(
        gone_within(pid, Duration::from_secs(3)).await,
        "{pid} still runs"
    );

    // a client that goes away takes its commands with it
    let other = Client::connect(&endpoint, "e2e/1").await.unwrap();
    let mut call = other
        .start(exec_request(&["sleep", "30"], false, None))
        .await
        .unwrap();
    let pid = match call.next().await.unwrap() {
        CallEvent::Item(StreamItem::Started { pid: Some(pid), .. }) => pid,
        other => panic!("{other:?}"),
    };
    drop(call);
    drop(other);
    assert!(
        gone_within(pid, Duration::from_secs(3)).await,
        "{pid} still runs"
    );

    // the operator's calls are audited
    let audit = std::fs::read_to_string(root.path().join("audit.ndjson")).unwrap();
    assert!(
        audit.lines().any(|l| l.contains("\"method\":\"exec\"")),
        "{audit}"
    );
    assert!(
        !audit.contains("\"hi\""),
        "an env value in the audit log: {audit}"
    );

    // ── a lab deployed behind the daemon's back is adopted ──
    let mut labs_sub = client
        .start(Request::Subscribe {
            lab: None,
            topics: vec![Topic::Labs],
            interval_ms: None,
        })
        .await
        .unwrap();
    let _ = next_notification(&mut labs_sub, Duration::from_secs(5)).await;
    let late = topology(LATE, 232)
        .deploy()
        .await
        .expect("deploy the late lab");
    let start = Instant::now();
    loop {
        match next_notification(&mut labs_sub, Duration::from_secs(3)).await {
            Notification::LabsChanged { added, .. } if added.iter().any(|l| l.lab == LATE) => break,
            _ => assert!(
                start.elapsed() < Duration::from_secs(3),
                "not adopted in 3 s"
            ),
        }
    }
    late.destroy().await.unwrap();
    loop {
        match next_notification(&mut labs_sub, Duration::from_secs(3)).await {
            Notification::LabsChanged { removed, .. } if removed.iter().any(|l| l == LATE) => break,
            _ => {}
        }
    }

    // ── uid 65534 is refused (over dial-stdio, which it runs) ──
    let nobody = Endpoint::Command {
        program: "setpriv".into(),
        args: vec![
            "--reuid=65534".into(),
            "--regid=65534".into(),
            "--clear-groups".into(),
            env!("CARGO_BIN_EXE_nlink-labd").into(),
            "dial-stdio".into(),
            "--socket".into(),
            socket.display().to_string(),
        ],
    };
    match Client::connect(&nobody, "e2e/nobody").await.unwrap_err() {
        ClientError::Api(e) => assert_eq!(e.code, ErrorCode::PermissionDenied),
        other => panic!("{other:?}"),
    }

    shutdown.cancel();
    running.destroy().await.unwrap();
    unsafe { std::env::remove_var("NLINK_LAB_STATE_DIR") };
}
