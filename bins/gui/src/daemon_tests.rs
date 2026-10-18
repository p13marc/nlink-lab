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
