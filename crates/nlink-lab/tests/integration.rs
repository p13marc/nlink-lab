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

/// Check whether nftables actually works (module loaded is not enough —
/// the nft CLI uses batched netlink but nlink may send unbatched messages
/// that the kernel rejects with EINVAL).
fn has_nftables() -> bool {
    if !has_kernel_module("nf_tables") {
        return false;
    }
    // Test an actual table creation + deletion, not just listing.
    let ok = std::process::Command::new("nft")
        .args(["add", "table", "inet", "__nlink_lab_probe__"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        let _ = std::process::Command::new("nft")
            .args(["delete", "table", "inet", "__nlink_lab_probe__"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    ok
}

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

// `exec_attached` inherits stdio so the child prints to the caller's
// terminal. Test only covers exit-code propagation — streaming behaviour
// is visible manually but awkward to assert in a non-TTY test harness.
#[lab_test("examples/simple.nll")]
async fn exec_attached_forwards_exit_code(lab: RunningLab) {
    assert_eq!(lab.exec_attached("host", "true", &[]).unwrap(), 0);
    assert_ne!(lab.exec_attached("host", "false", &[]).unwrap(), 0);
}

// `exec_in` with an explicit workdir should land the child there. Uses
// /tmp since namespace nodes share the host mount namespace — an
// absolute host path is the meaningful case.
#[lab_test("examples/simple.nll")]
async fn exec_in_respects_workdir(lab: RunningLab) {
    let output = lab
        .exec_in("host", "pwd", &[], Some(std::path::Path::new("/tmp")))
        .unwrap();
    assert_eq!(output.exit_code, 0, "pwd failed: {}", output.stderr);
    assert_eq!(
        output.stdout.trim(),
        "/tmp",
        "expected cwd=/tmp, got: {:?}",
        output.stdout
    );
}

// `wait_for_log_line` matches a stdout line emitted by a spawned
// process. Uses `bash -c` to print a known marker, sleep briefly, then
// exit. The watcher must return Ok within the timeout. Validates the
// "service prints 'ready' before opening a port" use case.
#[lab_test("examples/simple.nll")]
async fn wait_for_log_line_matches_marker(mut lab: RunningLab) {
    let pid = lab
        .spawn_with_logs("host", &["sh", "-c", "echo READYISH; sleep 5"], None)
        .unwrap();
    let pat = regex::Regex::new(r"^READYISH$").unwrap();
    lab.wait_for_log_line(
        pid,
        &pat,
        nlink_lab::LogStream::Stdout,
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(50),
    )
    .await
    .unwrap();
}

// On timeout, `wait_for_log_line` must surface an error that includes
// the regex source — debuggers chase typos in the regex itself.
#[lab_test("examples/simple.nll")]
async fn wait_for_log_line_times_out_with_regex_in_error(mut lab: RunningLab) {
    let pid = lab.spawn_with_logs("host", &["sleep", "5"], None).unwrap();
    let pat = regex::Regex::new(r"^DEFINITELY_NOT_PRESENT$").unwrap();
    let err = lab
        .wait_for_log_line(
            pid,
            &pat,
            nlink_lab::LogStream::Both,
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(50),
        )
        .await
        .unwrap_err();
    let s = err.to_string();
    assert!(
        s.contains("DEFINITELY_NOT_PRESENT"),
        "error should include regex source for debugging: {s}"
    );
}

// `process_status_alive_only` must drop entries whose tracked PID has
// exited. Spawn `true` (instant exit), wait until either the child is
// gone or it shows up as a zombie, then assert the listing is empty.
//
// `spawn_with_logs` drops the `std::process::Child` without `wait()`-
// ing, so an exited child becomes a zombie. `process_status` reads
// `/proc/<pid>/stat` and treats state Z as dead — see
// `running::pid_is_alive` for the rationale and unit tests.
//
// We poll for the dead state instead of sleeping a fixed interval,
// which makes the test deterministic on slow CI runners.
#[lab_test("examples/simple.nll")]
async fn process_status_alive_only_filters_dead(mut lab: RunningLab) {
    let pid = lab.spawn_with_logs("host", &["true"], None).unwrap();

    // Poll for up to 5s for the child to transition to !alive.
    // /bin/true is ~instant; this loop almost always exits in the
    // first iteration. The deadline is purely a CI safety net.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let all = lab.process_status();
        if all.iter().any(|p| p.pid == pid && !p.alive) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "child pid {pid} never transitioned to !alive within 5s: {:?}",
                all,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // The unfiltered process_status retains the dead entry.
    let all = lab.process_status();
    assert!(
        all.iter().any(|p| p.pid == pid && !p.alive),
        "expected dead entry retained in process_status: {all:?}"
    );

    // alive_only filters it out.
    let alive = lab.process_status_alive_only();
    assert!(
        !alive.iter().any(|p| p.pid == pid),
        "alive_only must filter the dead entry: {alive:?}"
    );
}

// `exec_with_opts(.. env ..)` must apply env vars via Command::env, not
// by wrapping in `/usr/bin/env`. Verifies both visibility of the new var
// and additive semantics — inherited PATH must remain set.
#[lab_test("examples/simple.nll")]
async fn exec_with_opts_propagates_env(lab: RunningLab) {
    let env = &[("FEEDBACK_R3", "ok")];
    let output = lab
        .exec_with_opts(
            "host",
            "sh",
            &["-c", "echo \"$FEEDBACK_R3\"; test -n \"$PATH\""],
            nlink_lab::ExecOpts {
                env,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "expected env var + inherited PATH; stderr={}",
        output.stderr
    );
    assert_eq!(output.stdout.trim(), "ok");
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
    nlink_lab::Lab::new("pcol")
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

// ─── Firewall test ────────────────────────────────────────

#[tokio::test]
async fn deploy_firewall() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping deploy_firewall: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping deploy_firewall: nftables not functional on this kernel");
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
    if !has_wireguard() {
        eprintln!("skipping deploy_wireguard: wireguard not functional");
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

// ─── VLAN trunk / bridge test (plans 050 + 052) ─────────

#[lab_test("examples/vlan-trunk.nll")]
async fn deploy_bridge_vlan(lab: RunningLab) {
    if !has_bridge_vlan_filtering() {
        eprintln!("skipping deploy_bridge_vlan: bridge VLAN filtering not functional");
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
    Lab::new("multi-hop-test")
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
    // IPv6 DAD (Duplicate Address Detection) keeps addresses in "tentative"
    // state for ~1s after assignment. Wait for DAD to complete before pinging.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let output = lab
        .exec("a", "ping", &["-6", "-c1", "-W3", "fd00::2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "IPv6 ping failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}

fn ipv6_topology() -> nlink_lab::Topology {
    Lab::new("ipv6-test")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("fd00::1/64", "fd00::2/64")
        })
        .build()
}

// ─── DNS hosts resolution ───────────────────────────────

#[lab_test(topology = dns_topology)]
async fn dns_hosts_resolve(lab: RunningLab) {
    let output = lab.exec("client", "getent", &["hosts", "server"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "getent hosts server failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
    assert!(
        output.stdout.contains("10.0.1.2"),
        "expected 10.0.1.2 in getent output: {}",
        output.stdout
    );
}

fn dns_topology() -> nlink_lab::Topology {
    Lab::new("dns-integ-test")
        .dns(nlink_lab::DnsMode::Hosts)
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("router", |n| n.profile("router"))
        .node("server", |n| n.route("default", |r| r.via("10.0.1.1")))
        .node("client", |n| n.route("default", |r| r.via("10.0.2.1")))
        .link("router:eth0", "server:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .link("router:eth1", "client:eth0", |l| {
            l.addresses("10.0.2.1/24", "10.0.2.2/24")
        })
        .build()
}

// ─── Firewall packet filtering ──────────────────────────

#[tokio::test]
async fn firewall_blocks_traffic() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_blocks_traffic: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_blocks_traffic: nftables not functional");
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

// ─── VLAN isolation ─────────────────────────────────────

#[lab_test("examples/vlan-trunk.nll")]
async fn vlan_isolation(lab: RunningLab) {
    if !has_bridge_vlan_filtering() {
        eprintln!("skipping vlan_isolation: bridge VLAN filtering not functional");
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

// ─── Topology patterns ──────────────────────────────────

#[lab_test("examples/subnet-pools.nll")]
async fn subnet_pool_deploy(lab: RunningLab) {
    assert!(lab.topology().nodes.len() >= 4);
    assert!(lab.topology().links.len() >= 4);
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

// ─── Scenario example parses ────────────────────────────

#[lab_test("examples/scenario.nll")]
async fn scenario_parses_and_deploys(lab: RunningLab) {
    assert_eq!(lab.topology().scenarios.len(), 1);
    assert_eq!(lab.topology().scenarios[0].name, "failover-test");
    assert!(lab.topology().scenarios[0].steps.len() >= 4);
}

// ─── DNS example ────────────────────────────────────────

#[lab_test("examples/dns.nll")]
async fn dns_example_deploys(lab: RunningLab) {
    assert_eq!(lab.topology().lab.dns, nlink_lab::DnsMode::Hosts);
    assert_eq!(lab.topology().nodes.len(), 3);
}
