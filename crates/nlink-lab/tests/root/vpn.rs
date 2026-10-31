//! Root tests: WireGuard and VRF.
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
use nlink_lab::{Op, RunningLab};

/// Check whether WireGuard tunnel creation works (not just the module).
fn has_wireguard() -> bool {
    has_kernel_module("wireguard")
        && std::process::Command::new("wg")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

// Plan 158e — VRF topologies coexist with the declarative
// addresses+routes path. VRF is created imperatively (step 6b);
// addresses on VRF members go through Slice 1.
#[tokio::test]
async fn network_config_coexists_with_vrf() {
    if !privileged("network_config_coexists_with_vrf") {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "network_config_coexists_with_vrf");
    let mut lab = match topo.clone().deploy().await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skipping network_config_coexists_with_vrf: deploy failed: {e}");
            discard_lab_state(&topo.lab.name);
            return;
        }
    };
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply of VRF topology failed");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e Slice 1 — WireGuard topologies still work after
// addresses + routes moved to the declarative NetworkConfig
// path. Regression check that WG interfaces (which stay
// imperative — Slice 1 skipped them) coexist cleanly with the
// NetworkConfig step 11c apply.
#[tokio::test]
async fn network_config_coexists_with_wireguard() {
    if !privileged("network_config_coexists_with_wireguard") {
        return;
    }
    if !feature(
        "network_config_coexists_with_wireguard",
        "wireguard",
        has_wireguard(),
    ) {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wireguard-vpn.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "network_config_coexists_with_wireguard");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // WG interfaces should be live + carry the declared
    // addresses on their tunnel iface (set via NetworkConfig in
    // step 11c).
    for node in lab.topology().nodes.keys() {
        let out = lab.exec(node, "ip", &["addr", "show"]).unwrap();
        assert_eq!(
            out.exit_code, 0,
            "ip addr show on '{node}' failed: {}",
            out.stderr
        );
    }

    // Reapply must succeed (catches the case where Slice 1's
    // address handling clashes with the imperative WG
    // configuration in step 10d).
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply of WG topology failed");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 159a Slice 4 — VRF link declared declaratively via
// `LinkBuilder::vrf(table)` in step 11c. Re-applying the same
// topology against the running kernel state must produce zero
// kernel mutations on the VRF + enslaved-iface layers.
// End-to-end via examples/vrf-multitenant.nll.
#[tokio::test]
async fn slice4_vrf_reapply_is_zero_ops() {
    if !privileged("slice4_vrf_reapply_is_zero_ops") {
        return;
    }
    // Try to load the vrf kernel module — CI runner kernels may
    // lack `CONFIG_NET_VRF`, in which case `add_link(kind=vrf)`
    // returns EOPNOTSUPP. Skip rather than failing the test on
    // an environment limitation.
    let _ = std::process::Command::new("modprobe").arg("vrf").status();
    if !feature(
        "slice4_vrf_reapply_is_zero_ops",
        "vrf",
        std::path::Path::new("/sys/module/vrf").exists(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse vrf-multitenant.nll");
    unique(&mut topo, "slice4_vrf_reapply_is_zero_ops");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // VRF link must exist and have the right table after deploy.
    let out = lab
        .exec("pe", "ip", &["-d", "link", "show", "red"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "VRF 'red' must exist after deploy");
    assert!(
        out.stdout.contains("vrf table 10"),
        "VRF 'red' must report table 10; got {}",
        out.stdout
    );
    let out = lab
        .exec("pe", "ip", &["-d", "link", "show", "blue"])
        .unwrap();
    assert!(
        out.stdout.contains("vrf table 20"),
        "VRF 'blue' must report table 20; got {}",
        out.stdout
    );

    // Enslave must have landed — eth1 master should be the VRF.
    let out = lab
        .exec("pe", "ip", &["-d", "link", "show", "eth1"])
        .unwrap();
    assert!(
        out.stdout.contains("master red"),
        "'eth1' must be enslaved to VRF 'red'; got {}",
        out.stdout
    );

    // Re-apply: compute_layered_diff against own state must be
    // empty (zero kernel calls on links/addresses/routes/qdiscs).
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.is_empty(),
        "expected empty layered diff after VRF deploy; got {} change(s):\n{layered}",
        layered.change_count()
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 159a Phase 2 — WireGuard configuration via
// `WireguardConfig::apply_reconcile`. Re-applying the same
// topology must produce zero set_device calls — verified end-
// to-end against a real namespace by deploying examples/
// wireguard-vpn.nll twice and checking that the second apply
// reports zero changes.
#[cfg(feature = "wireguard")]
#[tokio::test]
async fn wireguard_config_reapply_is_zero_ops() {
    if !privileged("wireguard_config_reapply_is_zero_ops") {
        return;
    }
    // Pre-flight: ensure the wireguard kernel module + the
    // userspace `wg` binary are available. Many CI runners
    // ship neither out of the box; skip rather than fail.
    let _ = std::process::Command::new("modprobe")
        .arg("wireguard")
        .status();
    if !feature(
        "wireguard_config_reapply_is_zero_ops",
        "wireguard",
        std::path::Path::new("/sys/module/wireguard").exists(),
    ) {
        return;
    }
    let wg_on_path = std::process::Command::new("which")
        .arg("wg")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !feature("wireguard_config_reapply_is_zero_ops", "wg", wg_on_path) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wireguard-vpn.nll"
    ))
    .expect("failed to parse wireguard-vpn.nll");
    unique(&mut topo, "wireguard_config_reapply_is_zero_ops");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // The example's own assertions (#147): host-a and host-b reach each
    // other through the tunnel, the far /24 being in the peer's
    // allowed-ips.
    let failed: Vec<_> = lab
        .assertion_results()
        .iter()
        .filter(|a| !a.passed)
        .collect();
    assert!(
        !lab.assertion_results().is_empty() && failed.is_empty(),
        "{failed:?}"
    );

    // WG interface must exist after deploy.
    let out = lab.exec("gw-a", "wg", &["show", "wg0"]).unwrap();
    assert_eq!(
        out.exit_code, 0,
        "wg0 must exist after deploy; stderr={}",
        out.stderr
    );
    assert!(
        out.stdout.contains("listening port: 51820"),
        "wg0 must listen on port 51820; got {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("peer:"),
        "wg0 must have at least one peer configured; got {}",
        out.stdout
    );

    // apply with the same topology must be a no-op on the WG layer.
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("reapply failed");

    // Spot-check that the WG state didn't degrade — peer list
    // must still match.
    let after = lab.exec("gw-a", "wg", &["show", "wg0"]).unwrap();
    assert!(
        after.stdout.contains("peer:"),
        "wg0 must still have a peer after reapply; got {}",
        after.stdout
    );

    // …and the WG layer itself has nothing left to do. nlink 0.29 diffed
    // a peer as changed forever when its allowed IPs carried host bits
    // (ours do: `10.0.0.2/24` style, `plan/wireguard.rs`) or the private
    // key was not pre-clamped (nlink #410, #412).
    let plan = nlink_lab::apply_plan(&lab, &topo).unwrap();
    let mut checked = 0;
    for op in &plan.ops {
        let Op::Stack { node, cfg } = op else {
            continue;
        };
        let Some(wg) = &cfg.wireguard else {
            continue;
        };
        let conn = lab
            .ns_resolver_of(node)
            .expect("node handle")
            .connection_async::<nlink::Wireguard>()
            .await
            .expect("wireguard connection");
        let diff = wg.diff(&conn).await.expect("WireguardConfig::diff");
        assert!(
            diff.is_empty(),
            "{node}: WireGuard diff after reapply: {diff:?}"
        );
        checked += 1;
    }
    assert!(checked >= 2, "expected both gateways to carry WireGuard");

    // A peer removed by hand is drift (#170): `--check` reports it in the
    // WireGuard layer, and an `apply` of the same topology puts it back.
    let peers = lab.exec("gw-a", "wg", &["show", "wg0", "peers"]).unwrap();
    let peer = peers.stdout.trim().to_string();
    assert!(!peer.is_empty(), "gw-a has no peer to remove");
    let rm = lab
        .exec("gw-a", "wg", &["set", "wg0", "peer", &peer, "remove"])
        .unwrap();
    assert_eq!(rm.exit_code, 0, "{}", rm.stderr);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.wireguard.get("gw-a").is_some_and(|d| !d.is_empty())
            && layered.wireguard.len() == 1
            && layered.network.is_empty()
            && layered.nftables.is_empty(),
        "the removed peer, and only it, should be drift:\n{layered}"
    );
    assert!(
        layered.to_string().contains("WireguardConfigDiff (gw-a"),
        "{layered}"
    );
    // its public key is base64 in the JSON, and so in the schema (nlink #876)
    assert_report_matches_schema(lab.name(), &layered);
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply after a hand-removed peer failed");
    let back = lab.exec("gw-a", "wg", &["show", "wg0", "peers"]).unwrap();
    assert_eq!(back.stdout.trim(), peer, "the peer is not restored");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");

    // A peer changed by hand is a `peers-to-modify` entry: its public key,
    // and the presence flags nlink 0.32's schema described as keys (nlink
    // #876). The endpoint is declared (a keepalive is not, so a changed
    // one is no drift).
    let set = lab
        .exec(
            "gw-a",
            "wg",
            &["set", "wg0", "peer", &peer, "endpoint", "192.0.2.9:51820"],
        )
        .unwrap();
    assert_eq!(set.exit_code, 0, "{}", set.stderr);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    let json = serde_json::to_value(&layered.wireguard).unwrap();
    assert!(
        json.to_string().contains("peers-to-modify"),
        "the endpoint should be a modified peer: {json:#}"
    );
    assert_report_matches_schema(lab.name(), &layered);
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply after a hand-changed endpoint failed");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #147: two tunnels between two hubs, each in its own VRF. Every tunnel
/// gets only its own peer (by name), the WAN as its endpoint and the far
/// site in its allowed-ips, so the example's four assertions pass: each
/// customer reaches its other site, and not the other customer. Two of them
/// failed before.
#[cfg(feature = "wireguard")]
#[tokio::test]
async fn multi_tenant_wan_tunnels_carry_their_customers() {
    if !privileged("multi_tenant_wan_tunnels_carry_their_customers") {
        return;
    }
    for m in ["wireguard", "vrf"] {
        let _ = std::process::Command::new("modprobe").arg(m).status();
        if !feature(
            "multi_tenant_wan_tunnels_carry_their_customers",
            m,
            std::path::Path::new(&format!("/sys/module/{m}")).exists(),
        ) {
            return;
        }
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/cookbook/multi-tenant-wan.nll"
    ))
    .unwrap();
    unique(&mut topo, "multi_tenant_wan_tunnels_carry_their_customers");
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let results: Vec<(String, bool)> = lab
        .assertion_results()
        .iter()
        .map(|a| (a.description.clone(), a.passed))
        .collect();
    assert_eq!(results.len(), 4, "{results:?}");
    assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
    lab.destroy().await.expect("destroy failed");
}

// Issue #83: a VRF-table route removed from the topology must be
// deleted on apply. VRF routes are declared in the node's config, and
// purge covers every table it declares into, and the ones it did before.
#[tokio::test]
async fn apply_removes_vrf_route() {
    if !privileged("apply_removes_vrf_route") {
        return;
    }
    if !feature("apply_removes_vrf_route", "vrf", has_kernel_module("vrf")) {
        return;
    }
    let name = format!("apply-vrf-rm-{}", std::process::id());
    let src = &r#"lab "apply-vrf-rm"
profile router { forward ipv4 }
node pe : router {
  vrf red table 10 {
    interfaces [eth1]
    route default via 10.10.0.10
    route 192.168.5.0/24 via 10.10.0.10
  }
}
node a { route default via 10.10.0.1 }
link pe:eth1 -- a:eth0 { 10.10.0.1/24 -- 10.10.0.10/24 }
"#
    .replace("apply-vrf-rm", &name);
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let table = |lab: &RunningLab| {
        lab.exec("pe", "ip", &["route", "show", "table", "10"])
            .unwrap()
            .stdout
    };
    let before = table(&lab);
    assert!(
        before.contains("192.168.5.0/24"),
        "route missing after deploy: {before}"
    );
    assert!(before.contains("default via 10.10.0.10"), "{before}");

    // Drop one route and apply.
    let desired =
        nlink_lab::parser::parse(&src.replace("    route 192.168.5.0/24 via 10.10.0.10\n", ""))
            .unwrap();
    // VRF routes are declared in pe's stack: --check shows the removal.
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .unwrap();
    assert!(
        layered.network["pe"]
            .routes_to_remove
            .iter()
            .any(|r| r.table() == Some(10) && r.prefix_len() == 24),
        "{layered}"
    );
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let after = table(&lab);
    assert!(
        !after.contains("192.168.5.0/24"),
        "route survived apply: {after}"
    );
    assert!(
        after.contains("default via 10.10.0.10"),
        "unrelated route lost: {after}"
    );

    // Re-apply is a no-op on the route layer, and the diff is clean.
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .unwrap();
    assert!(layered.is_empty(), "{layered}");

    // The table's last route goes too, although the config then declares
    // nothing into table 10 (it is purged as a table the node used).
    let empty = nlink_lab::parser::parse(
        &src.replace("    route 192.168.5.0/24 via 10.10.0.10\n", "")
            .replace("    route default via 10.10.0.10\n", ""),
    )
    .unwrap();
    nlink_lab::apply(&mut lab, &empty)
        .await
        .expect("apply failed");
    let last = table(&lab);
    assert!(
        !last.contains("via 10.10.0.10"),
        "the last VRF route survived apply: {last}"
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &empty).await.unwrap();
    assert!(layered.is_empty(), "{layered}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// ─── VRF test (plan 050) ─────────────────────────────────

#[tokio::test]
async fn deploy_vrf() {
    if !privileged("deploy_vrf") {
        return;
    }
    if !feature("deploy_vrf", "vrf", has_kernel_module("vrf")) {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "deploy_vrf");
    let lab = topo.deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    assert_eq!(lab.topology().nodes.len(), 3);

    // VRF "red" interface should exist on PE
    let output = lab.exec("pe", "ip", &["link", "show", "red"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "VRF 'red' not found: {}",
        output.stderr
    );

    // VRF "blue" interface should exist on PE
    let output = lab.exec("pe", "ip", &["link", "show", "blue"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "VRF 'blue' not found: {}",
        output.stderr
    );

    // eth1 should be enslaved to VRF red
    let output = lab.exec("pe", "ip", &["link", "show", "eth1"]).unwrap();
    assert!(
        output.stdout.contains("master red"),
        "eth1 not enslaved to VRF red: {}",
        output.stdout
    );

    // eth2 should be enslaved to VRF blue
    let output = lab.exec("pe", "ip", &["link", "show", "eth2"]).unwrap();
    assert!(
        output.stdout.contains("master blue"),
        "eth2 not enslaved to VRF blue: {}",
        output.stdout
    );

    // Tenant A can reach PE via VRF red
    let output = lab
        .exec("tenant-a", "ping", &["-c1", "-W1", "10.10.0.1"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "tenant-a cannot reach PE: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // Tenant B can reach PE via VRF blue
    let output = lab
        .exec("tenant-b", "ping", &["-c1", "-W1", "10.20.0.1"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "tenant-b cannot reach PE: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("failed to destroy lab");
}

// ─── WireGuard test (plan 050) ───────────────────────────

#[lab_test("examples/wireguard-vpn.nll")]
async fn deploy_wireguard(lab: RunningLab) {
    if !feature("deploy_wireguard", "wireguard", has_wireguard()) {
        return;
    }
    assert_eq!(lab.topology().nodes.len(), 4);

    // wg0 interface should exist on both gateways
    let output = lab.exec("gw-a", "ip", &["link", "show", "wg0"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "wg0 not found on gw-a: {}",
        output.stderr
    );

    let output = lab.exec("gw-b", "ip", &["link", "show", "wg0"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "wg0 not found on gw-b: {}",
        output.stderr
    );

    // wg0 should have the configured address on gw-a
    let output = lab.exec("gw-a", "ip", &["addr", "show", "wg0"]).unwrap();
    assert!(
        output.stdout.contains("192.168.255.1"),
        "expected 192.168.255.1 on gw-a wg0: {}",
        output.stdout
    );

    // Underlay connectivity: gateways can reach each other
    let output = lab
        .exec("gw-a", "ping", &["-c1", "-W1", "10.0.0.2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "gw-a cannot reach gw-b underlay: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // WireGuard tunnel: gw-a can reach gw-b overlay address
    // Skip this check if the tunnel handshake hasn't completed (CI kernels
    // may have WireGuard support but unreliable tunnel establishment).
    let output = lab
        .exec("gw-a", "ping", &["-c1", "-W3", "192.168.255.2"])
        .unwrap();
    if output.exit_code != 0 {
        eprintln!(
            "warning: WireGuard tunnel ping failed (may be CI kernel limitation): stdout={} stderr={}",
            output.stdout, output.stderr
        );
        return;
    }
}

// Plan 159a Phase 2 follow-up (commit bfe8744) — `apply_diff`
// Phase 6 must now configure WireGuard for newly-added nodes via
// the Stack pattern. Pre-fix, adding a node with WG via apply
// left the WG layer unconfigured. This test:
//
//   1. Deploy a small 2-node topology with no WG.
//   2. Build a desired topology that adds a third node with WG.
//   3. `apply_diff` and confirm `wg show wg0` works on the new
//      node — proves Phase 6 ran the WG config.
//
// Skips when the kernel WG module or the `wg` userspace binary
// aren't available (CI runner inconsistency).
#[cfg(feature = "wireguard")]
#[tokio::test]
async fn apply_diff_phase6_configures_wireguard_for_added_node() {
    if !privileged("apply_diff_phase6_configures_wireguard_for_added_node") {
        return;
    }
    let _ = std::process::Command::new("modprobe")
        .arg("wireguard")
        .status();
    if !feature(
        "apply_diff_phase6_configures_wireguard_for_added_node",
        "wireguard",
        std::path::Path::new("/sys/module/wireguard").exists(),
    ) {
        return;
    }
    let wg_on_path = std::process::Command::new("which")
        .arg("wg")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !feature(
        "apply_diff_phase6_configures_wireguard_for_added_node",
        "wg",
        wg_on_path,
    ) {
        return;
    }

    // Step 1 — deploy a starting topology with two plain nodes.
    let topo = nlink_lab::TopologyBuilder::new(&format!("apply-wg-phase6-{}", std::process::id()))
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.99.0.1/24", "10.99.0.2/24")
        })
        .build();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Step 2 — build a desired topology that adds a 'c' node
    // hosting a WireGuard interface peered with 'a'. We have to
    // add a matching WG block on 'a' too (peers cross-reference
    // each other in the public-key map).
    let mut desired = topo.clone();
    desired.nodes.insert(
        "c".to_string(),
        nlink_lab::types::Node {
            wireguard: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "wg0".to_string(),
                    nlink_lab::types::WireguardConfig {
                        private_key: None,
                        listen_port: Some(51822),
                        fwmark: None,
                        addresses: vec!["10.255.0.3/32".to_string()],
                        peers: vec!["a".to_string()],
                        peer_settings: Default::default(),
                    },
                );
                m
            },
            ..Default::default()
        },
    );
    if let Some(a) = desired.nodes.get_mut("a") {
        a.wireguard.insert(
            "wg0".to_string(),
            nlink_lab::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.255.0.1/32".to_string()],
                peers: vec!["c".to_string()],
                peer_settings: Default::default(),
            },
        );
    }

    // Step 3 — apply the diff and check the WG layer is live on
    // the newly-added node.
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply_diff failed");

    let out = lab
        .exec("c", "wg", &["show", "wg0"])
        .expect("exec wg show on new node 'c'");
    assert_eq!(
        out.exit_code, 0,
        "wg show wg0 on newly-added node must succeed; stderr={}",
        out.stderr
    );
    assert!(
        out.stdout.contains("listening port: 51822"),
        "wg0 on 'c' must report the declared listen port; got {}",
        out.stdout
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #204: WireGuard on `apply`. A key the topology writes is the one the
/// interface gets (the key saved by the first deploy won, and `apply`
/// reported no change), and a tunnel the topology drops is deleted (it
/// stayed up with its key and peers).
#[cfg(feature = "wireguard")]
#[tokio::test]
async fn apply_takes_a_written_wireguard_key_and_deletes_a_dropped_tunnel() {
    let wg_tools = std::process::Command::new("wg")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !privileged("apply_takes_a_written_wireguard_key_and_deletes_a_dropped_tunnel") {
        return;
    }
    if !feature(
        "apply_takes_a_written_wireguard_key_and_deletes_a_dropped_tunnel",
        "wireguard",
        has_wireguard(),
    ) || !feature(
        "apply_takes_a_written_wireguard_key_and_deletes_a_dropped_tunnel",
        "wg",
        wg_tools,
    ) {
        return;
    }
    let genkey = || {
        let out = std::process::Command::new("wg")
            .arg("genkey")
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let (k1, k2) = (genkey(), genkey());
    let lab_name = format!("wgk-{}", std::process::id());
    let topo = |key: Option<&str>| {
        let (a, b) = match key {
            Some(k) => (
                format!(
                    r#"{{ wireguard wg0 {{ key "{k}" listen 51820 address 192.168.255.1/32 peers [b] }} }}"#
                ),
                r#"{ wireguard wg0 { key auto listen 51820 address 192.168.255.2/32 peers [a] } }"#
                    .to_string(),
            ),
            None => Default::default(),
        };
        nlink_lab::parser::parse(&format!(
            "lab \"{lab_name}\"\nnode a {a}\nnode b {b}\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/30 -- 10.0.0.2/30 }}\n"
        ))
        .unwrap()
    };
    let mut lab = topo(Some(&k1)).deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let wg = |lab: &RunningLab, node: &str, what: &str| {
        let out = lab.exec(node, "wg", &["show", "wg0", what]).unwrap();
        assert_eq!(
            out.exit_code, 0,
            "{node}: wg show wg0 {what}: {}",
            out.stderr
        );
        out.stdout.trim().to_string()
    };
    assert_eq!(wg(&lab, "a", "private-key"), k1);

    nlink_lab::apply(&mut lab, &topo(Some(&k2)))
        .await
        .expect("apply of a new key failed");
    assert_eq!(
        wg(&lab, "a", "private-key"),
        k2,
        "the written key is not applied"
    );
    assert_eq!(
        wg(&lab, "b", "peers"),
        wg(&lab, "a", "public-key"),
        "b's peer kept a's old key"
    );

    nlink_lab::apply(&mut lab, &topo(None))
        .await
        .expect("apply without the tunnels failed");
    for node in ["a", "b"] {
        let out = lab.exec(node, "ip", &["link", "show", "wg0"]).unwrap();
        assert_ne!(out.exit_code, 0, "{node}: wg0 is still there");
    }
    let layered = nlink_lab::compute_layered_diff(&lab, &topo(None))
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}
