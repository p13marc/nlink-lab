//! Root tests: deploy, `#[lab_test]`, captures, progress, state, watch and events.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::RunningLab;
use nlink_lab::lab_test;
use nlink_lab::test_helpers::{feature, privileged};

// ─── File-based tests ─────────────────────────────────────

#[lab_test("examples/simple.nll")]
async fn deploy_simple(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 2);
    assert_eq!(lab.topology().links.len(), 1);
}

/// #222: `timeout` wrapped the body in `tokio::time::timeout`, which never
/// fired while the body blocked its thread in `exec`: this test waited out
/// the `sleep` and passed. It now ends at the deadline.
#[lab_test("examples/simple.nll", timeout = 3)]
#[should_panic(expected = "exceeded 3s timeout")]
async fn a_lab_test_timeout_ends_a_blocking_body(lab: RunningLab) {
    let _ = lab.exec("router", "sleep", &["20"]);
}

/// #222: the macro renamed the lab only, so the namespaces of a topology
/// with `prefix` were `dc-spine1`, shared by every test of that file.
#[lab_test("examples/spine-leaf.nll")]
async fn a_lab_test_of_a_prefixed_topology_gets_its_own_namespaces(lab: RunningLab) {
    let ns = lab.namespace_for("spine1").unwrap();
    assert!(
        ns.starts_with("dc-test-a_lab_test_of_a_prefixed_topology_gets_its_own_namespaces-"),
        "{ns}"
    );
}

/// #222: every interface of a node wrote `<namespace>.pcap`, truncating
/// the other's, and one file per node was kept.
#[tokio::test]
async fn lab_capture_keeps_one_pcap_per_interface() {
    if !privileged("lab_capture_keeps_one_pcap_per_interface") {
        return;
    }
    let lab_name = format!("cap2-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router
node a {{ route default via 10.0.1.1 }}
node b {{ route default via 10.0.2.1 }}
link a:eth0 -- r:eth0 {{ 10.0.1.2/24 -- 10.0.1.1/24 }}
link r:eth1 -- b:eth0 {{ 10.0.2.1/24 -- 10.0.2.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let cap = nlink_lab::test_helpers::LabCapture::start(&lab.capture_targets()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // two seconds of pings: on a loaded host a ring can come up after a
    // shorter burst is over, and caught nothing once
    let ping = lab
        .exec("a", "ping", &["-c10", "-i0.2", "10.0.2.2"])
        .unwrap();
    assert_eq!(ping.exit_code, 0, "{}", ping.stderr);
    let dir = tempfile::tempdir().unwrap();
    let kept = cap.persist_to(dir.path()).unwrap();
    let ns = lab.namespace_for("r").unwrap().to_string();
    for iface in ["eth0", "eth1"] {
        let path = dir.path().join(format!("{ns}-{iface}.pcap"));
        assert!(kept.contains(&path), "{path:?} not in {kept:?}");
        // the 24-byte pcap header and some of the twenty ICMP packets
        // (the first can go by before the ring is up)
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len > 24 + 3 * 100, "{path:?}: {len} bytes");
    }
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// ─── Builder-based test ───────────────────────────────────

#[lab_test(topology = builder_topology)]
async fn deploy_from_builder(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 2);

    let output = lab.exec("b", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("10.0.0.2/24"));
}

fn builder_topology() -> nlink_lab::Topology {
    nlink_lab::Lab::new("builder-test")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build()
}

// ─── State persistence test ───────────────────────────────

#[lab_test("examples/simple.nll")]
async fn state_persistence(lab: RunningLab) {
    let name = lab.name().to_string();
    assert!(nlink_lab::state::exists(&name));

    // Load from state and verify
    let loaded = nlink_lab::RunningLab::load(&name).unwrap();
    assert_eq!(loaded.namespace_count(), lab.namespace_count());
}

/// A watch ends cleanly when its lab goes away (#175): each node's route
/// and nftables subscriptions end with a `gone` event and their tasks
/// return, so their sockets stop pinning the namespaces. They used to
/// stream nothing, with the namespaces kept alive behind `destroy`.
#[tokio::test]
async fn watch_reports_gone_when_the_lab_is_destroyed() {
    if !privileged("watch_reports_gone_when_the_lab_is_destroyed") {
        return;
    }
    let lab_name = format!("wgone-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &nlink_lab::WatchOpts::default())
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lab.destroy().await.expect("destroy failed");

    let mut gone = std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                if matches!(ev.kind, nlink_lab::watch::WatchEventKind::Gone) {
                    gone.insert(format!("{}/{:?}", ev.node, ev.family));
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert_eq!(
        gone.into_iter().collect::<Vec<_>>(),
        ["a/Nftables", "a/Route", "b/Nftables", "b/Route"]
    );
    for t in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(2), t)
            .await
            .expect("a subscription task outlived its namespace")
            .unwrap();
    }
}

/// #226: across `destroy` and `deploy` of the same lab between two checks,
/// the node's name opened again (on the new namespace) and the watch stayed
/// on the dead one, reporting nothing more. And `--node` of a node the lab
/// does not have was an empty watch.
#[tokio::test]
async fn watch_reports_gone_when_the_lab_is_redeployed() {
    if !privileged("watch_reports_gone_when_the_lab_is_redeployed") {
        return;
    }
    let lab_name = format!("wredo-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let unknown = nlink_lab::WatchOpts {
        node: Some("nope".into()),
        ..Default::default()
    };
    assert!(matches!(
        nlink_lab::watch_stream(&lab, &unknown),
        Err(nlink_lab::Error::NodeNotFound { .. })
    ));
    let opts = nlink_lab::WatchOpts {
        node: Some("a".into()),
        family: nlink_lab::WatchFamily::Route,
        ..Default::default()
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // well inside one 2 s check
    lab.destroy().await.expect("destroy failed");
    let lab = topo.deploy().await.expect("redeploy failed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut gone = false;
    while !gone && std::time::Instant::now() < deadline {
        if let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            gone = matches!(ev.kind, nlink_lab::watch::WatchEventKind::Gone);
        }
    }
    assert!(gone, "the watch stayed on the destroyed namespace");
    for t in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(2), t)
            .await
            .expect("a subscription task outlived its namespace")
            .unwrap();
    }
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// ─── Lifecycle event log + drift stream (#70) ───────────

#[tokio::test]
async fn lifecycle_events_recorded_and_drift_streamed() {
    if !privileged("lifecycle_events_recorded_and_drift_streamed") {
        return;
    }
    let lab_name = format!("events-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
validate {{ reach a b }}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let pid = lab
        .spawn_with_logs("a", &["sleep", "30"], None)
        .expect("spawn failed");
    lab.set_impairment(
        "a:eth0",
        &nlink_lab::Impairment {
            delay: Some("5ms".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    lab.partition("b:eth0").await.unwrap();
    lab.heal("b:eth0").await.unwrap();
    lab.clear_impairment("a:eth0").await.unwrap();
    // A frozen process is still "alive" to the kernel; `signal_process`
    // must record the signal and never escalate to KILL.
    lab.signal_process(pid, nlink_lab::Signal::Stop).unwrap();
    assert!(
        lab.process_status().iter().any(|p| p.pid == pid && p.alive),
        "a STOPped process is still tracked as alive"
    );
    lab.signal_process(pid, nlink_lab::Signal::Cont).unwrap();
    assert!(lab.kill_process(pid).unwrap(), "it was running");
    let (st, tp) = nlink_lab::state::load(&lab_name).unwrap();
    nlink_lab::state::snapshot_save(&lab_name, "s1", None, &st, &tp).unwrap();

    let names: Vec<String> = nlink_lab::events::read(&lab_name)
        .unwrap()
        .iter()
        .map(|e| e.kind.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "deployed",
            "assertions_run",
            "spawned",
            "impaired",
            "partitioned",
            "healed",
            "impair_cleared",
            "signalled",
            "signalled",
            "killed",
            "snapshot_taken",
        ],
        "{names:?}"
    );
    assert!(matches!(
        &nlink_lab::events::read(&lab_name).unwrap()[7].kind,
        nlink_lab::LifecycleKind::Signalled { pid: p, signal, .. } if *p == pid && signal == "STOP"
    ));
    let evs = nlink_lab::events::read(&lab_name).unwrap();
    assert!(matches!(
        &evs[2].kind,
        nlink_lab::LifecycleKind::Spawned { node, pid: p, cmd } if node == "a" && *p == pid && cmd == "sleep 30"
    ));

    // Drift: a hand-made address inside a node shows up on the stream.
    let opts = nlink_lab::WatchOpts::default();
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lab.exec("a", "ip", &["addr", "add", "10.9.9.9/32", "dev", "eth0"])
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let json = serde_json::to_string(&ev).unwrap();
                if ev.node == "a" && json.contains("10.9.9.9") {
                    // nlink's view of the address (#195).
                    assert_eq!(ev.kind.name(), "new_address", "{json}");
                    assert!(json.contains(r#""cidr":"10.9.9.9/32""#), "{json}");
                    seen = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    for t in tasks {
        t.abort();
    }
    assert!(seen, "expected a drift event for the new address on a:eth0");

    lab.destroy().await.expect("destroy failed");
}

/// Two handles of one lab, loaded before either saved: each one's spawned
/// process survives the other's save. `save_state` used to write its own
/// stale copy over the file, so the first spawn was lost.
#[lab_test("examples/simple.nll")]
async fn concurrent_saves_keep_each_others_changes(lab: RunningLab) {
    let mut a = RunningLab::load(lab.name()).unwrap();
    let mut b = RunningLab::load(lab.name()).unwrap();
    let pa = a.spawn("host", &["sleep", "30"]).unwrap();
    a.save_state().unwrap();
    let pb = b.spawn("router", &["sleep", "31"]).unwrap();
    b.save_state().unwrap();

    let pids = |l: &RunningLab| -> Vec<u32> { l.pids().iter().map(|(_, p)| *p).collect() };
    let after = RunningLab::load(lab.name()).unwrap();
    assert!(
        pids(&after).contains(&pa),
        "a's spawn lost: {:?}",
        after.pids()
    );
    assert!(
        pids(&after).contains(&pb),
        "b's spawn lost: {:?}",
        after.pids()
    );
    for pid in [pa, pb] {
        let _ = after.kill_process(pid);
    }
}

// ─── watch: nftables events nlink 0.30 delivers (#158) ───

/// Set elements and named objects arrive typed; the generation event nlink
/// 0.30 sends after every committed batch does not show up at all.
#[tokio::test]
async fn watch_types_set_elements_and_objects_and_drops_generations() {
    if !privileged("watch_types_set_elements_and_objects_and_drops_generations") {
        return;
    }
    if !feature(
        "watch_types_set_elements_and_objects_and_drops_generations",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("wnft-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let opts = nlink_lab::WatchOpts {
        family: nlink_lab::WatchFamily::Nftables,
        ..Default::default()
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for cmd in [
        "add table inet probe",
        "add set inet probe s { type ipv4_addr ; }",
        "add element inet probe s { 10.0.0.9 }",
        "add counter inet probe c",
    ] {
        let args: Vec<&str> = cmd.split(' ').collect();
        let out = lab.exec("a", "nft", &args).unwrap();
        assert_eq!(out.exit_code, 0, "nft {cmd}: {}", out.stderr);
    }
    let mut kinds = Vec::new();
    let mut raw = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
            Ok(Some(ev)) if ev.node == "a" => {
                kinds.push(ev.kind.name());
                raw.push(serde_json::to_value(&ev).unwrap());
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    for t in tasks {
        t.abort();
    }
    // nlink's views (#195), named `<action>_<object>`.
    for want in [
        "new_nft_table",
        "new_nft_set",
        "new_nft_set_elements",
        "new_nft_object",
    ] {
        assert!(kinds.iter().any(|k| k == want), "no {want} event: {raw:?}");
    }
    assert!(
        !kinds.iter().any(|k| k.contains("nft_gen")),
        "generation event leaked: {raw:?}"
    );
    let all = serde_json::to_string(&raw).unwrap();
    assert!(all.contains(r#""set":"s""#), "set name missing: {all}");
    assert!(all.contains(r#""object_type":"counter""#), "{all}");
    lab.destroy().await.expect("destroy failed");
}

/// #236: a deploy reports its plan, then each stage in plan order, its
/// steps, and ends with `Finished`; the destroy reports its phases.
#[tokio::test]
async fn deploy_reports_progress_in_order() {
    use nlink_lab::api::{OpEvent, OpOutcome, OpPhase};
    use std::sync::{Arc, Mutex};
    if !privileged("deploy_reports_progress_in_order") {
        return;
    }
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .unwrap();
    let name = format!("prog-{}", std::process::id());
    let src = src.replacen("lab \"simple\"", &format!("lab \"{name}\""), 1);
    let topo = nlink_lab::parser::parse(&src).unwrap();
    let _guard = LabCleanup { name: name.clone() };
    let seen = Arc::new(Mutex::new(Vec::<OpEvent>::new()));
    let sink = {
        let seen = seen.clone();
        move |e: OpEvent| seen.lock().unwrap().push(e)
    };
    let ctx = nlink_lab::OpCtx::new("deploy", "").with_sink(Arc::new(sink));
    let lab = topo.deploy_with(&ctx).await.expect("deploy failed");

    let events = std::mem::take(&mut *seen.lock().unwrap());
    assert!(events.iter().all(|e| e.lab == name && e.op == ctx.id()));
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "{seqs:?}");
    assert!(matches!(&events[0].phase, OpPhase::Started { kind, .. } if kind == "deploy"));
    let OpPhase::Planned { stages, ops, .. } = &events[1].phase else {
        panic!("no plan first: {:?}", events[1]);
    };
    let staged: Vec<(String, usize)> = events
        .iter()
        .filter_map(|e| match &e.phase {
            OpPhase::Stage { stage, ops, .. } => Some((stage.clone(), *ops)),
            _ => None,
        })
        .collect();
    assert_eq!(&staged, stages, "stages out of plan order");
    let last_step = events.iter().rev().find_map(|e| match &e.phase {
        OpPhase::Step { done, total, .. } => Some((*done, *total)),
        _ => None,
    });
    assert_eq!(last_step, Some((*ops, *ops)));
    assert!(matches!(
        &events.last().unwrap().phase,
        OpPhase::Finished {
            outcome: OpOutcome::Succeeded,
            ..
        }
    ));

    std::mem::forget(_guard);
    let ctx = nlink_lab::OpCtx::new("destroy", "").with_sink(Arc::new({
        let seen = seen.clone();
        move |e: OpEvent| seen.lock().unwrap().push(e)
    }));
    lab.destroy_with(&ctx).await.expect("destroy failed");
    let events = std::mem::take(&mut *seen.lock().unwrap());
    let steps: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|e| match &e.phase {
            OpPhase::Step { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(steps.first(), Some(&(1, 6)), "{events:?}");
    assert_eq!(steps.last(), Some(&(6, 6)), "{events:?}");
}
