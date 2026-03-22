//! Integration tests for nlink-lab.
//!
//! These tests require root or CAP_NET_ADMIN. They create real network
//! namespaces, interfaces, and routes. Skipped automatically when not root.
//!
//! Run with: `sudo cargo test -p nlink-lab --test integration`

use nlink_lab::builder::Lab;
use nlink_lab::RunningLab;

/// Skip test if not running as root or if namespace operations are blocked (sandbox).
macro_rules! require_root {
    () => {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: requires root or CAP_NET_ADMIN");
            return;
        }
        // Check if setns actually works (may be blocked by seccomp/sandbox)
        if !can_use_namespaces() {
            eprintln!("skipping: namespace operations blocked (sandboxed environment)");
            return;
        }
    };
}

/// Test if namespace operations are functional (not just root, but setns allowed).
fn can_use_namespaces() -> bool {
    use std::sync::OnceLock;
    static RESULT: OnceLock<bool> = OnceLock::new();
    *RESULT.get_or_init(|| {
        let name = format!("nlink-probe-{}", std::process::id());
        if nlink::netlink::namespace::create(&name).is_err() {
            return false;
        }
        let ok = nlink::netlink::namespace::connection_for::<nlink::Route>(&name).is_ok();
        let _ = nlink::netlink::namespace::delete(&name);
        ok
    })
}

/// Generate a unique lab name to avoid conflicts in parallel test runs.
fn unique_name(base: &str) -> String {
    format!("{}-{}", base, std::process::id())
}

fn minimal_topology(name: &str) -> nlink_lab::Topology {
    Lab::new(name)
        .node("a", |n| n.route("default", |r| r.via("10.0.0.2")))
        .node("b", |n| n.route("default", |r| r.via("10.0.0.1")))
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build()
}

// ── Deploy / Destroy ───────────────────────────────────

#[tokio::test]
async fn deploy_minimal() {
    require_root!();
    let name = unique_name("t-min");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();
    assert_eq!(lab.namespace_count(), 2);

    // Verify namespaces exist
    let ns_a = topo.namespace_name("a");
    let ns_b = topo.namespace_name("b");
    assert!(nlink::netlink::namespace::exists(&ns_a));
    assert!(nlink::netlink::namespace::exists(&ns_b));

    lab.destroy().await.unwrap();

    // Verify namespaces cleaned up
    assert!(!nlink::netlink::namespace::exists(&ns_a));
    assert!(!nlink::netlink::namespace::exists(&ns_b));
}

#[tokio::test]
async fn deploy_already_exists() {
    require_root!();
    let name = unique_name("t-dup");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    // Second deploy should fail
    let result = topo.deploy().await;
    assert!(result.is_err());

    lab.destroy().await.unwrap();
}

// ── Exec ───────────────────────────────────────────────

#[tokio::test]
async fn exec_ip_addr() {
    require_root!();
    let name = unique_name("t-addr");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    let output = lab.exec("a", "ip", &["-4", "addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.0.0.1/24"),
        "expected 10.0.0.1/24 in: {}",
        output.stdout
    );

    let output = lab.exec("b", "ip", &["-4", "addr", "show", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("10.0.0.2/24"),
        "expected 10.0.0.2/24 in: {}",
        output.stdout
    );

    lab.destroy().await.unwrap();
}

#[tokio::test]
async fn exec_ip_route() {
    require_root!();
    let name = unique_name("t-route");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    let output = lab.exec("a", "ip", &["route", "show"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("default via 10.0.0.2"),
        "expected default route in: {}",
        output.stdout
    );

    lab.destroy().await.unwrap();
}

#[tokio::test]
async fn exec_ping() {
    require_root!();
    let name = unique_name("t-ping");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    // Ping from a to b
    let output = lab
        .exec("a", "ping", &["-c", "1", "-W", "2", "10.0.0.2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "ping failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // Ping from b to a
    let output = lab
        .exec("b", "ping", &["-c", "1", "-W", "2", "10.0.0.1"])
        .unwrap();
    assert_eq!(output.exit_code, 0);

    lab.destroy().await.unwrap();
}

#[tokio::test]
async fn exec_exit_code() {
    require_root!();
    let name = unique_name("t-exit");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    let output = lab.exec("a", "false", &[]).unwrap();
    assert_ne!(output.exit_code, 0);

    let output = lab.exec("a", "true", &[]).unwrap();
    assert_eq!(output.exit_code, 0);

    lab.destroy().await.unwrap();
}

#[tokio::test]
async fn exec_node_not_found() {
    require_root!();
    let name = unique_name("t-nf");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    let result = lab.exec("nonexistent", "true", &[]);
    assert!(result.is_err());

    lab.destroy().await.unwrap();
}

// ── Sysctls ────────────────────────────────────────────

#[tokio::test]
async fn deploy_with_sysctls() {
    require_root!();
    let name = unique_name("t-sysctl");
    let topo = Lab::new(&name)
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("r1", |n| n.profile("router"))
        .node("h1", |n| n)
        .link("r1:eth0", "h1:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build();

    let lab = topo.deploy().await.unwrap();

    let output = lab
        .exec("r1", "cat", &["/proc/sys/net/ipv4/ip_forward"])
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.trim(), "1");

    // h1 should have default (0 or not set via profile)
    let output = lab
        .exec("h1", "cat", &["/proc/sys/net/ipv4/ip_forward"])
        .unwrap();
    // Default in a new namespace is 0
    assert_eq!(output.stdout.trim(), "0");

    lab.destroy().await.unwrap();
}

// ── Netem ──────────────────────────────────────────────

#[tokio::test]
async fn deploy_with_netem() {
    require_root!();
    let name = unique_name("t-netem");
    let topo = Lab::new(&name)
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .impair("a:eth0", |i| i.delay("50ms").loss("1%"))
        .build();

    let lab = topo.deploy().await.unwrap();

    let output = lab.exec("a", "tc", &["qdisc", "show", "dev", "eth0"]).unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        output.stdout.contains("netem"),
        "expected netem qdisc in: {}",
        output.stdout
    );

    lab.destroy().await.unwrap();
}

// ── State persistence ──────────────────────────────────

#[tokio::test]
async fn state_persistence() {
    require_root!();
    let name = unique_name("t-state");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();

    // State should exist
    assert!(nlink_lab::state::exists(&name));

    // Load from state
    let loaded = RunningLab::load(&name).unwrap();
    assert_eq!(loaded.namespace_count(), 2);

    // Exec via loaded lab should work
    let output = loaded.exec("a", "true", &[]).unwrap();
    assert_eq!(output.exit_code, 0);

    lab.destroy().await.unwrap();

    // State should be gone
    assert!(!nlink_lab::state::exists(&name));
}

// ── Rollback ───────────────────────────────────────────

#[tokio::test]
async fn destroy_idempotent() {
    require_root!();
    let name = unique_name("t-idem");
    let topo = minimal_topology(&name);

    let lab = topo.deploy().await.unwrap();
    let ns_a = topo.namespace_name("a");

    lab.destroy().await.unwrap();
    assert!(!nlink::netlink::namespace::exists(&ns_a));

    // Second destroy via load should fail gracefully (state already removed)
    let result = RunningLab::load(&name);
    assert!(result.is_err());
}
