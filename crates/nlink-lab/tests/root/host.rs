//! Root tests: tests of host-global state, run one at a time: /etc/hosts, host-reachable mgmt, the subnet pool, macvlan in the root namespace, and process-wide zombie checks.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::Lab;
use nlink_lab::lab_test;
use nlink_lab::test_helpers::{feature, privileged};

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
