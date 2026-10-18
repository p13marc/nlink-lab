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
const MADE: &str = "labd-e2e-made";
const MADE2: &str = "labd-e2e-made2";

/// The tests share the process's state root variable: one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _serial = SERIAL.lock().await;
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

/// A daemon on a fresh state root and socket, and an operator's client.
async fn start_daemon() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    CancelToken,
    Endpoint,
    Client,
) {
    let root = tempfile::tempdir().unwrap();
    // SAFETY: set before the daemon, the deploys and their threads run.
    unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", root.path()) };
    let dir = tempfile::tempdir().unwrap();
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
    let endpoint = Endpoint::Unix(socket);
    let client = Client::connect(&endpoint, "e2e/0").await.unwrap();
    (root, dir, shutdown, endpoint, client)
}

/// A call to its end, with the progress events it streamed.
async fn finish(
    call: &mut nlink_lab_client::Call,
) -> (
    Vec<nlink_lab_shared::api::OpEvent>,
    Option<String>,
    Result<Response, ClientError>,
) {
    let (mut events, mut op) = (Vec::new(), None);
    while let Some(event) = call.next().await {
        match event {
            CallEvent::Item(StreamItem::Op { event }) => events.push(event),
            CallEvent::Item(StreamItem::Started { op: Some(id), .. }) => op = Some(id),
            CallEvent::End(r) => {
                return (events, op, r.map_err(|e| ClientError::Api(Box::new(e))));
            }
            _ => {}
        }
    }
    panic!("no end")
}

fn impairments(
    endpoints: &[nlink_lab_shared::api::EndpointImpairment],
    ep: &str,
) -> (bool, Option<String>) {
    let e = endpoints.iter().find(|e| e.endpoint == ep);
    (
        e.is_some_and(|e| e.partitioned),
        e.and_then(|e| e.live.as_ref())
            .and_then(|l| l.delay.clone()),
    )
}

async fn get_impairments(
    client: &Client,
    lab: &str,
) -> Vec<nlink_lab_shared::api::EndpointImpairment> {
    match client
        .call(Request::GetImpairments { lab: lab.into() })
        .await
        .unwrap()
    {
        Response::Impairments { endpoints } => endpoints,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_changes_labs() {
    // `just ci` runs it rootless; the privileged CI lane runs it as root
    if !nlink_lab::privileges::has_privileges() {
        eprintln!("skipping the_daemon_changes_labs: requires root");
        return;
    }
    use nlink_lab_shared::api::{
        DeployOptions, Direction, ImpairmentSpec, OpPhase, OpState, SourceBundle, WaitSpec,
    };
    let _serial = SERIAL.lock().await;
    let (root, _dir, shutdown, endpoint, client) = start_daemon().await;
    let _cleanup = Cleanup(&[MADE, MADE2]);
    let mut ops = client
        .start(Request::Subscribe {
            lab: None,
            topics: vec![Topic::Ops],
            interval_ms: None,
        })
        .await
        .unwrap();

    // ── a deploy from a source bundle, with a param ──
    let text = format!(
        r#"param delay default 5ms
lab "{MADE}"
node a
node b
link a:eth0 -- b:eth0 {{
  10.233.0.1/24 -- 10.233.0.2/24
  delay ${{delay}}
}}
scenario "slow" {{
  at 0s {{ log "begins" }}
  at 10s {{ log "ends" }}
}}
"#
    );
    let bundle = SourceBundle {
        path: "/somewhere/on/the/client/made.nll".into(),
        text,
        imports: Default::default(),
        params: vec![("delay".into(), "20ms".into())],
    };
    let mut call = client
        .start(Request::Deploy {
            topology: serde_json::Value::Null,
            options: DeployOptions::default(),
            source: Some(bundle),
        })
        .await
        .unwrap();
    let (events, op, end) = finish(&mut call).await;
    let op = op.expect("a deploy is a job");
    match end.unwrap() {
        Response::Done {
            op: done,
            lab,
            report,
        } => {
            assert_eq!((done.as_str(), lab.as_str()), (op.as_str(), MADE));
            assert_eq!(report["nodes"], 2);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e.phase, OpPhase::Planned { .. }))
    );
    assert!(matches!(
        events.last().map(|e| &e.phase),
        Some(OpPhase::Finished { .. })
    ));
    // the progress went to the ops topic too
    match next_notification(&mut ops, Duration::from_secs(5)).await {
        Notification::OpProgress { event } => assert_eq!(event.op, op),
        other => panic!("{other:?}"),
    }
    // its source is kept with the lab, the param applied
    let source = nlink_lab::state::source_of(MADE).expect("a recorded source");
    let path = std::path::PathBuf::from(source.path.unwrap());
    assert!(path.starts_with(root.path()), "{path:?}");
    assert!(path.exists());
    assert_eq!(
        source.params,
        vec![("delay".to_string(), "20ms".to_string())]
    );
    let imp = get_impairments(&client, MADE).await;
    assert_eq!(
        imp[0].declared.as_ref().unwrap().delay.as_deref(),
        Some("20ms")
    );

    // listed, and attachable after its end: the same events, the same end
    match client.call(Request::ListOps).await.unwrap() {
        Response::Ops { ops } => {
            let info = ops.iter().find(|o| o.op == op).expect("listed");
            assert_eq!(
                (info.kind.as_str(), info.state),
                ("deploy", OpState::Succeeded)
            );
            assert_eq!(info.lab, MADE);
        }
        other => panic!("{other:?}"),
    }
    let mut again = client
        .start(Request::AttachOp { op: op.clone() })
        .await
        .unwrap();
    let (replayed, _, end) = finish(&mut again).await;
    assert_eq!(replayed.len(), events.len());
    assert!(matches!(end.unwrap(), Response::Done { .. }));

    // ── impairments, both ways; a partition and a heal ──
    match client
        .call(Request::Impair {
            lab: MADE.into(),
            endpoint: "a:eth0".into(),
            direction: Direction::Both,
            spec: ImpairmentSpec {
                delay: Some("30ms".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
    {
        Response::Report { report, .. } => assert_eq!(report["peer"], "b:eth0"),
        other => panic!("{other:?}"),
    }
    let imp = get_impairments(&client, MADE).await;
    assert_eq!(impairments(&imp, "a:eth0"), (false, Some("30ms".into())));
    assert_eq!(impairments(&imp, "b:eth0"), (false, Some("30ms".into())));
    // a bad value is refused before the kernel sees it
    let bad = client
        .call(Request::Impair {
            lab: MADE.into(),
            endpoint: "a:eth0".into(),
            direction: Direction::Out,
            spec: ImpairmentSpec {
                delay: Some("30".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(bad, ClientError::Api(e) if e.code == ErrorCode::InvalidArgument));
    client
        .call(Request::Partition {
            lab: MADE.into(),
            endpoint: "a:eth0".into(),
        })
        .await
        .unwrap();
    assert!(impairments(&get_impairments(&client, MADE).await, "a:eth0").0);
    client
        .call(Request::Heal {
            lab: MADE.into(),
            endpoint: "a:eth0".into(),
        })
        .await
        .unwrap();
    assert!(!impairments(&get_impairments(&client, MADE).await, "a:eth0").0);

    // ── a snapshot, a change, a restore ──
    client
        .call(Request::Snapshot {
            lab: MADE.into(),
            name: "s1".into(),
            description: None,
        })
        .await
        .unwrap();
    client
        .call(Request::Impair {
            lab: MADE.into(),
            endpoint: "a:eth0".into(),
            direction: Direction::Out,
            spec: ImpairmentSpec {
                delay: Some("80ms".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    match client
        .call(Request::Restore {
            lab: MADE.into(),
            name: "s1".into(),
            dry_run: true,
        })
        .await
        .unwrap()
    {
        Response::Report { kind, report } => {
            assert_eq!(kind, "dry_run");
            assert_eq!(report["no_op"], false, "{report}");
        }
        other => panic!("{other:?}"),
    }
    let mut restore = client
        .start(Request::Restore {
            lab: MADE.into(),
            name: "s1".into(),
            dry_run: false,
        })
        .await
        .unwrap();
    let (_, _, end) = finish(&mut restore).await;
    assert!(matches!(end.unwrap(), Response::Done { .. }));
    let imp = get_impairments(&client, MADE).await;
    assert_eq!(impairments(&imp, "a:eth0").1.as_deref(), Some("30ms"));

    // ── a spawned process, waited for, then killed ──
    let pid = match client
        .call(Request::Spawn {
            lab: MADE.into(),
            node: "a".into(),
            argv: vec!["sleep".into(), "300".into()],
            env: vec![],
            workdir: None,
            wait: Some(WaitSpec {
                exec: Some("true".into()),
                timeout_ms: 5000,
                ..Default::default()
            }),
        })
        .await
        .unwrap()
    {
        Response::Spawned { pid, stdout, .. } => {
            assert!(stdout.is_some());
            pid
        }
        other => panic!("{other:?}"),
    };
    match client
        .call(Request::ListProcesses { lab: MADE.into() })
        .await
        .unwrap()
    {
        Response::Processes { processes } => {
            assert!(processes.iter().any(|p| p.pid == pid && p.alive));
        }
        other => panic!("{other:?}"),
    }
    client
        .call(Request::Kill {
            lab: MADE.into(),
            pid,
            signal: None,
        })
        .await
        .unwrap();
    assert!(gone_within(pid, Duration::from_secs(3)).await);

    // ── an apply that changes the topology is announced ──
    let mut topo_sub = client
        .start(Request::Subscribe {
            lab: Some(MADE.into()),
            topics: vec![Topic::Topology],
            interval_ms: None,
        })
        .await
        .unwrap();
    let changed = nlink_lab::parser::parse(&format!(
        "lab \"{MADE}\"\nnode a\nnode b\nnode c\nlink a:eth0 -- b:eth0 {{ 10.233.0.1/24 -- 10.233.0.2/24 delay 20ms }}\nlink a:eth1 -- c:eth0 {{ 10.233.1.1/24 -- 10.233.1.2/24 }}\nscenario \"slow\" {{ at 0s {{ log \"begins\" }} at 10s {{ log \"ends\" }} }}\n"
    ))
    .unwrap();
    let changed = serde_json::to_value(&changed).unwrap();
    match client
        .call(Request::CheckApply {
            lab: MADE.into(),
            topology: changed.clone(),
            options: Default::default(),
        })
        .await
        .unwrap()
    {
        Response::Report { report, .. } => assert_eq!(report["no_op"], false),
        other => panic!("{other:?}"),
    }
    let mut apply = client
        .start(Request::Apply {
            lab: MADE.into(),
            topology: changed.clone(),
            options: Default::default(),
        })
        .await
        .unwrap();
    let (_, _, end) = finish(&mut apply).await;
    match end.unwrap() {
        Response::Done { report, .. } => assert!(report["ops"].as_u64().unwrap() > 0),
        other => panic!("{other:?}"),
    }
    match next_notification(&mut topo_sub, Duration::from_secs(5)).await {
        Notification::TopologyChanged { lab, .. } => assert_eq!(lab, MADE),
        other => panic!("{other:?}"),
    }
    match client
        .call(Request::Verify { lab: MADE.into() })
        .await
        .unwrap()
    {
        Response::Report { report, .. } => assert_eq!(report["no_op"], true, "{report}"),
        other => panic!("{other:?}"),
    }

    // ── a job left and found again: a deploy detached at once ──
    let second = nlink_lab::parser::parse(&format!(
        "lab \"{MADE2}\"\nnode x\nnode y\nlink x:eth0 -- y:eth0 {{ 10.234.0.1/24 -- 10.234.0.2/24 }}\n"
    ))
    .unwrap();
    let mut deploy2 = client
        .start(Request::Deploy {
            topology: serde_json::to_value(&second).unwrap(),
            options: DeployOptions::default(),
            source: None,
        })
        .await
        .unwrap();
    let op2 = loop {
        match deploy2.next().await.unwrap() {
            CallEvent::Item(StreamItem::Started { op: Some(op), .. }) => break op,
            CallEvent::End(r) => panic!("{r:?}"),
            _ => {}
        }
    };
    deploy2.detach().unwrap();
    let (_, _, end) = finish(&mut deploy2).await;
    // it may have ended first; detached or done, it went on
    assert!(
        matches!(&end, Ok(Response::Detached { op }) if *op == op2)
            || matches!(&end, Ok(Response::Done { .. })),
        "{end:?}"
    );
    let mut follow = client
        .start(Request::AttachOp { op: op2.clone() })
        .await
        .unwrap();
    let (_, _, end) = finish(&mut follow).await;
    assert!(matches!(end.unwrap(), Response::Done { .. }));
    assert!(
        client
            .list_labs()
            .await
            .unwrap()
            .iter()
            .any(|l| l.lab == MADE2)
            || {
                // the registry sees it within a scan
                tokio::time::sleep(Duration::from_millis(400)).await;
                client
                    .list_labs()
                    .await
                    .unwrap()
                    .iter()
                    .any(|l| l.lab == MADE2)
            }
    );

    // ── a dropped call: an exec stops ──
    let mut exec_call = client
        .start(Request::Exec {
            lab: MADE.into(),
            node: "a".into(),
            argv: vec!["sleep".into(), "30".into()],
            env: vec![],
            workdir: None,
            stdin: false,
            tty: false,
            timeout_ms: None,
        })
        .await
        .unwrap();
    let pid = match exec_call.next().await.unwrap() {
        CallEvent::Item(StreamItem::Started { pid: Some(pid), .. }) => pid,
        other => panic!("{other:?}"),
    };
    drop(exec_call);
    assert!(
        gone_within(pid, Duration::from_secs(3)).await,
        "{pid} still runs"
    );

    // ── a running scenario cancelled with cancel_op ──
    let mut scenario = client
        .start(Request::RunScenario {
            lab: MADE.into(),
            name: "slow".into(),
        })
        .await
        .unwrap();
    let first = scenario.next().await.unwrap();
    if let CallEvent::Item(StreamItem::Started { op: Some(sop), .. }) = first {
        tokio::time::sleep(Duration::from_millis(300)).await;
        client.call(Request::CancelOp { op: sop }).await.unwrap();
        let started = Instant::now();
        let (_, _, end) = finish(&mut scenario).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            matches!(&end, Err(ClientError::Api(e)) if e.code == ErrorCode::Cancelled),
            "{end:?}"
        );
    } else {
        panic!("{first:?}");
    }

    // ── destroy, through the daemon ──
    for lab in [MADE, MADE2] {
        let mut destroy = client
            .start(Request::Destroy { lab: lab.into() })
            .await
            .unwrap();
        let (_, _, end) = finish(&mut destroy).await;
        assert!(matches!(end.unwrap(), Response::Done { .. }), "{lab}");
    }
    assert!(!nlink_lab::state::exists(MADE));
    drop(endpoint);
    shutdown.cancel();
    unsafe { std::env::remove_var("NLINK_LAB_STATE_DIR") };
}

const SESSIONS: &str = "labd-e2e-sessions";

/// Data of `stream` until `needle` shows up in it, within `within`.
async fn data_until(
    call: &mut nlink_lab_client::Call,
    stream: DataStream,
    needle: &str,
    within: Duration,
) -> String {
    let mut seen = String::new();
    let deadline = Instant::now() + within;
    while !seen.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, call.next()).await {
            Ok(Some(CallEvent::Data(s, bytes))) if s == stream => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
            }
            Ok(Some(CallEvent::End(r))) => panic!("ended: {r:?} after {seen:?}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("closed after {seen:?}"),
            Err(_) => panic!("no {needle:?} within {within:?}: {seen:?}"),
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_runs_sessions() {
    // `just ci` runs it rootless; the privileged CI lane runs it as root
    if !nlink_lab::privileges::has_privileges() {
        eprintln!("skipping the_daemon_runs_sessions: requires root");
        return;
    }
    use nlink_lab_shared::api::{LogStream, WaitSpec};
    let _serial = SERIAL.lock().await;
    let (_root, _dir, shutdown, _endpoint, client) = start_daemon().await;
    let _cleanup = Cleanup(&[SESSIONS]);
    let running = topology(SESSIONS, 235).deploy().await.expect("deploy");

    // ── a shell on a terminal: its size, a resize, its exit code ──
    let mut shell = client
        .start(Request::Shell {
            lab: SESSIONS.into(),
            node: "a".into(),
            shell: Some("sh".into()),
            rows: 24,
            cols: 80,
        })
        .await
        .unwrap();
    shell.stdin(b"stty size\n").unwrap();
    data_until(&mut shell, DataStream::Pty, "24 80", Duration::from_secs(5)).await;
    shell.resize(40, 100).unwrap();
    shell.stdin(b"stty size\n").unwrap();
    data_until(
        &mut shell,
        DataStream::Pty,
        "40 100",
        Duration::from_secs(5),
    )
    .await;
    shell.stdin(b"exit 3\n").unwrap();
    let (_, _, end) = finish(&mut shell).await;
    assert!(
        matches!(end, Ok(Response::Exited { code: Some(3), .. })),
        "{end:?}"
    );

    // ── a spawned process's log, followed ──
    let pid = match client
        .call(Request::Spawn {
            lab: SESSIONS.into(),
            node: "a".into(),
            argv: ["sh", "-c", "echo first; sleep 0.5; echo second; sleep 60"]
                .map(String::from)
                .to_vec(),
            env: vec![],
            workdir: None,
            wait: None,
        })
        .await
        .unwrap()
    {
        Response::Spawned { pid, .. } => pid,
        other => panic!("{other:?}"),
    };
    let mut logs = client
        .start(Request::Logs {
            lab: SESSIONS.into(),
            pid,
            stream: LogStream::Stdout,
            tail: None,
            follow: true,
        })
        .await
        .unwrap();
    let seen = data_until(
        &mut logs,
        DataStream::Stdout,
        "second",
        Duration::from_secs(5),
    )
    .await;
    assert!(seen.contains("first"), "{seen}");
    logs.cancel().unwrap();
    let (_, _, end) = finish(&mut logs).await;
    assert!(
        matches!(&end, Err(ClientError::Api(e)) if e.code == ErrorCode::Cancelled),
        "{end:?}"
    );
    client
        .call(Request::Kill {
            lab: SESSIONS.into(),
            pid,
            signal: None,
        })
        .await
        .unwrap();

    // ── a capture while a ping runs ──
    let ping = match client
        .call(Request::Spawn {
            lab: SESSIONS.into(),
            node: "a".into(),
            argv: ["ping", "-i", "0.2", "10.235.0.2"]
                .map(String::from)
                .to_vec(),
            env: vec![],
            workdir: None,
            wait: None,
        })
        .await
        .unwrap()
    {
        Response::Spawned { pid, .. } => pid,
        other => panic!("{other:?}"),
    };
    let mut capture = client
        .start(Request::Capture {
            lab: SESSIONS.into(),
            endpoint: "a:eth0".into(),
            filter: nlink_lab_shared::api::CaptureFilter {
                icmp: true,
                ..Default::default()
            },
            count: Some(4),
            duration_ms: Some(10_000),
        })
        .await
        .unwrap();
    let mut pcap = Vec::new();
    let report = loop {
        match capture.next().await.unwrap() {
            CallEvent::Data(DataStream::Pcap, bytes) => pcap.extend(bytes),
            CallEvent::End(r) => break r.unwrap(),
            _ => {}
        }
    };
    match report {
        Response::Report { kind, report } => {
            assert_eq!(kind, "capture");
            assert_eq!(report["packets"], 4, "{report}");
        }
        other => panic!("{other:?}"),
    }
    // the pcap's magic (nanosecond, little-endian) and more than a header
    assert_eq!(
        &pcap[..4],
        &0xa1b23c4du32.to_le_bytes(),
        "{:x?}",
        &pcap[..8]
    );
    assert!(pcap.len() > 24 + 4 * 60, "{} bytes", pcap.len());
    client
        .call(Request::Kill {
            lab: SESSIONS.into(),
            pid: ping,
            signal: None,
        })
        .await
        .unwrap();

    // ── waits: one met, one that times out ──
    client
        .call(Request::WaitFor {
            lab: SESSIONS.into(),
            node: "a".into(),
            cond: WaitSpec {
                file: Some("/proc/self".into()),
                timeout_ms: 5000,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let late = client
        .call(Request::WaitFor {
            lab: SESSIONS.into(),
            node: "a".into(),
            cond: WaitSpec {
                file: Some("/no/such/file".into()),
                timeout_ms: 600,
                ..Default::default()
            },
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&late, ClientError::Api(e) if e.code == ErrorCode::Timeout),
        "{late:?}"
    );

    // ── the lab as an archive ──
    let mut export = client
        .start(Request::Export {
            lab: SESSIONS.into(),
        })
        .await
        .unwrap();
    let mut archive = Vec::new();
    let report = loop {
        match export.next().await.unwrap() {
            CallEvent::Data(DataStream::Archive, bytes) => archive.extend(bytes),
            CallEvent::End(r) => break r.unwrap(),
            _ => {}
        }
    };
    match report {
        Response::Report { report, .. } => {
            assert_eq!(report["bytes"].as_u64(), Some(archive.len() as u64));
            assert!(archive.len() > 100);
        }
        other => panic!("{other:?}"),
    }

    // ── a hand edit is drift ──
    let mut drift = client
        .start(Request::Subscribe {
            lab: Some(SESSIONS.into()),
            topics: vec![Topic::Drift],
            interval_ms: None,
        })
        .await
        .unwrap();
    // the watch subscribes in every node first
    tokio::time::sleep(Duration::from_millis(800)).await;
    let out = running
        .exec("a", "ip", &["link", "set", "dev", "eth0", "mtu", "1400"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    match next_notification(&mut drift, Duration::from_secs(5)).await {
        Notification::Drift { lab, node, .. } => {
            assert_eq!((lab.as_str(), node.as_str()), (SESSIONS, "a"));
        }
        other => panic!("{other:?}"),
    }
    drop(drift);

    shutdown.cancel();
    running.destroy().await.unwrap();
    unsafe { std::env::remove_var("NLINK_LAB_STATE_DIR") };
}
