//! Root tests: exec, spawn, waits, process status and /proc stats.
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
    if !privileged("destroy_stops_what_the_lab_left_running") {
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
    // the subshell may not have exec'd its sleep yet
    let mut strays: Vec<u32> = Vec::new();
    for _ in 0..40 {
        strays = lab
            .node_pids("a")
            .unwrap()
            .into_iter()
            .filter(|&p| cmdline(p).starts_with("sleep 730"))
            .collect();
        if strays.len() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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

// Issue #66: `cpu` / `memory` on a namespace node become a cgroup v2
// subtree holding its background processes; destroy removes it.
#[tokio::test]
async fn namespace_node_cpu_memory_limits_apply_via_cgroups() {
    if !privileged("namespace_node_cpu_memory_limits_apply_via_cgroups") {
        return;
    }
    if !feature(
        "namespace_node_cpu_memory_limits_apply_via_cgroups",
        "cgroup",
        nlink_lab::cgroup::available(),
    ) {
        return;
    }
    let name = format!("cg-{}", std::process::id());
    let src = &r#"lab "cg"
node a { cpu 0.5  memory 64m  run ["sleep", "1000"] background }
"#
    .replace("cg", &name);
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let dir = nlink_lab::cgroup::node_dir(&name, "a");
    let usage = nlink_lab::cgroup::usage(&name, "a");
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

/// `tcp-connect` and `route-has` read the kernel through nlink (#179):
/// a socket made in the node's namespace, and a main-table route dump,
/// instead of `bash -c 'echo > /dev/tcp/…'` and `ip route show` text.
#[tokio::test]
async fn tcp_connect_and_route_has_read_the_kernel() {
    if !privileged("tcp_connect_and_route_has_read_the_kernel") {
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
    // Namespace nodes share the host's /tmp: a name of this lab's own.
    let marker = format!("/tmp/{}-wait-all", lab.name());
    lab.exec("host", "touch", &[&marker]).unwrap();
    let conditions = [
        WaitCondition::File(marker.clone()),
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
    let _ = std::fs::remove_file(&marker);

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
