//! Root tests: apply, verify and layered diffs.
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::test_helpers::privileged;
use nlink_lab::{RunningLab, TopologyBuilder};

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

/// Changing a VLAN id makes nlink delete and recreate the VLAN (nlink
/// #417). The VLAN carries nlink-lab's netem, which nlink keeps across the
/// recreate (`preserve_undeclared_tc`, nlink #645), and a VRF route, which
/// is declared and so put back; the VLAN keeps its VRF master.
#[tokio::test]
async fn apply_recreates_vlan_and_reinstalls_netem_and_vrf_route() {
    if !privileged("apply_recreates_vlan_and_reinstalls_netem_and_vrf_route") {
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
    if !privileged("apply_deletes_a_removed_vlan_and_check_lists_it") {
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

/// A recreate nlink would refuse — the VLAN carries a permanent neighbour
/// the topology cannot declare — fails before anything changes.
#[tokio::test]
async fn apply_refused_recreate_changes_nothing() {
    if !privileged("apply_refused_recreate_changes_nothing") {
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

// Plan 158f Phase 2 — `compute_layered_diff` on an unchanged
// deployed lab returns an empty bundle (every subdiff
// reports zero changes). Verified end-to-end against a real
// namespace.
#[tokio::test]
async fn compute_layered_diff_on_unchanged_topology_is_empty() {
    if !privileged("compute_layered_diff_on_unchanged_topology_is_empty") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    unique(
        &mut topo,
        "compute_layered_diff_on_unchanged_topology_is_empty",
    );
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
    if !privileged("compute_layered_diff_reports_non_empty_when_address_changes") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    unique(
        &mut topo,
        "compute_layered_diff_reports_non_empty_when_address_changes",
    );
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
    if !privileged("verify_json_emits_schema_v3_envelope") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "verify_json_emits_schema_v3_envelope");
    let lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    // Locate the test-built `nlink-lab` binary. cargo populates
    // CARGO_BIN_EXE_<binary> for `bin` targets, but the binary
    // lives in the workspace `bins/lab` crate, not this one: it sits
    // next to this test's `deps/` directory (wherever CARGO_TARGET_DIR
    // put it), else in the workspace's target/debug.
    let beside_deps = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("nlink-lab")));
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = beside_deps
        .into_iter()
        .chain(
            manifest
                .ancestors()
                .map(|p| p.join("target").join("debug").join("nlink-lab")),
        )
        .find(|candidate| candidate.exists())
        .expect(
            "expected debug nlink-lab binary in target/debug; \
             run `cargo build --bin nlink-lab` first",
        );

    // The file names the lab as deployed: simple.nll under this test's
    // unique name.
    let file = tempfile::Builder::new().suffix(".nll").tempfile().unwrap();
    std::fs::write(
        file.path(),
        nlink_lab::render::try_render(lab.topology()).unwrap(),
    )
    .unwrap();

    // `verify --file` is the drift gate `apply --check` was (#246)
    let output = std::process::Command::new(&target_dir)
        .args([
            "--json",
            "verify",
            "-L",
            lab.name(),
            "--file",
            file.path().to_str().unwrap(),
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
    if !privileged("network_config_reapply_is_zero_ops") {
        return;
    }

    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/simple.nll"
    ))
    .expect("failed to parse topology file");
    unique(&mut topo, "network_config_reapply_is_zero_ops");
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

// Issue #84: editing a background `run` line and applying must stop the
// old process and start the new one; removing the line stops it.
#[tokio::test]
async fn apply_restarts_edited_background_exec() {
    if !privileged("apply_restarts_edited_background_exec") {
        return;
    }
    let name = format!("apply-exec-edit-{}", std::process::id());
    let src = &r#"lab "apply-exec-edit"
node s { run ["sleep", "1000"] background }
"#
    .replace("apply-exec-edit", &name);
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
    let desired = nlink_lab::parser::parse(&format!("lab \"{name}\"\nnode s\n")).unwrap();
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
    if !privileged("apply_removed_network_deletes_bridge") {
        return;
    }
    let name = format!("apply-net-rm-{}", std::process::id());
    let src = &r#"lab "apply-net-rm"
node a
node b
node c
network lan { subnet 10.1.0.0/24  members [a:eth0, b:eth0] }
network dmz { subnet 10.2.0.0/24  members [b:eth1, c:eth0] }
"#
    .replace("apply-net-rm", &name);
    let topo = nlink_lab::parser::parse(src).unwrap();
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    // Bridges live in the lab's mgmt namespace; list them through
    // netlink (an `ip -n` child is not reliable in every CI container).
    async fn mgmt_bridges(name: &str) -> Vec<String> {
        let conn: nlink::Connection<nlink::Route> =
            nlink::Connection::new_in(format!("{name}-mgmt")).unwrap();
        conn.get_links()
            .await
            .unwrap()
            .iter()
            .filter_map(|l| l.name().map(str::to_string))
            .filter(|n| n.starts_with("nb"))
            .collect()
    }
    let before = mgmt_bridges(&name).await;
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
    let after = mgmt_bridges(&name).await;
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
    if !privileged("apply_runs_validate_assertions") {
        return;
    }
    let name = format!("apply-assert-{}", std::process::id());
    let src = &r#"lab "apply-assert"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#
    .replace("apply-assert", &name);
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

#[tokio::test]
async fn apply_add_node_and_link() {
    if !privileged("apply_add_node_and_link") {
        return;
    }

    let lab_name = format!("apply-add-{}", std::process::id());

    // Initial topology: two nodes, one link
    let initial = TopologyBuilder::new(&lab_name)
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
    let desired = TopologyBuilder::new(&lab_name)
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

#[tokio::test]
async fn apply_remove_node() {
    if !privileged("apply_remove_node") {
        return;
    }

    let lab_name = format!("apply-rm-{}", std::process::id());

    // Initial: three nodes
    let initial = TopologyBuilder::new(&lab_name)
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
    let desired = TopologyBuilder::new(&lab_name)
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
    if !privileged("apply_impairment_change") {
        return;
    }

    let lab_name = format!("apply-imp-{}", std::process::id());

    // Initial: link with 10ms delay
    let initial = TopologyBuilder::new(&lab_name)
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
    let desired = TopologyBuilder::new(&lab_name)
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
