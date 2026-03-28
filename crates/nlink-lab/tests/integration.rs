//! Integration tests for nlink-lab.
//!
//! These tests deploy real network namespaces and require root or CAP_NET_ADMIN.
//! They are automatically skipped when run as a non-root user.
//!
//! Run with: `sudo cargo test -p nlink-lab --test integration`

use nlink_lab::lab_test;
#[allow(unused_imports)]
use nlink_lab::RunningLab;

// ─── File-based tests ─────────────────────────────────────

#[lab_test("examples/simple.toml")]
async fn deploy_simple_toml(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 2);
    assert_eq!(lab.topology().links.len(), 1);
}

#[lab_test("examples/simple.nll")]
async fn deploy_simple_nll(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 2);
    assert_eq!(lab.topology().links.len(), 1);
}

#[lab_test("examples/simple.toml")]
async fn exec_ip_addr(lab: RunningLab) {
    let output = lab.exec("router", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.0.0.1/24"),
        "expected 10.0.0.1/24 in output: {}",
        output.stdout
    );
}

#[lab_test("examples/simple.toml")]
async fn exec_ip_route(lab: RunningLab) {
    let output = lab.exec("host", "ip", &["route", "show"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("default via 10.0.0.1"),
        "expected default route in output: {}",
        output.stdout
    );
}

#[lab_test("examples/simple.toml")]
async fn exec_ping(lab: RunningLab) {
    let output = lab
        .exec("host", "ping", &["-c1", "-W1", "10.0.0.1"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "ping failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}

#[lab_test("examples/simple.toml")]
async fn sysctl_forwarding(lab: RunningLab) {
    let output = lab
        .exec("router", "cat", &["/proc/sys/net/ipv4/ip_forward"])
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.trim(), "1");
}

#[lab_test("examples/simple.toml")]
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

#[lab_test("examples/simple.toml")]
async fn exit_code_forwarded(lab: RunningLab) {
    let output = lab.exec("host", "false", &[]).unwrap();
    assert_ne!(output.exit_code, 0);
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

// ─── Firewall test ────────────────────────────────────────

#[lab_test("examples/firewall.toml")]
async fn deploy_firewall(lab: RunningLab) {
    let output = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("filter") || output.stdout.contains("nlink"),
        "expected nftables rules in output: {}",
        output.stdout
    );
}

// ─── Spine-leaf test ──────────────────────────────────────

#[lab_test("examples/spine-leaf.toml")]
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

// ─── State persistence test ───────────────────────────────

#[lab_test("examples/simple.toml")]
async fn state_persistence(lab: RunningLab) {
    let name = lab.name().to_string();
    assert!(nlink_lab::state::exists(&name));

    // Load from state and verify
    let loaded = nlink_lab::RunningLab::load(&name).unwrap();
    assert_eq!(loaded.namespace_count(), lab.namespace_count());
}

// ─── VRF test (plan 050) ─────────────────────────────────

#[lab_test("examples/vrf-multitenant.toml")]
async fn deploy_vrf(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 3);

    // VRF "red" interface should exist on PE
    let output = lab.exec("pe", "ip", &["link", "show", "red"]).unwrap();
    assert_eq!(output.exit_code, 0, "VRF 'red' not found: {}", output.stderr);

    // VRF "blue" interface should exist on PE
    let output = lab.exec("pe", "ip", &["link", "show", "blue"]).unwrap();
    assert_eq!(output.exit_code, 0, "VRF 'blue' not found: {}", output.stderr);

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
}

// ─── WireGuard test (plan 050) ───────────────────────────

#[lab_test("examples/wireguard-vpn.toml")]
async fn deploy_wireguard(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 4);

    // wg0 interface should exist on both gateways
    let output = lab.exec("gw-a", "ip", &["link", "show", "wg0"]).unwrap();
    assert_eq!(output.exit_code, 0, "wg0 not found on gw-a: {}", output.stderr);

    let output = lab.exec("gw-b", "ip", &["link", "show", "wg0"]).unwrap();
    assert_eq!(output.exit_code, 0, "wg0 not found on gw-b: {}", output.stderr);

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
    let output = lab
        .exec("gw-a", "ping", &["-c1", "-W2", "192.168.255.2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "WireGuard tunnel not working: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}

// ─── VLAN trunk / bridge test (plans 050 + 052) ─────────

#[lab_test("examples/vlan-trunk.toml")]
async fn deploy_bridge_vlan(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 4);

    // host1 should have its address
    let output = lab
        .exec("host1", "ip", &["addr", "show", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("10.100.0.10/24"),
        "expected 10.100.0.10/24 on host1: {}",
        output.stdout
    );

    // host1 and host2 are on the same VLAN 100 — they should reach each other
    let output = lab
        .exec("host1", "ping", &["-c1", "-W1", "10.100.0.20"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "host1 cannot reach host2 on VLAN 100: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // host3 is on VLAN 200 — verify its address
    let output = lab
        .exec("host3", "ip", &["addr", "show", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("10.200.0.10/24"),
        "expected 10.200.0.10/24 on host3: {}",
        output.stdout
    );
}
