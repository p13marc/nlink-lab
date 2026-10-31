//! Root tests: FRR routing daemons (serial: they share /var/run/frr and the frr user).
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::RunningLab;
use nlink_lab::test_helpers::{feature, privileged};

// ─── FRR routing daemons (#65) ─────────────────────────

fn has_frr() -> bool {
    nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Zebra).is_some()
        && nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Ospfd).is_some()
        && nlink_lab::frr::frr_ids().is_some()
}

/// Poll `ip route show <dest>` on `node` until it carries `needle`.
async fn wait_route(lab: &RunningLab, node: &str, dest: &str, needle: &str, secs: u64) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        let out = lab.exec(node, "ip", &["route", "show", dest]).unwrap();
        last = out.stdout.clone();
        if last.contains(needle) {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("route {dest} on {node} never matched {needle:?}; last: {last:?}");
}

#[tokio::test]
async fn frr_ospf_converges_and_apply_restarts_daemons() {
    if !privileged("frr_ospf_converges_and_apply_restarts_daemons") {
        return;
    }
    if !feature(
        "frr_ospf_converges_and_apply_restarts_daemons",
        "frr",
        has_frr(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-ospf.nll").unwrap();
    let lab_name = format!("frr-{}", std::process::id());
    topo.lab.name = lab_name.clone();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // zebra + ospfd per router, tracked
    let routers = ["r1", "r2", "r3"];
    for r in routers {
        let n = lab.pids().iter().filter(|(node, _)| node == r).count();
        assert_eq!(n, 2, "{r} should track zebra + ospfd, got {:?}", lab.pids());
    }
    let ps = nlink_lab::frr::pathspace(&format!("{lab_name}-r1"));
    assert!(nlink_lab::frr::run_dir(&ps).join("zserv.api").exists());

    // OSPF route on r1 for the far LAN via r3, and end-to-end reachability.
    let route = wait_route(&lab, "r1", "10.3.0.0/24", "10.0.31.1", 60).await;
    assert!(
        route.contains("proto ospf") || route.contains("proto 188"),
        "expected an OSPF-installed route: {route}"
    );
    // a loopback is in the area too: only link ends were (#217)
    wait_route(&lab, "r2", "1.1.1.1", "via 10.0.12.1", 60).await;
    let mut ok = false;
    for _ in 0..10 {
        let ping = lab.exec("h1", "ping", &["-c1", "-W2", "10.3.0.2"]).unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "h1 → h2 through OSPF-learned routes failed");
    let r = lab.assertion_results();
    assert!(!r.is_empty() && r.iter().all(|a| a.passed), "{r:?}");

    // apply with `redistribute [connected]` on r1 only (timers must match
    // on both ends of a link, so they are not a safe single-node change):
    // r1's daemons restart, r2's do not, and the adjacency re-forms.
    let pid_of = |lab: &RunningLab, node: &str| -> Vec<u32> {
        let mut v: Vec<u32> = lab
            .pids()
            .iter()
            .filter(|(n, _)| n == node)
            .map(|(_, p)| *p)
            .collect();
        v.sort();
        v
    };
    let r1_before = pid_of(&lab, "r1");
    let r2_before = pid_of(&lab, "r2");
    let mut desired = lab.topology().clone();
    desired.nodes.get_mut("r1").unwrap().frr = Some(nlink_lab::types::FrrConfig {
        ospf: Some(nlink_lab::types::OspfConfig {
            redistribute: vec!["connected".to_string()],
            ..Default::default()
        }),
        bgp: None,
    });
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert_ne!(
        pid_of(&lab, "r1"),
        r1_before,
        "r1 daemons should have restarted"
    );
    assert_eq!(
        pid_of(&lab, "r2"),
        r2_before,
        "r2 daemons must be untouched"
    );
    let conf = std::fs::read_to_string(nlink_lab::frr::conf_path(
        &lab_name,
        "r1",
        nlink_lab::frr::Daemon::Ospfd,
    ))
    .unwrap();
    assert!(conf.contains("redistribute connected"), "{conf}");
    wait_route(&lab, "r1", "10.3.0.0/24", "10.0.31.1", 60).await;

    lab.destroy().await.expect("destroy failed");
    assert!(
        !nlink_lab::frr::run_dir(&ps).exists(),
        "run dir must be removed on destroy"
    );
    assert!(
        !nlink_lab::frr::lab_dir(&lab_name).exists(),
        "lab frr dir must be removed on destroy"
    );
    for pid in r1_before {
        // `pid_dead`, not a bare `/proc/<pid>` existence check: a zombie
        // keeps its /proc entry *and* its `comm`, so a daemon that has
        // exited but not been reaped looks alive. The integration lane's
        // container has no reaping PID 1, so every daemon killed here
        // lingers as `<defunct>` there and only there — which is what made
        // this lane red on master while every local run passed (#105).
        // The `comm` check stays: it tolerates PID reuse by an unrelated
        // process.
        assert!(
            pid_dead(pid) || {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map(|c| !c.contains("zebra") && !c.contains("ospfd"))
                    .unwrap_or(true)
            },
            "old daemon pid {pid} still alive"
        );
    }
}

#[tokio::test]
async fn frr_bgp_established() {
    if !privileged("frr_bgp_established") {
        return;
    }
    if !feature("frr_bgp_established", "frr", has_frr())
        || !feature(
            "frr_bgp_established",
            "bgpd",
            nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Bgpd).is_some(),
        )
    {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-bgp.nll").unwrap();
    topo.lab.name = format!("frrbgp-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let route = wait_route(&lab, "r1", "10.2.0.0/24", "192.0.2.2", 60).await;
    assert!(
        route.contains("proto bgp") || route.contains("proto 186"),
        "expected a BGP-installed route: {route}"
    );
    let mut ok = false;
    for _ in 0..10 {
        let ping = lab.exec("h1", "ping", &["-c1", "-W2", "10.2.0.2"]).unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "h1 → h2 through eBGP-learned routes failed");
    lab.destroy().await.expect("destroy failed");
}

#[tokio::test]
async fn frr_missing_daemon_is_a_clear_error_before_any_namespace() {
    if !privileged("frr_missing_daemon_is_a_clear_error_before_any_namespace") {
        return;
    }
    if has_frr() {
        eprintln!(
            "skipping frr_missing_daemon_is_a_clear_error_before_any_namespace: frr is installed"
        );
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-ospf.nll").unwrap();
    let lab_name = format!("frrmiss-{}", std::process::id());
    topo.lab.name = lab_name.clone();
    let err = topo
        .deploy()
        .await
        .err()
        .expect("deploy must fail without frr");
    let msg = err.to_string();
    assert!(
        msg.contains("zebra") || msg.contains("ospfd") || msg.contains("frr"),
        "{msg}"
    );
    assert!(msg.contains("/usr/lib/frr"), "{msg}");
    let ns = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&ns.stdout).contains(&lab_name),
        "no namespace may exist after a preflight failure"
    );
}
