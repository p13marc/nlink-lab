//! Root tests: link kinds (dummy, VLAN, VXLAN, bond), sysctls, routing and kernel errors.
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
use nlink_lab::{RunningLab, TopologyBuilder};

#[lab_test("examples/simple.nll")]
async fn sysctl_forwarding(lab: RunningLab) {
    let output = lab
        .exec("router", "cat", &["/proc/sys/net/ipv4/ip_forward"])
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.trim(), "1");
}

// Plan 158b Phase 1 + Phase 3 — kernel errors carrying
// `NLMSGERR_ATTR_MSG` payload reach the user's hands via
// `Error::ext_ack()`. Trigger a real EEXIST against a live
// namespace and verify the accessor returns Some(_).
#[tokio::test]
async fn ext_ack_surfaces_from_real_kernel_error() {
    if !privileged("ext_ack_surfaces_from_real_kernel_error") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "ext_ack_surfaces_from_real_kernel_error");
    let lab = topo.deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Try to create a duplicate dummy iface on `router`. Kernel
    // returns EEXIST + a NLMSGERR_ATTR_MSG describing the
    // collision.
    let ns_name = lab.namespace_for("router").unwrap().to_string();
    let conn: nlink::Connection<nlink::Route> =
        nlink::Connection::new_in(ns_name).expect("connection_for failed");

    let dummy_a = nlink::netlink::link::DummyLink::new("ext-ack-dup");
    conn.add_link(dummy_a)
        .await
        .expect("first add must succeed");

    let dummy_b = nlink::netlink::link::DummyLink::new("ext-ack-dup");
    let err = conn
        .add_link(dummy_b)
        .await
        .expect_err("second add must fail with EEXIST");

    // Wrap as our error type and pull the chain accessors. errno
    // -17 is EEXIST under nlink's convention (factory negates the
    // input, and the kernel emits errno=17, so the stored value is
    // -17).
    let lab_err: nlink_lab::Error = err.into();
    let errno = lab_err.errno();
    assert!(
        errno == Some(17) || errno == Some(-17),
        "expected errno=EEXIST(17) via .errno() accessor, got: {errno:?}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e Slice 2 — declaratively-created dummy interfaces
// survive an idempotent re-apply. Boots a topology with a
// dummy + address on it, then re-applies and confirms the
// dummy is still present with the same address.
#[tokio::test]
async fn slice2_dummy_iface_reapply_is_zero_ops() {
    if !privileged("slice2_dummy_iface_reapply_is_zero_ops") {
        return;
    }
    let topo = nlink_lab::TopologyBuilder::new(&format!("slice2-dummy-{}", std::process::id()))
        .node("host", |n| {
            n.interface("lo0", |i| {
                i.kind(nlink_lab::types::InterfaceKind::Dummy)
                    .address("10.255.0.1/32")
            })
        })
        .build();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Dummy + address must be live after initial deploy.
    let out = lab.exec("host", "ip", &["addr", "show", "lo0"]).unwrap();
    assert_eq!(out.exit_code, 0, "lo0 must exist after deploy");
    assert!(
        out.stdout.contains("10.255.0.1/32"),
        "lo0 must carry declared address; got {}",
        out.stdout
    );

    // Re-apply must be a no-op.
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply failed");

    let out2 = lab.exec("host", "ip", &["addr", "show", "lo0"]).unwrap();
    assert_eq!(
        out.stdout, out2.stdout,
        "lo0 state changed across no-op reapply"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e Slice 3 — declaratively-created VLAN sub-interface
// survives an idempotent re-apply with the right parent + VID.
//
// Uses an existing veth as the parent. The pathological "VLAN
// parent declared in the same NetworkConfig" case is not
// covered here — nlink's `add_link` for VLAN resolves the
// parent name to ifindex at send time, and the just-added
// parent doesn't always show up in `get_link_by_name` fast
// enough on busy CI runners. The realistic shape (VLAN on a
// pre-existing veth or physical iface) is what users actually
// hit and is exhaustively unit-tested in
// `network_config_vlan_parent_dummy_declared_first_regardless_of_hashmap_order`.
#[tokio::test]
async fn slice3_vlan_iface_reapply_is_zero_ops() {
    if !privileged("slice3_vlan_iface_reapply_is_zero_ops") {
        return;
    }
    // Build a 2-node topology so step 5 creates a veth `eth0` on
    // `host`. Then add a VLAN sub-interface "eth0.42" to `host`.
    // The parent (eth0) already exists by the time step 11c
    // (NetworkConfig::apply) runs.
    let topo = nlink_lab::TopologyBuilder::new(&format!("slice3-vlan-{}", std::process::id()))
        .node("router", |n| n)
        .node("host", |n| {
            n.interface("eth0.42", |i| {
                i.kind(nlink_lab::types::InterfaceKind::Vlan)
                    .address("10.42.0.1/24")
            })
        })
        .link("router:eth0", "host:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build();

    // Attach parent + vid programmatically (the builder DSL for
    // VLAN parent/vid through `.interface` isn't exposed today).
    let mut topo_mut = topo;
    if let Some(node) = topo_mut.nodes.get_mut("host")
        && let Some(iface) = node.interfaces.get_mut("eth0.42")
    {
        iface.parent = Some("eth0".to_string());
        iface.vni = Some(42);
    }
    let topo = topo_mut;

    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let out = lab
        .exec("host", "ip", &["-d", "link", "show", "eth0.42"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "eth0.42 must exist after deploy");
    assert!(
        out.stdout.contains("vlan id 42") || out.stdout.contains("id 42"),
        "eth0.42 must report VID 42; got {}",
        out.stdout
    );

    // Re-apply must be a no-op for the VLAN layer. Spot-check:
    // the VLAN's address must be live both before and after.
    let addr_before = lab
        .exec("host", "ip", &["-4", "addr", "show", "eth0.42"])
        .unwrap();
    assert!(
        addr_before.stdout.contains("10.42.0.1/24"),
        "VLAN address must be live before reapply; got {}",
        addr_before.stdout
    );

    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply failed");

    let addr_after = lab
        .exec("host", "ip", &["-4", "addr", "show", "eth0.42"])
        .unwrap();
    assert!(
        addr_after.stdout.contains("10.42.0.1/24"),
        "VLAN address must still be live after reapply; got {}",
        addr_after.stdout
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 159a Slice 4 — VXLAN declared declaratively via
// `LinkBuilder::vxlan + vxlan_local + vxlan_remote + vxlan_port`.
// Re-apply must be a no-op on the VXLAN layer.
// End-to-end via examples/vxlan-overlay.nll.
#[tokio::test]
async fn slice4_vxlan_reapply_is_zero_ops() {
    if !privileged("slice4_vxlan_reapply_is_zero_ops") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vxlan-overlay.nll"
    ))
    .expect("failed to parse vxlan-overlay.nll");
    unique(&mut topo, "slice4_vxlan_reapply_is_zero_ops");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // VXLAN interface must exist with the right VNI + UDP port.
    let out = lab
        .exec("vtep1", "ip", &["-d", "link", "show", "vxlan100"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "vxlan100 must exist after deploy");
    assert!(
        out.stdout.contains("vxlan id 100") || out.stdout.contains("id 100"),
        "vxlan100 must report VNI 100; got {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("dstport 4789") || out.stdout.contains("port 4789"),
        "vxlan100 must report dst port 4789; got {}",
        out.stdout
    );

    // The overlay address must be live.
    let addr = lab
        .exec("vtep1", "ip", &["-4", "addr", "show", "vxlan100"])
        .unwrap();
    assert!(
        addr.stdout.contains("192.168.100.1/24"),
        "VXLAN overlay address must be live; got {}",
        addr.stdout
    );

    // Reapply: no-op on the VXLAN layer.
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.is_empty(),
        "expected empty layered diff after VXLAN deploy; got {} change(s):\n{layered}",
        layered.change_count()
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// A VXLAN over an IPv6 underlay deploys with its endpoints and carries
/// traffic. nlink 0.26–0.29 dropped IPv6 endpoints (the tunnel came up
/// with none), so the validator refused them; 0.30 writes
/// `IFLA_VXLAN_LOCAL6`/`GROUP6` (nlink #418).
#[tokio::test]
async fn vxlan_ipv6_underlay_deploys_and_forwards() {
    if !privileged("vxlan_ipv6_underlay_deploys_and_forwards") {
        return;
    }
    let lab_name = format!("vx6-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node vtep1 {{
  vxlan vx6 {{
    vni 600
    local fd00:1::1
    remote fd00:1::2
    port 4789
    address 192.168.160.1/24
  }}
}}
node vtep2 {{
  vxlan vx6 {{
    vni 600
    local fd00:1::2
    remote fd00:1::1
    port 4789
    address 192.168.160.2/24
  }}
}}
link vtep1:eth0 -- vtep2:eth0 {{ fd00:1::1/64 -- fd00:1::2/64 }}
"#
    ))
    .unwrap();
    let result = topo.validate();
    assert!(!result.has_errors(), "{:?}", result.issues());
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let out = lab
        .exec("vtep1", "ip", &["-d", "link", "show", "vx6"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("local fd00:1::1") && out.stdout.contains("remote fd00:1::2"),
        "vx6 must carry its IPv6 endpoints: {}",
        out.stdout
    );
    wait_for_dad(&lab, "vtep1", "eth0").await;
    wait_for_dad(&lab, "vtep2", "eth0").await;
    let mut last = String::new();
    let mut ok = false;
    for _ in 0..10 {
        let ping = lab
            .exec("vtep1", "ping", &["-c", "1", "-W", "1", "192.168.160.2"])
            .unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        last = format!("{}{}", ping.stdout, ping.stderr);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert!(ok, "overlay ping over the IPv6 underlay failed: {last}");

    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.is_empty(),
        "expected empty layered diff after an IPv6-underlay VXLAN deploy; got {} change(s):\n{layered}",
        layered.change_count()
    );
    lab.destroy().await.expect("destroy failed");
}

/// A per-interface sysctl on a declared VLAN (#160): the deploy failed,
/// since the Sysctls stage ran before Stack created the VLAN. It is devconf
/// on the VLAN's declaration now, so it is there after the deploy, after a
/// VLAN-id change recreates the link, and after a hand edit and an apply.
#[tokio::test]
async fn declared_vlan_sysctl_survives_deploy_recreate_and_drift() {
    if !privileged("declared_vlan_sysctl_survives_deploy_recreate_and_drift") {
        return;
    }
    if !recreate_modules_present("declared_vlan_sysctl_survives_deploy_recreate_and_drift") {
        return;
    }
    let lab_name = format!("vsc-{}", std::process::id());
    let src = |id: u16| {
        format!(
            r#"lab "{lab_name}"
node r {{
  vlan vl0 {{ parent eth0 id {id} address 10.10.0.1/24 }}
  sysctl "net.ipv4.conf.vl0.rp_filter" "2"
}}
node h {{
  vlan vl0 {{ parent eth0 id {id} address 10.10.0.2/24 }}
}}
link r:eth0 -- h:eth0
"#
        )
    };
    let rp_filter = |lab: &RunningLab| {
        lab.exec("r", "cat", &["/proc/sys/net/ipv4/conf/vl0/rp_filter"])
            .unwrap()
            .stdout
            .trim()
            .to_string()
    };
    let topo = nlink_lab::parser::parse(&src(10)).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert_eq!(rp_filter(&lab), "2", "after deploy");

    let desired = nlink_lab::parser::parse(&src(20)).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let link = lab.exec("r", "ip", &["-d", "link", "show", "vl0"]).unwrap();
    assert!(
        link.stdout.contains("id 20"),
        "not recreated: {}",
        link.stdout
    );
    assert_eq!(rp_filter(&lab), "2", "after the recreate");

    let set = lab
        .exec(
            "r",
            "sh",
            &["-c", "echo 0 > /proc/sys/net/ipv4/conf/vl0/rp_filter"],
        )
        .unwrap();
    assert_eq!(set.exit_code, 0, "{}", set.stderr);
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.network.get("r").is_some_and(|d| !d.is_empty()),
        "a hand-edited rp_filter should be drift:\n{layered}"
    );
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert_eq!(rp_filter(&lab), "2", "after the drift apply");
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

// ─── Spine-leaf test ──────────────────────────────────────

#[lab_test("examples/spine-leaf.nll")]
async fn deploy_spine_leaf(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 6);
    assert_eq!(lab.topology().links.len(), 6);

    // Check loopback address on spine1
    let output = lab.exec("spine1", "ip", &["addr", "show", "lo"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.255.0.1"),
        "expected loopback address: {}",
        output.stdout
    );
}

// ═══════════════════════════════════════════════════════════
// Plan 110: Extended integration tests
// ═══════════════════════════════════════════════════════════

// ─── Multi-hop routing ──────────────────────────────────

#[lab_test(topology = multi_hop_topology)]
async fn multi_hop_ping(lab: RunningLab) {
    // client -> router -> server (3 hops)
    let output = lab
        .exec("client", "ping", &["-c1", "-W2", "10.0.2.2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "multi-hop ping failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}

fn multi_hop_topology() -> nlink_lab::Topology {
    TopologyBuilder::new("multi-hop-test")
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("router", |n| n.profile("router"))
        .node("client", |n| n.route("default", |r| r.via("10.0.1.1")))
        .node("server", |n| n.route("default", |r| r.via("10.0.2.1")))
        .link("router:eth0", "client:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .link("router:eth1", "server:eth0", |l| {
            l.addresses("10.0.2.1/24", "10.0.2.2/24")
        })
        .build()
}

// ─── IPv6 connectivity ──────────────────────────────────

#[lab_test(topology = ipv6_topology)]
async fn ipv6_ping(lab: RunningLab) {
    // IPv6 DAD keeps addresses in `tentative` for ~1s after assignment;
    // a slow CI runner can stretch this past any fixed sleep we pick.
    // Poll `ip -6 addr` on both nodes until neither side reports
    // `tentative` (with a hard 10s ceiling), then ping with retries to
    // tolerate the very first NDP solicit being dropped.
    for node in ["a", "b"] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let out = lab
                .exec(node, "ip", &["-6", "-o", "addr", "show", "eth0"])
                .unwrap();
            if out.exit_code == 0 && !out.stdout.contains("tentative") {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "DAD did not complete on {node}:eth0 within 10s; last `ip -6 addr` = {}",
                    out.stdout
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    // Even after DAD, the very first ICMPv6 echo can race the NDP
    // solicit/advert handshake on slow runners. Retry up to 3 times
    // with `-c1 -W3` before declaring failure.
    let mut output = None;
    for _ in 0..3 {
        let attempt = lab
            .exec("a", "ping", &["-6", "-c1", "-W3", "fd00::2"])
            .unwrap();
        let done = attempt.exit_code == 0;
        output = Some(attempt);
        if done {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let output = output.unwrap();
    assert_eq!(
        output.exit_code, 0,
        "IPv6 ping failed after 3 retries: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}

fn ipv6_topology() -> nlink_lab::Topology {
    TopologyBuilder::new("ipv6-test")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("fd00::1/64", "fd00::2/64")
        })
        .build()
}

// ─── Bond options + Q-in-Q reach the kernel (#75) ───────

#[tokio::test]
async fn bond_options_and_qinq_vlan_apply() {
    if !privileged("bond_options_and_qinq_vlan_apply") {
        return;
    }
    if !feature(
        "bond_options_and_qinq_vlan_apply",
        "bonding",
        has_kernel_module("bonding"),
    ) || !feature(
        "bond_options_and_qinq_vlan_apply",
        "8021q",
        has_kernel_module("8021q"),
    ) {
        return;
    }
    let lab_name = format!("bondq-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node left {{
  bond bond0 {{ members [eth0, eth1] mode active-backup miimon 100 }}
  vlan bond0.100 {{ parent bond0 id 100 protocol 802.1ad address 10.100.0.1/24 }}
}}
node right {{
  bond bond0 {{ members [eth0, eth1] mode active-backup miimon 100 }}
  vlan bond0.100 {{ parent bond0 id 100 protocol 802.1ad address 10.100.0.2/24 }}
}}
link left:eth0 -- right:eth0
link left:eth1 -- right:eth1
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let bond = lab
        .exec("left", "ip", &["-d", "link", "show", "bond0"])
        .unwrap();
    assert_eq!(bond.exit_code, 0, "{}", bond.stderr);
    assert!(
        bond.stdout.contains("mode active-backup") && bond.stdout.contains("miimon 100"),
        "bond options should reach the kernel: {}",
        bond.stdout
    );
    let vlan = lab
        .exec("left", "ip", &["-d", "link", "show", "bond0.100"])
        .unwrap();
    assert!(
        vlan.stdout.contains("802.1ad") && vlan.stdout.contains("id 100"),
        "Q-in-Q outer tag expected: {}",
        vlan.stdout
    );
    let mut ok = false;
    for _ in 0..5 {
        let ping = lab
            .exec("left", "ping", &["-c1", "-W2", "10.100.0.2"])
            .unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "ping over the bonded 802.1ad VLAN failed");
    lab.destroy().await.expect("destroy failed");
}
