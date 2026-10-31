//! Root tests: tests of host-global state, run one at a time: /etc/hosts, host-reachable mgmt, the subnet pool, macvlan in the root namespace, and process-wide zombie checks.
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
use nlink_lab::{Lab, RunningLab};

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

/// A topology's `run … background` gets an exit code like `spawn` does
/// (#320): it used to be spawned without a reaper, so `ps` and the
/// `ProcessExited` event of a dead service never had one. `exit 7` is 7, a
/// SIGTERM death 143; plain `RunningLab::spawn` is reaped too.
#[tokio::test]
async fn background_runs_report_their_exit_codes() {
    use nlink_lab_shared::messages::LabEventKind;
    if !privileged("background_runs_report_their_exit_codes") {
        return;
    }
    let lab_name = format!("rbx-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
node a {{
  run background ["sh", "-c", "sleep 4; exit 7"]
  run background ["sh", "-c", "sleep 4; kill -TERM $$"]
}}
"#
    ))
    .unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let plain = lab.spawn("a", &["sh", "-c", "sleep 4; exit 5"]).unwrap();
    let mut metrics = nlink_lab::metrics::MetricsCollector::new(&lab);
    let _ = metrics.snapshot(&lab).await.unwrap();
    assert_eq!(
        lab.process_status().iter().filter(|p| p.alive).count(),
        3,
        "all three alive at the first tick: {:?}",
        lab.process_status()
    );

    // dead, and the reaper has written its .rc (right after it reaps)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while lab
        .process_status()
        .iter()
        .any(|p| p.alive || p.exit_code.is_none())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no exit code 15 s on: {:?}",
            lab.process_status()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let (_, events) = metrics.snapshot(&lab).await.unwrap();
    let mut exited: Vec<Option<i32>> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LabEventKind::ProcessExited { exit_code, .. } => Some(*exit_code),
            _ => None,
        })
        .collect();
    exited.sort();
    assert_eq!(exited, [Some(5), Some(7), Some(143)], "{events:?}");

    let mut ps: Vec<(u32, Option<i32>)> = nlink_lab::ops::view::processes(&lab)
        .into_iter()
        .map(|p| (p.pid, p.exit_code))
        .collect();
    ps.sort_by_key(|(_, code)| *code);
    assert_eq!(
        ps.iter().map(|(_, code)| *code).collect::<Vec<_>>(),
        [Some(5), Some(7), Some(143)],
        "{ps:?}"
    );
    assert!(ps.iter().any(|(pid, _)| *pid == plain));
    assert!(
        our_zombie_children().is_empty(),
        "no reaper may be our zombie: {:?}",
        our_zombie_children()
    );
    lab.destroy().await.expect("destroy failed");
}

// Plan 158e — macvlan topologies coexist with the declarative
// addresses+routes path. Macvlan is created host-side and moved
// into the namespace imperatively (step 6a). Addresses on the
// macvlan iface go through Slice 1's NetworkConfig path. Tests
// that the combination still deploys and reapplies cleanly.
#[tokio::test]
async fn network_config_coexists_with_macvlan() {
    if !privileged("network_config_coexists_with_macvlan") {
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

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/macvlan.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "network_config_coexists_with_macvlan");
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

// ─── Topology patterns ──────────────────────────────────

#[lab_test("examples/subnet-pools.nll")]
async fn subnet_pool_deploy(lab: RunningLab) {
    assert!(lab.topology().nodes.len() >= 4);
    assert!(lab.topology().links.len() >= 4);
}

// ─── DNS example ────────────────────────────────────────

#[lab_test("examples/dns.nll")]
async fn dns_example_deploys(lab: RunningLab) {
    assert_eq!(lab.topology().lab.dns, nlink_lab::DnsMode::Hosts);
    assert_eq!(lab.topology().nodes.len(), 3);
}

#[tokio::test]
async fn mgmt_ipv6_host_reachable() {
    if !privileged("mgmt_ipv6_host_reachable") {
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

/// A restarted container gets back everything its old namespace had: its
/// links, its bridge-network port, its mgmt veth, its sysctls and its
/// background `run`. Only the point-to-point links came back, and a
/// network member could not be restarted at all (its bridge port was
/// missing when the links were brought up).
#[tokio::test]
async fn a_restarted_container_gets_its_ports_sysctls_and_runs_back() {
    if !privileged("a_restarted_container_gets_its_ports_sysctls_and_runs_back") {
        return;
    }
    if !feature(
        "a_restarted_container_gets_its_ports_sysctls_and_runs_back",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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

/// #204: a failed `apply` rolls back what it did, and only that. Its
/// journal removed the lab's whole cgroup subtree, killing the processes
/// of every node with a cpu/memory limit, its `/etc/hosts` section and
/// the `/etc/netns` overlay of every node it touched.
#[tokio::test]
async fn a_failed_apply_leaves_the_running_labs_processes_and_hosts_alone() {
    if !privileged("a_failed_apply_leaves_the_running_labs_processes_and_hosts_alone") {
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

/// #210: `mgmt SUBNET` without `host-reachable` gave no node a `mgmt0`;
/// now the nodes reach each other over it, at the addresses `ip` reports,
/// and destroy removes its namespace.
#[tokio::test]
async fn a_mgmt_subnet_connects_every_node() {
    if !privileged("a_mgmt_subnet_connects_every_node") {
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
    if !privileged("an_auto_network_subnet_deploys_and_applies_as_nothing") {
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

/// #236: cancelling a deploy while it waits for a healthcheck ends the
/// wait at once and rolls the deploy back: no namespace, no ownership tag,
/// no `/etc/netns` directory, no state is left.
#[tokio::test]
async fn cancel_during_a_healthcheck_rolls_back() {
    use nlink_lab::api::{OpEvent, OpOutcome, OpPhase, WaitKind};
    use std::sync::{Arc, Mutex};
    if !privileged("cancel_during_a_healthcheck_rolls_back") {
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

/// One mutation per lab at a time, from any process (#332). Two CLIs
/// change the impairments of two interfaces of one node, at the same
/// time, 20 rounds. Each change plans the node's whole traffic control
/// from the state it read: unlocked, one could reinstall the other's
/// interface as it was, or save its state after the other's kernel
/// change. After every round the kernel's netem is what the state says,
/// on both interfaces, and at the end `verify` finds no drift.
#[tokio::test]
async fn concurrent_impairs_from_two_processes_leave_the_kernel_and_the_state_agreeing() {
    use std::sync::{Arc, Barrier};
    let test = "concurrent_impairs_from_two_processes_leave_the_kernel_and_the_state_agreeing";
    if !privileged(test) {
        return;
    }
    let lab_name = format!("cimp-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        "lab \"{lab_name}\"\nnode a\nnode b\nnode c\n\
         link a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24  delay 1ms }}\n\
         link a:eth1 -- c:eth0 {{ 10.0.1.1/24 -- 10.0.1.2/24  delay 1ms }}\n"
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    const ROUNDS: u32 = 20;
    let cli = cli_binary();
    let barrier = Arc::new(Barrier::new(3));
    let run = |iface: &'static str, offset: u32| {
        let (cli, lab_name, barrier) = (cli.clone(), lab_name.clone(), barrier.clone());
        std::thread::spawn(move || {
            for i in 0..ROUNDS {
                barrier.wait();
                let out = std::process::Command::new(&cli)
                    .args(["impair", "-L", &lab_name, iface, "delay"])
                    .arg(format!("{}ms", offset + i))
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "impair {iface}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                barrier.wait();
            }
        })
    };
    let (one, two) = (run("a:eth0", 10), run("a:eth1", 100));
    let agree = |round: u32| {
        let lab = RunningLab::load(&lab_name).unwrap();
        for ep in ["a:eth0", "a:eth1"] {
            let want = lab.live_impairments()[ep].delay.clone().unwrap();
            let want_ms: f64 = want.trim_end_matches("ms").parse().unwrap();
            let shown = lab.impair_show_blocking(ep).unwrap().unwrap();
            assert_eq!(
                shown.delay_ms,
                Some(want_ms),
                "round {round}, {ep}: kernel {shown:?}, state {want}"
            );
        }
    };
    for round in 0..ROUNDS {
        barrier.wait(); // both start
        barrier.wait(); // both done
        agree(round);
    }
    one.join().unwrap();
    two.join().unwrap();

    let verify = std::process::Command::new(&cli)
        .args(["verify", "-L", &lab_name])
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "drift after concurrent impairs:\n{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// Two labs cannot both put a host-reachable mgmt subnet on the host when
/// the subnets overlap (#332): the second deploy fails before its first op
/// with `mgmt-subnet-overlap`, naming the first lab, and leaves nothing.
#[tokio::test]
async fn an_overlapping_host_reachable_mgmt_subnet_is_refused() {
    if !privileged("an_overlapping_host_reachable_mgmt_subnet_is_refused") {
        return;
    }
    let (first, second) = (
        format!("mgov1-{}", std::process::id()),
        format!("mgov2-{}", std::process::id()),
    );
    let src = |name: &str, mgmt: &str| {
        nlink_lab::parser::parse(&format!(
            "lab \"{name}\" {{ mgmt {mgmt} host-reachable }}\nnode a\n"
        ))
        .unwrap()
    };
    let lab = src(&first, "172.31.250.0/24")
        .deploy()
        .await
        .expect("deploy failed");
    let _guard = LabCleanup {
        name: first.clone(),
    };
    let _guard2 = LabCleanup {
        name: second.clone(),
    };
    let err = src(&second, "172.31.250.128/25")
        .deploy()
        .await
        .err()
        .expect("an overlapping mgmt subnet must be refused");
    let msg = err.to_string();
    assert!(msg.contains("mgmt-subnet-overlap"), "{msg}");
    assert!(msg.contains(&first), "{msg}");
    assert!(!nlink_lab::state::exists(&second));
    let ns = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&ns.stdout).contains(&second),
        "nothing of the refused lab"
    );
    assert!(
        !nlink_lab::host_registry::all()
            .unwrap()
            .contains_key(&second),
        "and no claim"
    );
    // the first one's claim goes with it
    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
    assert!(
        !nlink_lab::host_registry::all()
            .unwrap()
            .contains_key(&first)
    );
}

/// Eight labs with `dns hosts` and a host-reachable mgmt network deploy at
/// once, and destroy cleanly (#332): the host-global pieces they share
/// (`/etc/hosts`, the host registry, the subnet pool) take turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_host_reachable_dns_labs_deploy_in_parallel() {
    if !privileged("eight_host_reachable_dns_labs_deploy_in_parallel") {
        return;
    }
    let names: Vec<String> = (0..8)
        .map(|i| format!("par{i}-{}", std::process::id()))
        .collect();
    let _guards: Vec<LabCleanup> = names
        .iter()
        .map(|n| LabCleanup { name: n.clone() })
        .collect();
    let deploys = names.iter().enumerate().map(|(i, name)| {
        let topo = nlink_lab::parser::parse(&format!(
            "lab \"{name}\" {{ dns hosts\n  mgmt 172.31.{}.0/24 host-reachable }}\nnode a\nnode b\nlink a:eth0 -- b:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}\n",
            240 + i
        ))
        .unwrap();
        tokio::spawn(async move { topo.deploy().await })
    });
    let mut labs = Vec::new();
    for d in deploys.collect::<Vec<_>>() {
        labs.push(d.await.unwrap().expect("a parallel deploy failed"));
    }
    let hosts = std::fs::read_to_string("/etc/hosts").unwrap();
    for name in &names {
        assert!(
            hosts.contains(&format!("NLINK-LAB-{name}-START")),
            "{name}'s /etc/hosts section"
        );
    }
    let destroys: Vec<_> = labs
        .into_iter()
        .map(|lab| tokio::spawn(async move { lab.destroy().await }))
        .collect();
    for d in destroys {
        d.await.unwrap().expect("a parallel destroy failed");
    }
    let hosts = std::fs::read_to_string("/etc/hosts").unwrap();
    let claims = nlink_lab::host_registry::all().unwrap();
    for name in &names {
        assert!(!hosts.contains(&format!("NLINK-LAB-{name}-")), "{name}");
        assert!(!claims.contains_key(name), "{name}'s claim released");
    }
    for g in _guards {
        std::mem::forget(g);
    }
}

// ─── #344: foreign nodes ───

/// A command run as root on the host, its stdout; `None` when it failed.
fn host(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What a foreign-node test made itself, removed when it ends.
struct ForeignTargets {
    netns: Vec<String>,
    links: Vec<String>,
    containers: Vec<String>,
}

impl Drop for ForeignTargets {
    fn drop(&mut self) {
        for c in &self.containers {
            let _ = host("podman", &["rm", "-f", "-t", "0", c]);
        }
        for l in &self.links {
            let _ = host("ip", &["link", "del", l]);
        }
        for n in &self.netns {
            let _ = host("ip", &["netns", "del", n]);
        }
    }
}

/// Attach to persistent tap `name` (`ip tuntap add`), as a VM's hypervisor
/// would: with no one attached a tap has no carrier, and a bridge sends it
/// nothing.
fn attach_tap(name: &str) -> std::fs::File {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct IfReq {
        name: [u8; libc::IFNAMSIZ],
        flags: libc::c_short,
        _pad: [u8; 22],
    }
    const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .expect("/dev/net/tun");
    let mut req = IfReq {
        name: [0; libc::IFNAMSIZ],
        flags: (libc::IFF_TAP | libc::IFF_NO_PI) as libc::c_short,
        _pad: [0; 22],
    };
    req.name[..name.len()].copy_from_slice(name.as_bytes());
    // SAFETY: TUNSETIFF reads and writes an ifreq, which `req` is laid out as.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETIFF as _, &mut req) };
    assert_eq!(
        rc,
        0,
        "TUNSETIFF {name}: {}",
        std::io::Error::last_os_error()
    );
    file
}

/// Whether an ARP request for `ip` arrives on the tap `file` within 5 s.
fn tap_hears_arp_for(file: &std::fs::File, ip: std::net::Ipv4Addr) -> bool {
    use std::io::Read as _;
    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut buf = [0u8; 2048];
    let mut file = file;
    while std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, 200) } <= 0 {
            continue;
        }
        let Ok(n) = file.read(&mut buf) else { continue };
        // Ethernet II: ethertype at 12, ARP target address at 14 + 24
        if n >= 42 && buf[12..14] == [0x08, 0x06] && buf[38..42] == ip.octets() {
            return true;
        }
    }
    false
}

/// #344: a namespace node is linked to a netns `ip netns add` made, to a
/// tap (`ip tuntap add`, attached as a VM would be) and, with podman, to a
/// container another tool runs. Traffic passes and is impaired, `spawn`
/// is refused, and `destroy` leaves the netns, the tap and the container
/// as they were, without the lab's ends.
#[tokio::test]
async fn foreign_nodes_are_linked_and_left_as_they_were() {
    let test = "foreign_nodes_are_linked_and_left_as_they_were";
    if !privileged(test) {
        return;
    }
    let pid = std::process::id();
    let (ns, tap, ctr) = (format!("xf{pid}"), format!("xt{pid}"), format!("xc{pid}"));
    let podman = feature(test, "podman-alpine", has_local_image("alpine:latest"));
    // a container without /dev/net/tun (the CI runner) makes no tap
    let tun = feature(test, "tun", std::path::Path::new("/dev/net/tun").exists());
    let mut made = ForeignTargets {
        netns: vec![ns.clone()],
        links: vec![tap.clone()],
        containers: Vec::new(),
    };
    host("ip", &["netns", "add", &ns]).expect("ip netns add");
    let (tap_node, tap_link) = if tun {
        host("ip", &["tuntap", "add", "dev", &tap, "mode", "tap"]).expect("ip tuntap add");
        (
            format!(r#"node vm kind foreign {{ tap "{tap}" }}"#),
            "link r:eth1 -- vm:nic { 10.0.1.1/24 -- 10.0.1.2/24 }".to_string(),
        )
    } else {
        Default::default()
    };
    let (ctr_node, ctr_link) = if podman {
        made.containers.push(ctr.clone());
        host(
            "podman",
            &[
                "run",
                "-d",
                "--name",
                &ctr,
                "--network",
                "none",
                "alpine:latest",
                "sleep",
                "infinity",
            ],
        )
        .expect("podman run");
        (
            format!(r#"node c kind foreign {{ container "{ctr}"  manage addresses  manage tc }}"#),
            "link r:eth2 -- c:eth0 { 10.0.2.1/24 -- 10.0.2.2/24  <- delay 20ms }".to_string(),
        )
    } else {
        Default::default()
    };
    let mut topo = nlink_lab::parser::parse(&format!(
        r#"lab "foreign-{pid}" {{ runtime "podman" }}
node r
node fw kind foreign {{ netns "{ns}"  manage addresses }}
{tap_node}
{ctr_node}
link fw:eth0 -- r:eth0 {{ 10.0.0.2/24 -- 10.0.0.1/24  <- delay 30ms }}
{tap_link}
{ctr_link}
"#
    ))
    .unwrap();
    unique(&mut topo, test);
    let bridge = topo.lab.tap_bridge_name("vm");
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // the netns: its address is the lab's to set (`manage addresses`), the
    // delay is on the lab's end, and `exec` runs in it
    let rtt = avg_rtt_ms(&lab, "r", "10.0.0.2");
    assert!(rtt >= 30.0, "rtt {rtt} ms to the netns, 30 ms delay");
    let addrs = lab.exec("fw", "ip", &["-br", "addr"]).unwrap().stdout;
    assert!(addrs.contains("10.0.0.2/24"), "{addrs}");
    let err = lab.spawn("fw", &["sleep", "30"]).unwrap_err().to_string();
    assert!(err.contains("foreign"), "{err}");

    // the tap: on the lab's bridge, and the lab's frames reach the "VM"
    let vm = tun.then(|| {
        let master = std::fs::read_link(format!("/sys/class/net/{tap}/master")).unwrap();
        assert_eq!(master.file_name().unwrap().to_str().unwrap(), bridge);
        let vm = attach_tap(&tap);
        let ping = std::thread::spawn({
            let name = lab.name().to_string();
            move || {
                let lab = RunningLab::load(&name).unwrap();
                lab.exec(
                    "r",
                    "ping",
                    &["-c", "4", "-i", "0.5", "-W", "1", "10.0.1.2"],
                )
            }
        });
        assert!(
            tap_hears_arp_for(&vm, "10.0.1.2".parse().unwrap()),
            "no ARP request from the lab reached the tap"
        );
        let _ = ping.join();
        vm
    });

    // the container: its side's address and delay are the lab's to set
    if podman {
        let rtt = avg_rtt_ms(&lab, "r", "10.0.2.2");
        assert!(
            rtt >= 20.0,
            "rtt {rtt} ms to the container, 20 ms delay inside it"
        );
    }

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
    drop(vm);

    // the targets are as they were, without the lab's ends
    assert!(
        std::path::Path::new(&format!("/run/netns/{ns}")).exists(),
        "the netns was deleted"
    );
    assert!(
        host("ip", &["-n", &ns, "link", "show", "eth0"]).is_none(),
        "the lab's end stayed in the netns"
    );
    if tun {
        assert!(
            std::path::Path::new(&format!("/sys/class/net/{tap}")).exists(),
            "the tap was deleted"
        );
        assert!(
            !std::path::Path::new(&format!("/sys/class/net/{tap}/master")).exists(),
            "the tap is still enslaved"
        );
    }
    assert!(
        !std::path::Path::new(&format!("/sys/class/net/{bridge}")).exists(),
        "the lab's bridge stayed"
    );
    if podman {
        let running = host("podman", &["inspect", "-f", "{{.State.Running}}", &ctr]);
        assert_eq!(
            running.as_deref().map(str::trim),
            Some("true"),
            "the container stopped"
        );
        let pid: u32 = host("podman", &["inspect", "-f", "{{.State.Pid}}", &ctr])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            host(
                "nsenter",
                &["-t", &pid.to_string(), "-n", "ip", "link", "show", "eth0"]
            )
            .is_none(),
            "the lab's end stayed in the container"
        );
    }
    drop(made);
}

/// #344: what a deploy needs of its foreign targets is checked before its
/// first op, and a failed check leaves nothing: a missing netns, a tap a
/// bridge already has, an interface name the netns already uses.
#[tokio::test]
async fn a_foreign_target_that_cannot_be_used_fails_before_anything_is_made() {
    let test = "a_foreign_target_that_cannot_be_used_fails_before_anything_is_made";
    if !privileged(test) {
        return;
    }
    let pid = std::process::id();
    let (ns, tap, br) = (format!("xg{pid}"), format!("xu{pid}"), format!("xb{pid}"));
    let _made = ForeignTargets {
        netns: vec![ns.clone()],
        links: vec![tap.clone(), br.clone()],
        containers: Vec::new(),
    };
    host("ip", &["netns", "add", &ns]).expect("ip netns add");
    // a veth named eth0 in the netns, made over netlink: the CI runner
    // cannot load the dummy module, and its `ip -n` fails
    let conn: nlink::Connection<nlink::Route> =
        nlink::Connection::new_in(ns.as_str()).expect("a connection into the netns");
    conn.add_link(nlink::netlink::link::VethLink::new("eth0", "eth0p"))
        .await
        .expect("a veth named eth0 in the netns");
    // a container without /dev/net/tun (the CI runner) makes no tap
    let tun = feature(test, "tun", std::path::Path::new("/dev/net/tun").exists());
    if tun {
        host("ip", &["tuntap", "add", "dev", &tap, "mode", "tap"]).expect("ip tuntap add");
        host("ip", &["link", "add", &br, "type", "bridge"]).expect("bridge");
        host("ip", &["link", "set", &tap, "master", &br]).expect("enslave");
    }
    let cases = [
        (
            format!(r#"node x kind foreign {{ netns "xnone{pid}" }}"#),
            "foreign-target-missing",
        ),
        (
            format!(r#"node x kind foreign {{ tap "{tap}" }}"#),
            "already has a master",
        ),
        (
            format!(r#"node x kind foreign {{ netns "{ns}" }}"#),
            "already has an interface 'eth0'",
        ),
    ];
    for (i, (node, says)) in cases.iter().enumerate() {
        if !tun && node.contains("tap ") {
            continue;
        }
        let mut topo = nlink_lab::parser::parse(&format!(
            "lab \"fpre-{pid}\"\nnode r\n{node}\nlink r:eth0 -- x:eth0\n"
        ))
        .unwrap();
        unique(&mut topo, &format!("{test}-{i}"));
        let name = topo.lab.name.clone();
        let _guard = LabCleanup { name: name.clone() };
        let err = match topo.deploy().await {
            Ok(_) => panic!("case {i}: the deploy succeeded"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains(says), "case {i}: {err}");
        assert!(!nlink_lab::state::exists(&name), "case {i}: state left");
        assert!(
            !std::path::Path::new(&format!("/run/netns/{}-r", topo.lab.prefix())).exists(),
            "case {i}: the lab's namespace was made"
        );
    }
    // the bridge still has its tap
    if tun {
        let master = std::fs::read_link(format!("/sys/class/net/{tap}/master")).unwrap();
        assert_eq!(master.file_name().unwrap().to_str().unwrap(), br);
    }
}
