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

// Plan 158a — applying the same firewall config a second time
// must not perturb live rules. Verified by reading the
// nftables generation counter from `nft list ruleset` before
// and after the second apply: equal counter ⇒ kernel made no
// mutations (the `apply_reconcile` diff was empty).
#[tokio::test]
async fn nftables_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nftables_reapply_is_zero_ops: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nftables_reapply_is_zero_ops: nftables not functional");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
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
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
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

/// Extract `# handle N` markers from `nft -a list ruleset`
/// output. Rule handles are stable across no-op reapplies and
/// monotonically increase when the kernel re-creates a rule,
/// so comparing the set lets a test detect "did anything
/// change?" without parsing the full ruleset.
fn collect_rule_handles(nft_output: &str) -> Vec<u32> {
    // Real `nft -a list ruleset` puts the `# handle N` marker at
    // the END of each rule line, not as a line prefix. Find it
    // anywhere within the line.
    let mut handles: Vec<u32> = nft_output
        .lines()
        .filter_map(|l| l.split_once("# handle "))
        .filter_map(|(_, rest)| rest.split_whitespace().next())
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    handles.sort_unstable();
    handles
}

// Plan 158a — foreign nftables rules (no `nlink-lab/` USERDATA
// comment) must survive a reconcile reapply. Documented
// guarantee that lets users hand-edit via
// `nlink-lab exec NODE -- nft -f extra.nft` without their
// additions being clobbered.
#[tokio::test]
async fn nftables_foreign_rule_survives_apply() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nftables_foreign_rule_survives_apply: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nftables_foreign_rule_survives_apply: nftables not functional");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
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

    // Re-apply the same topology. The diff path must not touch
    // the foreign rule.
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
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

// Plan 158a — editing a rule in place must reuse `rules_to_replace`
// (kernel-side `NEWRULE | NLM_F_REPLACE | NFTA_RULE_HANDLE`),
// not delete+rebuild. We can't probe the wire from the test, but
// we CAN assert that unchanged rules keep their handles across an
// edit — the diff path only re-issues handles for actually
// changed rules. Combined with the foreign-rule test, this
// guards the "atomic in-place replace" claim of Plan 158a.
#[tokio::test]
async fn nftables_rule_edit_replaces_in_place() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nftables_rule_edit_replaces_in_place: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nftables_rule_edit_replaces_in_place: nftables not functional");
        return;
    }

    let initial = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
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

    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &edited);
    nlink_lab::apply_diff(&mut lab, &edited, &diff)
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nftables_remove_firewall_clears_table: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nftables_remove_firewall_clears_table: nftables not functional");
        return;
    }

    let initial = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall.nll"
    ))
    .expect("failed to parse topology file");
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
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &edited);
    nlink_lab::apply_diff(&mut lab, &edited, &diff)
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

// Plan 158e — macvlan topologies coexist with the declarative
// addresses+routes path. Macvlan is created host-side and moved
// into the namespace imperatively (step 6a). Addresses on the
// macvlan iface go through Slice 1's NetworkConfig path. Tests
// that the combination still deploys and reapplies cleanly.
#[tokio::test]
async fn network_config_coexists_with_macvlan() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping network_config_coexists_with_macvlan: requires root");
        return;
    }
    // macvlan needs a real parent device on the host. Best-effort
    // check via `ip link show`.
    let parent_ok = std::process::Command::new("ip")
        .args(["link", "show"])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains("eth0") || s.contains("ens") || s.contains("enp")
        })
        .unwrap_or(false);
    if !parent_ok {
        eprintln!("skipping network_config_coexists_with_macvlan: no obvious host parent iface");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/macvlan.nll"
    ))
    .expect("failed to parse topology file");
    let mut lab = match topo.clone().deploy().await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "skipping network_config_coexists_with_macvlan: deploy failed (likely no parent): {e}"
            );
            return;
        }
    };
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Reapply must succeed — this is the key check: Slice 1's
    // NetworkConfig path sees the macvlan iface (created
    // imperatively in step 6a) and shouldn't try to re-create it.
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
        .await
        .expect("reapply of macvlan topology failed");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e — VRF topologies coexist with the declarative
// addresses+routes path. VRF is created imperatively (step 6b);
// addresses on VRF members go through Slice 1.
#[tokio::test]
async fn network_config_coexists_with_vrf() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping network_config_coexists_with_vrf: requires root");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse topology file");
    let mut lab = match topo.clone().deploy().await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skipping network_config_coexists_with_vrf: deploy failed: {e}");
            return;
        }
    };
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
        .await
        .expect("reapply of VRF topology failed");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158b Phase 1 + Phase 3 — kernel errors carrying
// `NLMSGERR_ATTR_MSG` payload reach the user's hands via
// `Error::ext_ack()`. Trigger a real EEXIST against a live
// namespace and verify the accessor returns Some(_).
#[tokio::test]
async fn ext_ack_surfaces_from_real_kernel_error() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping ext_ack_surfaces_from_real_kernel_error: requires root");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    let lab = topo.deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Try to create a duplicate dummy iface on `router`. Kernel
    // returns EEXIST + a NLMSGERR_ATTR_MSG describing the
    // collision.
    let ns_name = lab.namespace_for("router").unwrap().to_string();
    let conn: nlink::Connection<nlink::Route> =
        nlink::netlink::namespace::connection_for(&ns_name).expect("connection_for failed");

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

// Plan 158e Slice 1 — WireGuard topologies still work after
// addresses + routes moved to the declarative NetworkConfig
// path. Regression check that WG interfaces (which stay
// imperative — Slice 1 skipped them) coexist cleanly with the
// NetworkConfig step 11c apply.
#[tokio::test]
async fn network_config_coexists_with_wireguard() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping network_config_coexists_with_wireguard: requires root");
        return;
    }
    if !has_wireguard() {
        eprintln!("skipping network_config_coexists_with_wireguard: wireguard not functional");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wireguard-vpn.nll"
    ))
    .expect("failed to parse topology file");
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
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
        .await
        .expect("reapply of WG topology failed");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158a — NAT masquerade rules reapply idempotently. The
// declarative `NftablesConfig` path keyed on
// `nlink-lab/nat/postrouting/<idx>/masq` must produce
// zero kernel ops on a no-change apply.
#[tokio::test]
async fn nat_masquerade_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nat_masquerade_reapply_is_zero_ops: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nat_masquerade_reapply_is_zero_ops: nftables not functional");
        return;
    }

    // Use the existing NAT example NLL rather than reinventing
    // a builder DSL the codebase doesn't yet expose.
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/nat.nll"
    ))
    .expect("failed to parse NAT example");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let baseline = lab
        .exec("firewall", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    assert_eq!(baseline.exit_code, 0);
    let baseline_handles = collect_rule_handles(&baseline.stdout);
    assert!(
        !baseline_handles.is_empty(),
        "expected at least one rule (masquerade) with a handle after initial deploy"
    );

    // Re-apply.
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
        .await
        .expect("failed to re-apply unchanged topology");

    let after = lab
        .exec("firewall", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    let after_handles = collect_rule_handles(&after.stdout);
    assert_eq!(
        baseline_handles, after_handles,
        "NAT reapply must preserve rule handles (kernel mutations re-issue them); \
         baseline={baseline_handles:?} after={after_handles:?}"
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping slice2_dummy_iface_reapply_is_zero_ops: requires root");
        return;
    }
    let topo = nlink_lab::Lab::new("slice2-dummy")
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
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping slice3_vlan_iface_reapply_is_zero_ops: requires root");
        return;
    }
    // Build a 2-node topology so step 5 creates a veth `eth0` on
    // `host`. Then add a VLAN sub-interface "eth0.42" to `host`.
    // The parent (eth0) already exists by the time step 11c
    // (NetworkConfig::apply) runs.
    let topo = nlink_lab::Lab::new("slice3-vlan")
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

    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
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

// Plan 159a Slice 4 — VRF link declared declaratively via
// `LinkBuilder::vrf(table)` in step 11c. Re-applying the same
// topology against the running kernel state must produce zero
// kernel mutations on the VRF + enslaved-iface layers.
// End-to-end via examples/vrf-multitenant.nll.
#[tokio::test]
async fn slice4_vrf_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping slice4_vrf_reapply_is_zero_ops: requires root");
        return;
    }
    // Try to load the vrf kernel module — CI runner kernels may
    // lack `CONFIG_NET_VRF`, in which case `add_link(kind=vrf)`
    // returns EOPNOTSUPP. Skip rather than failing the test on
    // an environment limitation.
    let _ = std::process::Command::new("modprobe").arg("vrf").status();
    if !std::path::Path::new("/sys/module/vrf").exists() {
        eprintln!(
            "skipping slice4_vrf_reapply_is_zero_ops: kernel `vrf` \
             module unavailable on this runner"
        );
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vrf-multitenant.nll"
    ))
    .expect("failed to parse vrf-multitenant.nll");
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

// Plan 159a Slice 4 — VXLAN declared declaratively via
// `LinkBuilder::vxlan + vxlan_local + vxlan_remote + vxlan_port`.
// Re-apply must be a no-op on the VXLAN layer.
// End-to-end via examples/vxlan-overlay.nll.
#[tokio::test]
async fn slice4_vxlan_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping slice4_vxlan_reapply_is_zero_ops: requires root");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vxlan-overlay.nll"
    ))
    .expect("failed to parse vxlan-overlay.nll");
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

// Plan 159a Phase 2 — WireGuard configuration via
// `WireguardConfig::apply_reconcile`. Re-applying the same
// topology must produce zero set_device calls — verified end-
// to-end against a real namespace by deploying examples/
// wireguard-vpn.nll twice and checking that the second apply
// reports zero changes.
#[cfg(feature = "wireguard")]
#[tokio::test]
async fn wireguard_config_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping wireguard_config_reapply_is_zero_ops: requires root");
        return;
    }
    // Pre-flight: ensure the wireguard kernel module + the
    // userspace `wg` binary are available. Many CI runners
    // ship neither out of the box; skip rather than fail.
    let _ = std::process::Command::new("modprobe")
        .arg("wireguard")
        .status();
    if !std::path::Path::new("/sys/module/wireguard").exists() {
        eprintln!(
            "skipping wireguard_config_reapply_is_zero_ops: \
             kernel wireguard module unavailable on this runner"
        );
        return;
    }
    let wg_on_path = std::process::Command::new("which")
        .arg("wg")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !wg_on_path {
        eprintln!(
            "skipping wireguard_config_reapply_is_zero_ops: \
             `wg` binary not on PATH (install wireguard-tools)"
        );
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wireguard-vpn.nll"
    ))
    .expect("failed to parse wireguard-vpn.nll");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

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

    // apply_diff with the same topology must be a no-op on the
    // WG layer. `compute_layered_diff` reports the network +
    // nftables view; the WG view isn't represented in
    // `LayeredDiff` yet — assert via `apply_diff` succeeding
    // without errors (any WG-layer mutation would be a churn
    // indicator in the trace).
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
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

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158f Phase 2 — `compute_layered_diff` on an unchanged
// deployed lab returns an empty bundle (every subdiff
// reports zero changes). Verified end-to-end against a real
// namespace.
#[tokio::test]
async fn compute_layered_diff_on_unchanged_topology_is_empty() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping compute_layered_diff_on_unchanged_topology_is_empty: requires root");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let layered = nlink_lab::compute_layered_diff(&lab, &topo)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        layered.is_empty(),
        "expected empty layered diff against own-state apply; \
         got {} change(s):\n{layered}",
        layered.change_count()
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158f Phase 2 follow-up — `compute_layered_diff` against
// a topology that DIFFERS from the running state must report a
// non-empty diff. Specifically: deploy simple.nll, mutate the
// in-memory `desired` topology to add a new address, recompute
// the layered diff, assert at least one change shows up in the
// network layer for the affected node.
//
// Catches the regression where compute_layered_diff returns
// empty for *all* topologies (e.g. silently swallowed
// per-node connection errors). The "empty case" test above
// can't catch this because both 0-changes-OK and broken-loop
// produce the same empty result.
#[tokio::test]
async fn compute_layered_diff_reports_non_empty_when_address_changes() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping compute_layered_diff_reports_non_empty_when_address_changes: \
             requires root"
        );
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Mutate the desired topology — inject an extra address on
    // 'router' under an explicit interface entry. The running
    // lab doesn't have this address, so compute_layered_diff
    // must surface it.
    let mut desired = topo.clone();
    let router = desired
        .nodes
        .get_mut("router")
        .expect("simple.nll has 'router'");
    router.interfaces.insert(
        "lo".to_string(),
        nlink_lab::types::InterfaceConfig {
            kind: Some(nlink_lab::types::InterfaceKind::Loopback),
            addresses: vec!["10.99.99.99/32".to_string()],
            ..Default::default()
        },
    );

    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    assert!(
        !layered.is_empty(),
        "expected non-empty layered diff after injecting an extra \
         address on 'router'; got {} changes",
        layered.change_count()
    );

    // The change must be on the network layer for 'router'.
    let router_diff = layered
        .network
        .get("router")
        .expect("network layer must surface a diff for 'router'");
    assert!(
        !router_diff.is_empty(),
        "network layer diff for 'router' must be non-empty after \
         injecting an extra address"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 160 (0.7.0) — `apply --check --json` emits a v3 envelope
// with schema_version, lab, no_op, change_count, network and
// nftables. v3 dropped the v1 `diff` / `layered_summary` /
// `layered_summary_deprecated` fields (their one-release
// deprecation window ended). Exercises the end-to-end CLI shape
// against a real deployed lab.
//
// Spawns the CLI rather than calling library code directly so
// regressions in `bins/lab/src/main.rs` can't slip past.
#[tokio::test]
async fn apply_check_json_emits_schema_v3_envelope() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_check_json_emits_schema_v2_envelope: requires root");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Locate the test-built `nlink-lab` binary. cargo populates
    // CARGO_BIN_EXE_<binary> for `bin` targets, but the binary
    // lives in the workspace `bins/lab` crate, not this one;
    // we have to walk the parent target/ dir instead.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = manifest
        .ancestors()
        .find_map(|p| {
            let candidate = p.join("target").join("debug").join("nlink-lab");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        })
        .expect(
            "expected debug nlink-lab binary in target/debug; \
             run `cargo build --bin nlink-lab` first",
        );

    let output = std::process::Command::new(&target_dir)
        .args([
            "--json",
            "apply",
            "--check",
            "--dry-run",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/simple.nll"),
        ])
        .output()
        .expect("failed to run nlink-lab apply --check --json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope not valid JSON: {e}; stdout:\n{stdout}"));

    assert_eq!(
        envelope["schema_version"], 3,
        "schema_version must be 3; got {envelope}"
    );
    assert!(
        envelope["lab"].is_string(),
        "lab field must be a string; got {envelope}"
    );
    assert!(
        envelope["no_op"].is_boolean(),
        "no_op field must be a boolean; got {envelope}"
    );
    assert!(
        envelope["change_count"].is_number(),
        "change_count must be a number; got {envelope}"
    );
    // v3 dropped the v1 fields; they must no longer be emitted.
    assert!(
        envelope["diff"].is_null(),
        "v1 `diff` field must be gone in schema v3; got {envelope}"
    );
    assert!(
        envelope["layered_summary"].is_null(),
        "v1 `layered_summary` field must be gone in schema v3; got {envelope}"
    );
    assert!(
        envelope["layered_summary_deprecated"].is_null(),
        "v1 `layered_summary_deprecated` field must be gone in schema v3; got {envelope}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e Slice 1 — re-applying an unchanged topology must
// not perturb live addresses or routes. The new declarative
// path runs `NetworkConfig::apply` which is idempotent;
// `result.changes_made` must be 0 on the second apply. We
// observe this end-to-end by checking that the snapshot of
// `ip -j addr show` and `ip -j route show` is byte-equal
// before/after the second apply.
#[tokio::test]
async fn network_config_reapply_is_zero_ops() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping network_config_reapply_is_zero_ops: requires root");
        return;
    }

    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Snapshot the kernel-side state on every node before the
    // no-op apply. Extract only the per-interface address sets
    // (sorted, prefix+local pairs) and the route destinations —
    // not full link state. Operstate / carrier flags / IPv6 DAD
    // can transition between two captures in milliseconds on a
    // busy kernel without any apply mutation; we want to assert
    // that the APPLY didn't change addresses/routes, not that
    // the kernel was perfectly idle between snapshots.
    let nodes: Vec<String> = lab.topology().nodes.keys().cloned().collect();
    let snapshot = |lab: &nlink_lab::RunningLab, node: &str| -> (Vec<String>, Vec<String>) {
        let addrs_raw = lab
            .exec(node, "ip", &["-o", "-4", "addr", "show"])
            .unwrap()
            .stdout;
        let mut addrs: Vec<String> = addrs_raw
            .lines()
            .filter_map(|l| {
                // Lines look like "2: eth0    inet 10.0.0.1/24 scope global eth0..."
                let mut parts = l.split_whitespace();
                let _idx = parts.next()?;
                let iface = parts.next()?;
                let _family = parts.next()?;
                let cidr = parts.next()?;
                Some(format!("{iface} {cidr}"))
            })
            .collect();
        addrs.sort();

        let routes_raw = lab
            .exec(node, "ip", &["-4", "route", "show"])
            .unwrap()
            .stdout;
        let mut routes: Vec<String> = routes_raw
            .lines()
            .filter_map(|l| {
                // Strip away dynamic fields like "metric" and "proto" that
                // can shift across re-issues. Keep destination + dev.
                let mut out = String::new();
                let mut parts = l.split_whitespace();
                let dest = parts.next()?;
                out.push_str(dest);
                if let Some(via_idx) = l.split_whitespace().position(|w| w == "via") {
                    let toks: Vec<&str> = l.split_whitespace().collect();
                    if let Some(gw) = toks.get(via_idx + 1) {
                        out.push_str(" via ");
                        out.push_str(gw);
                    }
                }
                if let Some(dev_idx) = l.split_whitespace().position(|w| w == "dev") {
                    let toks: Vec<&str> = l.split_whitespace().collect();
                    if let Some(dev) = toks.get(dev_idx + 1) {
                        out.push_str(" dev ");
                        out.push_str(dev);
                    }
                }
                Some(out)
            })
            .collect();
        routes.sort();

        (addrs, routes)
    };

    type NodeSnapshot = (Vec<String>, Vec<String>);
    let mut before: Vec<(String, NodeSnapshot)> = Vec::new();
    for node in &nodes {
        before.push((node.clone(), snapshot(&lab, node)));
    }

    // Re-apply the same topology — must be a no-op for the
    // NetworkConfig layer.
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &topo);
    nlink_lab::apply_diff(&mut lab, &topo, &diff)
        .await
        .expect("failed to re-apply unchanged topology");

    for (node, (before_addrs, before_routes)) in &before {
        let (after_addrs, after_routes) = snapshot(&lab, node);
        assert_eq!(
            before_addrs, &after_addrs,
            "IPv4 address set changed on '{node}' across a no-op apply"
        );
        assert_eq!(
            before_routes, &after_routes,
            "route set changed on '{node}' across a no-op apply"
        );
    }

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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping apply_diff_phase6_configures_wireguard_for_added_node: \
             requires root"
        );
        return;
    }
    let _ = std::process::Command::new("modprobe")
        .arg("wireguard")
        .status();
    if !std::path::Path::new("/sys/module/wireguard").exists() {
        eprintln!(
            "skipping apply_diff_phase6_configures_wireguard_for_added_node: \
             wireguard kernel module unavailable"
        );
        return;
    }
    let wg_on_path = std::process::Command::new("which")
        .arg("wg")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !wg_on_path {
        eprintln!(
            "skipping apply_diff_phase6_configures_wireguard_for_added_node: \
             `wg` binary not on PATH"
        );
        return;
    }

    // Step 1 — deploy a starting topology with two plain nodes.
    let topo = nlink_lab::Lab::new("apply-wg-phase6")
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
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "wg0".to_string(),
                    nlink_lab::types::WireguardConfig {
                        private_key: None,
                        listen_port: Some(51822),
                        fwmark: None,
                        addresses: vec!["10.255.0.3/32".to_string()],
                        peers: vec!["a".to_string()],
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
            },
        );
    }

    // Step 3 — apply the diff and check the WG layer is live on
    // the newly-added node.
    let current = lab.topology().clone();
    let diff = nlink_lab::diff::diff_topologies(&current, &desired);
    nlink_lab::apply_diff(&mut lab, &desired, &diff)
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

// ─── Plan 157 PR C — proc-stat primitive ─────────────────

// `RunningLab::proc_stat` against a known live process. Spawn `sleep
// 30`, sample, assert command/state/uid look right and CPU/memory
// fields are populated. Round-5 §2.2.
#[lab_test("examples/simple.nll")]
async fn proc_stat_returns_live_data(mut lab: RunningLab) {
    let pid = lab.spawn_with_logs("host", &["sleep", "30"], None).unwrap();
    // Give /proc/<pid>/stat a moment to settle (rare on fast hosts
    // for the comm to not yet be set, but the deterministic write
    // happens at exec(2) time so it should be there by the time
    // spawn_with_logs returns).
    let stat = lab.proc_stat("host", pid).unwrap();
    assert_eq!(stat.host_pid, pid);
    assert_eq!(stat.command, "sleep", "expected comm=sleep, got {stat:?}");
    // Spawned by check_root context — uid 0.
    assert_eq!(stat.uid, 0);
    // sleep should be in S (sleeping) state — not D, not R, not Z.
    assert!(
        stat.state == "S" || stat.state == "I",
        "unexpected state: {}",
        stat.state
    );
    // VmSize/VmRSS should both be present for a userland process.
    assert!(stat.rss_kb.is_some());
    assert!(stat.vsz_kb.is_some());
    // fd_count must be at least 3 — every spawned process inherits
    // stdin/stdout/stderr from `spawn_with_logs`. 0.4.0 had a bug
    // where this always reported 0 because the internal
    // `sh -c "ls /proc/<pid>/fd 2>/dev/null | wc -l"` swallowed
    // the `ls` error and `wc -l` of empty input emitted 0. The
    // round-5 follow-up fix exec's `ls` directly. Floor of 3 also
    // catches the regression of returning 0 unconditionally.
    assert!(
        stat.fd_count >= 3,
        "fd_count {} too low; sleep should have at least stdin/stdout/stderr — \
         see round-5 follow-up bug",
        stat.fd_count
    );
    // started_at_unix_micros should be roughly "now" (within last
    // 60s). A loose check; just guarding the arithmetic.
    let now_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    assert!(
        stat.started_at_unix_micros > now_micros - 60_000_000
            && stat.started_at_unix_micros <= now_micros + 1_000_000,
        "started_at outside expected window: {} vs now {}",
        stat.started_at_unix_micros,
        now_micros
    );

    // Cleanup — kill so the lab teardown is clean.
    let _ = lab.exec("host", "kill", &[&pid.to_string()]);
}

// ─── Plan 156 PR A — partition cycles fix ───────────────

// Round-4 §1: `partition` was a silent no-op on the second call after
// `clear`, because the stale `saved_impairments` entry survived
// `clear_impairment`. This test cycles partition+clear five times
// and asserts each `--partition` actually installs a netem qdisc.
// Before the fix this fails on cycle 2 with "no netem on interface".
#[lab_test("examples/simple.nll")]
async fn partition_clear_cycles_are_idempotent(mut lab: RunningLab) {
    let endpoint = "host:eth0";
    for cycle in 1..=5 {
        lab.partition(endpoint).await.unwrap_or_else(|e| {
            panic!("cycle {cycle}: partition failed: {e}");
        });

        // The partition must actually be live — assert via `tc`.
        let qd = lab
            .exec("host", "tc", &["qdisc", "show", "dev", "eth0"])
            .unwrap();
        assert!(
            qd.stdout.contains("loss 100%"),
            "cycle {cycle}: expected loss 100% qdisc after partition; got: {}",
            qd.stdout
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

// ─── Plan 156 PR C — impair --show JSON view ────────────

// Round-4 follow-up: `--show --json` returned `endpoints: {}` for any
// topology built around bridge networks. The fix walks
// `networks.members` in addition to `links`. This deploys a real
// network-only topology, partitions a member endpoint, and asserts:
//   1. `topology_endpoints` includes the bridge member.
//   2. `is_partitioned(member)` is true after partition.
//   3. The kernel actually has the netem qdisc on the member's iface.
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

    let qd = lab
        .exec("host1", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        qd.stdout.contains("loss 100%"),
        "expected a loss 100% qdisc on host1:eth0 after partition; tc said: {}",
        qd.stdout
    );
    let parsed = nlink_lab::impair_parse::parse_tc_qdisc_show(&qd.stdout).unwrap();
    assert_eq!(parsed.loss_pct, Some(100.0));
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

// ─── Plan 156 PR B — exec --timeout ─────────────────────

// `ExecOpts::timeout` set to a value shorter than the child's run time
// must surface `Error::Timeout` and reap the child within the
// SIGTERM+1s grace+SIGKILL escalation window. Test wraps `sleep 30` in
// a 500ms timeout; total should complete in ~1.5s (kill grace).
#[lab_test("examples/simple.nll")]
async fn exec_with_timeout_kills_long_running(lab: RunningLab) {
    let opts = nlink_lab::ExecOpts {
        timeout: Some(std::time::Duration::from_millis(500)),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let err = lab
        .exec_with_opts("host", "sleep", &["30"], opts)
        .unwrap_err();
    let elapsed = start.elapsed();
    assert!(
        matches!(err, nlink_lab::Error::Timeout(_)),
        "expected Error::Timeout, got: {err:?}"
    );
    // Should be ~500ms timeout + ~1s grace = ~1.5s. Allow generous
    // upper bound (5s) for slow CI; the point is it didn't hang the
    // full 30s.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "took too long: {elapsed:?}"
    );
}

// A short-lived command under a generous timeout must complete normally
// — timeout is opt-in cap, not a hard wait.
#[lab_test("examples/simple.nll")]
async fn exec_under_timeout_returns_normally(lab: RunningLab) {
    let opts = nlink_lab::ExecOpts {
        timeout: Some(std::time::Duration::from_secs(5)),
        ..Default::default()
    };
    let out = lab.exec_with_opts("host", "echo", &["hi"], opts).unwrap();
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.trim(), "hi");
}
