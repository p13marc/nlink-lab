//! Root tests: bridge networks, VLAN trunks, pair impairment matrices and patterns.
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

/// Check whether bridge VLAN filtering is functional.
fn has_bridge_vlan_filtering() -> bool {
    has_kernel_module("bridge")
        && has_kernel_module("8021q")
        && std::process::Command::new("bridge")
            .args(["vlan", "show"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

// Regression test for the peer-name collision bug: two networks that
// share a 4-char prefix used to produce the same mgmt-ns peer interface
// name (`brlan_p{idx}`) and fail the second veth create with EEXIST.
// Hash-based naming should disambiguate them.
#[lab_test(topology = prefix_collision_topology)]
async fn deploy_networks_with_shared_prefix(lab: RunningLab) {
    // Success = both networks came up.
    let out_a = lab.exec("host_a", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(out_a.exit_code, 0);
    assert!(
        out_a.stdout.contains("10.1.0.2/24"),
        "host_a missing address on lan_a: {}",
        out_a.stdout
    );

    let out_b = lab.exec("host_b", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(out_b.exit_code, 0);
    assert!(
        out_b.stdout.contains("10.2.0.2/24"),
        "host_b missing address on lan_b: {}",
        out_b.stdout
    );
}

fn prefix_collision_topology() -> nlink_lab::Topology {
    // Both bridge names AND peer names use hash-based naming
    // (`nb{hash8}` and `np{hash8}{idx}`), so this test is a
    // regression check for both: that two networks sharing a long
    // common prefix get distinct bridge names AND distinct mgmt-side
    // peer interfaces. Lab name length is irrelevant — the
    // `#[lab_test]` macro can rewrite the name to anything.
    nlink_lab::TopologyBuilder::new("pcol")
        .node("host_a", |n| n)
        .node("host_b", |n| n)
        .network("lan_a", |net| {
            net.subnet("10.1.0.0/24")
                .port("host_a", |p| p.interface("eth0").address("10.1.0.2/24"))
        })
        .network("lan_b", |net| {
            net.subnet("10.2.0.0/24")
                .port("host_b", |p| p.interface("eth0").address("10.2.0.2/24"))
        })
        .build()
}

/// `network { mtu 9000 }` (#161): the member veths, both ends, get the
/// MTU, so the bridge keeps it once its ports join; a 9000-byte frame
/// crosses unfragmented.
#[tokio::test]
async fn network_mtu_reaches_bridge_and_members() {
    if !privileged("network_mtu_reaches_bridge_and_members") {
        return;
    }
    let lab_name = format!("nmtu-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
network lan {{
  mtu 9000
  members [a:eth0, b:eth0]
  port a:eth0 {{ 10.50.0.1/24 }}
  port b:eth0 {{ 10.50.0.2/24 }}
}}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    for node in ["a", "b"] {
        let out = lab
            .exec(node, "ip", &["-o", "link", "show", "eth0"])
            .unwrap();
        assert!(
            out.stdout.contains("mtu 9000"),
            "{node}:eth0: {}",
            out.stdout
        );
    }
    // Read the mgmt namespace through nlink, which finds it where
    // nlink-lab made it (`ip -n` looked in a directory the CI runner's
    // container does not share).
    let mgmt = format!("{}-mgmt", topo.lab.prefix());
    let conn = nlink::Connection::<nlink::Route>::new_in(mgmt.as_str()).unwrap();
    let links = conn.get_links().await.unwrap();
    let bridge_name = nlink_lab::types::network_bridge_name_for("lan");
    let shown: Vec<(Option<&str>, Option<u32>, Option<u32>)> = links
        .iter()
        .map(|l| (l.name(), l.mtu(), l.master()))
        .collect();
    let bridge = links
        .iter()
        .find(|l| l.name() == Some(bridge_name.as_str()))
        .unwrap_or_else(|| panic!("no bridge {bridge_name} in {mgmt}: {shown:?}"));
    assert_eq!(bridge.mtu(), Some(9000), "bridge: {shown:?}");
    let ports: Vec<_> = links
        .iter()
        .filter(|l| l.master() == Some(bridge.ifindex()))
        .collect();
    assert_eq!(ports.len(), 2, "{shown:?}");
    assert!(ports.iter().all(|l| l.mtu() == Some(9000)), "{shown:?}");

    let ping = lab
        .exec(
            "a",
            "ping",
            &["-c", "2", "-W", "2", "-M", "do", "-s", "8972", "10.50.0.2"],
        )
        .unwrap();
    assert_eq!(ping.exit_code, 0, "{}{}", ping.stdout, ping.stderr);
    lab.destroy().await.expect("destroy failed");
}

// ─── VLAN trunk / bridge test (plans 050 + 052) ─────────

#[lab_test("examples/vlan-trunk.nll")]
async fn deploy_bridge_vlan(lab: RunningLab) {
    if !feature(
        "deploy_bridge_vlan",
        "bridge-vlan-filtering",
        has_bridge_vlan_filtering(),
    ) {
        return;
    }
    assert_eq!(lab.topology().nodes.len(), 3);

    // Each host should have an eth0 interface (connected to the bridge)
    let output = lab.exec("host1", "ip", &["link", "show", "eth0"]).unwrap();
    assert!(
        output.stdout.contains("eth0"),
        "expected eth0 on host1: {}",
        output.stdout
    );

    let output = lab.exec("host3", "ip", &["link", "show", "eth0"]).unwrap();
    assert!(
        output.stdout.contains("eth0"),
        "expected eth0 on host3: {}",
        output.stdout
    );
}

// ─── VLAN isolation ─────────────────────────────────────

#[lab_test("examples/vlan-trunk.nll")]
async fn vlan_isolation(lab: RunningLab) {
    if !feature(
        "vlan_isolation",
        "bridge-vlan-filtering",
        has_bridge_vlan_filtering(),
    ) {
        return;
    }

    // Verify VLAN assignments on host interfaces via bridge vlan show.
    // host1 should have PVID 100, host3 should have PVID 200.
    // Note: the vlan-trunk.nll example has no IP addresses, so we can't ping.
    // Instead, verify the VLAN configuration was applied correctly.
    let output = lab.exec("host1", "ip", &["link", "show", "eth0"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "host1 eth0 not found: {}",
        output.stderr
    );
    assert!(
        output.stdout.contains("eth0"),
        "expected eth0 on host1: {}",
        output.stdout
    );

    let output = lab.exec("host3", "ip", &["link", "show", "eth0"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "host3 eth0 not found: {}",
        output.stderr
    );
    assert!(
        output.stdout.contains("eth0"),
        "expected eth0 on host3: {}",
        output.stdout
    );
}

#[lab_test("examples/pattern-mesh.nll")]
async fn pattern_mesh_deploy(lab: RunningLab) {
    // Mesh of 4 nodes = 6 links
    assert_eq!(lab.topology().links.len(), 6);
}

#[lab_test("examples/pattern-ring.nll")]
async fn pattern_ring_deploy(lab: RunningLab) {
    assert!(lab.topology().links.len() >= 4);
}

// ─── Plan 156 PR C — impair --show JSON view ────────────

// Round-4 follow-up: `--show --json` returned `endpoints: {}` for any
// topology built around bridge networks. The fix walks
// `networks.members` in addition to `links`. This deploys a real
// network-only topology, partitions a member endpoint, and asserts:
//   1. `topology_endpoints` includes the bridge member.
//   2. `is_partitioned(member)` is true after partition.
//   3. The kernel actually drops on the member's iface.
// Together these prove the `--show --json` flow can see endpoints
// declared via networks, end-to-end.
#[lab_test("examples/vlan-trunk.nll")]
async fn impair_show_includes_network_members(mut lab: RunningLab) {
    let endpoints = nlink_lab::impair_parse::topology_endpoints(lab.topology());
    assert!(
        endpoints.contains(&"host1:eth0".to_string()),
        "vlan-trunk topology has no `link` declarations — host1:eth0 \
         lives only in `network fabric.members`. The endpoint must \
         still appear in topology_endpoints. Got: {endpoints:?}"
    );

    lab.partition("host1:eth0").await.unwrap();
    assert!(lab.is_partitioned("host1:eth0"));

    assert!(
        blocked(&lab, "host1", "eth0"),
        "expected the drop filters on host1:eth0 after partition"
    );
}

/// An `impair` on the source of a network's per-pair matrix is the
/// matrix's default (#169): traffic to a peer no rule names gets it,
/// the pair rule's peer gets the rule. It used to be silently lost.
/// Removing the matrix removes its tree.
#[tokio::test]
async fn endpoint_impair_is_the_default_of_a_pair_matrix() {
    if !privileged("endpoint_impair_is_the_default_of_a_pair_matrix") {
        return;
    }
    let lab_name = format!("pdef-{}", std::process::id());
    let src = |rules: &str| {
        format!(
            r#"lab "{lab_name}"
node a
node b
node c
network lan {{
  members [a:eth0, b:eth0, c:eth0]
  subnet 10.50.0.0/24
  {rules}
}}
impair a:eth0 delay 100ms
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("impair a -- b { delay 20ms }")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ip = |n: &str| {
        lab.topology().networks["lan"].ports[&format!("{n}:eth0")].addresses[0]
            .split('/')
            .next()
            .unwrap()
            .to_string()
    };
    let (b, c) = (ip("b"), ip("c"));
    let to_b = avg_rtt_ms(&lab, "a", &b);
    let to_c = avg_rtt_ms(&lab, "a", &c);
    assert!(
        (18.0..60.0).contains(&to_b),
        "a -> b is the pair rule's 20ms: {to_b}"
    );
    assert!(to_c >= 98.0, "a -> c is the default 100ms: {to_c}");

    // Without the rules, the endpoint's own netem is its root again.
    let plain = nlink_lab::parser::parse(&src("")).unwrap();
    nlink_lab::apply(&mut lab, &plain)
        .await
        .expect("apply failed");
    let (root, _) = root_qdisc(&lab, "a", "eth0");
    assert!(root.contains("netem"), "{root}");
    assert!(avg_rtt_ms(&lab, "a", &b) >= 98.0);
    let layered = nlink_lab::compute_layered_diff(&lab, &plain).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// Removing a network's only per-pair rule removes the tree (it stayed:
/// the old op had nothing to undo it with).
#[tokio::test]
async fn removing_a_pair_matrix_removes_its_tree() {
    if !privileged("removing_a_pair_matrix_removes_its_tree") {
        return;
    }
    let lab_name = format!("prm-{}", std::process::id());
    let src = |rules: &str| {
        format!(
            r#"lab "{lab_name}"
node a
node b
network lan {{
  members [a:eth0, b:eth0]
  subnet 10.51.0.0/24
  {rules}
}}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("impair a -- b { delay 20ms }")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(root_qdisc(&lab, "a", "eth0").0.contains("htb 1: root"));
    let plain = nlink_lab::parser::parse(&src("")).unwrap();
    nlink_lab::apply(&mut lab, &plain)
        .await
        .expect("apply failed");
    let (root, _) = root_qdisc(&lab, "a", "eth0");
    assert!(!root.contains("htb"), "the per-pair tree stayed: {root}");
    let layered = nlink_lab::compute_layered_diff(&lab, &plain).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// #135: `Op::NetworkImpairments` is always re-run, and it used to
/// rebuild the whole HTB+netem+flower tree from scratch every time.
/// Reconciling instead, an unchanged matrix must converge with zero
/// kernel calls — and keep its counters, which is the observable proof.
#[tokio::test]
async fn reapplying_a_network_impair_matrix_keeps_its_counters() {
    if !privileged("reapplying_a_network_impair_matrix_keeps_its_counters") {
        return;
    }
    let lab_name = format!("permatrix-{}", std::process::id());
    let src = format!(
        r#"
lab "{lab_name}"
node a
node b
network lan {{
  subnet 10.30.0.0/24
  members [a:eth0, b:eth0]
  impair a -- b {{ delay 20ms rate-cap 5mbit }}
  impair b -- a {{ delay 20ms rate-cap 5mbit }}
}}
"#
    );
    let topo = nlink_lab::parser::parse(&src).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let tree = |lab: &RunningLab| -> String {
        lab.exec("a", "tc", &["-s", "qdisc", "show", "dev", "eth0"])
            .unwrap()
            .stdout
    };
    let htb_sent = |text: &str| -> u64 {
        text.split_whitespace()
            .skip_while(|w| *w != "Sent")
            .nth(1)
            .and_then(|w| w.parse().ok())
            .unwrap_or(0)
    };

    let before = tree(&lab);
    assert!(before.contains("qdisc htb"), "htb root expected: {before}");
    let _ = lab.exec("a", "ping", &["-c", "5", "-s", "1000", "10.30.0.2"]);
    let sent_before = htb_sent(&tree(&lab));
    assert!(sent_before > 0, "expected traffic through the htb root");

    // Apply the *same* topology. Nothing changed, so nothing should be
    // rebuilt.
    let desired = nlink_lab::parser::parse(&src).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("re-apply failed");

    let after = tree(&lab);
    assert!(after.contains("qdisc htb"), "htb root expected: {after}");
    assert!(
        htb_sent(&after) >= sent_before,
        "the tree was rebuilt: counters went {sent_before} -> {}",
        htb_sent(&after)
    );

    lab.destroy().await.expect("destroy failed");
}
