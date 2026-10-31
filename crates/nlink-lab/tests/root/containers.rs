//! Root tests: container nodes.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::RunningLab;
use nlink_lab::test_helpers::{feature, privileged};

/// #157: `proc-stat` on a container node reads the host's `/proc`. Every
/// process has its host PID and the container's own; `--pid` names the
/// container's; pidfiles under the container's root give their names; and
/// no call enters the container (each exec costs about 0.25 s here, so a
/// call that execs takes far longer than the bound below). On a namespace
/// node, a PID outside the node is refused rather than sampled.
#[tokio::test]
async fn proc_stat_on_a_container_node_reads_the_host_proc() {
    if !privileged("proc_stat_on_a_container_node_reads_the_host_proc") {
        return;
    }
    if !feature(
        "proc_stat_on_a_container_node_reads_the_host_proc",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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

/// A container node whose recorded init PID now names another process (the
/// init exited and the number was reused, here by the test runner, which is
/// in the host's namespace) is not reached through it: the loaded lab has
/// PID 0 for it, and asking the node fails instead of reading the host's
/// interfaces.
#[tokio::test]
async fn a_container_whose_init_pid_was_reused_is_not_reached_through_it() {
    if !privileged("a_container_whose_init_pid_was_reused_is_not_reached_through_it") {
        return;
    }
    if !feature(
        "a_container_whose_init_pid_was_reused_is_not_reached_through_it",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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

/// #221: `restart` re-planned every node from the declared topology with
/// fresh WireGuard keys, so an `impair` on another node went back to the
/// declared value and every `key auto` interface was re-keyed.
#[tokio::test]
async fn a_restart_keeps_the_labs_runtime_impairments_and_keys() {
    if !privileged("a_restart_keeps_the_labs_runtime_impairments_and_keys") {
        return;
    }
    if !feature(
        "a_restart_keeps_the_labs_runtime_impairments_and_keys",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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

/// #285: a container's relative `env-file` and `config` are its topology
/// file's, wherever the deploy runs; an `apply` of the same file leaves
/// the container alone; a missing one is said by name.
#[tokio::test]
async fn container_host_files_are_the_topology_files() {
    if !privileged("container_host_files_are_the_topology_files") {
        return;
    }
    if !feature(
        "container_host_files_are_the_topology_files",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("lab")).unwrap();
    std::fs::write(dir.path().join("lab/app.env"), "LAB_GREETING=hello-285\n").unwrap();
    std::fs::write(dir.path().join("lab/motd"), "from-the-file\n").unwrap();
    let lab_name = format!("khf-{}", std::process::id());
    let file = dir.path().join("lab/lab.nll");
    std::fs::write(
        &file,
        format!(
            r#"lab "{lab_name}" {{ runtime "podman" }}
node c image "alpine:latest" cmd ["sleep", "infinity"] {{
  env-file "app.env"
  config "motd" "/etc/motd-lab"
}}
node h
link c:eth0 -- h:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
        ),
    )
    .unwrap();
    // the test runs in the crate's directory, not the file's
    let topo = nlink_lab::parser::parse_file(&file).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let out = |lab: &RunningLab, cmd: &str, args: &[&str]| lab.exec("c", cmd, args).unwrap().stdout;
    assert_eq!(out(&lab, "printenv", &["LAB_GREETING"]).trim(), "hello-285");
    assert_eq!(out(&lab, "cat", &["/etc/motd-lab"]).trim(), "from-the-file");
    // the same file applied: the container is the one that was
    lab.exec("c", "touch", &["/tmp/still-here"]).unwrap();
    nlink_lab::apply(&mut lab, &nlink_lab::parser::parse_file(&file).unwrap())
        .await
        .expect("apply failed");
    assert!(
        lab.exec("c", "test", &["-e", "/tmp/still-here"])
            .unwrap()
            .exit_code
            == 0,
        "the container was re-created"
    );
    lab.destroy().await.expect("destroy failed");
    std::mem::forget(_guard);

    // a missing one, named
    std::fs::remove_file(dir.path().join("lab/app.env")).unwrap();
    let err = nlink_lab::parser::parse_file(&file)
        .unwrap()
        .deploy()
        .await
        .err()
        .expect("deploying without the env file");
    let msg = err.to_string();
    assert!(
        msg.contains("env-file ") && msg.contains("lab/app.env does not exist"),
        "{msg}"
    );
}

/// #215: a process on a container node is `podman exec … CMD`, and the
/// tracked PID is the exec client's, which forwards no signal. `kill`
/// stopped the client and left CMD running in the container; an `apply`
/// that changed a `run` started the new command next to the old one.
#[tokio::test]
async fn container_processes_are_stopped_inside_the_container() {
    if !privileged("container_processes_are_stopped_inside_the_container") {
        return;
    }
    if !feature(
        "container_processes_are_stopped_inside_the_container",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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
    // how many of `what` run in the container once it settles on `want`:
    // a `podman exec` takes its time to start under load, so a fixed
    // sleep was a coin toss
    let running = |lab: &RunningLab, what: &str, want: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ps = lab.exec("c", "ps", &[]).unwrap().stdout;
            let n = ps.lines().filter(|l| l.contains(what)).count();
            if n == want || std::time::Instant::now() > deadline {
                return n;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };
    let pid = lab.spawn_with_logs("c", &["sleep", "4242"], None).unwrap();
    assert_eq!(running(&lab, "sleep 4242", 1), 1);
    assert!(lab.kill_process(pid).unwrap());
    assert_eq!(
        running(&lab, "sleep 4242", 0),
        0,
        "killed, still running in the container"
    );

    nlink_lab::apply(&mut lab, &topo("6666"))
        .await
        .expect("apply failed");
    assert_eq!(running(&lab, "sleep 6666", 1), 1);
    assert_eq!(
        running(&lab, "sleep 5555", 0),
        0,
        "the old run is still running"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// #226: at an `exec --timeout` on a container node only the runtime's
/// client was killed, and the command ran on in the container.
#[tokio::test]
async fn an_exec_timeout_stops_the_command_in_the_container() {
    if !privileged("an_exec_timeout_stops_the_command_in_the_container") {
        return;
    }
    if !feature(
        "an_exec_timeout_stops_the_command_in_the_container",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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
    if !privileged("container_options_reach_the_container") {
        return;
    }
    if !feature(
        "container_options_reach_the_container",
        "podman-alpine",
        has_local_image("alpine:latest"),
    ) {
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
