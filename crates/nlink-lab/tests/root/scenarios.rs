//! Root tests: scenarios, benchmarks and the test runner.
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

// ─── Scenario example parses ────────────────────────────

#[lab_test("examples/scenario.nll")]
async fn scenario_parses_and_deploys(lab: RunningLab) {
    let names: Vec<&str> = lab
        .topology()
        .scenarios
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["failover-test", "wan-faults"]);
    assert!(lab.topology().scenarios[0].steps.len() >= 4);
}

/// #208: a scenario's `down` / `up` / `clear` on a container node (they
/// resolved namespace nodes only and failed), and `clear` is
/// `impair --clear`: the netem goes, the declared rate limit stays, and
/// the state file records the clear. It deleted the whole root qdisc and
/// told nobody. (`keep`: without it the scenario puts the netem back when
/// it ends, #326.)
#[tokio::test]
async fn a_scenario_acts_on_a_container_node_and_clears_like_impair() {
    if !privileged("a_scenario_acts_on_a_container_node_and_clears_like_impair") {
        return;
    }
    if !feature(
        "a_scenario_acts_on_a_container_node_and_clears_like_impair",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
        return;
    }
    let lab_name = format!("scn-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"]
node h
link c:eth0 -- h:eth0 {{
  10.0.0.1/24 -- 10.0.0.2/24
  delay 50ms
  rate egress 10mbit
}}
scenario "flap" {{
  keep
  at 0s {{ down c:eth0 }}
  at 1s {{ up c:eth0 }}
  at 2s {{ clear c:eth0 }}
}}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let result = nlink_lab::scenario::run_scenario(&mut lab, &topo.scenarios[0])
        .await
        .expect("scenario failed to run");
    assert!(result.passed, "{result:?}");

    let conn: nlink::Connection<nlink::Route> =
        lab.ns_resolver_of("c").unwrap().connection().unwrap();
    let link = conn.get_link_by_name("eth0").await.unwrap().unwrap();
    assert!(link.is_up(), "c:eth0 is not up again");
    let root = conn.get_root_qdisc_by_name("eth0").await.unwrap();
    let kind = root.as_ref().and_then(|q| q.kind()).map(str::to_string);
    assert!(
        kind.is_some() && kind.as_deref() != Some("netem"),
        "the rate limit should stay and the netem go: root is {kind:?}"
    );
    let reloaded = RunningLab::load(lab.name()).unwrap();
    assert_eq!(
        reloaded.live_impairments().get("c:eth0"),
        Some(&nlink_lab::types::Impairment::default()),
        "the clear is not recorded"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #208: benchmark thresholds in any unit the lexer takes (`1s` parsed
/// as nothing and never passed), and a UDP iperf3 at the rate asked: it
/// ran at iperf3's 1 Mbit/s default, whatever the link.
#[tokio::test]
async fn benchmarks_read_any_unit_and_run_udp_at_the_rate_asked() {
    let iperf3 = std::process::Command::new("iperf3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !privileged("benchmarks_read_any_unit_and_run_udp_at_the_rate_asked") {
        return;
    }
    if !feature(
        "benchmarks_read_any_unit_and_run_udp_at_the_rate_asked",
        "iperf3",
        iperf3,
    ) {
        return;
    }
    let lab_name = format!("bmk-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
benchmark "b" {{
  ping a b {{
    count 3
    assert avg below 1s
    assert loss below 1%
  }}
  iperf3 a b {{
    duration 2s
    udp
    rate 20mbit
    assert bandwidth above 10mbit
    assert loss below 5%
  }}
}}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let result = nlink_lab::benchmark::run_benchmark(&lab, &topo.benchmarks[0]).unwrap();
    assert!(result.passed, "{result:#?}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// A scenario changes the lab at runtime and puts it back (#326): it
/// impairs an endpoint over its declared impairment, sets a pair rule,
/// partitions both ends of a link (a `no-reach` passes then, and the
/// report's timeline shows the partition before it), heals, takes an
/// interface down; when it ends, the declared impairment, the pair
/// rules, the partitions and the interface are as they were, and the lab
/// has no drift. A `keep` scenario leaves its change.
#[tokio::test]
async fn a_scenario_impairs_partitions_and_puts_the_lab_back() {
    if !privileged("a_scenario_impairs_partitions_and_puts_the_lab_back") {
        return;
    }
    let lab_name = format!("scf-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
node c
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
network lan {{
  members [a:eth1, c:eth0]
  subnet 10.1.0.0/24
}}
impair a:eth0 delay 5ms
scenario "faults" {{
  at 0s {{
    impair a:eth0 delay 30ms
    impair lan a -- c {{ delay 100ms }}
  }}
  at 1s {{
    partition a:eth0 both
    validate {{ no-reach a b }}
  }}
  # the no-reach ping waits out its 2 s; the gaps after it and after the
  # heal let a's ARP for b, whose probes the partition swallowed, give up
  # and start over, as an application's retries would
  at 4s {{ heal a:eth0 both }}
  at 6s {{
    validate {{ reach a b }}
    down b:eth0
  }}
}}
scenario "kept" {{
  keep
  at 0s {{ impair a:eth0 delay 77ms }}
}}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let netem = |lab: &RunningLab| root_qdisc(lab, "a", "eth0").0;
    assert!(netem(&lab).contains("delay 5ms"), "{}", netem(&lab));

    let faults = topo.scenarios.iter().find(|s| s.name == "faults").unwrap();
    let r = nlink_lab::scenario::run_scenario(&mut lab, faults)
        .await
        .expect("scenario failed to run");
    assert!(r.passed, "{r:#?}");
    let at_1s: Vec<&str> = r.steps[1]
        .actions
        .iter()
        .map(|a| a.description.as_str())
        .collect();
    assert_eq!(at_1s, ["partition a:eth0 both", "validate"], "{r:#?}");
    let cleanup: Vec<&str> = r.cleanup.iter().map(|a| a.description.as_str()).collect();
    assert!(
        cleanup.contains(&"cleanup: up b:eth0")
            && cleanup.iter().any(|c| c.contains("pair impairment"))
            && cleanup
                .iter()
                .any(|c| c.contains("impairments and partitions")),
        "{cleanup:?}"
    );

    // as it was before the scenario
    assert!(netem(&lab).contains("delay 5ms"), "{}", netem(&lab));
    assert!(!lab.is_partitioned("a:eth0") && !lab.is_partitioned("b:eth0"));
    assert!(
        lab.live_impairments().is_empty(),
        "{:?}",
        lab.live_impairments()
    );
    assert!(lab.topology().networks["lan"].impairments.is_empty());
    let link = lab
        .exec("b", "ip", &["-o", "link", "show", "eth0"])
        .unwrap()
        .stdout;
    assert!(link.contains("state UP") || link.contains(",UP"), "{link}");
    let ping = lab
        .exec("a", "ping", &["-c", "1", "-W", "2", "10.0.0.2"])
        .unwrap();
    assert_eq!(ping.exit_code, 0, "{}", ping.stdout);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "the scenario left drift:\n{layered}");

    let kept = topo.scenarios.iter().find(|s| s.name == "kept").unwrap();
    let r = nlink_lab::scenario::run_scenario(&mut lab, kept)
        .await
        .unwrap();
    assert!(r.passed && r.cleanup.is_empty(), "{r:#?}");
    assert!(netem(&lab).contains("delay 77ms"), "{}", netem(&lab));
    assert!(lab.live_impairments().contains_key("a:eth0"));
    lab.destroy().await.expect("destroy failed");
}

/// C7: `test --scenarios` runs every scenario after the assertions, each
/// action one more result; `keep_failed` leaves a failed lab deployed.
#[tokio::test]
async fn a_test_runs_scenarios_and_keeps_a_failed_lab() {
    if !privileged("a_test_runs_scenarios_and_keeps_a_failed_lab") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let name = format!("tsc-{}", std::process::id());
    let file = dir.path().join("t.nll");
    std::fs::write(
        &file,
        format!(
            "lab \"{name}\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}\n\
             scenario \"flap\" {{\n  at 0s {{ down a:eth0 }}\n  at 100ms {{ up a:eth0 }}\n  at 200ms {{ exec a \"false\" }}\n}}\n"
        ),
    )
    .unwrap();
    let _guard = LabCleanup { name: name.clone() };
    let opts = nlink_lab::test_runner::TestOptions {
        scenarios: true,
        benchmarks: false,
        keep_failed: true,
        seed: None,
        bundle_dir: None,
    };
    let r = nlink_lab::test_runner::run_test_opts(&file, &[], opts, &nlink_lab::OpCtx::default())
        .await
        .expect("test ran");
    let rows: Vec<(&str, bool)> = r
        .assertions
        .iter()
        .map(|a| (a.description.as_str(), a.passed))
        .collect();
    assert!(
        rows.iter()
            .any(|(d, ok)| d.starts_with("scenario flap: down a:eth0") && *ok),
        "{rows:?}"
    );
    // `exec a false` fails: the test fails, and the lab is kept
    assert!(!r.passed, "{rows:?}");
    assert_eq!(r.kept.as_deref(), Some(name.as_str()));
    assert!(nlink_lab::state::exists(&name));
    std::mem::forget(_guard);
    nlink_lab::RunningLab::load(&name)
        .unwrap()
        .destroy()
        .await
        .unwrap();
}

/// The netem delays (ms) on `iface` of `node`: its root's and every
/// per-pair leaf's, read over netlink.
async fn netem_delays(lab: &RunningLab, node: &str, iface: &str) -> Vec<f64> {
    use nlink::netlink::tc_options::QdiscOptions;
    let conn: nlink::Connection<nlink::Route> =
        lab.ns_resolver_of(node).unwrap().connection().unwrap();
    let mut out: Vec<f64> = conn
        .get_qdiscs_by_name(iface)
        .await
        .unwrap()
        .iter()
        .filter_map(|q| match q.options() {
            Some(QdiscOptions::Netem(n)) => n.delay().map(|d| d.as_secs_f64() * 1000.0),
            _ => None,
        })
        .collect();
    out.sort_by(f64::total_cmp);
    out
}

/// A replayed trace reaches the kernel in time (#340): a scenario plays a
/// trace that goes from 10 ms to 50 ms in 2 s, on an endpoint and on a
/// per-pair rule. Sampled mid-way, both netems are where the trace is,
/// within a step or so; after the scenario both are back at 1 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_trace_reaches_the_kernel_in_time() {
    use std::time::{Duration, Instant};
    if !privileged("a_replayed_trace_reaches_the_kernel_in_time") {
        return;
    }
    let lab_name = format!("rpl-{}", std::process::id());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ramp.csv"),
        "t_ms,delay_ms\n0,10\n2000,50\n",
    )
    .unwrap();
    let file = dir.path().join("lab.nll");
    std::fs::write(
        &file,
        format!(
            r#"lab "{lab_name}"
node a
node b
node c
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24  delay 1ms }}
network net {{
  members [a:eth1, b:eth1, c:eth0]
  subnet 10.0.5.0/24
  impair a -- b {{ delay 1ms }}
}}
scenario "ramp" {{
  at 0s {{
    replay a:eth0 "ramp.csv" {{ step 100ms }}
    replay net a -- b "ramp.csv" {{ step 100ms }}
  }}
  at 3s {{ log "done" }}
}}
"#
        ),
    )
    .unwrap();
    let topo = nlink_lab::parser::parse_file(&file).unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let scenario = lab.topology().scenarios[0].clone();
    let mut runner = lab.clone();
    let started = Instant::now();
    let run = tokio::spawn(async move {
        nlink_lab::scenario::run_scenario_with(&mut runner, &scenario, &nlink_lab::OpCtx::default())
            .await
    });
    for at in [Duration::from_millis(700), Duration::from_millis(1400)] {
        tokio::time::sleep_until((started + at).into()).await;
        let want = 10.0 + 20.0 * started.elapsed().as_secs_f64();
        let root = netem_delays(&lab, "a", "eth0").await;
        let leaves = netem_delays(&lab, "a", "eth1").await;
        // a step is 2 ms of this ramp; allow for the scheduling around it
        let near = |v: f64| (v - want).abs() < 8.0;
        assert!(
            root.iter().any(|v| near(*v)),
            "endpoint {root:?}, trace at {want:.1}"
        );
        assert!(
            leaves.iter().any(|v| near(*v)),
            "pair {leaves:?}, trace at {want:.1}"
        );
    }
    let result = run.await.unwrap().expect("the scenario ran");
    assert!(result.passed, "{result:?}");
    assert!(
        result
            .cleanup
            .iter()
            .any(|a| a.description.contains("replay") && a.description.contains("put back")),
        "{:?}",
        result.cleanup
    );
    assert_eq!(
        netem_delays(&lab, "a", "eth0").await,
        [1.0],
        "the endpoint is back"
    );
    assert!(
        netem_delays(&lab, "a", "eth1").await.contains(&1.0),
        "the pair rule is back"
    );
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}
