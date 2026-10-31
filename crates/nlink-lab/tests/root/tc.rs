//! Root tests: impairments, partitions, rate limits and qdiscs.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::lab_test;
use nlink_lab::test_helpers::{feature, privileged};
use nlink_lab::{Lab, RunningLab};

#[lab_test("examples/simple.nll")]
async fn netem_applied(lab: RunningLab) {
    let output = lab
        .exec("router", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("netem"),
        "expected netem qdisc in output: {}",
        output.stdout
    );
}

// ─── Asymmetric impairment ──────────────────────────────

#[lab_test(topology = asymmetric_topology)]
async fn asymmetric_netem(lab: RunningLab) {
    let output = lab
        .exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("netem"),
        "expected netem on a:eth0: {}",
        output.stdout
    );

    let output = lab
        .exec("b", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("netem"),
        "expected netem on b:eth0: {}",
        output.stdout
    );
}

fn asymmetric_topology() -> nlink_lab::Topology {
    Lab::new("asymmetric-test")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .impair("a:eth0", |i| i.delay("10ms"))
        .impair("b:eth0", |i| i.delay("50ms"))
        .build()
}

// ─── Runtime impairment modification ────────────────────

#[lab_test(topology = runtime_impair_topology)]
async fn runtime_set_impairment(lab: RunningLab) {
    // Set impairment at runtime
    lab.set_impairment(
        "a:eth0",
        &nlink_lab::Impairment {
            delay: Some("20ms".into()),
            ..Default::default()
        },
    )
    .await
    .expect("set_impairment failed");

    // Verify it's applied
    let output = lab
        .exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("netem"),
        "expected netem after set_impairment: {}",
        output.stdout
    );
}

fn runtime_impair_topology() -> nlink_lab::Topology {
    Lab::new("runtime-impair-test")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build()
}

// Round-4 §1: `partition` was a silent no-op on the second call after
// `clear`, because the stale `saved_impairments` entry survived
// `clear_impairment`. This test cycles partition+clear five times
// and asserts each `--partition` actually installs its drop filters.
// Before the fix this fails on cycle 2.
#[lab_test("examples/simple.nll")]
async fn partition_clear_cycles_are_idempotent(mut lab: RunningLab) {
    let endpoint = "host:eth0";
    for cycle in 1..=5 {
        lab.partition(endpoint).await.unwrap_or_else(|e| {
            panic!("cycle {cycle}: partition failed: {e}");
        });

        // The partition must actually be live — assert via `tc`.
        assert!(
            blocked(&lab, "host", "eth0"),
            "cycle {cycle}: expected the drop filters after partition"
        );

        lab.clear_impairment(endpoint).await.unwrap_or_else(|e| {
            panic!("cycle {cycle}: clear_impairment failed: {e}");
        });
    }
}

// `clear_impairment` is now idempotent: calling it on an interface
// with no qdisc must succeed (kernel returns ENOENT, we treat it as
// "already cleared"). Before the fix this failed with "qdisc not
// found: root on ifindex 3".
#[lab_test("examples/simple.nll")]
async fn clear_impairment_idempotent_on_fresh_deploy(mut lab: RunningLab) {
    // Topology has no impairments declared. The very first clear must
    // succeed — there's nothing to clear.
    lab.clear_impairment("host:eth0").await.unwrap();
    // Second call must also succeed.
    lab.clear_impairment("host:eth0").await.unwrap();
}

// ─── #153 — netem loss models end to end ────────────────

// A loss model set through `set_impairment` (the path `impair --loss` and
// `edit --set-impair` take) reaches the kernel as the model, `--show`'s
// parser reports it, and it drops what it describes. Deterministic models,
// so the outcome is exact: Gilbert-Elliott that never enters the bad state
// and loses everything in the good one drops every ping; the same with
// `1-k` 0 drops none.
#[lab_test("examples/simple.nll")]
async fn loss_model_reaches_the_kernel_and_drops(lab: RunningLab) {
    let lose_all = nlink_lab::Impairment {
        loss: Some("gemodel 0% 100% 100% 100%".into()),
        ..Default::default()
    };
    lab.set_impairment("router:eth0", &lose_all).await.unwrap();
    let qd = lab
        .exec("router", "tc", &["-s", "qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(qd.stdout.contains("loss gemodel"), "tc said: {}", qd.stdout);
    let parsed = nlink_lab::impair_parse::parse_tc_qdisc_show(&qd.stdout).unwrap();
    assert!(
        parsed
            .loss_model
            .as_deref()
            .is_some_and(|m| m.starts_with("gemodel")),
        "{parsed:?}"
    );
    // `impair --show` reads the same over netlink, without `tc` (#179).
    let read = lab.impair_show_blocking("router:eth0").unwrap();
    assert_eq!(read.as_ref(), Some(&parsed), "tc said: {}", qd.stdout);
    let ping = lab
        .exec(
            "router",
            "ping",
            &["-c", "3", "-i", "0.2", "-W", "1", "10.0.0.2"],
        )
        .unwrap();
    assert_ne!(
        ping.exit_code, 0,
        "every packet must be lost: {}",
        ping.stdout
    );
    let stats = lab
        .exec("router", "tc", &["-s", "qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        !stats.stdout.contains("dropped 0,"),
        "netem must count drops: {}",
        stats.stdout
    );

    let lose_none = nlink_lab::Impairment {
        loss: Some("gemodel 0%".into()),
        ..Default::default()
    };
    lab.set_impairment("router:eth0", &lose_none).await.unwrap();
    let ping = lab
        .exec(
            "router",
            "ping",
            &["-c", "3", "-i", "0.2", "-W", "1", "10.0.0.2"],
        )
        .unwrap();
    assert_eq!(
        ping.exit_code, 0,
        "a model that never loses must not drop: {}",
        ping.stdout
    );

    // The plain values too: netlink's view equals tc's.
    let plain = nlink_lab::Impairment {
        delay: Some("10ms".into()),
        jitter: Some("2ms".into()),
        loss: Some("1.5%".into()),
        rate: Some("10mbit".into()),
        ..Default::default()
    };
    lab.set_impairment("router:eth0", &plain).await.unwrap();
    let qd = lab
        .exec("router", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    let parsed = nlink_lab::impair_parse::parse_tc_qdisc_show(&qd.stdout);
    let read = lab.impair_show_blocking("router:eth0").unwrap();
    assert_eq!(read, parsed, "tc said: {}", qd.stdout);
    assert_eq!(read.and_then(|r| r.rate_bps), Some(10_000_000));
    lab.clear_impairment("router:eth0").await.unwrap();
    assert_eq!(lab.impair_show_blocking("router:eth0").unwrap(), None);
}

// `is_partitioned` should reflect partition/heal lifecycle, not raw
// `--loss 100%` installs. Before partition: false. After partition:
// true. After clear_impairment (which prunes saved_impairments): false
// again. This is the contract `impair --show --json` exposes via the
// `partition` field per-endpoint.
#[lab_test("examples/simple.nll")]
async fn is_partitioned_tracks_partition_clear_lifecycle(mut lab: RunningLab) {
    let endpoint = "host:eth0";
    assert!(!lab.is_partitioned(endpoint), "fresh: not partitioned");

    lab.partition(endpoint).await.unwrap();
    assert!(lab.is_partitioned(endpoint), "after partition: partitioned");

    lab.clear_impairment(endpoint).await.unwrap();
    assert!(
        !lab.is_partitioned(endpoint),
        "after clear: flag must be reset (this is the round-4 §1 fix)"
    );
}

// ─── Rate limit `burst` reaches tc (#71) ────────────────

#[tokio::test]
async fn rate_limit_burst_applied() {
    if !privileged("rate_limit_burst_applied") {
        return;
    }
    let lab_name = format!("burst-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
rate a:eth0 egress 10mbit burst 64kbyte
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // nlink's RateLimiter builds an HTB root with a shaping class; the
    // burst is a class attribute: `class htb 1:a root rate 10Mbit ceil
    // 10Mbit burst 64Kb cburst …`.
    let out = lab
        .exec("a", "tc", &["class", "show", "dev", "eth0"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    let lower = out.stdout.to_ascii_lowercase();
    assert!(
        lower.contains("burst 64kb") || lower.contains("burst 65536b"),
        "expected the configured burst in `tc class show`: {}",
        out.stdout
    );
    lab.destroy().await.expect("destroy failed");
}

/// `rate … ingress` passes traffic (#149), and removing it removes all of
/// it: the root shaper, the `ffff:` ingress qdisc with its redirect
/// filter, and the `ifb_<iface>` device. Clearing only the root qdisc left
/// the other two running (#158).
#[tokio::test]
async fn rate_limit_removal_takes_ingress_and_ifb() {
    if !privileged("rate_limit_removal_takes_ingress_and_ifb") {
        return;
    }
    let lab_name = format!("rmrate-{}", std::process::id());
    let src = |rate: &str| {
        format!(
            r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
{rate}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("rate a:eth0 egress 10mbit ingress 10mbit")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let qdiscs = |lab: &RunningLab| {
        lab.exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
            .unwrap()
            .stdout
    };
    let has_ifb = |lab: &RunningLab| {
        lab.exec("a", "ip", &["link", "show", "ifb_eth0"])
            .unwrap()
            .exit_code
            == 0
    };
    let out = qdiscs(&lab);
    assert!(out.contains("qdisc htb"), "{out}");
    assert!(out.contains("qdisc ingress"), "{out}");
    assert!(has_ifb(&lab), "ingress shaping should create ifb_eth0");
    // Ingress shaping passes traffic (#149): nlink before 0.31 redirected
    // to the IFB with mirred *ingress*, and every packet a:eth0 received
    // was lost. The redirect is a mirred egress redirect now.
    let filters = lab
        .exec("a", "tc", &["filter", "show", "dev", "eth0", "ingress"])
        .unwrap()
        .stdout;
    assert!(filters.contains("Egress Redirect"), "{filters}");
    let ping = lab
        .exec(
            "b",
            "ping",
            &["-c", "3", "-i", "0.2", "-W", "1", "10.0.0.1"],
        )
        .unwrap();
    assert_eq!(
        ping.exit_code, 0,
        "no traffic through rate … ingress: {}",
        ping.stdout
    );

    let desired = nlink_lab::parser::parse(&src("")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (no rate) failed");
    let out = qdiscs(&lab);
    assert!(!out.contains("qdisc htb"), "root shaper left behind: {out}");
    assert!(
        !out.contains("qdisc ingress"),
        "ingress qdisc left behind: {out}"
    );
    assert!(!has_ifb(&lab), "ifb_eth0 left behind");
    lab.destroy().await.expect("destroy failed");
}

// ─── `qdisc` blocks reach tc and follow apply (#67) ─────

#[tokio::test]
async fn qdisc_blocks_apply_replace_and_clear() {
    if !privileged("qdisc_blocks_apply_replace_and_clear") {
        return;
    }
    let lab_name = format!("qdisc-{}", std::process::id());
    let src = |qdisc: &str| {
        format!(
            r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
{qdisc}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 tbf { rate 10mbit burst 32kb limit 100kb }",
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let show = |lab: &RunningLab| {
        lab.exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
            .unwrap()
            .stdout
    };
    let out = show(&lab);
    assert!(out.contains("qdisc tbf"), "tbf expected: {out}");
    assert!(out.to_ascii_lowercase().contains("rate 10mbit"), "{out}");
    // The TBF must pass traffic, not just exist: one without a queue limit
    // installed fine on nlink 0.29 and dropped every packet (nlink #398).
    let ping = lab
        .exec(
            "a",
            "ping",
            &["-c", "3", "-i", "0.2", "-W", "2", "10.0.0.2"],
        )
        .unwrap();
    assert_eq!(
        ping.exit_code, 0,
        "ping through the tbf failed: {}{}",
        ping.stdout, ping.stderr
    );

    // Change the kind: apply must replace the root qdisc.
    let desired = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 fq_codel { target 5ms interval 100ms ecn }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (fq_codel) failed");
    let out = show(&lab);
    assert!(out.contains("qdisc fq_codel"), "fq_codel expected: {out}");
    assert!(!out.contains("qdisc tbf"), "tbf must be gone: {out}");
    assert!(out.contains("ecn"), "{out}");

    // Remove the block: apply must clear the root qdisc.
    let desired = nlink_lab::parser::parse(&src("")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (clear) failed");
    let out = show(&lab);
    assert!(
        !out.contains("qdisc fq_codel") && !out.contains("qdisc tbf"),
        "root qdisc should be cleared: {out}"
    );

    // sfq and prio parameters reach the kernel too.
    let desired = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 sfq { perturb 10s }\nqdisc b:eth0 prio { bands 4 }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (sfq/prio) failed");
    let out = show(&lab);
    assert!(
        out.contains("qdisc sfq") && out.contains("perturb 10sec"),
        "{out}"
    );
    let out_b = lab
        .exec("b", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap()
        .stdout;
    assert!(
        out_b.contains("qdisc prio") && out_b.contains("bands 4"),
        "{out_b}"
    );

    // Changing an sfq *parameter* while sfq is live. `sch_sfq` sets
    // `.change = NULL`, so the in-place replace #108 introduced gets
    // EINVAL ("Change operation not supported by specified qdisc") —
    // the one kind here that has to be deleted and re-added. Before the
    // fix this failed the apply outright; before #108 the plan's
    // unconditional teardown happened to cover it.
    let desired = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 sfq { perturb 20s }\nqdisc b:eth0 prio { bands 4 }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (sfq perturb change) failed");
    let out = show(&lab);
    assert!(
        out.contains("qdisc sfq") && out.contains("perturb 20sec"),
        "sfq must still be installed, with the new period: {out}"
    );

    // An fq_codel that sets `flows`, edited on a live lab (#171): the
    // kernel refuses any `flows` on a live fq_codel, and nlink before 0.31
    // always sent it, so every apply after the first was EINVAL.
    let desired = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 fq_codel { flows 512 limit 1000 }\nqdisc b:eth0 prio { bands 2 }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (fq_codel flows, prio bands 2) failed");
    let desired = nlink_lab::parser::parse(&src(
        "qdisc a:eth0 fq_codel { flows 512 limit 2000 }\nqdisc b:eth0 prio { bands 2 }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (fq_codel limit edit) failed");
    let out = show(&lab);
    assert!(
        out.contains("qdisc fq_codel") && out.contains("limit 2000p") && out.contains("flows 512"),
        "{out}"
    );
    // `prio { bands 2 }` installs (#172): nlink fits the default 3-band
    // priomap to it; it used to be EINVAL.
    let out_b = lab
        .exec("b", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap()
        .stdout;
    assert!(
        out_b.contains("qdisc prio") && out_b.contains("bands 2"),
        "{out_b}"
    );

    lab.destroy().await.expect("destroy failed");
}

/// Traffic control is declared (#108): a re-apply changes nothing (same
/// handle, counters kept, empty diff), a qdisc deleted by hand is drift
/// that `--check` reports, and `apply` puts it back.
#[tokio::test]
async fn declared_tc_reapplies_as_nothing_and_repairs_drift() {
    if !privileged("declared_tc_reapplies_as_nothing_and_repairs_drift") {
        return;
    }
    let lab_name = format!("dtc-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
impair a:eth0 delay 10ms
qdisc b:eth0 prio {{ bands 3 }}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(avg_rtt_ms(&lab, "b", "10.0.0.1") >= 9.0);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(
        layered.is_empty(),
        "fresh deploy should be converged:\n{layered}"
    );
    let (netem, sent) = root_qdisc(&lab, "a", "eth0");
    assert!(netem.contains("netem") && sent > 0, "{netem} sent {sent}");

    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("re-apply failed");
    let (netem_after, sent_after) = root_qdisc(&lab, "a", "eth0");
    assert_eq!(netem, netem_after, "the netem is the same qdisc");
    assert!(sent_after >= sent, "counters kept: {sent} -> {sent_after}");

    // Deleted by hand: drift `--check` reports, `apply` repairs.
    let del = lab
        .exec("a", "tc", &["qdisc", "del", "dev", "eth0", "root"])
        .unwrap();
    assert_eq!(del.exit_code, 0, "{}", del.stderr);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    let a = layered.network.get("a").expect("a's network layer drifted");
    // The kernel put its default root back: the netem replaces it.
    assert_eq!(
        a.qdiscs_to_add.len() + a.qdiscs_to_replace.len(),
        1,
        "{layered}"
    );
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply failed");
    assert!(root_qdisc(&lab, "a", "eth0").0.contains("netem"));
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// A partition drops both directions and leaves the root alone (#168):
/// the `prio` is still there while partitioned, an `apply` meanwhile keeps
/// the partition, and heal finds the prio as it was.
#[tokio::test]
async fn partition_and_heal_keep_a_prio() {
    if !privileged("partition_and_heal_keep_a_prio") {
        return;
    }
    let lab_name = format!("pprio-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
qdisc a:eth0 prio {{ bands 3 }}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ping = |lab: &RunningLab, from: &str, to: &str| {
        lab.exec(from, "ping", &["-c", "1", "-W", "1", to])
            .unwrap()
            .exit_code
            == 0
    };
    assert!(ping(&lab, "a", "10.0.0.2"));

    lab.partition("a:eth0").await.expect("partition failed");
    assert!(!ping(&lab, "a", "10.0.0.2"), "a still sends");
    assert!(!ping(&lab, "b", "10.0.0.1"), "a still receives");
    assert!(root_qdisc(&lab, "a", "eth0").0.contains("prio 1: root"));

    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply failed");
    assert!(lab.is_partitioned("a:eth0"));
    assert!(blocked(&lab, "a", "eth0"), "the apply kept the partition");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "a partition is not drift:\n{layered}");

    lab.heal("a:eth0").await.expect("heal failed");
    assert!(ping(&lab, "a", "10.0.0.2") && ping(&lab, "b", "10.0.0.1"));
    assert!(
        root_qdisc(&lab, "a", "eth0").0.contains("prio 1: root"),
        "heal must find the prio"
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// A partition is a black hole (#347): the partitioned node's own stack
/// cannot tell it from a dead network. A UDP send with `IP_RECVERR`
/// succeeds (a drop on egress would fail it with ENOBUFS), and a TCP
/// stream across the cut stays established and backs off (a drop on
/// egress made its TCP retry every 500 ms without backing off and give up
/// with ETIMEDOUT about 8 s after the cut). After the heal the stream
/// carries data again.
#[tokio::test]
async fn a_partition_is_a_black_hole_to_the_sender() {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    if !privileged("a_partition_is_a_black_hole_to_the_sender") {
        return;
    }
    let lab_name = format!("pbh-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ns = |node: &str| lab.namespace_for(node).unwrap().to_string();
    // A socket stays in the namespace it was created in.
    let in_ns = |node: &str| nlink::netlink::namespace::enter(&ns(node)).unwrap();

    let listener = {
        let _in = in_ns("b");
        std::net::TcpListener::bind("10.0.0.2:9000").unwrap()
    };
    let received = Arc::new(AtomicU64::new(0));
    let reader = {
        let received = received.clone();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            while let Ok(n @ 1..) = conn.read(&mut buf) {
                received.fetch_add(n as u64, Ordering::Relaxed);
            }
        })
    };
    let (mut stream, udp) = {
        let _in = in_ns("a");
        let stream = std::net::TcpStream::connect_timeout(
            &"10.0.0.2:9000".parse().unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        (stream, std::net::UdpSocket::bind("10.0.0.1:0").unwrap())
    };
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let on: libc::c_int = 1;
    // SAFETY: setsockopt on a socket this test owns, with an int option.
    let rc = unsafe {
        libc::setsockopt(
            udp.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_RECVERR,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "IP_RECVERR: {}", std::io::Error::last_os_error());
    let chunk = [b'x'; 4096];
    // Only a full send buffer (EAGAIN after the write timeout) is fine.
    let send = |stream: &mut std::net::TcpStream, since: Instant| match stream.write(&chunk) {
        Ok(_) => {}
        Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {}
        Err(e) => panic!(
            "the sender saw the partition {:.1?} after the cut: {e}",
            since.elapsed()
        ),
    };
    let start = Instant::now();
    while received.load(Ordering::Relaxed) == 0 {
        send(&mut stream, start);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no data before the cut"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    lab.partition("a:eth0").await.expect("partition a failed");
    lab.partition("b:eth0").await.expect("partition b failed");
    let cut = Instant::now();
    assert!(blocked(&lab, "a", "eth0") && blocked(&lab, "b", "eth0"));
    // The neighbour entry for 10.0.0.2 is warm from the TCP stream, so the
    // datagram reaches the egress hook at once: that is where a drop
    // reports ENOBUFS (an unresolved neighbour would queue it and hide
    // the difference).
    udp.send_to(b"ping", "10.0.0.2:9001")
        .expect("a UDP send through a partition must look sent");

    while cut.elapsed() < Duration::from_secs(10) {
        send(&mut stream, cut);
        std::thread::sleep(Duration::from_millis(20));
    }
    // The first bytes of `struct tcp_info`: state, ca_state, retransmits,
    // probes, backoff.
    let mut info = [0u8; 8];
    let mut len = info.len() as libc::socklen_t;
    // SAFETY: getsockopt into a buffer of `len` bytes; the kernel copies
    // at most that many.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            info.as_mut_ptr().cast(),
            &mut len,
        )
    };
    assert_eq!(rc, 0, "TCP_INFO: {}", std::io::Error::last_os_error());
    assert_eq!(
        info[0], 1,
        "not ESTABLISHED after 10 s of partition: {info:?}"
    );
    assert!(info[4] > 0, "the sender never backed off: {info:?}");

    let before = received.load(Ordering::Relaxed);
    lab.heal("a:eth0").await.expect("heal a failed");
    lab.heal("b:eth0").await.expect("heal b failed");
    let healed = Instant::now();
    while received.load(Ordering::Relaxed) == before {
        send(&mut stream, cut);
        assert!(
            healed.elapsed() < Duration::from_secs(30),
            "no data 30 s after the heal"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!(
        "after 10 s of partition: ESTABLISHED, backoff {}, retransmits {}; data again {:.1?} after the heal",
        info[4],
        info[2],
        healed.elapsed()
    );
    drop(stream);
    reader.join().unwrap();
    lab.destroy().await.expect("destroy failed");
}

/// `fq_codel { noecn }` turns ECN off; without either flag it is the
/// kernel's default, which is on (#173).
#[tokio::test]
async fn fq_codel_noecn_turns_ecn_off() {
    if !privileged("fq_codel_noecn_turns_ecn_off") {
        return;
    }
    let lab_name = format!("noecn-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
qdisc a:eth0 fq_codel {{ noecn }}
qdisc b:eth0 fq_codel {{ limit 1000 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let has_ecn = |n: &str| {
        let (root, _) = root_qdisc(&lab, n, "eth0");
        assert!(root.contains("fq_codel"), "{root}");
        root.split_whitespace().any(|w| w == "ecn")
    };
    assert!(!has_ecn("a"), "noecn");
    assert!(has_ecn("b"), "the kernel's default is on");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// Changing one netem property used to go netem -> `noqueue` -> a new
/// netem with a new handle and a new seed: a moment with no impairment
/// at all, and the accumulated byte counters gone with it. A consumer
/// measuring a capped link lost an interval per change and, worse, the
/// link was briefly uncapped during a phase whose premise was that it
/// was not.
///
/// This asserts the kernel-visible consequences: same handle, and a
/// `Sent` counter that never steps backwards.
#[tokio::test]
async fn changing_an_impairment_keeps_the_handle_and_the_counters() {
    if !privileged("changing_an_impairment_keeps_the_handle_and_the_counters") {
        return;
    }
    let lab_name = format!("qreplace-{}", std::process::id());
    let src = |delay: &str| {
        format!(
            r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24  delay {delay} rate 5mbit }}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("10ms")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // `tc -s qdisc show` prints `qdisc netem <handle>: root ...` and a
    // `Sent <n> bytes` line; both are what a consumer keys on.
    let probe = |lab: &RunningLab| -> (String, u64) {
        let out = lab
            .exec("a", "tc", &["-s", "qdisc", "show", "dev", "eth0"])
            .unwrap()
            .stdout;
        let handle = out
            .split_whitespace()
            .skip_while(|w| *w != "netem")
            .nth(1)
            .unwrap_or("?")
            .to_string();
        let sent = out
            .split_whitespace()
            .skip_while(|w| *w != "Sent")
            .nth(1)
            .and_then(|w| w.parse().ok())
            .unwrap_or(0);
        (handle, sent)
    };

    // Move some bytes so the counter is non-zero and a reset is visible.
    let _ = lab.exec("a", "ping", &["-c", "5", "-s", "1000", "10.0.0.2"]);
    let (handle_before, sent_before) = probe(&lab);
    assert!(
        sent_before > 0,
        "expected the netem counter to have moved: {sent_before}"
    );

    let desired = nlink_lab::parser::parse(&src("40ms")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (delay 40ms) failed");

    let (handle_after, sent_after) = probe(&lab);
    assert_eq!(
        handle_before, handle_after,
        "the qdisc was replaced, not changed in place: {handle_before} -> {handle_after}"
    );
    assert!(
        sent_after >= sent_before,
        "counters were reset by the change: {sent_before} -> {sent_after}"
    );
    // And the new value really did reach the kernel.
    let out = lab
        .exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap()
        .stdout;
    assert!(out.contains("40ms"), "delay 40ms expected: {out}");

    lab.destroy().await.expect("destroy failed");
}

// ─── Runtime impairments persist; snapshot / restore (#59) ───

#[tokio::test]
async fn impair_persists_across_apply_and_snapshot_restore() {
    if !privileged("impair_persists_across_apply_and_snapshot_restore") {
        return;
    }
    let lab_name = format!("snap-{}", std::process::id());
    let src = |extra: &str| {
        format!(
            r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }}
{extra}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let qdisc = |lab: &RunningLab, node: &str| {
        lab.exec(node, "tc", &["qdisc", "show", "dev", "eth0"])
            .unwrap()
            .stdout
    };

    // Baseline snapshot: declared 1ms delay on a:eth0, nothing else.
    let (st, tp) = nlink_lab::state::load(&lab_name).unwrap();
    nlink_lab::state::snapshot_save(&lab_name, "base", Some("baseline"), &st, &tp).unwrap();

    // Runtime impairment: recorded in state, overrides the declaration.
    lab.set_impairment(
        "a:eth0",
        &nlink_lab::Impairment {
            delay: Some("50ms".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(qdisc(&lab, "a").contains("50"), "{}", qdisc(&lab, "a"));
    let reloaded = RunningLab::load(&lab_name).unwrap();
    assert_eq!(
        reloaded.live_impairments()["a:eth0"].delay.as_deref(),
        Some("50ms")
    );

    // apply with an unrelated change keeps the live impairment …
    let desired = nlink_lab::parser::parse(&src(
        "node c\nlink b:eth1 -- c:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }",
    ))
    .unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert!(
        qdisc(&lab, "a").contains("50"),
        "live impairment must survive apply: {}",
        qdisc(&lab, "a")
    );
    assert!(lab.live_impairments().contains_key("a:eth0"));

    // … a partition on top drops everything and leaves the netem be …
    lab.partition("a:eth0").await.unwrap();
    assert!(blocked(&lab, "a", "eth0"));
    assert!(
        qdisc(&lab, "a").contains("50"),
        "the netem stays under the partition: {}",
        qdisc(&lab, "a")
    );
    let (st, tp) = nlink_lab::state::load(&lab_name).unwrap();
    nlink_lab::state::snapshot_save(&lab_name, "chaos", None, &st, &tp).unwrap();
    lab.heal("a:eth0").await.unwrap();
    assert!(!blocked(&lab, "a", "eth0"));
    assert!(
        qdisc(&lab, "a").contains("50"),
        "heal leaves the live value: {}",
        qdisc(&lab, "a")
    );

    // --reset-impairments converges on the declaration again.
    let reset = nlink_lab::ApplyOptions {
        reset_impairments: true,
        ..Default::default()
    };
    nlink_lab::apply_with(&mut lab, &desired, &reset)
        .await
        .expect("apply --reset failed");
    let out = qdisc(&lab, "a");
    assert!(
        out.contains("1ms") || out.contains("1.0ms"),
        "declared delay back: {out}"
    );
    assert!(lab.live_impairments().is_empty());

    // restore "chaos": node c stays gone? No — chaos was taken *after*
    // apply, so c exists, a:eth0 is partitioned with 50ms saved.
    let chaos = nlink_lab::state::snapshot_load(&lab_name, "chaos").unwrap();
    nlink_lab::restore(&mut lab, &chaos)
        .await
        .expect("restore chaos failed");
    assert!(blocked(&lab, "a", "eth0"));
    assert!(qdisc(&lab, "a").contains("50"), "{}", qdisc(&lab, "a"));
    assert!(lab.is_partitioned("a:eth0"));
    assert!(lab.topology().nodes.contains_key("c"));

    // restore "base": back to two nodes, declared 1ms, no partition.
    let base = nlink_lab::state::snapshot_load(&lab_name, "base").unwrap();
    nlink_lab::restore(&mut lab, &base)
        .await
        .expect("restore base failed");
    assert!(!lab.topology().nodes.contains_key("c"));
    assert!(!lab.is_partitioned("a:eth0"));
    assert!(!blocked(&lab, "a", "eth0"));
    let out = qdisc(&lab, "a");
    assert!(out.contains("1ms") || out.contains("1.0ms"), "{out}");
    assert!(!out.contains("loss"), "{out}");
    let ns_c = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&ns_c.stdout).contains(&format!("{lab_name}-c")),
        "node c namespace must be gone after restoring the base snapshot"
    );

    assert_eq!(nlink_lab::state::snapshot_list(&lab_name).unwrap().len(), 2);
    lab.destroy().await.expect("destroy failed");
}

/// The same lab seed loses the same datagrams (#337): 1000 numbered UDP
/// datagrams through a per-pair `loss 10%`, deployed twice with seed 7,
/// miss the same sequence numbers; seed 8 misses others. The loss sits on
/// a per-pair rule because its flower filter matches only IPv4 to the
/// destination: ARP and IPv6 neighbour traffic never reach that netem,
/// so they cannot shift its random sequence.
#[tokio::test]
async fn a_lab_seed_loses_the_same_datagrams() {
    use std::collections::BTreeSet;
    use std::time::Duration;

    if !privileged("a_lab_seed_loses_the_same_datagrams") {
        return;
    }
    if !feature(
        "a_lab_seed_loses_the_same_datagrams",
        "netem-seed",
        nlink_lab::seed::kernel_honours(),
    ) {
        return;
    }
    async fn lost(seed: u64, tag: &str) -> BTreeSet<u32> {
        let lab_name = format!("{tag}-{}", std::process::id());
        let topo = nlink_lab::parser::parse(&format!(
            r#"lab "{lab_name}" {{ seed {seed} }}
node a
node b
network lan {{
  members [a:eth0, b:eth0]
  subnet 10.61.0.0/24
  impair a -- b {{ loss 10% }}
}}
"#
        ))
        .unwrap();
        let lab = topo.deploy().await.expect("deploy failed");
        let _guard = LabCleanup {
            name: lab.name().to_string(),
        };
        assert_eq!(lab.topology().lab.seed, Some(seed));
        let addr = |node: &str| {
            let addrs = lab.node_addresses(node).unwrap();
            let a = addrs["eth0"]
                .iter()
                .find(|a| a.contains('.'))
                .unwrap()
                .clone();
            a.split('/').next().unwrap().to_string()
        };
        let (a, b) = (addr("a"), addr("b"));
        let in_ns = |node: &str| {
            nlink::netlink::namespace::enter(lab.namespace_for(node).unwrap()).unwrap()
        };
        let rx = {
            let _in = in_ns("b");
            std::net::UdpSocket::bind(format!("{b}:9000")).unwrap()
        };
        rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        // read while the datagrams arrive: the receive buffer holds a
        // few hundred, not a thousand
        let reader = std::thread::spawn(move || {
            let mut got = BTreeSet::new();
            let mut buf = [0u8; 16];
            while let Ok(n) = rx.recv(&mut buf) {
                if n == 4 {
                    got.insert(u32::from_be_bytes(buf[..4].try_into().unwrap()));
                }
            }
            got
        });
        let tx = {
            let _in = in_ns("a");
            std::net::UdpSocket::bind(format!("{a}:0")).unwrap()
        };
        for seq in 0u32..1000 {
            tx.send_to(&seq.to_be_bytes(), format!("{b}:9000")).unwrap();
            // paced: netem's queue (1000) and the socket buffers never fill
            if seq % 50 == 49 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let got = reader.join().unwrap();
        std::mem::forget(_guard);
        lab.destroy().await.expect("destroy failed");
        (0..1000).filter(|s| !got.contains(s)).collect()
    }
    let first = lost(7, "seeda").await;
    let again = lost(7, "seedb").await;
    let other = lost(8, "seedc").await;
    assert!(
        (30..250).contains(&first.len()),
        "10% loss of 1000 lost {}",
        first.len()
    );
    assert_eq!(first, again, "seed 7 must lose the same datagrams twice");
    assert_ne!(first, other, "seed 8 must lose other datagrams");
}
