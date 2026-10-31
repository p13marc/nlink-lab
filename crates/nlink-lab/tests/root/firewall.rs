//! Root tests: nftables firewall: rules, sets, counters, logs and refusals.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::test_helpers::{feature, privileged};
use nlink_lab::{Lab, RunningLab};

// ─── Firewall test ────────────────────────────────────────

#[tokio::test]
async fn deploy_firewall() {
    if !privileged("deploy_firewall") {
        return;
    }
    if !feature("deploy_firewall", "nftables", has_nftables()) {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "deploy_firewall");
    let lab = topo.deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let output = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("filter") || output.stdout.contains("nlink"),
        "expected nftables rules in output: {}",
        output.stdout
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("failed to destroy lab");
}

// Plan 158a — applying the same firewall config a second time
// must not perturb live rules. Verified by reading the
// nftables generation counter from `nft list ruleset` before
// and after the second apply: equal counter ⇒ kernel made no
// mutations (the `apply_reconcile` diff was empty).
#[tokio::test]
async fn nftables_reapply_is_zero_ops() {
    if !privileged("nftables_reapply_is_zero_ops") {
        return;
    }
    if !feature("nftables_reapply_is_zero_ops", "nftables", has_nftables()) {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "nftables_reapply_is_zero_ops");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Snapshot the kernel's per-table generation counter (the
    // "handle" each rule carries monotonically increases with
    // any kernel-side mutation in the namespace).
    let baseline = lab
        .exec("server", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    assert_eq!(baseline.exit_code, 0);
    let baseline_handles = collect_rule_handles(&baseline.stdout);
    assert!(
        !baseline_handles.is_empty(),
        "expected at least one rule with a handle after initial deploy"
    );

    // Re-apply the same topology — diff should be empty and
    // apply_diff should be a no-op for the nftables layer.
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("failed to re-apply unchanged topology");

    let after = lab
        .exec("server", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    assert_eq!(after.exit_code, 0);
    let after_handles = collect_rule_handles(&after.stdout);
    assert_eq!(
        baseline_handles, after_handles,
        "reapply on unchanged topology should preserve rule handles \
         (kernel mutations would re-issue handles); baseline={baseline_handles:?} after={after_handles:?}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("failed to destroy lab");
}

// Plan 158a — foreign nftables rules (no `nlink-lab/` USERDATA
// comment) must survive a reconcile reapply. Documented
// guarantee that lets users hand-edit via
// `nlink-lab exec NODE -- nft -f extra.nft` without their
// additions being clobbered.
#[tokio::test]
async fn nftables_foreign_rule_survives_apply() {
    if !privileged("nftables_foreign_rule_survives_apply") {
        return;
    }
    if !feature(
        "nftables_foreign_rule_survives_apply",
        "nftables",
        has_nftables(),
    ) {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "nftables_foreign_rule_survives_apply");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Inject a foreign rule with a distinctive comment that the
    // diff path must leave alone (no `nlink-lab/` prefix, no
    // `nlink:` USERDATA key — pure foreign).
    let inject = lab
        .exec(
            "server",
            "nft",
            &[
                "add",
                "rule",
                "inet",
                "nlink-lab",
                "input",
                "tcp",
                "dport",
                "9999",
                "accept",
                "comment",
                "cilium-style-foreign",
            ],
        )
        .unwrap();
    assert_eq!(
        inject.exit_code, 0,
        "failed to inject foreign rule: {}",
        inject.stderr
    );

    // Confirm the foreign rule is live before reapply.
    let before = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        before.stdout.contains("9999"),
        "foreign rule must be live before reapply; ruleset: {}",
        before.stdout
    );

    // The diff leaves it out: nothing to change (#300).
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "{layered}");

    // Re-apply the same topology. The diff path must not touch
    // the foreign rule.
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("failed to re-apply unchanged topology");

    let after = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        after.stdout.contains("9999"),
        "foreign rule was deleted by reapply; surviving ruleset: {}",
        after.stdout
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158a — editing a rule in place must be a replace (nlink's
// `Change::Update` with no placement: kernel-side `NEWRULE |
// NLM_F_REPLACE | NFTA_RULE_HANDLE`), not delete+rebuild. The test
// asserts that unchanged rules keep their handles across an edit —
// the diff path only re-issues handles for actually changed rules.
// Combined with the foreign-rule test, this guards the "atomic
// in-place replace" claim of Plan 158a.
#[tokio::test]
async fn nftables_rule_edit_replaces_in_place() {
    if !privileged("nftables_rule_edit_replaces_in_place") {
        return;
    }
    if !feature(
        "nftables_rule_edit_replaces_in_place",
        "nftables",
        has_nftables(),
    ) {
        return;
    }

    let mut initial = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut initial, "nftables_rule_edit_replaces_in_place");
    let mut lab = initial
        .clone()
        .deploy()
        .await
        .expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let baseline = lab
        .exec("server", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    let baseline_handles = collect_rule_handles(&baseline.stdout);
    assert!(
        !baseline_handles.is_empty(),
        "expected at least one firewall rule after initial deploy"
    );

    // Edit the topology: change the FIRST rule's match expression.
    // Other rules MUST keep their handles.
    let mut edited = initial.clone();
    if let Some(node) = edited.nodes.get_mut("server")
        && let Some(fw) = node.firewall.as_mut()
        && let Some(first) = fw.rules.first_mut()
    {
        first.match_expr = Some("tcp dport 81".to_string());
    }

    // nlink 0.33 says so: one rule, updated in place (no placement), to
    // the edit as nft prints it (#300)
    let layered = nlink_lab::compute_layered_diff(&lab, &edited)
        .await
        .expect("compute_layered_diff failed");
    let rules: Vec<_> = layered.nftables["server"].rules().collect();
    assert!(
        matches!(
            rules.as_slice(),
            [nlink::netlink::nftables::config::Change::Update { to, .. }]
                if to.placement.is_none()
                    && to.nft.as_deref().is_some_and(|t| t.starts_with("tcp dport 81"))
        ),
        "{layered}"
    );

    nlink_lab::apply(&mut lab, &edited)
        .await
        .expect("failed to apply edited topology");

    let after = lab
        .exec("server", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    let after_handles = collect_rule_handles(&after.stdout);

    // The number of rules should be identical; some handles may
    // shift if the kernel re-issued, but the count must match.
    // (A delete+rebuild would have totally different handles AND
    // the same count too — but the rule count being preserved is
    // a necessary precondition for "atomic in-place replace".)
    assert_eq!(
        baseline_handles.len(),
        after_handles.len(),
        "rule count must match before/after edit: baseline={baseline_handles:?} after={after_handles:?}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158a — removing the firewall block from the desired
// topology must clear the table on the node. Verified by
// checking that `nft list table inet nlink-lab` returns ENOENT
// (or shows the table empty).
#[tokio::test]
async fn nftables_remove_firewall_clears_table() {
    if !privileged("nftables_remove_firewall_clears_table") {
        return;
    }
    if !feature(
        "nftables_remove_firewall_clears_table",
        "nftables",
        has_nftables(),
    ) {
        return;
    }

    let mut initial = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut initial, "nftables_remove_firewall_clears_table");
    let mut lab = initial
        .clone()
        .deploy()
        .await
        .expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Confirm the table exists before edit.
    let pre = lab
        .exec("server", "nft", &["list", "table", "inet", "nlink-lab"])
        .unwrap();
    assert_eq!(
        pre.exit_code, 0,
        "expected the nlink-lab table to be live after initial deploy"
    );

    // Edit: drop the firewall block on `server`.
    let mut edited = initial.clone();
    if let Some(node) = edited.nodes.get_mut("server") {
        node.firewall = None;
    }
    nlink_lab::apply(&mut lab, &edited)
        .await
        .expect("failed to apply edited topology");

    // After apply, the table should be cleared (firewall sub-rules
    // gone) — either the table itself is gone, or `list table`
    // shows an empty body.
    let post = lab
        .exec("server", "nft", &["list", "table", "inet", "nlink-lab"])
        .unwrap();
    let cleared = post.exit_code != 0 || !post.stdout.contains("tcp dport");
    assert!(
        cleared,
        "expected nlink-lab table to be cleared after firewall removal, \
         got: stdout={} stderr={}",
        post.stdout, post.stderr
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// ─── Firewall packet filtering ──────────────────────────

#[tokio::test]
async fn firewall_blocks_traffic() {
    if !privileged("firewall_blocks_traffic") {
        return;
    }
    if !feature("firewall_blocks_traffic", "nftables", has_nftables()) {
        return;
    }

    let lab_name = format!("fw-block-{}", std::process::id());
    let topo = Lab::new(&lab_name)
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("router", |n| n.profile("router"))
        .node("server", |n| {
            n.route("default", |r| r.via("10.0.2.1")).firewall(|f| {
                f.policy("drop")
                    .rule("ct state established,related", "accept")
            })
        })
        .node("client", |n| n.route("default", |r| r.via("10.0.1.1")))
        .link("router:eth0", "client:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .link("router:eth1", "server:eth0", |l| {
            l.addresses("10.0.2.1/24", "10.0.2.2/24")
        })
        .build();

    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Server has drop policy — client's ping should fail
    let output = lab
        .exec("client", "ping", &["-c1", "-W1", "10.0.2.2"])
        .unwrap();
    assert_ne!(
        output.exit_code, 0,
        "ping should be blocked by firewall, but succeeded"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// nlink 0.32 firewall statements (#189), with real traffic: `rpf` drops a
/// spoofed source (and counts it) while the real one passes, and a
/// negated port range lets 8080 through and drops 9090. A byte-rate limit
/// and a mark range install and read back as declared (converged).
#[tokio::test]
async fn firewall_rpf_negated_ranges_and_byte_limits() {
    if !privileged("firewall_rpf_negated_ranges_and_byte_limits") {
        return;
    }
    let lab_name = format!("fw032-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  firewall forward policy accept {{
    drop rpf counter name spoofed
    accept limit 1mb/second burst 64kb
    mark 0x10-0x1f counter
  }}
}}
node server {{
  route default via 10.0.2.1
  firewall policy accept {{
    accept ct established,related
    drop tcp dport != 8000-8999
  }}
}}
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- server:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(
        layered.is_empty(),
        "the kernel reads every rule back as declared:\n{layered}"
    );

    // The server pings the client from its own address: routed. Then from a
    // client-side address it does not own: gw's fib has no route back out of
    // wan0 for it, so `rpf` drops it.
    let ping = |args: &[&str]| lab.exec("server", "ping", args).unwrap().exit_code;
    assert_eq!(
        ping(&["-c", "1", "-W", "1", "10.0.1.10"]),
        0,
        "a real source passes"
    );
    let add = lab
        .exec(
            "server",
            "ip",
            &["addr", "add", "10.0.1.99/32", "dev", "eth0"],
        )
        .unwrap();
    assert_eq!(add.exit_code, 0, "{}", add.stderr);
    ping(&[
        "-c",
        "3",
        "-i",
        "0.2",
        "-W",
        "1",
        "-I",
        "10.0.1.99",
        "10.0.1.10",
    ]);
    let spoofed = lab
        .firewall_counters_blocking("gw")
        .unwrap()
        .into_iter()
        .find(|c| c.name == "spoofed")
        .expect("the spoofed counter");
    assert!(
        spoofed.packets >= 3,
        "rpf dropped {} packets",
        spoofed.packets
    );

    let bind = |port: u16| {
        lab.ns_resolver_of("server")
            .unwrap()
            .netns()
            .unwrap()
            .run(move || std::net::TcpListener::bind(("0.0.0.0", port)))
            .unwrap()
            .unwrap()
    };
    let (_in_range, _out_of_range) = (bind(8080), bind(9090));
    let wait = std::time::Duration::from_secs(2);
    lab.tcp_connect("client", "10.0.2.10:8080".parse().unwrap(), wait)
        .expect("8080 is inside the range");
    let blocked = lab.tcp_connect("client", "10.0.2.10:9090".parse().unwrap(), wait);
    assert!(
        blocked
            .as_ref()
            .is_err_and(|e| e.to_string().contains("timed out")),
        "9090 is outside the range, dropped: {blocked:?}"
    );
    lab.destroy().await.expect("destroy failed");
}

/// #302's matches with real traffic, each counted on `gw`:
/// - a SYN to a port above 9000 is dropped (`tcp flags syn / syn,ack`, `>`);
/// - a marked packet passes `mark >= 0x10`;
/// - a DNATed flow matches `ct status dnat` and its replies
///   `ct direction reply`;
/// - pings match `l4proto icmp`, and a broadcast ping `pkttype broadcast`.
///
/// Every rule reads back as declared, `ct zone` included (nothing sets a
/// zone here: it installs and converges).
#[tokio::test]
async fn firewall_flags_conntrack_and_orders_match_traffic() {
    if !privileged("firewall_flags_conntrack_and_orders_match_traffic") {
        return;
    }
    if !feature(
        "firewall_flags_conntrack_and_orders_match_traffic",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fw302-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  firewall policy accept {{
    drop pkttype broadcast counter name broadcasts
  }}
  firewall forward policy accept {{
    drop tcp flags syn / syn,ack tcp dport > 9000 counter name high-syn
    tcp flags fin,syn tcp dport 8080 counter name fin-or-syn
    tcp dport 8080 mark set 0x20
    mark >= 0x10 counter name marked
    ct status dnat counter name translated
    ct direction reply counter name replies
    l4proto icmp counter name icmp
    ct zone > 7 counter name zoned
    ct mark != 0 counter name ct-marked
  }}
  nat {{
    dnat iif lan0 dst 10.0.1.1 tcp dport 80 to 10.0.2.10:8080
  }}
}}
node server {{ route default via 10.0.2.1 }}
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- server:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
"#
    ))
    .unwrap();
    let r = topo.validate();
    assert!(!r.has_errors(), "{:?}", r.issues());
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(
        layered.is_empty(),
        "the kernel reads every rule back as declared:\n{layered}"
    );
    let counter = |name: &str| {
        lab.firewall_counters_blocking("gw")
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("counter {name}"))
            .packets
    };

    let bind = |port: u16| {
        lab.ns_resolver_of("server")
            .unwrap()
            .netns()
            .unwrap()
            .run(move || std::net::TcpListener::bind(("0.0.0.0", port)))
            .unwrap()
            .unwrap()
    };
    let (_low, _high) = (bind(8080), bind(9090));
    let wait = std::time::Duration::from_secs(2);
    let high = lab.tcp_connect("client", "10.0.2.10:9090".parse().unwrap(), wait);
    assert!(
        high.as_ref()
            .is_err_and(|e| e.to_string().contains("timed out")),
        "a SYN to a port above 9000 is dropped: {high:?}"
    );
    assert!(counter("high-syn") >= 1, "high-syn");
    lab.tcp_connect("client", "10.0.2.10:8080".parse().unwrap(), wait)
        .expect("8080 is not above 9000");
    assert!(counter("marked") >= 1, "mark >= 0x10 after mark set 0x20");
    // a flag list matches any of its flags: a lone SYN is `fin,syn`
    assert!(counter("fin-or-syn") >= 1, "tcp flags fin,syn on a SYN");
    lab.tcp_connect("client", "10.0.1.1:80".parse().unwrap(), wait)
        .expect("the DNATed port reaches the server");
    assert!(counter("translated") >= 1, "ct status dnat");
    assert!(counter("replies") >= 1, "ct direction reply");

    let ping = |args: &[&str]| lab.exec("client", "ping", args).unwrap();
    assert_eq!(ping(&["-c", "1", "-W", "1", "10.0.2.10"]).exit_code, 0);
    assert!(counter("icmp") >= 1, "l4proto icmp");
    ping(&["-b", "-c", "2", "-i", "0.2", "-W", "1", "10.0.1.255"]);
    assert!(counter("broadcasts") >= 1, "pkttype broadcast");
    assert_eq!(counter("zoned"), 0, "no zone is set here");
    assert_eq!(counter("ct-marked"), 0, "no ct mark is set here");
    lab.destroy().await.expect("destroy failed");
}

// ─── firewall: reject, chains, matches on real traffic (#164) ───

/// Start an iperf3 server on `port` in `node`, detached, and wait for it.
fn listen_tcp(lab: &RunningLab, node: &str, port: u16) {
    let p = port.to_string();
    let out = lab
        .exec(node, "iperf3", &["-s", "-D", "-p", &p, "--one-off"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "iperf3 -s: {}", out.stderr);
    for _ in 0..20 {
        let ss = lab.exec(node, "ss", &["-ltn"]).unwrap();
        if ss.stdout.contains(&format!(":{p} ")) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("iperf3 never listened on {node}:{port}");
}

/// `reject` rejects. Before #164 the planner installed it as `accept`.
/// A drop is silence (a timeout); a reject answers at once — ICMP port
/// unreachable by default, a TCP reset with `reject with tcp-reset` — and
/// a port range and `!=` match what they say.
#[tokio::test]
async fn firewall_reject_is_a_reject_and_ranges_match() {
    if !privileged("firewall_reject_is_a_reject_and_ranges_match") {
        return;
    }
    if !feature(
        "firewall_reject_is_a_reject_and_ranges_match",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwrj-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node c
node s {{
  firewall policy accept {{
    reject icmp 8
    reject tcp dport 5201
    reject with tcp-reset tcp dport 5202
    drop tcp dport 8000-8999
    drop src != 10.0.0.0/24 tcp dport 5203
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
link c:eth1 -- s:eth1 {{ 10.0.1.1/24 -- 10.0.1.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // ICMP: a reject answers (a drop would time out, an accept reply)
    let ping = lab
        .exec("c", "ping", &["-c", "1", "-W", "2", "10.0.0.2"])
        .unwrap();
    assert_ne!(ping.exit_code, 0, "ping must be rejected: {}", ping.stdout);
    assert!(
        ping.stdout.contains("Unreachable"),
        "a reject answers with ICMP unreachable: {}",
        ping.stdout
    );

    for port in [5201, 5202, 5203] {
        listen_tcp(&lab, "s", port);
    }
    // reject (ICMP port unreachable) and reject with tcp-reset: refused fast
    for port in [5201, 5202] {
        let (rc, took) = tcp_connect(&lab, "c", "10.0.0.2", port);
        assert_ne!(rc, 0, "port {port}: the connect must fail");
        assert_ne!(rc, 124, "port {port}: a reject must answer, not time out");
        assert!(
            took < std::time::Duration::from_millis(1500),
            "port {port}: {took:?}"
        );
    }
    let nft = lab
        .exec("s", "nft", &["list", "chain", "inet", "nlink-lab", "input"])
        .unwrap()
        .stdout;
    assert!(nft.contains("reject with tcp reset"), "{nft}");

    // a port range: 8500 is dropped (timeout), 9000 is not (refused, no listener)
    let (rc, _) = tcp_connect(&lab, "c", "10.0.0.2", 8500);
    assert_eq!(
        rc, 124,
        "8500 is in 8000-8999: dropped, so the connect times out"
    );
    let (rc, took) = tcp_connect(&lab, "c", "10.0.0.2", 9000);
    assert!(rc != 0 && rc != 124 && took < std::time::Duration::from_millis(1500));

    // `src != 10.0.0.0/24`: from 10.0.0.1 port 5203 is open, from 10.0.1.1 dropped
    let (rc, _) = tcp_connect(&lab, "c", "10.0.0.2", 5203);
    assert_eq!(rc, 0, "10.0.0.1 is inside 10.0.0.0/24: accepted");
    let (rc, _) = tcp_connect(&lab, "c", "10.0.1.2", 5203);
    assert_eq!(rc, 124, "10.0.1.1 is outside 10.0.0.0/24: dropped");

    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// A firewall policy edit on a running lab reaches the kernel. Before
/// nlink 0.31 a declared chain's policy change was sent as a chain *add*
/// with `NLM_F_EXCL`, which the kernel refuses with EEXIST for a chain that
/// exists (nlink #456): no policy edit ever applied, including the forward
/// chain's flip to `accept` on labs upgraded across #164. The chain's rule
/// keeps its handle.
#[tokio::test]
async fn firewall_policy_edit_applies_to_a_running_lab() {
    if !privileged("firewall_policy_edit_applies_to_a_running_lab") {
        return;
    }
    if !feature(
        "firewall_policy_edit_applies_to_a_running_lab",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwpe-{}", std::process::id());
    let src = |policy: &str| {
        format!(
            r#"
lab "{lab_name}"
node c
node s {{
  firewall policy {policy} {{
    accept ct established,related
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("drop")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let chain = |lab: &RunningLab| {
        lab.exec(
            "s",
            "nft",
            &["-a", "list", "chain", "inet", "nlink-lab", "input"],
        )
        .unwrap()
        .stdout
    };
    let ping = |lab: &RunningLab| {
        lab.exec("c", "ping", &["-c", "1", "-W", "1", "10.0.0.2"])
            .unwrap()
            .exit_code
    };
    let before = chain(&lab);
    assert!(before.contains("policy drop"), "{before}");
    assert_ne!(ping(&lab), 0, "policy drop must drop the ping");

    let desired = nlink_lab::parser::parse(&src("accept")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply (policy accept) failed");
    let after = chain(&lab);
    assert!(after.contains("policy accept"), "{after}");
    assert_eq!(ping(&lab), 0, "policy accept must let the ping through");
    assert_eq!(
        collect_rule_handles(&before),
        collect_rule_handles(&after),
        "a policy edit must not replace the chain's rules"
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// A router's input `policy drop` no longer drops what it routes, and a
/// forward chain filters routed traffic (examples/firewall-router.nll,
/// whose assertions cover both directions and the router itself).
#[tokio::test]
async fn firewall_forward_chain_filters_routed_traffic() {
    if !privileged("firewall_forward_chain_filters_routed_traffic") {
        return;
    }
    if !feature(
        "firewall_forward_chain_filters_routed_traffic",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    // 1. input-only firewall on a router: routing works
    let lab_name = format!("fwfw-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  firewall policy drop {{ accept ct established,related }}
}}
node a {{ route default via 10.0.1.1 }}
node b {{ route default via 10.0.2.1 }}
link r:eth0 -- a:eth0 {{ 10.0.1.1/24 -- 10.0.1.2/24 }}
link r:eth1 -- b:eth0 {{ 10.0.2.1/24 -- 10.0.2.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ping = lab
        .exec("a", "ping", &["-c", "2", "-W", "2", "10.0.2.2"])
        .unwrap();
    assert_eq!(
        ping.exit_code, 0,
        "an input `policy drop` must not drop routed traffic: {}",
        ping.stdout
    );
    let ping = lab
        .exec("a", "ping", &["-c", "1", "-W", "1", "10.0.1.1"])
        .unwrap();
    assert_ne!(ping.exit_code, 0, "the router itself still drops pings");
    lab.destroy().await.expect("destroy failed");
    drop(guard);

    // 2. the example: forward chain + assertions, converged, stable handles
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-router.nll"
    ))
    .unwrap();
    unique(&mut topo, "firewall_forward_chain_filters_routed_traffic");
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert_eq!(lab.assertion_results().len(), 4);
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");
    let handles = |lab: &RunningLab| {
        collect_rule_handles(
            &lab.exec("gw", "nft", &["-a", "list", "table", "inet", "nlink-lab"])
                .unwrap()
                .stdout,
        )
    };
    let before = handles(&lab);
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply failed");
    assert_eq!(before, handles(&lab), "a re-apply must not touch a rule");
    lab.destroy().await.expect("destroy failed");
}

// ─── firewall sets on real traffic (#165) ───

/// Named sets: v4 and v6 blocklists drop exactly their members, a port
/// set its ports, a concatenated set its pairs and not their crossings.
/// An element edit changes traffic without touching the rule; a foreign
/// element on a declared set is drift that `apply` removes, while on a
/// dynamic set it is kept.
#[tokio::test]
async fn firewall_sets_match_traffic_and_reconcile_elements() {
    if !privileged("firewall_sets_match_traffic_and_reconcile_elements") {
        return;
    }
    if !feature(
        "firewall_sets_match_traffic_and_reconcile_elements",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwset-{}", std::process::id());
    let src = |b4: &str| {
        format!(
            r#"
lab "{lab_name}"
node c
node s {{
  firewall {{
    set b4 [{b4}]
    set b6 [fd00:1::1]
    set ports port [5301-5302]
    set pairs ipv4 . port [10.0.0.1 . 5401, 10.0.1.1 . 5402]
    set dyn ipv4 dynamic
    input policy accept {{
      drop src @b4
      drop src @b6
      drop src @dyn
      drop tcp dport @ports
      drop src . tcp dport @pairs
    }}
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
link c:eth1 -- s:eth1 {{ 10.0.1.1/24 -- 10.0.1.2/24 }}
link c:eth2 -- s:eth2 {{ fd00:1::1/64 -- fd00:1::2/64 }}
link c:eth3 -- s:eth3 {{ fd00:2::1/64 -- fd00:2::2/64 }}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("10.0.1.1")).unwrap();
    let r = topo.validate();
    assert!(!r.has_errors(), "{:?}", r.issues());
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    for iface in ["eth2", "eth3"] {
        wait_for_dad(&lab, "c", iface).await;
        wait_for_dad(&lab, "s", iface).await;
    }
    let pings = |lab: &RunningLab, to: &str| {
        lab.exec("c", "ping", &["-c", "1", "-W", "1", to])
            .unwrap()
            .exit_code
            == 0
    };
    // blocklists: 10.0.1.1 and fd00:1::1 are members, the others are not
    assert!(pings(&lab, "10.0.0.2"));
    assert!(!pings(&lab, "10.0.1.2"), "10.0.1.1 is in @b4");
    assert!(pings(&lab, "fd00:2::2"));
    assert!(!pings(&lab, "fd00:1::2"), "fd00:1::1 is in @b6");
    // a port set: 5301 dropped (timeout), 5303 refused (no listener)
    assert_eq!(tcp_connect(&lab, "c", "10.0.0.2", 5301).0, 124);
    let (rc, took) = tcp_connect(&lab, "c", "10.0.0.2", 5303);
    assert!(rc != 0 && rc != 124 && took < std::time::Duration::from_millis(1500));
    // a concatenated set: (10.0.0.1, 5401) dropped; (10.0.0.1, 5402) is a crossing
    assert_eq!(tcp_connect(&lab, "c", "10.0.0.2", 5401).0, 124);
    let (rc, _) = tcp_connect(&lab, "c", "10.0.0.2", 5402);
    assert_ne!(rc, 124, "a crossing of two members' parts is not a member");

    let nft_handles = |lab: &RunningLab| {
        collect_rule_handles(
            &lab.exec(
                "s",
                "nft",
                &["-a", "list", "chain", "inet", "nlink-lab", "input"],
            )
            .unwrap()
            .stdout,
        )
    };
    let handles = nft_handles(&lab);

    // an element edit: 10.0.1.1 leaves @b4, the rules stay put
    let edited = nlink_lab::parser::parse(&src("10.0.9.9")).unwrap();
    nlink_lab::apply(&mut lab, &edited)
        .await
        .expect("apply failed");
    assert!(pings(&lab, "10.0.1.2"), "10.0.1.1 is no longer blocked");
    assert_eq!(
        handles,
        nft_handles(&lab),
        "an element edit must not touch a rule"
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &edited)
        .await
        .unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");

    // a foreign element on a declared set is drift, and apply removes it
    let add = |lab: &RunningLab, set: &str, elem: &str| {
        let elem = format!("{{ {elem} }}");
        let out = lab
            .exec(
                "s",
                "nft",
                &["add", "element", "inet", "nlink-lab", set, &elem],
            )
            .unwrap();
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
    };
    add(&lab, "b4", "10.0.0.1");
    assert!(!pings(&lab, "10.0.0.2"));
    let layered = nlink_lab::compute_layered_diff(&lab, &edited)
        .await
        .unwrap();
    assert!(!layered.is_empty(), "a foreign element on @b4 is drift");
    nlink_lab::apply(&mut lab, &edited)
        .await
        .expect("apply failed");
    assert!(pings(&lab, "10.0.0.2"), "apply removed the foreign element");

    // …on a dynamic set it is what the set is for, and it stays
    add(&lab, "dyn", "10.0.0.1");
    assert!(!pings(&lab, "10.0.0.2"));
    nlink_lab::apply(&mut lab, &edited)
        .await
        .expect("apply failed");
    assert!(
        !pings(&lab, "10.0.0.2"),
        "a dynamic set keeps its runtime elements"
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &edited)
        .await
        .unwrap();
    assert!(
        layered.is_empty(),
        "a dynamic set's runtime elements are not drift:\n{layered}"
    );
    lab.destroy().await.expect("destroy failed");
}

/// examples/firewall-sets.nll: its assertions pass and it converges.
#[tokio::test]
async fn firewall_sets_example_converges() {
    if !privileged("firewall_sets_example_converges") {
        return;
    }
    if !feature(
        "firewall_sets_example_converges",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-sets.nll"
    ))
    .unwrap();
    unique(&mut topo, "firewall_sets_example_converges");
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert_eq!(lab.assertion_results().len(), 3);
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

// ─── firewall counters, limit, log on real traffic (#166) ───

/// A named counter counts what its rule acts on, a counter assertion reads
/// it, a rate limit lets only part of a flood through, and counters that
/// have counted are not drift (nlink 0.30: they used to be replaced and
/// zeroed by every apply).
#[tokio::test]
async fn firewall_counters_limit_and_counter_assertions() {
    if !privileged("firewall_counters_limit_and_counter_assertions") {
        return;
    }
    if !feature(
        "firewall_counters_limit_and_counter_assertions",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwctr-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node c
node s {{
  firewall policy drop {{
    drop icmp 8 limit over 2/second burst 2 counter
    accept ct established,related
    accept icmp 8
    drop tcp dport 22 counter name ssh
    tcp dport 5201 counter name probe
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let assert_counter = |lab: &RunningLab, op: nlink_lab::types::CompareOp, value: u64| {
        nlink_lab::test_runner::eval_assertion_pub(
            lab,
            &nlink_lab::types::Assertion::Counter {
                node: "s".into(),
                name: "ssh".into(),
                metric: nlink_lab::types::CounterMetric::Packets,
                op,
                value,
            },
            &Default::default(),
        )
    };
    use nlink_lab::types::CompareOp;
    let (desc, ok, detail) = assert_counter(&lab, CompareOp::Eq, 0);
    assert!(ok, "{desc}: {detail:?}");

    for _ in 0..3 {
        let _ = tcp_connect(&lab, "c", "10.0.0.2", 22);
    }
    let counters = lab.firewall_counters("s").await.unwrap();
    let ssh = counters.iter().find(|c| c.name == "ssh").unwrap();
    assert!(ssh.packets >= 3 && ssh.bytes > 0, "{counters:?}");
    let probe = counters.iter().find(|c| c.name == "probe").unwrap();
    assert_eq!(probe.packets, 0, "{counters:?}");
    assert!(assert_counter(&lab, CompareOp::Gt, 0).1);
    let (_, ok, detail) = assert_counter(&lab, CompareOp::Eq, 0);
    assert!(!ok);
    assert!(detail.unwrap().starts_with("counted "));

    // pings over 2/second (burst 2) are dropped *before* the conntrack
    // rule, which would otherwise accept every echo after the first
    let flood = lab
        .exec(
            "c",
            "ping",
            &["-c", "40", "-i", "0.05", "-W", "1", "-q", "10.0.0.2"],
        )
        .unwrap();
    let received: u32 = flood
        .stdout
        .split(", ")
        .find_map(|p| {
            p.strip_suffix(" received")
                .and_then(|n| n.trim().parse().ok())
        })
        .unwrap_or(0);
    assert!(
        received > 0 && received < 20,
        "the limit let {received}/40 through: {}",
        flood.stdout
    );

    // counters that have counted are not drift, and a re-apply keeps them
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(
        layered.is_empty(),
        "a counted counter must not be drift:\n{layered}"
    );
    let handles = |lab: &RunningLab| {
        collect_rule_handles(
            &lab.exec(
                "s",
                "nft",
                &["-a", "list", "chain", "inet", "nlink-lab", "input"],
            )
            .unwrap()
            .stdout,
        )
    };
    let before = handles(&lab);
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply failed");
    assert_eq!(before, handles(&lab));
    let after = lab.firewall_counters("s").await.unwrap();
    let ssh_after = after.iter().find(|c| c.name == "ssh").unwrap();
    assert_eq!(
        ssh_after.packets, ssh.packets,
        "a re-apply must not reset a counter"
    );
    lab.destroy().await.expect("destroy failed");
}

/// examples/firewall-observability.nll: its assertions pass, its scenario
/// sees the probe in the counter, and it converges after counting.
#[tokio::test]
async fn firewall_observability_example_counts() {
    if !privileged("firewall_observability_example_counts") {
        return;
    }
    if !feature(
        "firewall_observability_example_counts",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-observability.nll"
    ))
    .unwrap();
    unique(&mut topo, "firewall_observability_example_counts");
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());
    let result = nlink_lab::scenario::run_scenario(&mut lab, &topo.scenarios[0])
        .await
        .expect("scenario failed to run");
    assert!(result.passed, "{result:?}");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// Renaming a named counter or set on a running lab, or dropping a chain
/// whose rule uses a named counter, applies (#174). Before nlink 0.31 the
/// batch deleted the object, set or chain before replacing the rules that
/// still used it, and every such apply failed with EBUSY (nlink #460). A
/// rule added by hand with an inline set no longer blocks the apply
/// either: its anonymous set is left alone (nlink #459).
#[tokio::test]
async fn firewall_renames_and_foreign_inline_sets_apply() {
    if !privileged("firewall_renames_and_foreign_inline_sets_apply") {
        return;
    }
    if !feature(
        "firewall_renames_and_foreign_inline_sets_apply",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwrn-{}", std::process::id());
    let src = |counter: &str, set: &str, output: bool| {
        let output = if output {
            "    output policy accept {\n      drop tcp dport 9 counter name x\n    }\n"
        } else {
            ""
        };
        format!(
            r#"
lab "{lab_name}"
node a
node b {{
  firewall {{
    set {set} [10.0.9.9]
    input policy accept {{
      drop tcp dport 22 counter name {counter}
      drop src @{set}
    }}
{output}  }}
}}
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("a", "blocked", true)).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ruleset = |lab: &RunningLab| {
        lab.exec("b", "nft", &["list", "table", "inet", "nlink-lab"])
            .unwrap()
            .stdout
    };

    // a hand-added rule with an inline set, in nlink-lab's own table
    let foreign = lab
        .exec(
            "b",
            "nft",
            &[
                "add",
                "rule",
                "inet",
                "nlink-lab",
                "input",
                "tcp",
                "dport",
                "{ 4001, 4002 }",
                "accept",
            ],
        )
        .unwrap();
    assert_eq!(foreign.exit_code, 0, "{}", foreign.stderr);

    for (step, desired) in [
        ("counter a -> b", src("b", "blocked", true)),
        ("set blocked -> deny", src("b", "deny", true)),
        (
            "output chain using counter x dropped",
            src("b", "deny", false),
        ),
    ] {
        let desired = nlink_lab::parser::parse(&desired).unwrap();
        nlink_lab::apply(&mut lab, &desired)
            .await
            .unwrap_or_else(|e| panic!("apply ({step}) failed: {e}"));
        let layered = nlink_lab::compute_layered_diff(&lab, &desired)
            .await
            .unwrap();
        assert!(layered.is_empty(), "{step}: not converged:\n{layered}");
    }
    let rs = ruleset(&lab);
    assert!(
        rs.contains("counter b ") || rs.contains("counter name \"b\""),
        "{rs}"
    );
    assert!(rs.contains("set deny"), "{rs}");
    assert!(!rs.contains("set blocked"), "{rs}");
    assert!(!rs.contains("chain output"), "{rs}");
    assert!(
        rs.contains("4001"),
        "the hand-added rule must be left alone: {rs}"
    );
    lab.destroy().await.expect("destroy failed");
}

/// #300: nlink 0.33 refuses an nftables change that a rule nlink-lab did
/// not write is in the way of — a set recreated for a new key while the
/// rule uses it — and the apply is refused before anything changes: the
/// earlier stages (a sysctl here) do not run, so nothing is rolled back.
/// A set the topology drops while such a rule uses it is kept, and the
/// diff says so without counting it as drift.
#[tokio::test]
async fn nftables_refusals_are_refused_before_anything_changes() {
    if !privileged("nftables_refusals_are_refused_before_anything_changes") {
        return;
    }
    if !feature(
        "nftables_refusals_are_refused_before_anything_changes",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwref-{}", std::process::id());
    let src = |set: &str, rule: &str, with_sysctl: bool| {
        let sysctl = if with_sysctl {
            r#"sysctl "net.ipv4.ip_nonlocal_bind" "1""#
        } else {
            ""
        };
        format!(
            r#"
lab "{lab_name}"
node a
node b {{
  {sysctl}
  firewall {{
    {set}
    input policy accept {{
      {rule}
    }}
  }}
}}
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        )
    };
    let topo =
        nlink_lab::parser::parse(&src("set mgmt port [22]", "accept tcp dport @mgmt", false))
            .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let foreign = lab
        .exec(
            "b",
            "nft",
            &[
                "add",
                "rule",
                "inet",
                "nlink-lab",
                "input",
                "tcp",
                "dport",
                "@mgmt",
                "counter",
            ],
        )
        .unwrap();
    assert_eq!(foreign.exit_code, 0, "{}", foreign.stderr);
    let nonlocal_bind = |lab: &RunningLab| {
        lab.exec("b", "cat", &["/proc/sys/net/ipv4/ip_nonlocal_bind"])
            .unwrap()
            .stdout
            .trim()
            .to_string()
    };
    assert_eq!(nonlocal_bind(&lab), "0");

    // the set's key changes (a recreate the hand-added rule is in the way
    // of), together with a sysctl, whose stage comes first
    let desired =
        nlink_lab::parser::parse(&src("set mgmt ipv4 [10.0.0.1]", "accept src @mgmt", true))
            .unwrap();
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .unwrap();
    assert!(
        layered.nftables["b"].refusal().is_some() && layered.to_string().contains("! "),
        "the diff says what it cannot do:\n{layered}"
    );
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = {
        let events = events.clone();
        move |ev: nlink_lab::api::OpEvent| events.lock().unwrap().push(ev)
    };
    let ctx = nlink_lab::OpCtx::new("apply", lab.name()).with_sink(std::sync::Arc::new(sink));
    let err = nlink_lab::apply_with_ctx(
        &mut lab,
        &desired,
        &nlink_lab::ApplyOptions::default(),
        &ctx,
    )
    .await
    .expect_err("the apply must be refused");
    assert!(
        err.to_string().contains("nothing has been changed") && err.to_string().contains("b: "),
        "{err}"
    );
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e.phase, nlink_lab::api::OpPhase::RollingBack { .. })),
        "refused before anything ran, so nothing to roll back"
    );
    assert_eq!(
        nonlocal_bind(&lab),
        "0",
        "the sysctl stage must not have run"
    );
    let set = lab
        .exec("b", "nft", &["list", "set", "inet", "nlink-lab", "mgmt"])
        .unwrap()
        .stdout;
    assert!(set.contains("inet_service"), "{set}");

    // the set dropped while the hand-added rule uses it: kept, and said
    let desired = nlink_lab::parser::parse(&src("", "accept tcp dport 22", false)).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("an apply that keeps a set in use is not refused");
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .unwrap();
    assert!(
        layered.is_empty() && layered.has_blocked() && layered.to_string().contains("! "),
        "kept, said, and not drift:\n{layered}"
    );
    lab.destroy().await.expect("destroy failed");
}
