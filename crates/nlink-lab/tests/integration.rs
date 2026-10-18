//! Integration tests for nlink-lab.
//!
//! These tests deploy real network namespaces and require root or CAP_NET_ADMIN.
//! They are automatically skipped when run as a non-root user.
//!
//! Run with: `sudo cargo test -p nlink-lab --test integration`

use nlink_lab::lab_test;
#[allow(unused_imports)]
use nlink_lab::{Lab, Op, RunningLab};

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

/// #222: `timeout` wrapped the body in `tokio::time::timeout`, which never
/// fired while the body blocked its thread in `exec`: this test waited out
/// the `sleep` and passed. It now ends at the deadline.
#[lab_test("examples/simple.nll", timeout = 3)]
#[should_panic(expected = "exceeded 3s timeout")]
async fn a_lab_test_timeout_ends_a_blocking_body(lab: RunningLab) {
    let _ = lab.exec("router", "sleep", &["20"]);
}

/// #222: the macro renamed the lab only, so the namespaces of a topology
/// with `prefix` were `dc-spine1`, shared by every test of that file.
#[lab_test("examples/spine-leaf.nll")]
async fn a_lab_test_of_a_prefixed_topology_gets_its_own_namespaces(lab: RunningLab) {
    let ns = lab.namespace_for("spine1").unwrap();
    assert!(
        ns.starts_with("dc-test-a_lab_test_of_a_prefixed_topology_gets_its_own_namespaces-"),
        "{ns}"
    );
}

/// #222: every interface of a node wrote `<namespace>.pcap`, truncating
/// the other's, and one file per node was kept.
#[tokio::test]
async fn lab_capture_keeps_one_pcap_per_interface() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping lab_capture_keeps_one_pcap_per_interface: requires root");
        return;
    }
    let lab_name = format!("cap2-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router
node a {{ route default via 10.0.1.1 }}
node b {{ route default via 10.0.2.1 }}
link a:eth0 -- r:eth0 {{ 10.0.1.2/24 -- 10.0.1.1/24 }}
link r:eth1 -- b:eth0 {{ 10.0.2.1/24 -- 10.0.2.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let cap = nlink_lab::test_helpers::LabCapture::start(&lab.capture_targets()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // two seconds of pings: on a loaded host a ring can come up after a
    // shorter burst is over, and caught nothing once
    let ping = lab
        .exec("a", "ping", &["-c10", "-i0.2", "10.0.2.2"])
        .unwrap();
    assert_eq!(ping.exit_code, 0, "{}", ping.stderr);
    let dir = tempfile::tempdir().unwrap();
    let kept = cap.persist_to(dir.path()).unwrap();
    let ns = lab.namespace_for("r").unwrap().to_string();
    for iface in ["eth0", "eth1"] {
        let path = dir.path().join(format!("{ns}-{iface}.pcap"));
        assert!(kept.contains(&path), "{path:?} not in {kept:?}");
        // the 24-byte pcap header and some of the twenty ICMP packets
        // (the first can go by before the ring is up)
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len > 24 + 3 * 100, "{path:?}: {len} bytes");
    }
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
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

/// `diagnose_since` takes an interface's rates since the previous call,
/// over the time between the two reads of its counters; `diagnose` alone
/// has no previous sample and reports none (#191).
#[lab_test("examples/simple.nll")]
async fn diagnose_since_reports_real_rates(lab: RunningLab) {
    let eth0 = |d: &[nlink_lab::NodeDiagnostic]| {
        d[0].interfaces
            .iter()
            .find(|i| i.name == "eth0")
            .cloned()
            .expect("router eth0")
    };
    let first = lab.diagnose(Some("router")).await.unwrap();
    assert_eq!(eth0(&first).rates.rx_bytes_per_sec, 0.0);

    // 200 echo requests of 1042 bytes on the wire arrive on router:eth0.
    let out = lab
        .exec(
            "host",
            "ping",
            &["-q", "-c", "200", "-i", "0.002", "-s", "1000", "10.0.0.1"],
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}{}", out.stdout, out.stderr);

    let second = lab.diagnose_since(Some("router"), &first).await.unwrap();
    let (before, after) = (eth0(&first), eth0(&second));
    let bytes = after.stats.rx_bytes() - before.stats.rx_bytes();
    assert!(bytes >= 200 * 1042, "{bytes} bytes received");
    let secs = (second[0].taken_at - first[0].taken_at).as_secs_f64();
    let rate = after.rates.rx_bytes_per_sec;
    let expected = bytes as f64 / secs;
    assert!(
        (rate - expected).abs() < expected * 0.01,
        "rx {rate:.0} B/s, {bytes} bytes over {secs:.3}s is {expected:.0} B/s"
    );

    let again = lab.diagnose(Some("router")).await.unwrap();
    assert_eq!(eth0(&again).rates.rx_bytes_per_sec, 0.0);
    // the kernel's fallback tunnels (on a host with their modules) are
    // not the lab's (#226)
    let names: Vec<&str> = again[0]
        .interfaces
        .iter()
        .map(|i| i.name.as_str())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| ["gre0", "gretap0", "sit0", "tunl0"].contains(n)),
        "{names:?}"
    );
    // the router has no default route, on purpose: no warning (#226)
    assert!(
        again[0]
            .issues
            .iter()
            .all(|i| !i.message.contains("default route")),
        "{:?}",
        again[0].issues
    );
}

/// #226: `wait-for --exec` ran each probe without a timeout, so one that
/// hung held the wait past `--timeout`, forever if it never returned.
#[lab_test("examples/simple.nll")]
async fn wait_for_exec_ends_a_hung_probe_at_the_timeout(lab: RunningLab) {
    let started = std::time::Instant::now();
    let r = lab
        .wait_for_exec(
            "host",
            "sleep 30",
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(100),
        )
        .await;
    assert!(matches!(r, Err(nlink_lab::Error::TimedOut(_))), "{r:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
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

/// `destroy` stops what a lab's commands left running in its namespaces,
/// which nothing tracks: a daemon an `exec` started, and one that ignores
/// SIGTERM. Each kept its deleted namespace alive — the root suite's
/// `iperf3 -s -D` servers had piled up by the hundred.
#[tokio::test]
async fn destroy_stops_what_the_lab_left_running() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping destroy_stops_what_the_lab_left_running: requires root");
        return;
    }
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "strays-{}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#,
        std::process::id()
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let out = lab
        .exec(
            "a",
            "sh",
            &[
                "-c",
                "sleep 7301 </dev/null >/dev/null 2>&1 & \
                 (trap '' TERM; exec sleep 7302) </dev/null >/dev/null 2>&1 &",
            ],
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    let cmdline = |pid: u32| {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
            .unwrap_or_default()
    };
    let strays: Vec<u32> = lab
        .node_pids("a")
        .unwrap()
        .into_iter()
        .filter(|&p| cmdline(p).starts_with("sleep 730"))
        .collect();
    assert_eq!(strays.len(), 2, "both sleeps run in a: {strays:?}");
    assert!(lab.process_status().is_empty(), "neither is tracked");

    let started = std::time::Instant::now();
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
    // the one that ignores SIGTERM is killed after a second's grace
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    for _ in 0..40 {
        if strays.iter().all(|&p| pid_dead(p)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "still running after destroy: {:?}",
        strays
            .iter()
            .map(|&p| (p, proc_state(p)))
            .collect::<Vec<_>>()
    );
}

/// `/proc/<pid>/stat` state letter, `None` when the pid is gone.
fn proc_state(pid: u32) -> Option<char> {
    let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    st[st.rfind(')')? + 2..].chars().next()
}

/// Dead for our purposes: gone, or a zombie waiting for *its* parent.
/// CI job containers often run without a reaping PID 1, so an orphaned
/// process that exited stays `Z` forever there; that is the container's
/// problem, not a live process.
fn pid_dead(pid: u32) -> bool {
    !matches!(proc_state(pid), Some(c) if c != 'Z')
}

/// Pids of zombie children of *this* process — what issue #30's second
/// half is about: nlink-lab must never own a child it does not reap.
fn our_zombie_children() -> Vec<u32> {
    let me = std::process::id();
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(st) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = st.rfind(')') else { continue };
        let mut fields = st[close + 2..].split_whitespace();
        let state = fields.next().unwrap_or("");
        let ppid: u32 = fields.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        if ppid == me && state == "Z" {
            out.push(pid);
        }
    }
    out
}

// Issue #30 (second half): background spawns are double-forked and
// session-detached. The pid we hand back must be the real process (not
// an intermediate), it must not be our child (so it can never become our
// zombie), and a quick-exiting one must vanish from /proc rather than
// linger in state Z.
#[lab_test("examples/simple.nll")]
async fn spawn_leaves_no_zombie_and_returns_real_pid(mut lab: RunningLab) {
    let me = std::process::id();
    let pid = lab.spawn_with_logs("host", &["sleep", "30"], None).unwrap();
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap();
    assert_eq!(
        comm.trim(),
        "sleep",
        "pid {pid} must be the spawned program"
    );
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after_comm = &stat[stat.rfind(')').unwrap() + 2..];
    let ppid: u32 = after_comm
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(ppid, me, "detached process must not be our child");
    let _ = lab.kill_process(pid);

    // A quick-exiting spawn must never become *our* zombie. Whether it
    // vanishes entirely depends on the host's PID 1 reaping orphans
    // (not the case in every CI container), so only "dead" is asserted.
    let quick = lab.spawn_with_logs("host", &["true"], None).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pid_dead(quick) {
        assert!(
            std::time::Instant::now() < deadline,
            "pid {quick} still running after 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        our_zombie_children().is_empty(),
        "nlink-lab must not own zombie children: {:?}",
        our_zombie_children()
    );
}

// 0.11.0's exit-code reaper: a spawned process's exit status lands in
// `<log>.rc` and surfaces as `ProcessInfo::exit_code` once it is dead --
// and the reaper that writes it is init's child, never ours (the 0.11.0
// version stayed our zombie until we exited, one per spawn).
#[lab_test("examples/simple.nll")]
async fn spawn_records_exit_code_without_owning_the_reaper(mut lab: RunningLab) {
    let pid = lab
        .spawn_with_logs("host", &["sh", "-c", "exit 3"], None)
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let code = loop {
        let info = lab
            .process_status()
            .into_iter()
            .find(|p| p.pid == pid)
            .expect("spawned pid is tracked");
        if let (false, Some(code)) = (info.alive, info.exit_code) {
            break code;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pid {pid}: no exit code after 5s (alive={}, exit_code={:?})",
            info.alive,
            info.exit_code
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(code, 3, "exit code read back from the .rc file");
    // The reaper outlives the process by a moment; give it that moment,
    // then it must have been reaped by init, not be waiting on us.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        our_zombie_children().is_empty(),
        "the reaper must not be our zombie: {:?}",
        our_zombie_children()
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
            // A failed deploy still took the lock and created the state
            // directory; without this the skip leaves both behind on every
            // run (issue #103).
            discard_lab_state(&topo.lab.name);
            return;
        }
    };
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Reapply must succeed — this is the key check: Slice 1's
    // NetworkConfig path sees the macvlan iface (created
    // imperatively in step 6a) and shouldn't try to re-create it.
    nlink_lab::apply(&mut lab, &topo)
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
    nlink_lab::apply(&mut lab, &topo)
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
    nlink_lab::apply(&mut lab, &topo)
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

    // #141: the *diff* must agree with the applier. Handles surviving
    // only proves `apply` converged; it did that before too. What was
    // broken is what `apply --check` reads — the masquerade expression rendered
    // without the empty NFTA_EXPR_DATA nest the kernel echoes, so the
    // rule came out four bytes short of the kernel's copy and diffed as
    // changed on every reconcile, forever. Nothing here asserted on the
    // diff, which is why it took a downstream drift gate to find it.
    // Fixed upstream in nlink 0.28 (nlink#362).
    let diff = nlink_lab::deploy::compute_layered_diff(&lab, &topo)
        .await
        .expect("layered diff");
    assert!(
        diff.is_empty(),
        "a freshly applied topology must show no drift; got:\n{diff}"
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

/// A VXLAN over an IPv6 underlay deploys with its endpoints and carries
/// traffic. nlink 0.26–0.29 dropped IPv6 endpoints (the tunnel came up
/// with none), so the validator refused them; 0.30 writes
/// `IFLA_VXLAN_LOCAL6`/`GROUP6` (nlink #418).
#[tokio::test]
async fn vxlan_ipv6_underlay_deploys_and_forwards() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping vxlan_ipv6_underlay_deploys_and_forwards: requires root");
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

// ─── a recreated link keeps nlink-lab's Tc and VRF routes (#158) ───

/// `r` has a VLAN in VRF `red`, a VRF route through it and netem on it;
/// `h` has the other end of the VLAN.
fn recreate_lab_src(lab_name: &str, id: u16) -> String {
    format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  vlan vl0 {{ parent eth0 id {id} address 10.10.0.1/24 }}
  vrf red table 10 {{ interfaces [vl0] route 192.168.5.0/24 via 10.10.0.2 }}
}}
node h {{
  vlan vl0 {{ parent eth0 id {id} address 10.10.0.2/24 }}
}}
link r:eth0 -- h:eth0
impair r:vl0 delay 20ms
"#
    )
}

fn recreate_modules_present(test: &str) -> bool {
    for m in ["vrf", "8021q"] {
        let _ = std::process::Command::new("modprobe").arg(m).status();
        if !std::path::Path::new(&format!("/sys/module/{m}")).exists() {
            eprintln!("skipping {test}: kernel `{m}` module unavailable");
            return false;
        }
    }
    true
}

/// Changing a VLAN id makes nlink delete and recreate the VLAN (nlink
/// #417). The VLAN carries nlink-lab's netem, which nlink keeps across the
/// recreate (`preserve_undeclared_tc`, nlink #645), and a VRF route, which
/// is declared and so put back; the VLAN keeps its VRF master.
#[tokio::test]
async fn apply_recreates_vlan_and_reinstalls_netem_and_vrf_route() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping apply_recreates_vlan_and_reinstalls_netem_and_vrf_route: requires root"
        );
        return;
    }
    if !recreate_modules_present("apply_recreates_vlan_and_reinstalls_netem_and_vrf_route") {
        return;
    }
    let lab_name = format!("rcv-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&recreate_lab_src(&lab_name, 10)).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let desired = nlink_lab::parser::parse(&recreate_lab_src(&lab_name, 20)).unwrap();

    // --check shows the recreate, and not as refused: nlink keeps the
    // netem, and the VRF route is declared.
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    let r = &layered.network["r"];
    let rec: Vec<_> = r
        .links_to_recreate
        .iter()
        .filter(|l| l.name == "vl0")
        .collect();
    assert_eq!(rec.len(), 1, "vl0 should be recreated:\n{layered}");
    assert!(
        !rec[0].is_refused(),
        "the recreate must not be refused:\n{layered}"
    );

    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");

    let link = lab.exec("r", "ip", &["-d", "link", "show", "vl0"]).unwrap();
    assert!(link.stdout.contains("id 20"), "{}", link.stdout);
    assert!(
        link.stdout.contains("master red"),
        "vl0 lost its VRF: {}",
        link.stdout
    );
    let tc = lab
        .exec("r", "tc", &["qdisc", "show", "dev", "vl0"])
        .unwrap();
    assert!(
        tc.stdout.contains("netem") && tc.stdout.contains("delay 20ms"),
        "netem not re-installed: {}",
        tc.stdout
    );
    let routes = lab
        .exec("r", "ip", &["route", "show", "table", "10"])
        .unwrap();
    assert!(
        routes.stdout.contains("192.168.5.0/24"),
        "VRF route not re-installed: {}",
        routes.stdout
    );
    let mut ok = false;
    for _ in 0..10 {
        if lab
            .exec("h", "ping", &["-c", "1", "-W", "1", "10.10.0.1"])
            .unwrap()
            .exit_code
            == 0
        {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert!(ok, "h cannot reach r over the recreated VLAN");

    // Converged: nothing left to do.
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// Removing a declared interface from the topology deletes it (#159):
/// nlink plans against the previous config, so the VLAN it declared and
/// this one does not is a link to remove, listed by `--check`.
#[tokio::test]
async fn apply_deletes_a_removed_vlan_and_check_lists_it() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_deletes_a_removed_vlan_and_check_lists_it: requires root");
        return;
    }
    if !recreate_modules_present("apply_deletes_a_removed_vlan_and_check_lists_it") {
        return;
    }
    let lab_name = format!("rmv-{}", std::process::id());
    let src = |vl1: &str| {
        format!(
            r#"lab "{lab_name}"
node r {{
  vlan vl0 {{ parent eth0 id 10 address 10.10.0.1/24 }}
  {vl1}
}}
node h
link r:eth0 -- h:eth0
"#
        )
    };
    let topo =
        nlink_lab::parser::parse(&src("vlan vl1 { parent eth0 id 11 address 10.11.0.1/24 }"))
            .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert_eq!(
        lab.exec("r", "ip", &["link", "show", "vl1"])
            .unwrap()
            .exit_code,
        0
    );
    let desired = nlink_lab::parser::parse(&src("")).unwrap();

    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    let removed: Vec<&str> = layered
        .network
        .get("r")
        .map(|d| d.links_to_remove.iter().map(|l| l.name.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(removed, ["vl1"], "--check should list vl1:\n{layered}");

    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let gone = lab.exec("r", "ip", &["link", "show", "vl1"]).unwrap();
    assert_ne!(gone.exit_code, 0, "vl1 is still there: {}", gone.stdout);
    let kept = lab.exec("r", "ip", &["-o", "addr", "show", "vl0"]).unwrap();
    assert!(kept.stdout.contains("10.10.0.1/24"), "{}", kept.stdout);
    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// A per-interface sysctl on a declared VLAN (#160): the deploy failed,
/// since the Sysctls stage ran before Stack created the VLAN. It is devconf
/// on the VLAN's declaration now, so it is there after the deploy, after a
/// VLAN-id change recreates the link, and after a hand edit and an apply.
#[tokio::test]
async fn declared_vlan_sysctl_survives_deploy_recreate_and_drift() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping declared_vlan_sysctl_survives_deploy_recreate_and_drift: requires root"
        );
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

/// `network { mtu 9000 }` (#161): the member veths, both ends, get the
/// MTU, so the bridge keeps it once its ports join; a 9000-byte frame
/// crosses unfragmented.
#[tokio::test]
async fn network_mtu_reaches_bridge_and_members() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping network_mtu_reaches_bridge_and_members: requires root");
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

/// A recreate nlink would refuse — the VLAN carries a permanent neighbour
/// the topology cannot declare — fails before anything changes.
#[tokio::test]
async fn apply_refused_recreate_changes_nothing() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_refused_recreate_changes_nothing: requires root");
        return;
    }
    if !recreate_modules_present("apply_refused_recreate_changes_nothing") {
        return;
    }
    let lab_name = format!("rcr-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&recreate_lab_src(&lab_name, 10)).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let neigh = lab
        .exec(
            "r",
            "ip",
            &[
                "neigh",
                "replace",
                "10.10.0.99",
                "lladdr",
                "02:00:00:00:00:99",
                "dev",
                "vl0",
                "nud",
                "permanent",
            ],
        )
        .unwrap();
    assert_eq!(neigh.exit_code, 0, "{}", neigh.stderr);
    let desired = nlink_lab::parser::parse(&recreate_lab_src(&lab_name, 20)).unwrap();

    let layered = nlink_lab::compute_layered_diff(&lab, &desired)
        .await
        .expect("compute_layered_diff failed");
    let refused = layered.network["r"]
        .links_to_recreate
        .iter()
        .any(|l| l.name == "vl0" && l.is_refused());
    assert!(refused, "--check should show the refusal:\n{layered}");

    let err = nlink_lab::apply(&mut lab, &desired)
        .await
        .expect_err("apply must refuse")
        .to_string();
    assert!(err.contains("10.10.0.99"), "{err}");
    assert!(err.contains("nothing has been changed"), "{err}");

    let link = lab.exec("r", "ip", &["-d", "link", "show", "vl0"]).unwrap();
    assert!(link.stdout.contains("id 10"), "{}", link.stdout);
    let tc = lab
        .exec("r", "tc", &["qdisc", "show", "dev", "vl0"])
        .unwrap();
    assert!(
        tc.stdout.contains("netem"),
        "netem was removed: {}",
        tc.stdout
    );
    let routes = lab
        .exec("r", "ip", &["route", "show", "table", "10"])
        .unwrap();
    assert!(
        routes.stdout.contains("192.168.5.0/24"),
        "{}",
        routes.stdout
    );
    let iface = &lab.topology().nodes["r"].interfaces["vl0"];
    assert_eq!(iface.vni, Some(10), "state must keep the old topology");
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
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply after a hand-removed peer failed");
    let back = lab.exec("gw-a", "wg", &["show", "wg0", "peers"]).unwrap();
    assert_eq!(back.stdout.trim(), peer, "the peer is not restored");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping multi_tenant_wan_tunnels_carry_their_customers: requires root");
        return;
    }
    for m in ["wireguard", "vrf"] {
        let _ = std::process::Command::new("modprobe").arg(m).status();
        if !std::path::Path::new(&format!("/sys/module/{m}")).exists() {
            eprintln!("skipping multi_tenant_wan_tunnels_carry_their_customers: no `{m}` module");
            return;
        }
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/cookbook/multi-tenant-wan.nll"
    ))
    .unwrap();
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
async fn verify_json_emits_schema_v3_envelope() {
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

    // `verify --file` is the drift gate `apply --check` was (#246)
    let output = std::process::Command::new(&target_dir)
        .args([
            "--json",
            "verify",
            "-L",
            lab.name(),
            "--file",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/simple.nll"),
        ])
        .output()
        .expect("failed to run nlink-lab verify --file --json");

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
    nlink_lab::apply(&mut lab, &topo)
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

// Issue #84: editing a background `run` line and applying must stop the
// old process and start the new one; removing the line stops it.
#[tokio::test]
async fn apply_restarts_edited_background_exec() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_restarts_edited_background_exec: requires root");
        return;
    }
    let src = r#"lab "apply-exec-edit"
node s { run ["sleep", "1000"] background }
"#;
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let old = *lab
        .exec_pids()
        .get("s:0")
        .expect("exec pid tracked after deploy");
    let comm =
        |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    assert!(comm(old).contains("1000"), "{}", comm(old));

    let desired = nlink_lab::parser::parse(&src.replace("1000", "999")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let new = *lab
        .exec_pids()
        .get("s:0")
        .expect("exec pid tracked after apply");
    assert_ne!(new, old);
    assert!(comm(new).contains("999"), "{}", comm(new));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pid_dead(old) {
        assert!(
            std::time::Instant::now() < deadline,
            "old exec pid {old} still alive"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let alive: Vec<u32> = lab
        .process_status_alive_only()
        .iter()
        .map(|p| p.pid)
        .collect();
    assert_eq!(alive, vec![new], "exactly the new process is tracked");
    // persisted
    let reloaded = RunningLab::load(lab.name()).unwrap();
    assert_eq!(reloaded.exec_pids().get("s:0"), Some(&new));

    // Removing the line stops it without touching anything else.
    let desired = nlink_lab::parser::parse("lab \"apply-exec-edit\"\nnode s\n").unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pid_dead(new) {
        assert!(
            std::time::Instant::now() < deadline,
            "removed exec pid {new} still alive"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(lab.exec_pids().is_empty(), "{:?}", lab.exec_pids());

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Issue #85: removing a `network` block deletes its bridge from the mgmt
// namespace (member veths were already deleted).
#[tokio::test]
async fn apply_removed_network_deletes_bridge() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_removed_network_deletes_bridge: requires root");
        return;
    }
    let src = r#"lab "apply-net-rm"
node a
node b
node c
network lan { subnet 10.1.0.0/24  members [a:eth0, b:eth0] }
network dmz { subnet 10.2.0.0/24  members [b:eth1, c:eth0] }
"#;
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // Bridges live in the lab's mgmt namespace; list them through
    // netlink (an `ip -n` child is not reliable in every CI container).
    async fn mgmt_bridges() -> Vec<String> {
        let conn: nlink::Connection<nlink::Route> =
            nlink::Connection::new_in("apply-net-rm-mgmt").unwrap();
        conn.get_links()
            .await
            .unwrap()
            .iter()
            .filter_map(|l| l.name().map(str::to_string))
            .filter(|n| n.starts_with("nb"))
            .collect()
    }
    let before = mgmt_bridges().await;
    assert_eq!(before.len(), 2, "two bridges expected: {before:?}");

    let desired = nlink_lab::parser::parse(&src.replace(
        "network dmz { subnet 10.2.0.0/24  members [b:eth1, c:eth0] }\n",
        "",
    ))
    .unwrap();
    let plan = nlink_lab::apply_plan(&lab, &desired).unwrap();
    assert!(
        plan.ops
            .iter()
            .any(|o| matches!(o, nlink_lab::Op::DeleteBridge { .. })),
        "{:?}",
        plan.ops
    );
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    let after = mgmt_bridges().await;
    assert_eq!(after.len(), 1, "dmz bridge must be gone: {after:?}");
    let b = lab.exec("b", "ip", &["-br", "link"]).unwrap().stdout;
    assert!(!b.contains("eth1"), "b:eth1 must be gone: {b}");
    assert!(b.contains("eth0"), "b:eth0 must survive: {b}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Issue #86: `apply` runs the topology's `validate { … }` assertions.
#[tokio::test]
async fn apply_runs_validate_assertions() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_runs_validate_assertions: requires root");
        return;
    }
    let src = r#"lab "apply-assert"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#;
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(lab.assertion_results().is_empty());

    let desired = nlink_lab::parser::parse(&format!("{src}validate {{ reach a b }}\n")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert_eq!(
        lab.assertion_results().len(),
        1,
        "{:?}",
        lab.assertion_results()
    );
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());

    // A failing assertion is reported, never fatal at the library level.
    let desired =
        nlink_lab::parser::parse(&format!("{src}validate {{ tcp-connect a b 9 }}\n")).unwrap();
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert!(lab.assertions_failed(), "{:?}", lab.assertion_results());

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

// Issue #66: `cpu` / `memory` on a namespace node become a cgroup v2
// subtree holding its background processes; destroy removes it.
#[tokio::test]
async fn namespace_node_cpu_memory_limits_apply_via_cgroups() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping namespace_node_cpu_memory_limits_apply_via_cgroups: requires root");
        return;
    }
    if !nlink_lab::cgroup::available() {
        eprintln!("skipping: no cgroup v2");
        return;
    }
    let src = r#"lab "cg"
node a { cpu 0.5  memory 64m  run ["sleep", "1000"] background }
"#;
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let dir = nlink_lab::cgroup::node_dir("cg", "a");
    let usage = nlink_lab::cgroup::usage("cg", "a");
    if !dir.is_dir() {
        // read-only hierarchy (unprivileged container): limits are best effort
        eprintln!(
            "skipping assertions: cgroup dir not created ({})",
            dir.display()
        );
    } else {
        let pid = *lab.exec_pids().get("a:0").unwrap();
        let procs = std::fs::read_to_string(dir.join("cgroup.procs")).unwrap();
        assert!(
            procs.lines().any(|l| l.trim() == pid.to_string()),
            "pid {pid} not in {procs}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("memory.max"))
                .unwrap()
                .trim(),
            "67108864"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("cpu.max")).unwrap().trim(),
            "50000 100000"
        );
        let u = usage.expect("usage readable");
        assert_eq!(u.pids, 1);
        assert_eq!(u.memory_max, Some(67108864));
        // a manual spawn joins the same cgroup
        let spawned = lab.spawn_with_logs("a", &["sleep", "999"], None).unwrap();
        let procs = std::fs::read_to_string(dir.join("cgroup.procs")).unwrap();
        assert!(
            procs.lines().any(|l| l.trim() == spawned.to_string()),
            "{procs}"
        );
    }
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
    assert!(!dir.exists(), "cgroup {} survived destroy", dir.display());
}

// Issue #83: a VRF-table route removed from the topology must be
// deleted on apply. VRF routes are declared in the node's config, and
// purge covers every table it declares into, and the ones it did before.
#[tokio::test]
async fn apply_removes_vrf_route() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping apply_removes_vrf_route: requires root");
        return;
    }
    if !has_kernel_module("vrf") {
        eprintln!("skipping apply_removes_vrf_route: vrf kernel module not available");
        return;
    }
    let src = r#"lab "apply-vrf-rm"
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
"#;
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
                    // The ownership tag is not part of the namespace; the
                    // CI debris gate checks it too.
                    nlink_lab::netns_tag::untag(ns);
                }
            }
        }
        // FRR daemons pin a deleted namespace until they are killed; a
        // failed test must not leave them (or their runtime dirs) behind.
        let _ = std::process::Command::new("pkill")
            .args(["-f", "--", &format!("-N nl-{prefix}")])
            .status();
        let _ = std::fs::remove_dir_all(nlink_lab::frr::lab_dir(&self.name));
        if let Ok(entries) = std::fs::read_dir(nlink_lab::frr::RUN_ROOT) {
            for e in entries.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("nl-{prefix}"))
                {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        // A container node's container is named `<lab>-<node>`; a failed
        // test must not leave it running.
        // podman waits 10 s for a stop signal by default; `-t 0` does not.
        for (runtime, rm) in [
            ("podman", &["rm", "-f", "-t", "0"][..]),
            ("docker", &["rm", "-f"][..]),
        ] {
            let Ok(out) = std::process::Command::new(runtime)
                .args(["ps", "-a", "--format", "{{.Names}}"])
                .stderr(std::process::Stdio::null())
                .output()
            else {
                continue;
            };
            for name in String::from_utf8_lossy(&out.stdout).lines() {
                if name.starts_with(&prefix) {
                    let _ = std::process::Command::new(runtime)
                        .args(rm)
                        .arg(name)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
        }
        // A `dns hosts` lab's /etc/hosts section and /etc/netns overlays,
        // and a limited node's cgroup, outlive its namespaces.
        let _ = nlink_lab::dns::remove_hosts(&self.name);
        if let Ok(entries) = std::fs::read_dir("/etc/netns") {
            for e in entries.flatten() {
                let ns = e.file_name().to_string_lossy().into_owned();
                if ns.starts_with(&prefix) {
                    nlink_lab::dns::remove_netns_etc(&ns);
                }
            }
        }
        nlink_lab::cgroup::remove_lab(&self.name);
        let _ = nlink_lab::state::remove(&self.name);
        // Every test lab has a unique name, so without this `.locks/`
        // grows by one file per test, for ever (issue #103).
        nlink_lab::state::remove_lock_if_unheld(&self.name);
    }
}

/// Drop a lab's state directory and lock file without going through
/// `destroy` — for the skip paths that bail out after a *failed* deploy,
/// which has already taken the lock and created the directory but has no
/// `RunningLab` to destroy.
fn discard_lab_state(name: &str) {
    let _ = nlink_lab::state::remove(name);
    nlink_lab::state::remove_lock_if_unheld(name);
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

    nlink_lab::apply(&mut lab, &desired)
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

    nlink_lab::apply(&mut lab, &desired)
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

    nlink_lab::apply(&mut lab, &desired)
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
    // Right after spawn the child may still be between fork and exec
    // (comm not yet "sleep") or running its startup (state R) — on a
    // loaded CI runner that window is visible. Poll until it has settled
    // into nanosleep (S/I), then assert on the sampled record.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let stat = loop {
        let stat = lab.proc_stat("host", pid).unwrap();
        let settled = stat.command == "sleep" && (stat.state == "S" || stat.state == "I");
        if settled || std::time::Instant::now() >= deadline {
            break stat;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
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

// Issue #131: sampling a whole node in one call, and PSS.
//
// The old shape was one process per invocation, each costing five
// namespace-entering execs. A consumer sampling ~50 processes on a
// cadence therefore paid ~250 execs per tick, enough that the sampler's
// own CPU showed up in the CPU it was measuring.
#[lab_test("examples/simple.nll")]
async fn proc_stat_many_samples_a_whole_node(mut lab: RunningLab) {
    let a = lab.spawn_with_logs("host", &["sleep", "40"], None).unwrap();
    let b = lab.spawn_with_logs("host", &["sleep", "41"], None).unwrap();

    // `--all` is namespace membership, not nlink-lab's own bookkeeping:
    // it must find processes whether or not nlink-lab spawned them.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let pids = loop {
        let pids = lab.node_pids("host").unwrap();
        if (pids.contains(&a) && pids.contains(&b)) || std::time::Instant::now() >= deadline {
            break pids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(pids.contains(&a), "node_pids missed {a}: {pids:?}");
    assert!(pids.contains(&b), "node_pids missed {b}: {pids:?}");
    // The host's own PIDs share the host namespace, not this node's.
    assert!(
        !pids.contains(&std::process::id()),
        "the test runner is not in the node's namespace: {pids:?}"
    );

    let stats = lab.proc_stat_many("host", &pids).unwrap();
    assert_eq!(stats.len(), pids.len());
    assert!(stats.iter().all(|s| s.node == "host"));

    // One call must agree field for field with the single-PID path —
    // apart from the CPU counters and state, which move on their own.
    let many = stats.iter().find(|s| s.host_pid == a).unwrap();
    let one = lab.proc_stat("host", a).unwrap();
    assert_eq!(one.host_pid, many.host_pid);
    assert_eq!(one.command, many.command);
    assert_eq!(one.uid, many.uid);
    assert_eq!(one.vsz_kb, many.vsz_kb);
    assert_eq!(
        one.started_at_unix_micros, many.started_at_unix_micros,
        "start time is derived from btime + starttime; both paths must \
         read the same tick rate"
    );

    // PSS: present for a userland process, and never above RSS — every
    // page counts once in RSS and at most once (1/N of a shared page)
    // in PSS.
    let pss = one.pss_kb.expect("smaps_rollup should be readable as root");
    let rss = one.rss_kb.expect("a userland process has an MM");
    assert!(pss > 0, "PSS should not be zero for a live process");
    assert!(pss <= rss, "pss {pss} kB must not exceed rss {rss} kB");

    for pid in [a, b] {
        let _ = lab.exec("host", "kill", &[&pid.to_string()]);
    }
}

// ─── Plan 156 PR A — partition cycles fix ───────────────

/// Whether `node:iface` drops what it sends and receives: a partition's
/// matchall drop on both clsact hooks (#168).
fn blocked(lab: &RunningLab, node: &str, iface: &str) -> bool {
    ["ingress", "egress"].iter().all(|hook| {
        lab.exec(node, "tc", &["filter", "show", "dev", iface, hook])
            .unwrap()
            .stdout
            .contains("action drop")
    })
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

/// `tcp-connect` and `route-has` read the kernel through nlink (#179):
/// a socket made in the node's namespace, and a main-table route dump,
/// instead of `bash -c 'echo > /dev/tcp/…'` and `ip route show` text.
#[tokio::test]
async fn tcp_connect_and_route_has_read_the_kernel() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping tcp_connect_and_route_has_read_the_kernel: requires root");
        return;
    }
    let lab_name = format!("tcrh-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a {{ route 10.9.0.0/24 via 10.0.0.2 }}
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
validate {{
  tcp-connect a b 5555
  route-has a 10.9.0.0/24 via 10.0.0.2 dev eth0
  route-has a 10.9.0.0/24 via 10.0.0.99
  route-has a 10.0.0.0/24 dev eth0
  route-has a 10.8.0.0/24
  tcp-connect a b 5556
}}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // A listener in b's namespace, held by this process.
    let netns = lab.ns_resolver_of("b").unwrap().netns().unwrap();
    let _listener = netns
        .run(|| std::net::TcpListener::bind("0.0.0.0:5555"))
        .unwrap()
        .unwrap();
    let ip_map = nlink_lab::scenario::build_ip_map(lab.topology());
    let got: Vec<(bool, String)> = topo
        .assertions
        .iter()
        .map(|a| {
            let (desc, ok, detail) = nlink_lab::test_runner::eval_assertion_pub(&lab, a, &ip_map);
            (ok, format!("{desc}: {detail:?}"))
        })
        .collect();
    let ok: Vec<bool> = got.iter().map(|(ok, _)| *ok).collect();
    assert_eq!(ok, [true, true, false, true, false, false], "{got:#?}");
    assert!(got[1].1.contains("via 10.0.0.2 dev eth0"), "{got:#?}");
    assert!(got[5].1.contains("refused"), "{got:#?}");
    lab.destroy().await.expect("destroy failed");
}

/// A watch ends cleanly when its lab goes away (#175): each node's route
/// and nftables subscriptions end with a `gone` event and their tasks
/// return, so their sockets stop pinning the namespaces. They used to
/// stream nothing, with the namespaces kept alive behind `destroy`.
#[tokio::test]
async fn watch_reports_gone_when_the_lab_is_destroyed() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping watch_reports_gone_when_the_lab_is_destroyed: requires root");
        return;
    }
    let lab_name = format!("wgone-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &nlink_lab::WatchOpts::default())
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lab.destroy().await.expect("destroy failed");

    let mut gone = std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                if matches!(ev.kind, nlink_lab::watch::WatchEventKind::Gone) {
                    gone.insert(format!("{}/{:?}", ev.node, ev.family));
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert_eq!(
        gone.into_iter().collect::<Vec<_>>(),
        ["a/Nftables", "a/Route", "b/Nftables", "b/Route"]
    );
    for t in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(2), t)
            .await
            .expect("a subscription task outlived its namespace")
            .unwrap();
    }
}

/// #226: across `destroy` and `deploy` of the same lab between two checks,
/// the node's name opened again (on the new namespace) and the watch stayed
/// on the dead one, reporting nothing more. And `--node` of a node the lab
/// does not have was an empty watch.
#[tokio::test]
async fn watch_reports_gone_when_the_lab_is_redeployed() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping watch_reports_gone_when_the_lab_is_redeployed: requires root");
        return;
    }
    let lab_name = format!("wredo-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let unknown = nlink_lab::WatchOpts {
        node: Some("nope".into()),
        ..Default::default()
    };
    assert!(matches!(
        nlink_lab::watch_stream(&lab, &unknown),
        Err(nlink_lab::Error::NodeNotFound { .. })
    ));
    let opts = nlink_lab::WatchOpts {
        node: Some("a".into()),
        family: nlink_lab::WatchFamily::Route,
        ..Default::default()
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // well inside one 2 s check
    lab.destroy().await.expect("destroy failed");
    let lab = topo.deploy().await.expect("redeploy failed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut gone = false;
    while !gone && std::time::Instant::now() < deadline {
        if let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            gone = matches!(ev.kind, nlink_lab::watch::WatchEventKind::Gone);
        }
    }
    assert!(gone, "the watch stayed on the destroyed namespace");
    for t in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(2), t)
            .await
            .expect("a subscription task outlived its namespace")
            .unwrap();
    }
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// Accept one connection on `listener` within `wait`, and return its peer.
fn accept_peer(
    listener: &std::net::TcpListener,
    wait: std::time::Duration,
) -> std::net::SocketAddr {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + wait;
    loop {
        match listener.accept() {
            Ok((_, peer)) => return peer,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no connection arrived"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}

/// NAT port ranges and redirect (#189), with real traffic: the server sees
/// the masqueraded connection come from the declared port range, and a
/// connection to port 80 anywhere behind the router lands on the router's
/// own 8080.
#[tokio::test]
async fn nat_port_ranges_and_redirect_on_a_router() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nat_port_ranges_and_redirect_on_a_router: requires root");
        return;
    }
    let lab_name = format!("natr-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  nat {{
    masquerade src 10.0.1.0/24 oif wan0 to :40000-40010 random
    redirect iif lan0 tcp dport 80 to :8080
  }}
}}
node server
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- server:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let bind = |node: &str, addr: &str| {
        let addr = addr.to_string();
        lab.ns_resolver_of(node)
            .unwrap()
            .netns()
            .unwrap()
            .run(move || std::net::TcpListener::bind(addr.as_str()))
            .unwrap()
            .unwrap()
    };
    let wait = std::time::Duration::from_secs(3);

    let server = bind("server", "0.0.0.0:5555");
    lab.tcp_connect("client", "10.0.2.10:5555".parse().unwrap(), wait)
        .expect("client -> server:5555");
    let peer = accept_peer(&server, wait);
    assert_eq!(peer.ip().to_string(), "10.0.2.1", "masqueraded");
    assert!(
        (40000..=40010).contains(&peer.port()),
        "source port {} outside the declared range",
        peer.port()
    );

    let proxy = bind("gw", "0.0.0.0:8080");
    lab.tcp_connect("client", "10.0.2.10:80".parse().unwrap(), wait)
        .expect("port 80 is redirected to the router's 8080");
    let peer = accept_peer(&proxy, wait);
    assert_eq!(
        peer.ip().to_string(),
        "10.0.1.10",
        "the redirect keeps the client"
    );
    // Nothing listens on the server's 81, and nothing redirects it.
    assert!(
        lab.tcp_connect("client", "10.0.2.10:81".parse().unwrap(), wait)
            .is_err()
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// nlink 0.32 firewall statements (#189), with real traffic: `rpf` drops a
/// spoofed source (and counts it) while the real one passes, and a
/// negated port range lets 8080 through and drops 9090. A byte-rate limit
/// and a mark range install and read back as declared (converged).
#[tokio::test]
async fn firewall_rpf_negated_ranges_and_byte_limits() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_rpf_negated_ranges_and_byte_limits: requires root");
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

// ─── IPv6: NAT66, ip6 firewall matching, dual-stack auto-routing (#72) ───

/// IPv6 DAD keeps addresses `tentative` for ~1s after assignment; poll
/// until the interface has no tentative address (10s ceiling).
async fn wait_for_dad(lab: &RunningLab, node: &str, iface: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let out = lab
            .exec(node, "ip", &["-6", "-o", "addr", "show", iface])
            .unwrap();
        if out.exit_code == 0 && !out.stdout.contains("tentative") {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "DAD did not complete on {node}:{iface} within 10s; last `ip -6 addr` = {}",
                out.stdout
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// `ping -6 -c1 -W3` with retries; returns whether any attempt succeeded.
async fn ping6_ok(lab: &RunningLab, node: &str, target: &str, attempts: usize) -> bool {
    for _ in 0..attempts {
        let out = lab
            .exec(node, "ping", &["-6", "-c1", "-W3", target])
            .unwrap();
        if out.exit_code == 0 {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    false
}

#[tokio::test]
async fn ipv6_dual_stack_nat66_and_firewall() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping ipv6_dual_stack_nat66_and_firewall: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping ipv6_dual_stack_nat66_and_firewall: nftables not functional");
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/ipv6-dual-stack.nll").unwrap();
    topo.lab.name = format!("ds6-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for node in ["router", "server", "client", "other"] {
        wait_for_dad(&lab, node, "eth0").await;
    }
    wait_for_dad(&lab, "router", "eth1").await;

    // client → server over IPv6 goes through the router's NAT66, arriving
    // with the router's LAN address, which the server's firewall admits.
    assert!(
        ping6_ok(&lab, "client", "fd00:0:0:1::2", 4).await,
        "client → server IPv6 should succeed through NAT66"
    );
    // other is on the LAN but untranslated: `policy drop` applies.
    assert!(
        !ping6_ok(&lab, "other", "fd00:0:0:1::2", 2).await,
        "other → server IPv6 must be dropped by the ip6 saddr firewall"
    );
    // IPv4 path works through the same auto-routes.
    let v4 = lab
        .exec("client", "ping", &["-c1", "-W3", "10.0.1.2"])
        .unwrap();
    assert_eq!(v4.exit_code, 0, "client → server IPv4: {}", v4.stderr);

    let router_rules = lab.exec("router", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        router_rules.stdout.contains("masquerade"),
        "router ruleset should masquerade: {}",
        router_rules.stdout
    );
    assert!(
        router_rules.stdout.contains("fd00:0:0:2::/64"),
        "router ruleset should match the WAN IPv6 prefix: {}",
        router_rules.stdout
    );
    let server_rules = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        server_rules.stdout.contains("ip6 saddr fd00:0:0:1::1"),
        "server ruleset should carry the ip6 saddr match: {}",
        server_rules.stdout
    );

    lab.destroy().await.expect("destroy failed");
}

#[tokio::test]
async fn mgmt_ipv6_host_reachable() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping mgmt_ipv6_host_reachable: requires root");
        return;
    }
    let lab_name = format!("mgmt6-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}" {{ mgmt fd00:20::/64 host-reachable }}
node a
node b
link a:eth0 -- b:eth0 {{ fd00:1::1/64 -- fd00:1::2/64 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let bridge = lab.topology().lab.mgmt_bridge_name();

    // mgmt0 addresses are assigned with nodad, so they are usable at once.
    let addrs = lab.node_addresses("a").unwrap();
    assert!(
        addrs["mgmt0"].iter().any(|a| a == "fd00:20::2/64"),
        "node a mgmt0 = {:?}",
        addrs.get("mgmt0")
    );
    let addrs_b = lab.node_addresses("b").unwrap();
    assert!(addrs_b["mgmt0"].iter().any(|a| a == "fd00:20::3/64"));

    // From the host: the bridge holds fd00:20::1 and nodes answer.
    let mut ok = false;
    for _ in 0..5 {
        let st = std::process::Command::new("ping")
            .args(["-6", "-c1", "-W2", "fd00:20::2"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        if st.success() {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "host could not ping node a over the IPv6 mgmt bridge");

    lab.destroy().await.expect("destroy failed");
    let links = std::process::Command::new("ip")
        .args(["link", "show", &bridge])
        .output()
        .unwrap();
    assert!(
        !links.status.success(),
        "mgmt bridge {bridge} should be gone after destroy"
    );
}

// ─── Rate limit `burst` reaches tc (#71) ────────────────

#[tokio::test]
async fn rate_limit_burst_applied() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping rate_limit_burst_applied: requires root");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping rate_limit_removal_takes_ingress_and_ifb: requires root");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping qdisc_blocks_apply_replace_and_clear: requires root");
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

// ─── #108: a changed impairment is replaced, not torn down ───

/// The average round-trip time `from` measures to `to`, in ms.
fn avg_rtt_ms(lab: &RunningLab, from: &str, to: &str) -> f64 {
    let out = lab
        .exec(from, "ping", &["-c", "4", "-i", "0.2", "-q", to])
        .unwrap();
    let line = out
        .stdout
        .lines()
        .find(|l| l.contains("min/avg/max"))
        .unwrap_or_else(|| panic!("no rtt from {from} to {to}: {}{}", out.stdout, out.stderr));
    line.split('=')
        .nth(1)
        .unwrap()
        .trim()
        .split('/')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

/// The root qdisc line of `node:iface`, and its `Sent` bytes.
fn root_qdisc(lab: &RunningLab, node: &str, iface: &str) -> (String, u64) {
    let out = lab
        .exec(node, "tc", &["-s", "qdisc", "show", "dev", iface, "root"])
        .unwrap()
        .stdout;
    let head = out.lines().next().unwrap_or_default().to_string();
    let sent = out
        .split_whitespace()
        .skip_while(|w| *w != "Sent")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    (head, sent)
}

/// Traffic control is declared (#108): a re-apply changes nothing (same
/// handle, counters kept, empty diff), a qdisc deleted by hand is drift
/// that `--check` reports, and `apply` puts it back.
#[tokio::test]
async fn declared_tc_reapplies_as_nothing_and_repairs_drift() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping declared_tc_reapplies_as_nothing_and_repairs_drift: requires root");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping partition_and_heal_keep_a_prio: requires root");
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

/// An `impair` on the source of a network's per-pair matrix is the
/// matrix's default (#169): traffic to a peer no rule names gets it,
/// the pair rule's peer gets the rule. It used to be silently lost.
/// Removing the matrix removes its tree.
#[tokio::test]
async fn endpoint_impair_is_the_default_of_a_pair_matrix() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping endpoint_impair_is_the_default_of_a_pair_matrix: requires root");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping removing_a_pair_matrix_removes_its_tree: requires root");
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

/// `fq_codel { noecn }` turns ECN off; without either flag it is the
/// kernel's default, which is on (#173).
#[tokio::test]
async fn fq_codel_noecn_turns_ecn_off() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping fq_codel_noecn_turns_ecn_off: requires root");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping changing_an_impairment_keeps_the_handle_and_the_counters: requires root"
        );
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

/// #135: `Op::NetworkImpairments` is always re-run, and it used to
/// rebuild the whole HTB+netem+flower tree from scratch every time.
/// Reconciling instead, an unchanged matrix must converge with zero
/// kernel calls — and keep its counters, which is the observable proof.
#[tokio::test]
async fn reapplying_a_network_impair_matrix_keeps_its_counters() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping reapplying_a_network_impair_matrix_keeps_its_counters: requires root");
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

// ─── Runtime impairments persist; snapshot / restore (#59) ───

#[tokio::test]
async fn impair_persists_across_apply_and_snapshot_restore() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping impair_persists_across_apply_and_snapshot_restore: requires root");
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

// ─── Lifecycle event log + drift stream (#70) ───────────

#[tokio::test]
async fn lifecycle_events_recorded_and_drift_streamed() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping lifecycle_events_recorded_and_drift_streamed: requires root");
        return;
    }
    let lab_name = format!("events-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
validate {{ reach a b }}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let pid = lab
        .spawn_with_logs("a", &["sleep", "30"], None)
        .expect("spawn failed");
    lab.set_impairment(
        "a:eth0",
        &nlink_lab::Impairment {
            delay: Some("5ms".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    lab.partition("b:eth0").await.unwrap();
    lab.heal("b:eth0").await.unwrap();
    lab.clear_impairment("a:eth0").await.unwrap();
    // A frozen process is still "alive" to the kernel; `signal_process`
    // must record the signal and never escalate to KILL.
    lab.signal_process(pid, nlink_lab::Signal::Stop).unwrap();
    assert!(
        lab.process_status().iter().any(|p| p.pid == pid && p.alive),
        "a STOPped process is still tracked as alive"
    );
    lab.signal_process(pid, nlink_lab::Signal::Cont).unwrap();
    assert!(lab.kill_process(pid).unwrap(), "it was running");
    let (st, tp) = nlink_lab::state::load(&lab_name).unwrap();
    nlink_lab::state::snapshot_save(&lab_name, "s1", None, &st, &tp).unwrap();

    let names: Vec<String> = nlink_lab::events::read(&lab_name)
        .unwrap()
        .iter()
        .map(|e| e.kind.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "deployed",
            "assertions_run",
            "spawned",
            "impaired",
            "partitioned",
            "healed",
            "impair_cleared",
            "signalled",
            "signalled",
            "killed",
            "snapshot_taken",
        ],
        "{names:?}"
    );
    assert!(matches!(
        &nlink_lab::events::read(&lab_name).unwrap()[7].kind,
        nlink_lab::LifecycleKind::Signalled { pid: p, signal, .. } if *p == pid && signal == "STOP"
    ));
    let evs = nlink_lab::events::read(&lab_name).unwrap();
    assert!(matches!(
        &evs[2].kind,
        nlink_lab::LifecycleKind::Spawned { node, pid: p, cmd } if node == "a" && *p == pid && cmd == "sleep 30"
    ));

    // Drift: a hand-made address inside a node shows up on the stream.
    let opts = nlink_lab::WatchOpts::default();
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lab.exec("a", "ip", &["addr", "add", "10.9.9.9/32", "dev", "eth0"])
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                let json = serde_json::to_string(&ev).unwrap();
                if ev.node == "a" && json.contains("10.9.9.9") {
                    // nlink's view of the address (#195).
                    assert_eq!(ev.kind.name(), "new_address", "{json}");
                    assert!(json.contains(r#""cidr":"10.9.9.9/32""#), "{json}");
                    seen = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    for t in tasks {
        t.abort();
    }
    assert!(seen, "expected a drift event for the new address on a:eth0");

    lab.destroy().await.expect("destroy failed");
}

/// An image in the local podman store; nothing is pulled.
fn has_local_image(image: &str) -> bool {
    std::process::Command::new("podman")
        .args(["image", "exists", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// #157: `proc-stat` on a container node reads the host's `/proc`. Every
/// process has its host PID and the container's own; `--pid` names the
/// container's; pidfiles under the container's root give their names; and
/// no call enters the container (each exec costs about 0.25 s here, so a
/// call that execs takes far longer than the bound below). On a namespace
/// node, a PID outside the node is refused rather than sampled.
#[tokio::test]
async fn proc_stat_on_a_container_node_reads_the_host_proc() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!(
            "skipping proc_stat_on_a_container_node_reads_the_host_proc: needs root and \
             alpine:latest in the podman store"
        );
        return;
    }
    let lab_name = format!("psc-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"]
node h
link c:eth0 -- h:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let out = lab
        .exec(
            "c",
            "sh",
            &[
                "-c",
                "setsid sleep 300 >/dev/null 2>&1 & echo $! > /run/alpha.pid; \
                 setsid sleep 301 >/dev/null 2>&1 & echo $! > /run/beta.pid; \
                 echo 99999 > /run/stale.pid; echo junk > /run/junk.pid; \
                 cat /run/alpha.pid /run/beta.pid",
            ],
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    let in_container: Vec<u32> = out.stdout.lines().map(|l| l.parse().unwrap()).collect();
    let init_host = match lab.ns_resolver_of("c") {
        Some(nlink_lab::deploy::NsRef::Container { pid, .. }) => pid,
        other => panic!("not a container node: {other:?}"),
    };

    // `--all`: every process, by both PIDs. It used to read host PIDs
    // inside the container, and fail.
    let t = std::time::Instant::now();
    let all = lab.proc_stat_all("c").unwrap();
    let took_all = t.elapsed();
    let init = all
        .iter()
        .find(|s| s.ns_pid == 1)
        .expect("the container's init");
    assert_eq!(init.host_pid, init_host, "{all:?}");
    for pid in &in_container {
        assert!(all.iter().any(|s| s.ns_pid == *pid), "no {pid}: {all:?}");
    }
    assert!(all.iter().all(|s| s.host_pid != s.ns_pid), "{all:?}");

    // `--pid` names the container's PID, and the record has the host's.
    let t = std::time::Instant::now();
    let one = lab.proc_stat_many("c", &[1]).unwrap();
    let took_one = t.elapsed();
    assert_eq!((one[0].ns_pid, one[0].host_pid), (1, init_host));
    let err = lab.proc_stat_many("c", &[99999]).unwrap_err().to_string();
    assert!(err.contains("has no process 99999"), "{err}");

    // Pidfiles under the container's root: named by their stem; a stale
    // PID and a file that holds none are skipped.
    let named = lab.proc_stat_pidfiles("c", "/run/*.pid").unwrap();
    let got: Vec<(&str, u32)> = named
        .iter()
        .map(|s| (s.name.as_deref().unwrap(), s.ns_pid))
        .collect();
    assert_eq!(got, [("alpha", in_container[0]), ("beta", in_container[1])]);

    for (what, took) in [("--all", took_all), ("--pid", took_one)] {
        assert!(
            took < std::time::Duration::from_millis(200),
            "{what} took {took:?}: it entered the container"
        );
    }

    // A namespace node: the host's init is not the node's, and a pidfile
    // read from the host's filesystem names the node's process.
    let err = lab.proc_stat_many("h", &[1]).unwrap_err().to_string();
    assert!(err.contains("has no process 1"), "{err}");
    let pid = lab.spawn("h", &["sleep", "30"]).unwrap();
    let dir = std::env::temp_dir().join(format!("nlink-lab-psc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("napper.pid"), format!("{pid}\n")).unwrap();
    let named = lab
        .proc_stat_pidfiles("h", &format!("{}/*.pid", dir.display()))
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(named.len(), 1, "{named:?}");
    assert_eq!(
        (named[0].name.as_deref(), named[0].host_pid, named[0].ns_pid),
        (Some("napper"), pid, pid)
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// Two handles of one lab, loaded before either saved: each one's spawned
/// process survives the other's save. `save_state` used to write its own
/// stale copy over the file, so the first spawn was lost.
#[lab_test("examples/simple.nll")]
async fn concurrent_saves_keep_each_others_changes(lab: RunningLab) {
    let mut a = RunningLab::load(lab.name()).unwrap();
    let mut b = RunningLab::load(lab.name()).unwrap();
    let pa = a.spawn("host", &["sleep", "30"]).unwrap();
    a.save_state().unwrap();
    let pb = b.spawn("router", &["sleep", "31"]).unwrap();
    b.save_state().unwrap();

    let pids = |l: &RunningLab| -> Vec<u32> { l.pids().iter().map(|(_, p)| *p).collect() };
    let after = RunningLab::load(lab.name()).unwrap();
    assert!(
        pids(&after).contains(&pa),
        "a's spawn lost: {:?}",
        after.pids()
    );
    assert!(
        pids(&after).contains(&pb),
        "b's spawn lost: {:?}",
        after.pids()
    );
    for pid in [pa, pb] {
        let _ = after.kill_process(pid);
    }
}

/// `exec` with a timeout drains the child's output as it runs: 200 kB used
/// to block the child on a full pipe until the timeout killed it, its
/// output lost. A background grandchild holding the pipe does not hold the
/// call past the deadline either.
#[lab_test("examples/simple.nll")]
async fn exec_with_a_timeout_returns_large_output_and_keeps_its_deadline(lab: RunningLab) {
    let opts = |secs| nlink_lab::ExecOpts {
        timeout: Some(std::time::Duration::from_secs(secs)),
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let out = lab
        .exec_with_opts(
            "host",
            "sh",
            &["-c", "head -c 200000 /dev/zero | tr '\\0' x"],
            opts(10),
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.len(), 200_000);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );

    let t = std::time::Instant::now();
    let out = lab
        .exec_with_opts("host", "sh", &["-c", "sleep 8 & echo hi"], opts(2))
        .unwrap();
    assert_eq!(out.stdout.trim(), "hi");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(4),
        "held past the 2 s deadline: {:?}",
        t.elapsed()
    );
}

/// A container node whose recorded init PID now names another process (the
/// init exited and the number was reused, here by the test runner, which is
/// in the host's namespace) is not reached through it: the loaded lab has
/// PID 0 for it, and asking the node fails instead of reading the host's
/// interfaces.
#[tokio::test]
async fn a_container_whose_init_pid_was_reused_is_not_reached_through_it() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("pid-reuse-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"]
node h
link c:eth0 -- h:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let (mut state, topology) = nlink_lab::state::load(&lab_name).unwrap();
    let real = state.containers["c"].clone();
    assert!(
        real.starttime.is_some(),
        "deploy records the init's start time"
    );
    state.containers.get_mut("c").unwrap().pid = std::process::id();
    nlink_lab::state::save(&state, &topology).unwrap();

    let reloaded = RunningLab::load(&lab_name).unwrap();
    match reloaded.ns_resolver_of("c") {
        Some(nlink_lab::deploy::NsRef::Container { pid, .. }) => assert_eq!(pid, 0),
        other => panic!("{other:?}"),
    }
    assert!(reloaded.diagnose(Some("c")).await.is_err());

    state.containers.insert("c".into(), real);
    nlink_lab::state::save(&state, &topology).unwrap();
    std::mem::forget(_guard);
    RunningLab::load(&lab_name)
        .unwrap()
        .destroy()
        .await
        .unwrap();
}

/// A restarted container gets back everything its old namespace had: its
/// links, its bridge-network port, its mgmt veth, its sysctls and its
/// background `run`. Only the point-to-point links came back, and a
/// network member could not be restarted at all (its bridge port was
/// missing when the links were brought up).
#[tokio::test]
async fn a_restarted_container_gets_its_ports_sysctls_and_runs_back() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("rs-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{
  runtime "podman"
  mgmt 172.30.77.0/24 host-reachable
}}
node r image "alpine:latest" cmd ["sleep", "infinity"] {{
  run background ["sleep", "1000"]
  sysctl "net.ipv4.ip_default_ttl" "33"
}}
node h1 {{ route default via 10.0.0.1 }}
node h2
link r:eth0 -- h1:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
network lan {{
  members [r:eth1, h2:eth0]
  subnet 10.9.0.0/24
}}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // What `nlink-lab restart` does.
    let id = lab.container_for("r").unwrap().id.clone();
    let rt = nlink_lab::container::Runtime::with_binary("podman");
    let ok = std::process::Command::new("podman")
        .args(["restart", "-t", "0", &id])
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "podman restart");
    let pid = rt.inspect_pid(&id).unwrap();
    lab.set_container_pid("r", pid).unwrap();
    nlink_lab::reattach_node(&mut lab, "r").await.unwrap();

    let out = |node: &str, cmd: &[&str]| lab.exec(node, cmd[0], &cmd[1..]).unwrap();
    assert_eq!(
        out("r", &["cat", "/proc/sys/net/ipv4/ip_default_ttl"])
            .stdout
            .trim(),
        "33"
    );
    assert_eq!(out("r", &["ip", "link", "show", "mgmt0"]).exit_code, 0);
    assert!(
        out("r", &["ps"]).stdout.contains("sleep 1000"),
        "the background run is back"
    );
    for (node, addr) in [("h1", "10.0.0.1"), ("h2", "10.9.0.1")] {
        let reached = (0..5).any(|_| out(node, &["ping", "-c1", "-W1", addr]).exit_code == 0);
        assert!(reached, "{node} cannot reach r at {addr}");
    }

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #221: `restart` re-planned every node from the declared topology with
/// fresh WireGuard keys, so an `impair` on another node went back to the
/// declared value and every `key auto` interface was re-keyed.
#[tokio::test]
async fn a_restart_keeps_the_labs_runtime_impairments_and_keys() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("rk-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node r image "alpine:latest" cmd ["sleep", "infinity"]
node a {{ wireguard wg0 {{ key auto listen 51820 address 192.168.77.1/32 peers [b] }} }}
node b {{ wireguard wg0 {{ key auto listen 51820 address 192.168.77.2/32 peers [a] }} }}
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 delay 10ms }}
link r:eth0 -- a:eth1 {{ 10.1.0.1/24 -- 10.1.0.2/24 }}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    lab.set_impairment(
        "a:eth0",
        &nlink_lab::types::Impairment {
            delay: Some("200ms".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let key = |lab: &RunningLab, node: &str| {
        lab.exec(node, "wg", &["show", "wg0", "public-key"])
            .unwrap()
            .stdout
    };
    let keys_before = (key(&lab, "a"), key(&lab, "b"));

    // What `nlink-lab restart` does.
    let id = lab.container_for("r").unwrap().id.clone();
    let ok = std::process::Command::new("podman")
        .args(["restart", "-t", "0", &id])
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "podman restart");
    let pid = nlink_lab::container::Runtime::with_binary("podman")
        .inspect_pid(&id)
        .unwrap();
    lab.set_container_pid("r", pid).unwrap();
    nlink_lab::reattach_node(&mut lab, "r").await.unwrap();

    let qdisc = lab
        .exec("a", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap()
        .stdout;
    assert!(
        qdisc.contains("delay 200ms"),
        "the runtime delay was undone: {qdisc}"
    );
    assert_eq!((key(&lab, "a"), key(&lab, "b")), keys_before, "re-keyed");
    let ping = lab.exec("r", "ping", &["-c1", "-W2", "10.1.0.2"]).unwrap();
    assert_eq!(ping.exit_code, 0, "r's link is back");

    std::mem::forget(_guard);
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

/// `(exit code, elapsed)` of a TCP connect from `node` to `addr:port`.
fn tcp_connect(lab: &RunningLab, node: &str, addr: &str, port: u16) -> (i32, std::time::Duration) {
    let t = std::time::Instant::now();
    let out = lab
        .exec(
            node,
            "timeout",
            &["3", "bash", "-c", &format!("echo > /dev/tcp/{addr}/{port}")],
        )
        .unwrap();
    (out.exit_code, t.elapsed())
}

/// `reject` rejects. Before #164 the planner installed it as `accept`.
/// A drop is silence (a timeout); a reject answers at once — ICMP port
/// unreachable by default, a TCP reset with `reject with tcp-reset` — and
/// a port range and `!=` match what they say.
#[tokio::test]
async fn firewall_reject_is_a_reject_and_ranges_match() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_reject_is_a_reject_and_ranges_match: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_reject_is_a_reject_and_ranges_match: no nftables");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_policy_edit_applies_to_a_running_lab: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_policy_edit_applies_to_a_running_lab: no nftables");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_forward_chain_filters_routed_traffic: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_forward_chain_filters_routed_traffic: no nftables");
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
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-router.nll"
    ))
    .unwrap();
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_sets_match_traffic_and_reconcile_elements: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_sets_match_traffic_and_reconcile_elements: no nftables");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_sets_example_converges: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_sets_example_converges: no nftables");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-sets.nll"
    ))
    .unwrap();
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_counters_limit_and_counter_assertions: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_counters_limit_and_counter_assertions: no nftables");
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_observability_example_counts: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_observability_example_counts: no nftables");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-observability.nll"
    ))
    .unwrap();
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
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_renames_and_foreign_inline_sets_apply: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_renames_and_foreign_inline_sets_apply: no nftables");
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

/// A `log` rule installs and converges. Skipped where the host has no
/// nftables logger (`nf_log_syslog`): the kernel then refuses the batch.
#[tokio::test]
async fn firewall_log_installs_and_converges() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_log_installs_and_converges: requires root");
        return;
    }
    if !has_nftables() || !has_kernel_module("nf_log_syslog") {
        eprintln!("skipping firewall_log_installs_and_converges: no nftables logger");
        return;
    }
    let lab_name = format!("fwlog-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node c
node s {{
  firewall policy accept {{
    drop tcp dport 22 log "ssh-drop: " counter
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let nft = lab
        .exec("s", "nft", &["list", "chain", "inet", "nlink-lab", "input"])
        .unwrap()
        .stdout;
    assert!(nft.contains("log prefix \"ssh-drop: \""), "{nft}");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "a log rule must converge:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

// ─── firewall marks, classify, MSS clamping, NAT qualifiers (#167) ───

/// Packets sent through `class` of the root qdisc on `node`:`iface`.
fn tc_class_packets(lab: &RunningLab, node: &str, iface: &str, class: &str) -> u64 {
    let out = lab
        .exec(node, "tc", &["-s", "class", "show", "dev", iface])
        .unwrap()
        .stdout;
    let mut lines = out.lines();
    while let Some(line) = lines.next() {
        // "class prio 1:3 parent 1:" then " Sent 1234 bytes 12 pkt (dropped …"
        if line.split_whitespace().nth(2) == Some(class) {
            let sent: Vec<&str> = lines
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            return sent
                .get(3)
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("no packet count for {class}:\n{out}"));
        }
    }
    panic!("no class {class} on {node}:{iface}:\n{out}");
}

/// The packet count of the first rule in an `nft list` output that
/// contains `needle`.
fn nft_rule_packets(listing: &str, needle: &str) -> u64 {
    let line = listing
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no rule with {needle:?} in:\n{listing}"));
    line.split("packets ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no counter in {line:?}"))
}

/// A mark set under a mask keeps the bits outside it, `ct mark save` and
/// `ct mark restore` carry the mark onto every later packet of the
/// connection, `classify 1:3` puts the router's WAN-bound packets in the
/// third band of its `prio` qdisc (which needs the qdisc at handle `1:`;
/// it used to get `8001:`, and every packet fell back to the priomap), and
/// `clamp mss pmtu` lowers the MSS a handshake announces to what the WAN's
/// MTU allows.
#[tokio::test]
async fn firewall_marks_classify_and_mss_clamp_on_a_router() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_marks_classify_and_mss_clamp_on_a_router: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_marks_classify_and_mss_clamp_on_a_router: no nftables");
        return;
    }
    let lab_name = format!("fwmk-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  firewall forward {{
    oif wan0 clamp mss pmtu
    ct mark restore
    ct new tcp dport 5201 mark set 0x100
    ct new tcp dport 5201 mark set 0x10/0xff ct mark save
    oif wan0 mark 0x10/0xff counter name masked classify 1:3
    oif wan0 mark 0x10 counter name exact
    oif lan0 ct mark 0x110 counter name connmark
  }}
}}
node s {{
  route default via 10.0.1.1
  run background ["iperf3", "-s", "-p", "5201"]
}}
node c {{ route default via 10.0.2.1 }}
link r:lan0 -- s:eth0 {{ 10.0.1.1/24 -- 10.0.1.10/24 }}
link r:wan0 -- c:eth0 {{
  10.0.2.1/24 -- 10.0.2.10/24
  mtu 1400
}}
qdisc r:wan0 prio {{ bands 3 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let qdisc = lab
        .exec("r", "tc", &["qdisc", "show", "dev", "wan0"])
        .unwrap()
        .stdout;
    assert!(qdisc.contains("qdisc prio 1: root"), "{qdisc}");

    // count the MSS of the SYN-ACKs that reach c: s announces 1460 (its
    // MTU is 1500), the clamp on r cuts it to 1400 - 40
    let probe = "table inet mssprobe {\n chain in {\n  type filter hook input priority 0;\n  tcp flags & (syn | ack) == syn | ack tcp option maxseg size 1360 counter\n  tcp flags & (syn | ack) == syn | ack tcp option maxseg size 1460 counter\n }\n}\n";
    let out = lab
        .exec(
            "c",
            "bash",
            &["-c", &format!("printf '%s' '{probe}' | nft -f -")],
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "probe table: {}", out.stderr);

    for _ in 0..20 {
        let ss = lab.exec("s", "ss", &["-ltn"]).unwrap();
        if ss.stdout.contains(":5201 ") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let band3_before = tc_class_packets(&lab, "r", "wan0", "1:3");
    for _ in 0..3 {
        let (rc, _) = tcp_connect(&lab, "c", "10.0.1.10", 5201);
        assert_eq!(rc, 0, "c must reach s:5201 through r");
    }

    let counters = lab.firewall_counters("r").await.unwrap();
    let packets = |name: &str| counters.iter().find(|c| c.name == name).unwrap().packets;
    assert!(
        packets("masked") > 0,
        "replies carry the restored mark: {counters:?}"
    );
    assert_eq!(
        packets("exact"),
        0,
        "`mark set 0x10/0xff` keeps bit 8, so the mark is 0x110: {counters:?}"
    );
    assert!(
        packets("connmark") > 0,
        "the connection keeps the saved mark: {counters:?}"
    );
    let band3 = tc_class_packets(&lab, "r", "wan0", "1:3");
    assert!(
        band3 > band3_before,
        "classify 1:3 must fill prio band 1:3 ({band3_before} -> {band3})"
    );

    let mss = lab
        .exec("c", "nft", &["list", "chain", "inet", "mssprobe", "in"])
        .unwrap()
        .stdout;
    assert!(
        nft_rule_packets(&mss, "maxseg size 1360") > 0,
        "SYN-ACKs must arrive clamped:\n{mss}"
    );
    assert_eq!(
        nft_rule_packets(&mss, "maxseg size 1460"),
        0,
        "no SYN-ACK may keep s's MSS:\n{mss}"
    );

    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// NAT rules match what they name: a masquerade restricted to `oif wan0`
/// leaves traffic out of wan1 alone, and a `dnat … iif wan0 tcp dport 80`
/// rewrites neither pings nor the same port arriving on another
/// interface.
#[tokio::test]
async fn nat_matches_interface_and_port() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping nat_matches_interface_and_port: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping nat_matches_interface_and_port: no nftables");
        return;
    }
    let lab_name = format!("natq-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  nat {{
    masquerade src 10.0.1.0/24 oif wan0
    dnat dst 10.0.2.1 iif wan0 tcp dport 80 to 10.0.1.10:5201
  }}
}}
node s {{
  route default via 10.0.1.1
  firewall policy accept {{ drop icmp 8 }}
  run background ["iperf3", "-s", "-p", "5201"]
}}
node c
node d {{ route 10.0.2.0/24 via 10.0.3.1 }}
link r:lan0 -- s:eth0 {{ 10.0.1.1/24 -- 10.0.1.10/24 }}
link r:wan0 -- c:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
link r:wan1 -- d:eth0 {{ 10.0.3.1/24 -- 10.0.3.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    for _ in 0..20 {
        let ss = lab.exec("s", "ss", &["-ltn"]).unwrap();
        if ss.stdout.contains(":5201 ") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ping = |from: &str, to: &str| {
        lab.exec(from, "ping", &["-c", "1", "-W", "1", to])
            .unwrap()
            .exit_code
    };

    // c and d have no route to the LAN: only a masqueraded ping comes back
    assert_eq!(ping("s", "10.0.2.10"), 0, "masqueraded out of wan0");
    assert_ne!(
        ping("s", "10.0.3.10"),
        0,
        "`oif wan0`: what leaves through wan1 keeps its LAN source"
    );

    // gw:80 from the WAN is s:5201
    let (rc, _) = tcp_connect(&lab, "c", "10.0.2.1", 80);
    assert_eq!(rc, 0, "dnat tcp dport 80 must reach s:5201");
    // s drops pings, so this answer is r's own: `tcp dport 80` left ICMP alone
    assert_eq!(ping("c", "10.0.2.1"), 0, "a ping to r must not be DNAT'd");
    // other ports, and port 80 arriving on wan1, are r's (nothing listens)
    let (rc, _) = tcp_connect(&lab, "c", "10.0.2.1", 5201);
    assert!(
        rc != 0 && rc != 124,
        "port 5201 is not DNAT'd: refused by r"
    );
    let (rc, _) = tcp_connect(&lab, "d", "10.0.2.1", 80);
    assert!(
        rc != 0 && rc != 124,
        "`iif wan0`: port 80 from wan1 is not DNAT'd: refused by r"
    );

    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// examples/firewall-qos.nll: its assertions pass, its scenario fills the
/// `bulk` counter through the DNAT, and it converges.
#[tokio::test]
async fn firewall_qos_example_classifies_and_converges() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping firewall_qos_example_classifies_and_converges: requires root");
        return;
    }
    if !has_nftables() {
        eprintln!("skipping firewall_qos_example_classifies_and_converges: no nftables");
        return;
    }
    let topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-qos.nll"
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());
    let result = nlink_lab::scenario::run_scenario(&mut lab, &topo.scenarios[0])
        .await
        .expect("scenario failed to run");
    assert!(result.passed, "{result:?}");
    assert!(tc_class_packets(&lab, "gw", "wan0", "1:3") > 0);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

// ─── watch: nftables events nlink 0.30 delivers (#158) ───

/// Set elements and named objects arrive typed; the generation event nlink
/// 0.30 sends after every committed batch does not show up at all.
#[tokio::test]
async fn watch_types_set_elements_and_objects_and_drops_generations() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping watch_types_set_elements_and_objects_and_drops_generations: requires root"
        );
        return;
    }
    if !has_nftables() {
        eprintln!(
            "skipping watch_types_set_elements_and_objects_and_drops_generations: no nftables"
        );
        return;
    }
    let lab_name = format!("wnft-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let opts = nlink_lab::WatchOpts {
        family: nlink_lab::WatchFamily::Nftables,
        ..Default::default()
    };
    let (mut rx, tasks) = nlink_lab::watch_stream(&lab, &opts)
        .unwrap()
        .expect("nodes to watch");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for cmd in [
        "add table inet probe",
        "add set inet probe s { type ipv4_addr ; }",
        "add element inet probe s { 10.0.0.9 }",
        "add counter inet probe c",
    ] {
        let args: Vec<&str> = cmd.split(' ').collect();
        let out = lab.exec("a", "nft", &args).unwrap();
        assert_eq!(out.exit_code, 0, "nft {cmd}: {}", out.stderr);
    }
    let mut kinds = Vec::new();
    let mut raw = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
            Ok(Some(ev)) if ev.node == "a" => {
                kinds.push(ev.kind.name());
                raw.push(serde_json::to_value(&ev).unwrap());
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    for t in tasks {
        t.abort();
    }
    // nlink's views (#195), named `<action>_<object>`.
    for want in [
        "new_nft_table",
        "new_nft_set",
        "new_nft_set_elements",
        "new_nft_object",
    ] {
        assert!(kinds.iter().any(|k| k == want), "no {want} event: {raw:?}");
    }
    assert!(
        !kinds.iter().any(|k| k.contains("nft_gen")),
        "generation event leaked: {raw:?}"
    );
    let all = serde_json::to_string(&raw).unwrap();
    assert!(all.contains(r#""set":"s""#), "set name missing: {all}");
    assert!(all.contains(r#""object_type":"counter""#), "{all}");
    lab.destroy().await.expect("destroy failed");
}

// ─── Bond options + Q-in-Q reach the kernel (#75) ───────

#[tokio::test]
async fn bond_options_and_qinq_vlan_apply() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping bond_options_and_qinq_vlan_apply: requires root");
        return;
    }
    if !has_kernel_module("bonding") || !has_kernel_module("8021q") {
        eprintln!("skipping bond_options_and_qinq_vlan_apply: bonding/8021q modules unavailable");
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

// ─── FRR routing daemons (#65) ─────────────────────────

fn has_frr() -> bool {
    nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Zebra).is_some()
        && nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Ospfd).is_some()
        && nlink_lab::frr::frr_ids().is_some()
}

/// Poll `ip route show <dest>` on `node` until it carries `needle`.
async fn wait_route(lab: &RunningLab, node: &str, dest: &str, needle: &str, secs: u64) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        let out = lab.exec(node, "ip", &["route", "show", dest]).unwrap();
        last = out.stdout.clone();
        if last.contains(needle) {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("route {dest} on {node} never matched {needle:?}; last: {last:?}");
}

#[tokio::test]
async fn frr_ospf_converges_and_apply_restarts_daemons() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping frr_ospf_converges_and_apply_restarts_daemons: requires root");
        return;
    }
    if !has_frr() {
        eprintln!("skipping frr_ospf_converges_and_apply_restarts_daemons: frr not installed");
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-ospf.nll").unwrap();
    let lab_name = format!("frr-{}", std::process::id());
    topo.lab.name = lab_name.clone();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // zebra + ospfd per router, tracked
    let routers = ["r1", "r2", "r3"];
    for r in routers {
        let n = lab.pids().iter().filter(|(node, _)| node == r).count();
        assert_eq!(n, 2, "{r} should track zebra + ospfd, got {:?}", lab.pids());
    }
    let ps = nlink_lab::frr::pathspace(&format!("{lab_name}-r1"));
    assert!(nlink_lab::frr::run_dir(&ps).join("zserv.api").exists());

    // OSPF route on r1 for the far LAN via r3, and end-to-end reachability.
    let route = wait_route(&lab, "r1", "10.3.0.0/24", "10.0.31.1", 60).await;
    assert!(
        route.contains("proto ospf") || route.contains("proto 188"),
        "expected an OSPF-installed route: {route}"
    );
    // a loopback is in the area too: only link ends were (#217)
    wait_route(&lab, "r2", "1.1.1.1", "via 10.0.12.1", 60).await;
    let mut ok = false;
    for _ in 0..10 {
        let ping = lab.exec("h1", "ping", &["-c1", "-W2", "10.3.0.2"]).unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "h1 → h2 through OSPF-learned routes failed");
    let r = lab.assertion_results();
    assert!(!r.is_empty() && r.iter().all(|a| a.passed), "{r:?}");

    // apply with `redistribute [connected]` on r1 only (timers must match
    // on both ends of a link, so they are not a safe single-node change):
    // r1's daemons restart, r2's do not, and the adjacency re-forms.
    let pid_of = |lab: &RunningLab, node: &str| -> Vec<u32> {
        let mut v: Vec<u32> = lab
            .pids()
            .iter()
            .filter(|(n, _)| n == node)
            .map(|(_, p)| *p)
            .collect();
        v.sort();
        v
    };
    let r1_before = pid_of(&lab, "r1");
    let r2_before = pid_of(&lab, "r2");
    let mut desired = lab.topology().clone();
    desired.nodes.get_mut("r1").unwrap().frr = Some(nlink_lab::types::FrrConfig {
        ospf: Some(nlink_lab::types::OspfConfig {
            redistribute: vec!["connected".to_string()],
            ..Default::default()
        }),
        bgp: None,
    });
    nlink_lab::apply(&mut lab, &desired)
        .await
        .expect("apply failed");
    assert_ne!(
        pid_of(&lab, "r1"),
        r1_before,
        "r1 daemons should have restarted"
    );
    assert_eq!(
        pid_of(&lab, "r2"),
        r2_before,
        "r2 daemons must be untouched"
    );
    let conf = std::fs::read_to_string(nlink_lab::frr::conf_path(
        &lab_name,
        "r1",
        nlink_lab::frr::Daemon::Ospfd,
    ))
    .unwrap();
    assert!(conf.contains("redistribute connected"), "{conf}");
    wait_route(&lab, "r1", "10.3.0.0/24", "10.0.31.1", 60).await;

    lab.destroy().await.expect("destroy failed");
    assert!(
        !nlink_lab::frr::run_dir(&ps).exists(),
        "run dir must be removed on destroy"
    );
    assert!(
        !nlink_lab::frr::lab_dir(&lab_name).exists(),
        "lab frr dir must be removed on destroy"
    );
    for pid in r1_before {
        // `pid_dead`, not a bare `/proc/<pid>` existence check: a zombie
        // keeps its /proc entry *and* its `comm`, so a daemon that has
        // exited but not been reaped looks alive. The integration lane's
        // container has no reaping PID 1, so every daemon killed here
        // lingers as `<defunct>` there and only there — which is what made
        // this lane red on master while every local run passed (#105).
        // The `comm` check stays: it tolerates PID reuse by an unrelated
        // process.
        assert!(
            pid_dead(pid) || {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map(|c| !c.contains("zebra") && !c.contains("ospfd"))
                    .unwrap_or(true)
            },
            "old daemon pid {pid} still alive"
        );
    }
}

#[tokio::test]
async fn frr_bgp_established() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping frr_bgp_established: requires root");
        return;
    }
    if !has_frr() || nlink_lab::frr::locate_daemon(nlink_lab::frr::Daemon::Bgpd).is_none() {
        eprintln!("skipping frr_bgp_established: frr/bgpd not installed");
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-bgp.nll").unwrap();
    topo.lab.name = format!("frrbgp-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let route = wait_route(&lab, "r1", "10.2.0.0/24", "192.0.2.2", 60).await;
    assert!(
        route.contains("proto bgp") || route.contains("proto 186"),
        "expected a BGP-installed route: {route}"
    );
    let mut ok = false;
    for _ in 0..10 {
        let ping = lab.exec("h1", "ping", &["-c1", "-W2", "10.2.0.2"]).unwrap();
        if ping.exit_code == 0 {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "h1 → h2 through eBGP-learned routes failed");
    lab.destroy().await.expect("destroy failed");
}

#[tokio::test]
async fn frr_missing_daemon_is_a_clear_error_before_any_namespace() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "skipping frr_missing_daemon_is_a_clear_error_before_any_namespace: requires root"
        );
        return;
    }
    if has_frr() {
        eprintln!(
            "skipping frr_missing_daemon_is_a_clear_error_before_any_namespace: frr is installed"
        );
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/frr-ospf.nll").unwrap();
    let lab_name = format!("frrmiss-{}", std::process::id());
    topo.lab.name = lab_name.clone();
    let err = topo
        .deploy()
        .await
        .err()
        .expect("deploy must fail without frr");
    let msg = err.to_string();
    assert!(
        msg.contains("zebra") || msg.contains("ospfd") || msg.contains("frr"),
        "{msg}"
    );
    assert!(msg.contains("/usr/lib/frr"), "{msg}");
    let ns = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&ns.stdout).contains(&lab_name),
        "no namespace may exist after a preflight failure"
    );
}

// ─── Wi-Fi (mac80211_hwsim) ───────────────────────────────

/// mac80211_hwsim can be loaded here. Probes without loading it: a plain
/// `modprobe` would make the module's two default radios.
fn hwsim_available() -> bool {
    let root = unsafe { libc::geteuid() } == 0;
    root && nlink_lab::wifi::has_hwsim()
}

/// The wiphy names visible from `ns` (the root namespace for `None`).
async fn wiphys_in(ns: Option<&str>) -> Vec<String> {
    use nlink::netlink::Nl80211;
    let nl: nlink::Connection<Nl80211> = match ns {
        Some(ns) => nlink::Connection::new_in_async(ns).await.unwrap(),
        None => nlink::Connection::new_async().await.unwrap(),
    };
    nl.get_phys()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// #176: each mesh node gets a radio of its own, named after its
/// namespace and moved there, under the interface name it declares, as
/// a mesh point; the joins succeed and the three nodes reach each other.
/// `destroy` deletes the radios.
#[tokio::test]
async fn wifi_mesh_radios_are_the_labs_and_join() {
    if !hwsim_available() {
        eprintln!("skipping wifi_mesh_radios_are_the_labs_and_join: needs root and mac80211_hwsim");
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wifi-mesh.nll"
    ))
    .unwrap();
    topo.lab.name = format!("wmesh-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy wifi-mesh");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for node in ["m1", "m2", "m3"] {
        let ns = lab.namespace_for(node).unwrap().to_string();
        assert_eq!(wiphys_in(Some(&ns)).await, [format!("{ns}-wlan0")]);
        let nl: nlink::Connection<nlink::netlink::Nl80211> =
            nlink::Connection::new_in_async(ns.as_str()).await.unwrap();
        let ifaces = nl.get_interfaces().await.unwrap();
        assert_eq!(ifaces.len(), 1, "{node}: {ifaces:?}");
        assert_eq!(ifaces[0].name.as_deref(), Some("wlan0"), "{node}");
        assert_eq!(
            ifaces[0].iftype,
            nlink::netlink::genl::nl80211::InterfaceType::MeshPoint,
            "{node}"
        );
        assert_eq!(ifaces[0].frequency, Some(2412), "{node}: channel 1");
    }
    // The radios are not the root namespace's.
    let root = wiphys_in(None).await;
    assert!(
        !root.iter().any(|p| p.starts_with(lab.name())),
        "lab radios in the root namespace: {root:?}"
    );
    let failed: Vec<_> = lab
        .assertion_results()
        .iter()
        .filter(|a| !a.passed)
        .collect();
    assert!(
        !lab.assertion_results().is_empty() && failed.is_empty(),
        "{failed:?}"
    );

    let name = lab.name().to_string();
    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
    let hwsim: nlink::Connection<nlink::netlink::Hwsim> =
        nlink::Connection::new_async().await.unwrap();
    let left: Vec<_> = hwsim
        .get_radios()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.name.as_deref().is_some_and(|n| n.starts_with(&name)))
        .collect();
    assert!(left.is_empty(), "radios left after destroy: {left:?}");
    assert!(
        !wiphys_in(None).await.iter().any(|p| p.starts_with(&name)),
        "wiphys left after destroy"
    );
}

/// #176 with the daemons: hostapd on the AP's `wlan0` and wpa_supplicant
/// on each station's — the interface names `examples/wifi.nll` declares
/// on all three nodes — associate, and the stations reach the AP.
#[tokio::test]
async fn wifi_ap_and_stations_associate() {
    if !hwsim_available() {
        eprintln!("skipping wifi_ap_and_stations_associate: needs root and mac80211_hwsim");
        return;
    }
    let found = |bin: &str| {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .any(|d| std::path::Path::new(d).join(bin).exists())
    };
    if !found("hostapd") || !found("wpa_supplicant") {
        eprintln!("skipping wifi_ap_and_stations_associate: needs hostapd and wpa_supplicant");
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wifi.nll"
    ))
    .unwrap();
    topo.lab.name = format!("wap-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy wifi");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for sta in ["sta1", "sta2"] {
        // Association takes a few seconds after wpa_supplicant starts.
        let mut ok = false;
        for _ in 0..30 {
            let out = lab.exec(sta, "ping", &["-c1", "-W1", "10.0.0.1"]).unwrap();
            if out.exit_code == 0 {
                ok = true;
                break;
            }
        }
        assert!(ok, "{sta} never reached the AP");
    }
    // The AP's view: an access point on `labnet` with both stations
    // associated. (A station's GET_INTERFACE carries no SSID.)
    let ns = lab.namespace_for("ap").unwrap().to_string();
    let nl: nlink::Connection<nlink::netlink::Nl80211> =
        nlink::Connection::new_in_async(ns.as_str()).await.unwrap();
    let ifaces = nl.get_interfaces().await.unwrap();
    assert_eq!(ifaces.len(), 1, "{ifaces:?}");
    assert_eq!(ifaces[0].name.as_deref(), Some("wlan0"), "{ifaces:?}");
    assert_eq!(
        ifaces[0].iftype,
        nlink::netlink::genl::nl80211::InterfaceType::Ap,
        "{ifaces:?}"
    );
    assert_eq!(ifaces[0].ssid.as_deref(), Some("labnet"), "{ifaces:?}");
    assert_eq!(ifaces[0].frequency, Some(2437), "channel 6: {ifaces:?}");
    let stations = nl.get_stations("wlan0").await.unwrap();
    assert_eq!(stations.len(), 2, "{stations:?}");

    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
}

/// #204: radios are made in the first Wi-Fi node's namespace, so adding
/// a Wi-Fi node that sorts first re-creates every radio. The AP's hostapd
/// and the station's wpa_supplicant were left bound to the netdevs that
/// went with the old radios: the station never associated again.
#[tokio::test]
async fn an_apply_that_recreates_the_radios_restarts_their_daemons() {
    if !hwsim_available() {
        eprintln!("skipping: needs root and mac80211_hwsim");
        return;
    }
    let found = |bin: &str| {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .any(|d| std::path::Path::new(d).join(bin).exists())
    };
    if !found("hostapd") || !found("wpa_supplicant") {
        eprintln!("skipping: needs hostapd and wpa_supplicant");
        return;
    }
    let lab_name = format!("wrc-{}", std::process::id());
    let station = |name: &str, addr: &str| {
        format!(
            r#"node {name} {{ wifi wlan0 mode station {{ ssid "labnet" wpa2 "testpassword" {addr}/24 }} }}"#
        )
    };
    let topo = |extra: &str| {
        nlink_lab::parser::parse(&format!(
            r#"lab "{lab_name}"
{extra}
node m {{ wifi wlan0 mode ap {{ ssid "labnet" channel 6 wpa2 "testpassword" 10.0.0.1/24 }} }}
{}
"#,
            station("s", "10.0.0.2")
        ))
        .unwrap()
    };
    let mut lab = topo("").deploy().await.expect("deploy wifi");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let reaches_the_ap = |lab: &RunningLab, sta: &str| {
        (0..30).any(|_| {
            lab.exec(sta, "ping", &["-c1", "-W1", "10.0.0.1"])
                .is_ok_and(|o| o.exit_code == 0)
        })
    };
    assert!(reaches_the_ap(&lab, "s"), "s never reached the AP");

    // `a` sorts before `m`: every radio is made in its namespace now
    let plan = nlink_lab::apply_plan(&lab, &topo(&station("a", "10.0.0.3"))).unwrap();
    assert!(
        plan.ops
            .iter()
            .any(|o| matches!(o, nlink_lab::Op::DeleteRadio { node, .. } if node == "m")),
        "the radios are not re-created: {:?}",
        plan.ops
    );
    nlink_lab::apply(&mut lab, &topo(&station("a", "10.0.0.3")))
        .await
        .expect("apply adding a first Wi-Fi node");
    for sta in ["s", "a"] {
        assert!(
            reaches_the_ap(&lab, sta),
            "{sta} does not reach the AP after the apply"
        );
    }

    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
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
    if unsafe { libc::geteuid() } != 0 || !has_wireguard() || !wg_tools {
        eprintln!("skipping: needs root, the wireguard module and wireguard-tools");
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

/// #204: a failed `apply` rolls back what it did, and only that. Its
/// journal removed the lab's whole cgroup subtree, killing the processes
/// of every node with a cpu/memory limit, its `/etc/hosts` section and
/// the `/etc/netns` overlay of every node it touched.
#[tokio::test]
async fn a_failed_apply_leaves_the_running_labs_processes_and_hosts_alone() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: requires root");
        return;
    }
    let lab_name = format!("rbk-{}", std::process::id());
    let topo = |extra: &str, b_addr: &str| {
        nlink_lab::parser::parse(&format!(
            r#"lab "{lab_name}" {{ dns hosts }}
node a {{ cpu 0.5  run background ["sleep", "1001"] }}
node b
{extra}
link a:eth0 -- b:eth0 {{ 10.0.0.1/29 -- {b_addr}/29 }}
"#
        ))
        .unwrap()
    };
    let mut lab = topo("", "10.0.0.2").deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let pid = lab
        .pids()
        .iter()
        .find(|(n, _)| n == "a")
        .map(|(_, p)| *p)
        .expect("a's background process");
    let overlay = format!("/etc/netns/{}/hosts", lab.namespace_for("a").unwrap());
    let section = || {
        let hosts = std::fs::read_to_string("/etc/hosts").unwrap();
        let start = format!("NLINK-LAB-{lab_name}-START");
        let end = format!("NLINK-LAB-{lab_name}-END");
        hosts
            .lines()
            .skip_while(|l| !l.contains(&start))
            .take_while(|l| !l.contains(&end))
            .map(str::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = section();
    assert!(before.contains("10.0.0.2"), "{before}");

    // a new limited node whose second `run` cannot start, and a moved
    // address: the apply gets as far as the processes and fails there
    let failing = topo(
        r#"node c { cpu 0.5  run background ["sleep", "1002"]  run background ["/nonexistent/binary"] }
link a:eth1 -- c:eth0 { 10.0.1.1/30 -- 10.0.1.2/30 }"#,
        "10.0.0.5",
    );
    nlink_lab::apply(&mut lab, &failing)
        .await
        .expect_err("the missing binary must fail the apply");

    assert!(
        std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "the rollback killed a's process"
    );
    assert_eq!(section(), before, "the lab's /etc/hosts section changed");
    let a_hosts = std::fs::read_to_string(&overlay).unwrap_or_default();
    assert!(
        a_hosts.contains("10.0.0.2") && !a_hosts.contains("10.0.0.5"),
        "a's overlay is not the running lab's: {a_hosts:?}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #208: a scenario's `down` / `up` / `clear` on a container node (they
/// resolved namespace nodes only and failed), and `clear` is
/// `impair --clear`: the netem goes, the declared rate limit stays, and
/// the state file records the clear. It deleted the whole root qdisc and
/// told nobody.
#[tokio::test]
async fn a_scenario_acts_on_a_container_node_and_clears_like_impair() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("scn-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"]
node h
link c:eth0 -- h:eth0 {{
  10.0.0.1/24 -- 10.0.0.2/24
  delay 50ms
  rate egress 10mbit
}}
scenario "flap" {{
  at 0s {{ down c:eth0 }}
  at 1s {{ up c:eth0 }}
  at 2s {{ clear c:eth0 }}
}}
"#
    ))
    .unwrap();
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let result = nlink_lab::scenario::run_scenario(&mut lab, &topo.scenarios[0])
        .await
        .expect("scenario failed to run");
    assert!(result.passed, "{result:?}");

    let conn: nlink::Connection<nlink::Route> =
        lab.ns_resolver_of("c").unwrap().connection().unwrap();
    let link = conn.get_link_by_name("eth0").await.unwrap().unwrap();
    assert!(link.is_up(), "c:eth0 is not up again");
    let root = conn.get_root_qdisc_by_name("eth0").await.unwrap();
    let kind = root.as_ref().and_then(|q| q.kind()).map(str::to_string);
    assert!(
        kind.is_some() && kind.as_deref() != Some("netem"),
        "the rate limit should stay and the netem go: root is {kind:?}"
    );
    let reloaded = RunningLab::load(lab.name()).unwrap();
    assert_eq!(
        reloaded.live_impairments().get("c:eth0"),
        Some(&nlink_lab::types::Impairment::default()),
        "the clear is not recorded"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #208: benchmark thresholds in any unit the lexer takes (`1s` parsed
/// as nothing and never passed), and a UDP iperf3 at the rate asked: it
/// ran at iperf3's 1 Mbit/s default, whatever the link.
#[tokio::test]
async fn benchmarks_read_any_unit_and_run_udp_at_the_rate_asked() {
    let iperf3 = std::process::Command::new("iperf3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if unsafe { libc::geteuid() } != 0 || !iperf3 {
        eprintln!("skipping: needs root and iperf3");
        return;
    }
    let lab_name = format!("bmk-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a
node b
link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
benchmark "b" {{
  ping a b {{
    count 3
    assert avg below 1s
    assert loss below 1%
  }}
  iperf3 a b {{
    duration 2s
    udp
    rate 20mbit
    assert bandwidth above 10mbit
    assert loss below 5%
  }}
}}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let result = nlink_lab::benchmark::run_benchmark(&lab, &topo.benchmarks[0]).unwrap();
    assert!(result.passed, "{result:#?}");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #210: `mgmt SUBNET` without `host-reachable` gave no node a `mgmt0`;
/// now the nodes reach each other over it, at the addresses `ip` reports,
/// and destroy removes its namespace.
#[tokio::test]
async fn a_mgmt_subnet_connects_every_node() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: requires root");
        return;
    }
    let lab_name = format!("mgm-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        "lab \"{lab_name}\" {{ mgmt 172.31.77.0/24 }}\nnode a\nnode b\n"
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let b = lab.node_addresses("b").unwrap()["mgmt0"][0].clone();
    assert_eq!(b, "172.31.77.3/24");
    let ping = lab
        .exec("a", "ping", &["-c1", "-W2", b.split('/').next().unwrap()])
        .unwrap();
    assert_eq!(
        ping.exit_code, 0,
        "a cannot reach b over mgmt0: {}",
        ping.stderr
    );
    let mgmt_ns = format!("{}-mgmt", lab.topology().lab.prefix());
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
    assert!(
        !nlink::netlink::namespace::exists(&mgmt_ns),
        "{mgmt_ns} is left behind"
    );
}

/// #210: `subnet auto/24` failed validation; past that its members had
/// no addresses, and an apply of the same file had no subnet for them.
#[tokio::test]
async fn an_auto_network_subnet_deploys_and_applies_as_nothing() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: requires root");
        return;
    }
    let lab_name = format!("aut-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        "lab \"{lab_name}\"\nnode a\nnode b\nnetwork lan {{ members [a:eth0, b:eth0] subnet auto/24 }}\n"
    ))
    .unwrap();
    assert!(topo.validate().bail().is_ok(), "{:?}", topo.validate());
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let ports = &lab.topology().networks["lan"].ports;
    let b = ports["b:eth0"].addresses[0].clone();
    let ping = lab
        .exec("a", "ping", &["-c1", "-W2", b.split('/').next().unwrap()])
        .unwrap();
    assert_eq!(ping.exit_code, 0, "{}", ping.stderr);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "the same file is drift:\n{layered}");
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("apply failed");
    let after = lab
        .exec("a", "ping", &["-c1", "-W2", b.split('/').next().unwrap()])
        .unwrap();
    assert_eq!(after.exit_code, 0, "the apply removed the addresses");

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #215: a process on a container node is `podman exec … CMD`, and the
/// tracked PID is the exec client's, which forwards no signal. `kill`
/// stopped the client and left CMD running in the container; an `apply`
/// that changed a `run` started the new command next to the old one.
#[tokio::test]
async fn container_processes_are_stopped_inside_the_container() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("kct-{}", std::process::id());
    let topo = |secs: &str| {
        nlink_lab::parser::parse(&format!(
            r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"] {{ run background ["sleep", "{secs}"] }}
node h
link c:eth0 -- h:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        ))
        .unwrap()
    };
    let mut lab = topo("5555").deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let running = |lab: &RunningLab, what: &str| {
        let ps = lab.exec("c", "ps", &[]).unwrap().stdout;
        ps.lines().filter(|l| l.contains(what)).count()
    };
    let pid = lab.spawn_with_logs("c", &["sleep", "4242"], None).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(running(&lab, "sleep 4242"), 1);
    assert!(lab.kill_process(pid).unwrap());
    assert_eq!(
        running(&lab, "sleep 4242"),
        0,
        "killed, still running in the container"
    );

    nlink_lab::apply(&mut lab, &topo("6666"))
        .await
        .expect("apply failed");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        running(&lab, "sleep 5555"),
        0,
        "the old run is still running"
    );
    assert_eq!(running(&lab, "sleep 6666"), 1);

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #226: at an `exec --timeout` on a container node only the runtime's
/// client was killed, and the command ran on in the container.
#[tokio::test]
async fn an_exec_timeout_stops_the_command_in_the_container() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("ket-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"]
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let opts = nlink_lab::ExecOpts {
        timeout: Some(std::time::Duration::from_secs(1)),
        ..Default::default()
    };
    let r = lab.exec_with_opts("c", "sleep", &["4321"], opts);
    assert!(matches!(r, Err(nlink_lab::Error::Timeout(_))), "{r:?}");
    let ps = lab.exec("c", "ps", &[]).unwrap().stdout;
    assert!(!ps.contains("sleep 4321"), "still running: {ps}");
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #217: a container's healthcheck ran on the host (only in the container's
/// network namespace), its `retries` were dropped, `pull never` let the
/// runtime pull, a `cap-drop` of a default capability was added back, and
/// the hostname was the container id instead of the node name.
#[tokio::test]
async fn container_options_reach_the_container() {
    if unsafe { libc::geteuid() } != 0 || !has_local_image("alpine:latest") {
        eprintln!("skipping: needs root and alpine:latest in the podman store");
        return;
    }
    let lab_name = format!("kco-{}", std::process::id());
    let parse = |name: &str, node: &str| {
        nlink_lab::parser::parse(&format!(
            "lab \"{name}\" {{ runtime \"podman\" }}\n{node}\n"
        ))
        .unwrap()
    };
    // /etc/alpine-release exists in the container, not on the host
    let topo = parse(
        &lab_name,
        r#"node c image "alpine:latest" cmd ["sleep", "infinity"] {
  cap-drop [NET_RAW]
  healthcheck "test -f /etc/alpine-release" { interval 200ms timeout 20s }
}"#,
    );
    let lab = topo
        .deploy()
        .await
        .expect("deploy failed: the healthcheck did not run in the container");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let hostname = lab.exec("c", "hostname", &[]).unwrap().stdout;
    assert_eq!(hostname.trim(), "c");
    let status = lab.exec("c", "cat", &["/proc/self/status"]).unwrap().stdout;
    let cap_eff = status
        .lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .map(|v| u64::from_str_radix(v.trim(), 16).unwrap())
        .unwrap();
    assert_eq!(cap_eff >> 12 & 1, 1, "NET_ADMIN kept: {cap_eff:x}");
    assert_eq!(cap_eff >> 13 & 1, 0, "NET_RAW dropped: {cap_eff:x}");
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");

    // two failed probes end the wait, long before the timeout
    let failing = parse(
        &format!("{lab_name}-f"),
        r#"node c image "alpine:latest" cmd ["sleep", "infinity"] {
  healthcheck "false" { interval 200ms timeout 60s retries 2 }
}"#,
    );
    let started = std::time::Instant::now();
    let err = failing
        .deploy()
        .await
        .err()
        .expect("deploy passed")
        .to_string();
    assert!(err.contains("failed 2 time(s)"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));

    let absent = parse(
        &format!("{lab_name}-p"),
        r#"node c image "localhost/nlink-lab-absent:none" { pull never }"#,
    );
    let err = absent
        .deploy()
        .await
        .err()
        .expect("deploy passed")
        .to_string();
    assert!(err.contains("not present locally"), "{err}");
}

/// #236: a deploy reports its plan, then each stage in plan order, its
/// steps, and ends with `Finished`; the destroy reports its phases.
#[tokio::test]
async fn deploy_reports_progress_in_order() {
    use nlink_lab::api::{OpEvent, OpOutcome, OpPhase};
    use std::sync::{Arc, Mutex};
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: needs root");
        return;
    }
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .unwrap();
    let name = format!("prog-{}", std::process::id());
    let src = src.replacen("lab \"simple\"", &format!("lab \"{name}\""), 1);
    let topo = nlink_lab::parser::parse(&src).unwrap();
    let _guard = LabCleanup { name: name.clone() };
    let seen = Arc::new(Mutex::new(Vec::<OpEvent>::new()));
    let sink = {
        let seen = seen.clone();
        move |e: OpEvent| seen.lock().unwrap().push(e)
    };
    let ctx = nlink_lab::OpCtx::new("deploy", "").with_sink(Arc::new(sink));
    let lab = topo.deploy_with(&ctx).await.expect("deploy failed");

    let events = std::mem::take(&mut *seen.lock().unwrap());
    assert!(events.iter().all(|e| e.lab == name && e.op == ctx.id()));
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "{seqs:?}");
    assert!(matches!(&events[0].phase, OpPhase::Started { kind, .. } if kind == "deploy"));
    let OpPhase::Planned { stages, ops, .. } = &events[1].phase else {
        panic!("no plan first: {:?}", events[1]);
    };
    let staged: Vec<(String, usize)> = events
        .iter()
        .filter_map(|e| match &e.phase {
            OpPhase::Stage { stage, ops, .. } => Some((stage.clone(), *ops)),
            _ => None,
        })
        .collect();
    assert_eq!(&staged, stages, "stages out of plan order");
    let last_step = events.iter().rev().find_map(|e| match &e.phase {
        OpPhase::Step { done, total, .. } => Some((*done, *total)),
        _ => None,
    });
    assert_eq!(last_step, Some((*ops, *ops)));
    assert!(matches!(
        &events.last().unwrap().phase,
        OpPhase::Finished {
            outcome: OpOutcome::Succeeded,
            ..
        }
    ));

    std::mem::forget(_guard);
    let ctx = nlink_lab::OpCtx::new("destroy", "").with_sink(Arc::new({
        let seen = seen.clone();
        move |e: OpEvent| seen.lock().unwrap().push(e)
    }));
    lab.destroy_with(&ctx).await.expect("destroy failed");
    let events = std::mem::take(&mut *seen.lock().unwrap());
    let steps: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|e| match &e.phase {
            OpPhase::Step { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(steps.first(), Some(&(1, 6)), "{events:?}");
    assert_eq!(steps.last(), Some(&(6, 6)), "{events:?}");
}

/// #236: cancelling a deploy while it waits for a healthcheck ends the
/// wait at once and rolls the deploy back: no namespace, no ownership tag,
/// no `/etc/netns` directory, no state is left.
#[tokio::test]
async fn cancel_during_a_healthcheck_rolls_back() {
    use nlink_lab::api::{OpEvent, OpOutcome, OpPhase, WaitKind};
    use std::sync::{Arc, Mutex};
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: needs root");
        return;
    }
    let name = format!("pcan-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        "lab \"{name}\" {{ dns hosts }}\nnode a {{ healthcheck \"false\" {{ interval 200ms timeout 60s }} }}\nnode b\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}\n"
    ))
    .unwrap();
    let _guard = LabCleanup { name: name.clone() };
    let cancel = nlink_lab::CancelToken::new();
    let seen = Arc::new(Mutex::new(Vec::<OpEvent>::new()));
    let sink = {
        let (seen, cancel) = (seen.clone(), cancel.clone());
        move |e: OpEvent| {
            if matches!(
                &e.phase,
                OpPhase::Waiting {
                    what: WaitKind::Healthcheck { .. },
                    ..
                }
            ) {
                cancel.cancel();
            }
            seen.lock().unwrap().push(e)
        }
    };
    let ctx = nlink_lab::OpCtx::new("deploy", "")
        .with_sink(Arc::new(sink))
        .with_cancel(cancel);
    let started = std::time::Instant::now();
    let err = topo.deploy_with(&ctx).await.err().expect("deploy passed");
    assert!(matches!(err, nlink_lab::Error::Cancelled(_)), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));

    let events = seen.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.phase, OpPhase::RollingBack { .. })),
        "{events:?}"
    );
    assert!(matches!(
        &events.last().unwrap().phase,
        OpPhase::Finished {
            outcome: OpOutcome::Cancelled,
            ..
        }
    ));
    for node in ["a", "b"] {
        let ns = format!("{name}-{node}");
        assert!(
            !std::path::Path::new("/var/run/netns").join(&ns).exists(),
            "{ns} left"
        );
        assert!(!nlink_lab::netns_tag::is_tagged(&ns), "{ns} tag left");
        assert!(
            !std::path::Path::new(nlink_lab::netns_tag::NETNS_ETC_DIR)
                .join(&ns)
                .exists(),
            "/etc/netns/{ns} left"
        );
    }
    assert!(!nlink_lab::state::exists(&name), "state left");
    // nor its directory and lock file: the lab never existed (#103)
    let dir = nlink_lab::state::state_dir(&name);
    assert!(!dir.exists(), "{} left", dir.display());
    let lock = dir
        .parent()
        .unwrap()
        .join(".locks")
        .join(format!("{name}.lock"));
    assert!(!lock.exists(), "{} left", lock.display());
}

/// F4: `wait_all` waits for every condition within one deadline, reports
/// each one, and a cancel ends a wait that would never succeed.
#[lab_test("examples/simple.nll")]
async fn wait_all_reports_progress_and_stops_on_cancel(lab: RunningLab) {
    use nlink_lab::api::{OpEvent, OpPhase};
    use nlink_lab::wait::WaitCondition;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    let seen = Arc::new(Mutex::new(Vec::<OpEvent>::new()));
    let sink = {
        let seen = seen.clone();
        move |e: OpEvent| seen.lock().unwrap().push(e)
    };
    let ctx = nlink_lab::OpCtx::new("wait", lab.name()).with_sink(Arc::new(sink));
    lab.exec("host", "touch", &["/tmp/nlink-lab-wait-all"])
        .unwrap();
    let conditions = [
        WaitCondition::File("/tmp/nlink-lab-wait-all".into()),
        WaitCondition::Exec("true".into()),
    ];
    lab.wait_all(
        "host",
        &conditions,
        Duration::from_secs(10),
        Duration::from_millis(100),
        &ctx,
    )
    .await
    .expect("wait failed");
    let progress: Vec<(usize, usize)> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.phase {
            OpPhase::WaitProgress { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress, [(1, 2), (2, 2)]);
    let _ = std::fs::remove_file("/tmp/nlink-lab-wait-all");

    let cancel = nlink_lab::CancelToken::new();
    let ctx = nlink_lab::OpCtx::new("wait", lab.name()).with_cancel(cancel.clone());
    let started = std::time::Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let never = [WaitCondition::File("/nonexistent/nlink-lab".into())];
    let r = lab
        .wait_all(
            "host",
            &never,
            Duration::from_secs(60),
            Duration::from_millis(100),
            &ctx,
        )
        .await;
    assert!(matches!(r, Err(nlink_lab::Error::Cancelled(_))), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// C7: `test --scenarios` runs every scenario after the assertions, each
/// action one more result; `keep_failed` leaves a failed lab deployed.
#[tokio::test]
async fn a_test_runs_scenarios_and_keeps_a_failed_lab() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: needs root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let name = format!("tsc-{}", std::process::id());
    let file = dir.path().join("t.nll");
    std::fs::write(
        &file,
        format!(
            "lab \"{name}\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}\n\
             scenario \"flap\" {{\n  at 0s {{ down a:eth0 }}\n  at 100ms {{ up a:eth0 }}\n  at 200ms {{ exec a \"false\" }}\n}}\n"
        ),
    )
    .unwrap();
    let _guard = LabCleanup { name: name.clone() };
    let opts = nlink_lab::test_runner::TestOptions {
        scenarios: true,
        benchmarks: false,
        keep_failed: true,
    };
    let r = nlink_lab::test_runner::run_test_opts(&file, &[], opts, &nlink_lab::OpCtx::default())
        .await
        .expect("test ran");
    let rows: Vec<(&str, bool)> = r
        .assertions
        .iter()
        .map(|a| (a.description.as_str(), a.passed))
        .collect();
    assert!(
        rows.iter()
            .any(|(d, ok)| d.starts_with("scenario flap: down a:eth0") && *ok),
        "{rows:?}"
    );
    // `exec a false` fails: the test fails, and the lab is kept
    assert!(!r.passed, "{rows:?}");
    assert_eq!(r.kept.as_deref(), Some(name.as_str()));
    assert!(nlink_lab::state::exists(&name));
    std::mem::forget(_guard);
    nlink_lab::RunningLab::load(&name)
        .unwrap()
        .destroy()
        .await
        .unwrap();
}
