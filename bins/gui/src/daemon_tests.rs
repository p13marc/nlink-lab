//! The app's backend against a real daemon, in-process on a temporary
//! socket and state root, as the user running the tests (no root, no
//! lab): connecting, the labs and their subscription, a lab that is not
//! there, and over Zenoh — which streams nothing — the polling.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use iced::futures::StreamExt;
use nlink_lab::CancelToken;
use nlink_lab_client::Endpoint;
use nlink_lab_shared::api::{Notification, Request, Response, Role, Topic};

use crate::backend::{Backend, Feed, Target};

/// One state root for the test binary: the variable is the process's.
fn state_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        // SAFETY: set once, before any daemon thread reads it; nothing
        // else in this binary reads it.
        unsafe { std::env::set_var("NLINK_LAB_STATE_DIR", &dir) };
        dir
    })
}

/// The name of the user's primary group: the daemon's operators here.
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
    unix: Endpoint,
    zenoh: Option<Endpoint>,
    shutdown: CancelToken,
    _dir: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn exists(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

async fn daemon(zenoh: bool) -> Daemon {
    let root = state_root();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("api.sock");
    let zsock = dir.path().join("zenoh/zenoh.sock");
    let config = nlink_labd::Config {
        socket: socket.clone(),
        operator_group: my_group(),
        viewer_group: "no-such-group-nlink".into(),
        rescan_ms: 100,
        audit_log: Some(root.join("audit.ndjson")),
        zenoh: nlink_labd::config::ZenohConfig {
            enabled: zenoh,
            listen: vec![format!("unixsock-stream/{}", zsock.display())],
            ..Default::default()
        },
        ..nlink_labd::Config::default()
    };
    let listener = nlink_labd::listen::bind(&socket).unwrap();
    let shutdown = CancelToken::new();
    tokio::spawn(nlink_labd::serve(config, listener, shutdown.clone()));
    let zenoh = if zenoh {
        exists(&zsock).await;
        Some(Endpoint::Zenoh {
            endpoint: format!("unixsock-stream/{}", zsock.display()),
            tls: None,
        })
    } else {
        None
    };
    Daemon {
        unix: Endpoint::Unix(socket),
        zenoh,
        shutdown,
        _dir: dir,
    }
}

/// The first item of a stream, within a few seconds.
async fn first<T>(mut s: impl iced::futures::Stream<Item = T> + Unpin) -> T {
    tokio::time::timeout(Duration::from_secs(10), s.next())
        .await
        .expect("an item in time")
        .expect("an item")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_app_reads_a_daemon() {
    let d = daemon(false).await;
    let (backend, who) = Backend::connect(Target::Daemon(d.unix.clone()))
        .await
        .unwrap();
    assert_eq!(who.role, Role::Operator);
    assert!(!backend.is_demo());
    assert!(matches!(
        backend.call(Request::ListLabs).await.unwrap(),
        Response::Labs { labs } if labs.is_empty()
    ));
    // the labs' subscription starts with the labs there are
    let labs = backend.clone().subscribe(None, vec![Topic::Labs], 1000);
    match first(labs).await {
        Ok(Feed::Note(n)) => assert!(
            matches!(*n, Notification::LabsChanged { ref added, .. } if added.is_empty()),
            "{n:?}"
        ),
        other => panic!("{other:?}"),
    }
    // a lab that is not there: the subscription says why, and ends
    let lab = backend.clone().subscribe(
        Some("nope".into()),
        vec![
            Topic::Metrics,
            Topic::Topology,
            Topic::Lifecycle,
            Topic::Runtime,
        ],
        1000,
    );
    match first(lab).await {
        Err(reason) => assert!(reason.contains("nope"), "{reason}"),
        other => panic!("{other:?}"),
    }
    // and a fetch of it fails, said so
    let fetched = crate::effect::fetched(
        backend
            .call(crate::effect::request_of(
                "nope",
                &crate::effect::Fetch::Impairments,
            ))
            .await,
        Default::default(),
    );
    assert!(
        matches!(fetched, crate::message::Fetched::Failed(_)),
        "{fetched:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_zenoh_the_app_asks_instead() {
    let d = daemon(true).await;
    let (backend, who) = Backend::connect(Target::Daemon(d.zenoh.clone().unwrap()))
        .await
        .unwrap();
    // no TLS: a viewer, whoever connects
    assert_eq!(who.role, Role::Viewer);
    let labs = backend.clone().subscribe(None, vec![Topic::Labs], 1000);
    match first(labs).await {
        Ok(Feed::Labs(labs)) => assert!(labs.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn no_daemon_is_an_error_to_retry() {
    let dir = tempfile::tempdir().unwrap();
    let err = Backend::connect(Target::Daemon(Endpoint::Unix(dir.path().join("none.sock"))))
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}

/// A lab's nodes as the sessions reach them: a command's output and exit,
/// a command stopped, a capture into a file, a shell through the bridge.
/// Root: it deploys a lab.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_reach_a_real_lab() {
    use crate::backend::SessionItem;
    use nlink_lab_shared::api::DataStream;
    if !nlink_lab::privileges::has_privileges() {
        eprintln!("skipping sessions_reach_a_real_lab: requires root");
        return;
    }
    state_root();
    let lab = format!("gui-e2e-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        "lab \"{lab}\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 {{ 10.239.0.1/24 -- 10.239.0.2/24 }}\n"
    ))
    .unwrap();
    let running = topo.deploy().await.expect("deploy");
    struct Destroy(Option<nlink_lab::RunningLab>);
    impl Drop for Destroy {
        fn drop(&mut self) {
            if let Some(lab) = self.0.take() {
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    let _ = rt.block_on(lab.destroy());
                })
                .join()
                .unwrap();
            }
        }
    }
    let _destroy = Destroy(Some(running));
    let d = daemon(false).await;
    let (backend, _) = Backend::connect(Target::Daemon(d.unix.clone()))
        .await
        .unwrap();
    let exec = |argv: &[&str]| Request::Exec {
        lab: lab.clone(),
        node: "a".into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: Vec::new(),
        workdir: None,
        stdin: false,
        tty: false,
        timeout_ms: None,
    };

    // a command: its streams, its exit
    let items: Vec<SessionItem> = backend
        .clone()
        .session(
            exec(&["sh", "-c", "echo hi; echo oops >&2; exit 3"]),
            CancelToken::new(),
        )
        .collect()
        .await;
    let out: Vec<u8> = items
        .iter()
        .filter_map(|i| match i {
            SessionItem::Data(DataStream::Stdout, b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(String::from_utf8_lossy(&out), "hi\n");
    assert!(
        items
            .iter()
            .any(|i| matches!(i, SessionItem::Data(DataStream::Stderr, _)))
    );
    assert!(
        matches!(
            items.last(),
            Some(SessionItem::End(Ok(Response::Exited { code: Some(3), .. })))
        ),
        "{items:?}"
    );

    // a command stopped: SIGINT, and it ends
    let cancel = CancelToken::new();
    let mut sleep = backend
        .clone()
        .session(exec(&["sleep", "30"]), cancel.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    let end = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(SessionItem::End(r)) = sleep.next().await {
                return r;
            }
        }
    })
    .await
    .expect("stopped in time");
    assert!(
        matches!(
            end,
            Ok(Response::Exited {
                code: None | Some(130),
                ..
            })
        ),
        "{end:?}"
    );

    // a capture while a ping crosses the link, into a pcap file
    let dir = tempfile::tempdir().unwrap();
    let pcap = dir.path().join("a.pcap");
    let cancel = CancelToken::new();
    let capture = backend.clone().session(
        Request::Capture {
            lab: lab.clone(),
            endpoint: "a:eth0".into(),
            filter: Default::default(),
            count: None,
            duration_ms: None,
        },
        cancel.clone(),
    );
    let written = tokio::spawn(crate::effect::capture_to(capture, pcap.clone()));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ping: Vec<SessionItem> = backend
        .clone()
        .session(
            exec(&["ping", "-c", "3", "-i", "0.2", "10.239.0.2"]),
            CancelToken::new(),
        )
        .collect()
        .await;
    assert!(
        matches!(
            ping.last(),
            Some(SessionItem::End(Ok(Response::Exited { code: Some(0), .. })))
        ),
        "{ping:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    let (how, ok) = tokio::time::timeout(Duration::from_secs(10), written)
        .await
        .expect("the capture ends")
        .unwrap();
    assert!(ok, "{how}");
    let bytes = std::fs::read(&pcap).unwrap();
    assert!(bytes.len() > 24 + 3 * 98, "{} bytes: {how}", bytes.len());
    assert!(
        [
            [0xd4, 0xc3, 0xb2, 0xa1],
            [0xa1, 0xb2, 0xc3, 0xd4],
            [0x4d, 0x3c, 0xb2, 0xa1]
        ]
        .contains(&[bytes[0], bytes[1], bytes[2], bytes[3]]),
        "a pcap file"
    );

    // a shell through the bridge: what is typed, its output, its exit
    let (mut keys, input) = tokio::io::duplex(4096);
    let (output, mut screen) = tokio::io::duplex(65536);
    let shell = tokio::spawn({
        let endpoint = d.unix.clone();
        let lab = lab.clone();
        async move {
            crate::bridge::relay_io(
                &endpoint,
                crate::bridge::Shell {
                    lab,
                    node: "b".into(),
                    shell: Some("/bin/sh".into()),
                },
                (24, 80),
                input,
                output,
                None,
            )
            .await
        }
    });
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    keys.write_all(b"echo from-$(hostname -s 2>/dev/null || echo b)-shell; exit 7\n")
        .await
        .unwrap();
    let code = tokio::time::timeout(Duration::from_secs(15), shell)
        .await
        .expect("the shell ends")
        .unwrap();
    assert_eq!(code, Ok(7));
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(500), screen.read_to_end(&mut seen)).await;
    assert!(
        String::from_utf8_lossy(&seen).contains("-shell"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
}

/// A design deployed, changed and applied as the app does it: the plan
/// first, the file sent with its imports, the progress as it comes, the
/// diff before the apply, the assertions run. Root: it deploys a lab.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflows_reach_a_real_daemon() {
    use crate::backend::SessionItem;
    use nlink_lab_shared::api::{OpOutcome, OpPhase};
    if !nlink_lab::privileges::has_privileges() {
        eprintln!("skipping workflows_reach_a_real_daemon: requires root");
        return;
    }
    state_root();
    let lab = format!("gui-wf-{}", std::process::id());
    struct Destroy(String);
    impl Drop for Destroy {
        fn drop(&mut self) {
            let lab = self.0.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                if let Ok(running) = nlink_lab::RunningLab::load(&lab) {
                    let _ = rt.block_on(running.destroy());
                }
            })
            .join()
            .unwrap();
        }
    }
    let _destroy = Destroy(lab.clone());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pair.nll"),
        "lab \"pair\"\nnode r1\nnode r2\nlink r1:eth0 -- r2:eth0 { 10.238.0.1/24 -- 10.238.0.2/24 }\n",
    )
    .unwrap();
    let main = dir.path().join("main.nll");
    let design = |extra: &str| {
        format!("import \"pair.nll\" as p\nlab \"{lab}\"\nvalidate {{ reach p-r1 p-r2 }}\n{extra}")
    };
    std::fs::write(&main, design("")).unwrap();
    let d = daemon(false).await;
    let (backend, _) = Backend::connect(Target::Daemon(d.unix.clone()))
        .await
        .unwrap();
    let topology =
        |path: &Path| serde_json::to_value(nlink_lab::parser::parse_file(path).unwrap()).unwrap();
    let ends = |items: &[SessionItem]| -> Option<OpOutcome> {
        items.iter().find_map(|i| match i {
            SessionItem::Op(e) => match &e.phase {
                OpPhase::Finished { outcome, .. } => Some(outcome.clone()),
                _ => None,
            },
            _ => None,
        })
    };

    // the plan, before anything runs
    let Response::Report { report, .. } = backend
        .call(Request::PlanDeploy {
            topology: topology(&main),
        })
        .await
        .unwrap()
    else {
        panic!("a report")
    };
    let plan = crate::control::describe_plan(&report);
    assert!(plan.contains("step(s)") && plan.contains("p-r1"), "{plan}");

    // the deploy: the file and its import, its progress, its end
    let source =
        crate::effect::bundle(&crate::doc::Source::File(main.clone()), Vec::new()).unwrap();
    assert_eq!(source.imports.keys().collect::<Vec<_>>(), ["pair.nll"]);
    let items: Vec<SessionItem> = backend
        .clone()
        .session(
            Request::Deploy {
                topology: topology(&main),
                options: Default::default(),
                source: Some(source),
            },
            CancelToken::new(),
        )
        .collect()
        .await;
    assert!(
        items
            .iter()
            .any(|i| matches!(i, SessionItem::Op(e) if matches!(e.phase, OpPhase::Stage { .. }))),
        "{items:?}"
    );
    assert!(
        matches!(ends(&items), Some(OpOutcome::Succeeded)),
        "{items:?}"
    );
    assert!(
        matches!(items.last(), Some(SessionItem::End(Ok(Response::Done { lab: l, .. }))) if *l == lab),
        "{items:?}"
    );
    // the job's view of it
    let mut job = crate::job::Job::new(1, &lab, crate::job::JobKind::Deploy);
    for i in &items {
        if let SessionItem::Op(e) = i {
            job.event(e);
        }
    }
    assert!(
        matches!(job.state, crate::job::JobState::Done { ok: true, .. }),
        "{job:?}"
    );
    assert!(job.op.is_some());

    // a change: the diff first, then the apply
    std::fs::write(
        &main,
        design("node c\nlink c:eth0 -- p-r1:eth1 { 10.237.0.2/24 -- 10.237.0.1/24 }\n"),
    )
    .unwrap();
    let Response::Report { report, .. } = backend
        .call(Request::CheckApply {
            lab: lab.clone(),
            topology: topology(&main),
            options: Default::default(),
        })
        .await
        .unwrap()
    else {
        panic!("a report")
    };
    let diff = crate::control::describe_plan(&report);
    assert!(diff.contains("+ node: c"), "{diff}");
    let items: Vec<SessionItem> = backend
        .clone()
        .session(
            Request::Apply {
                lab: lab.clone(),
                topology: topology(&main),
                options: Default::default(),
            },
            CancelToken::new(),
        )
        .collect()
        .await;
    assert!(
        matches!(items.last(), Some(SessionItem::End(Ok(_)))),
        "{items:?}"
    );
    let Response::Topology { topology: now, .. } = backend
        .call(Request::GetTopology {
            lab: lab.clone(),
            redact: true,
        })
        .await
        .unwrap()
    else {
        panic!("a topology")
    };
    assert!(now["nodes"].get("c").is_some());

    // the assertions, each said
    let items: Vec<SessionItem> = backend
        .clone()
        .session(
            Request::RunAssertions { lab: lab.clone() },
            CancelToken::new(),
        )
        .collect()
        .await;
    assert!(
        items.iter().any(|i| matches!(
            i,
            SessionItem::Op(e) if matches!(&e.phase, OpPhase::Assertion { passed: true, .. })
        )),
        "{items:?}"
    );
    assert!(
        matches!(items.last(), Some(SessionItem::End(Ok(_)))),
        "{items:?}"
    );

    // and the lab goes
    let items: Vec<SessionItem> = backend
        .clone()
        .session(Request::Destroy { lab: lab.clone() }, CancelToken::new())
        .collect()
        .await;
    assert!(
        matches!(items.last(), Some(SessionItem::End(Ok(_)))),
        "{items:?}"
    );
    assert!(nlink_lab::RunningLab::load(&lab).is_err());
}
