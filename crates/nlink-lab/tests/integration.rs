//! Integration tests for nlink-lab.
//!
//! These tests deploy real network namespaces and require root or CAP_NET_ADMIN.
//! They are automatically skipped when run as a non-root user.
//!
//! Run with: `sudo cargo test -p nlink-lab --test integration`

use nlink_lab::lab_test;
#[allow(unused_imports)]
use nlink_lab::{Lab, RunningLab};

/// Check whether a kernel module is available (loaded or loadable).
fn has_kernel_module(name: &str) -> bool {
    // Check if already loaded
    if let Ok(modules) = std::fs::read_to_string("/proc/modules")
        && modules.lines().any(|l| l.starts_with(name))
    {
        return true;
    }
    // Try to load it
    std::process::Command::new("modprobe")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ─── File-based tests ─────────────────────────────────────

#[lab_test("examples/simple.nll")]
async fn deploy_simple(lab: RunningLab) {
    assert_eq!(lab.topology().nodes.len(), 2);
    assert_eq!(lab.topology().links.len(), 1);
}

#[lab_test("examples/simple.nll")]
async fn exec_ip_addr(lab: RunningLab) {
    let output = lab.exec("router", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.0.0.1/24"),
        "expected 10.0.0.1/24 in output: {}",
        output.stdout
    );
}

#[lab_test("examples/simple.nll")]
async fn exec_ip_route(lab: RunningLab) {
    let output = lab.exec("host", "ip", &["route", "show"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("default via 10.0.0.1"),
        "expected default route in output: {}",
        output.stdout
    );
}

#[lab_test("examples/simple.nll")]
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

#[lab_test("examples/simple.nll")]
async fn sysctl_forwarding(lab: RunningLab) {
    let output = lab
        .exec("router", "cat", &["/proc/sys/net/ipv4/ip_forward"])
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.trim(), "1");
}

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

#[lab_test("examples/simple.nll")]
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

#[tokio::test]
async fn deploy_firewall() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping deploy_firewall: requires root");
        return;
    }
    if !has_kernel_module("nf_tables") {
        eprintln!("skipping deploy_firewall: nf_tables kernel module not available");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
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

// ─── State persistence test ───────────────────────────────

#[lab_test("examples/simple.nll")]
async fn state_persistence(lab: RunningLab) {
    let name = lab.name().to_string();
    assert!(nlink_lab::state::exists(&name));

    // Load from state and verify
    let loaded = nlink_lab::RunningLab::load(&name).unwrap();
    assert_eq!(loaded.namespace_count(), lab.namespace_count());
}

// ─── VRF test (plan 050) ─────────────────────────────────

#[tokio::test]
async fn deploy_vrf() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping deploy_vrf: requires root");
        return;
    }
    if !has_kernel_module("vrf") {
        eprintln!("skipping deploy_vrf: vrf kernel module not available");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse topology file");
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
    if !has_kernel_module("wireguard") {
        eprintln!("skipping deploy_wireguard: wireguard kernel module not available");
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

#[lab_test("examples/vlan-trunk.nll")]
async fn deploy_bridge_vlan(lab: RunningLab) {
    if !has_kernel_module("8021q") {
        eprintln!("skipping deploy_bridge_vlan: 8021q kernel module not available");
        return;
    }
    assert_eq!(lab.topology().nodes.len(), 4);

    // host1 should have its address
    let output = lab.exec("host1", "ip", &["addr", "show", "eth0"]).unwrap();
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
    let output = lab.exec("host3", "ip", &["addr", "show", "eth0"]).unwrap();
    assert!(
        output.stdout.contains("10.200.0.10/24"),
        "expected 10.200.0.10/24 on host3: {}",
        output.stdout
    );
}

// ─── apply_diff tests ────────────────────────────────────

/// Helper: deploy a topology and return the running lab, with a panic-safe cleanup guard.
/// Returns (lab, guard) — forget the guard after destroy.
struct LabCleanup {
    name: String,
}
impl Drop for LabCleanup {
    fn drop(&mut self) {
        let prefix = format!("{}-", self.name);
        if let Ok(output) = std::process::Command::new("ip")
            .args(["netns", "list"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let ns = line.split_whitespace().next().unwrap_or("");
                if ns.starts_with(&prefix) {
                    let _ = std::process::Command::new("ip")
                        .args(["netns", "delete", ns])
                        .status();
                }
            }
        }
        let _ = nlink_lab::state::remove(&self.name);
    }
}

#[tokio::test]
async fn apply_add_node_and_link() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_add_node_and_link: requires root");
        return;
    }

    let lab_name = format!("apply-add-{}", std::process::id());

    // Initial topology: two nodes, one link
    let initial = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build();

    let mut lab = initial.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Desired topology: add node c and link b--c
    let desired = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .node("c", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .link("b:eth1", "c:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .build();

    let diff = nlink_lab::diff_topologies(lab.topology(), &desired);
    assert_eq!(diff.nodes_added, vec!["c"]);
    assert_eq!(diff.links_added.len(), 1);

    nlink_lab::apply_diff(&mut lab, &desired, &diff)
        .await
        .expect("apply_diff failed");

    // Verify: node c exists and has the right address
    let output = lab.exec("c", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.0.1.2/24"),
        "expected 10.0.1.2/24 on c:eth0: {}",
        output.stdout
    );

    // Verify: b can ping c
    let output = lab.exec("b", "ping", &["-c1", "-W1", "10.0.1.2"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "b cannot ping c: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // Clean up
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

#[tokio::test]
async fn apply_remove_node() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_remove_node: requires root");
        return;
    }

    let lab_name = format!("apply-rm-{}", std::process::id());

    // Initial: three nodes
    let initial = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .node("c", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .link("b:eth1", "c:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .build();

    let mut lab = initial.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Desired: remove node c and its link
    let desired = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build();

    let diff = nlink_lab::diff_topologies(lab.topology(), &desired);
    assert_eq!(diff.nodes_removed, vec!["c"]);
    assert_eq!(diff.links_removed.len(), 1);

    nlink_lab::apply_diff(&mut lab, &desired, &diff)
        .await
        .expect("apply_diff failed");

    // Verify: node c's namespace no longer exists
    assert!(
        lab.exec("c", "ip", &["addr"]).is_err(),
        "node c should no longer exist"
    );

    // Verify: a and b still work
    let output = lab.exec("a", "ping", &["-c1", "-W1", "10.0.0.2"]).unwrap();
    assert_eq!(output.exit_code, 0);

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

#[tokio::test]
async fn apply_impairment_change() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_impairment_change: requires root");
        return;
    }

    let lab_name = format!("apply-imp-{}", std::process::id());

    // Initial: link with 10ms delay
    let initial = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .impair("a:eth0", |i| i.delay("10ms"))
        .build();

    let mut lab = initial.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Desired: change delay to 50ms
    let desired = Lab::new(&lab_name)
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .impair("a:eth0", |i| i.delay("50ms"))
        .build();

    let diff = nlink_lab::diff_topologies(lab.topology(), &desired);
    assert_eq!(diff.impairments_changed.len(), 1);

    nlink_lab::apply_diff(&mut lab, &desired, &diff)
        .await
        .expect("apply_diff failed");

    // Verify: netem shows updated delay
    let output = lab
        .exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("50"),
        "expected 50ms delay in netem output: {}",
        output.stdout
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}
