//! What the root test binaries share: the kernel and tool probes, the
//! cleanup guard, and the helpers more than one area uses (#339). Each
//! `tests/root/<area>.rs` is its own test binary and uses part of it.
#![allow(dead_code)]

#[allow(unused_imports)]
use nlink_lab::test_helpers::{feature, privileged};
#[allow(unused_imports)]
use nlink_lab::{Lab, Op, RunningLab};

#[path = "../../../src/schema_check.rs"]
mod schema_check;

/// Panics unless the `verify --json` report of `layered` matches the
/// schema `docs-gen` publishes for it.
pub fn assert_report_matches_schema(lab: &str, layered: &nlink_lab::diff::LayeredDiff) {
    use nlink_lab::ops::view::{DryRunReport, output_schema_for};
    let report = DryRunReport::new(lab, layered, &[]);
    schema_check::assert_matches_schema(&report, output_schema_for::<DryRunReport<'static>>());
}

/// Check whether a kernel module is available (loaded or loadable).
pub fn has_kernel_module(name: &str) -> bool {
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
pub fn has_nftables() -> bool {
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

/// `/proc/<pid>/stat` state letter, `None` when the pid is gone.
pub fn proc_state(pid: u32) -> Option<char> {
    let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    st[st.rfind(')')? + 2..].chars().next()
}

/// Dead for our purposes: gone, or a zombie waiting for *its* parent.
/// CI job containers often run without a reaping PID 1, so an orphaned
/// process that exited stays `Z` forever there; that is the container's
/// problem, not a live process.
pub fn pid_dead(pid: u32) -> bool {
    !matches!(proc_state(pid), Some(c) if c != 'Z')
}

/// Extract `# handle N` markers from `nft -a list ruleset`
/// output. Rule handles are stable across no-op reapplies and
/// monotonically increase when the kernel re-creates a rule,
/// so comparing the set lets a test detect "did anything
/// change?" without parsing the full ruleset.
pub fn collect_rule_handles(nft_output: &str) -> Vec<u32> {
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

pub fn recreate_modules_present(test: &str) -> bool {
    for m in ["vrf", "8021q"] {
        let _ = std::process::Command::new("modprobe").arg(m).status();
        if !feature(
            test,
            m,
            std::path::Path::new(&format!("/sys/module/{m}")).exists(),
        ) {
            return false;
        }
    }
    true
}

// ─── apply_diff tests ────────────────────────────────────

/// Give a topology a lab name (and prefix) no other test uses:
/// `<name>-<test>-<pid>`, the way `#[lab_test]` names its labs (#222).
/// The root binaries run their tests in parallel (#339), and a lab
/// deployed under an example's own name would share its namespaces'
/// prefix with every other test of that example. The pid comes last, so
/// no lab's name is a prefix of another's.
pub fn unique(topology: &mut nlink_lab::Topology, test: &str) {
    topology
        .lab
        .add_suffix(&format!("{test}-{}", std::process::id()));
}

/// A panic-safe cleanup guard for tests that deploy without
/// `#[lab_test]`: dropping it sweeps whatever the lab left on the host.
/// Forget it after a clean `destroy`.
pub struct LabCleanup {
    pub name: String,
}

impl Drop for LabCleanup {
    fn drop(&mut self) {
        // The lab's own topology when it got as far as a state file;
        // otherwise its name, which is also its namespaces' prefix.
        let topology = RunningLab::load(&self.name)
            .map(|lab| lab.topology().clone())
            .unwrap_or_else(|_| {
                let mut t = nlink_lab::Topology::default();
                t.lab.name = self.name.clone();
                t
            });
        let prefix = format!("{}-", topology.lab.prefix());
        // `cleanup_lab` checks namespace ownership tags, kills what runs
        // in the namespaces (FRR daemons too), and removes the
        // `/etc/hosts` section, overlays, cgroups, state and lock.
        let _ = nlink_lab::test_helpers::cleanup_lab_blocking(&topology);
        // A container node's container is named `<prefix>-<node>`; without
        // a state file the topology does not say which nodes had one.
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
    }
}

/// Drop a lab's state directory and lock file without going through
/// `destroy` — for the skip paths that bail out after a *failed* deploy,
/// which has already taken the lock and created the directory but has no
/// `RunningLab` to destroy.
pub fn discard_lab_state(name: &str) {
    let _ = nlink_lab::state::remove(name);
    nlink_lab::state::remove_lock_if_unheld(name);
}

// ─── Plan 156 PR A — partition cycles fix ───────────────

/// Whether `node:iface` drops what it sends and receives: a partition's
/// matchall on both clsact hooks (#168), a drop on ingress and a steal
/// on egress (#347).
pub fn blocked(lab: &RunningLab, node: &str, iface: &str) -> bool {
    [("ingress", "action drop"), ("egress", "action stolen")]
        .iter()
        .all(|(hook, action)| {
            lab.exec(node, "tc", &["filter", "show", "dev", iface, hook])
                .unwrap()
                .stdout
                .contains(action)
        })
}

// ─── IPv6: NAT66, ip6 firewall matching, dual-stack auto-routing (#72) ───

/// IPv6 DAD keeps addresses `tentative` for ~1s after assignment; poll
/// until the interface has no tentative address (10s ceiling).
pub async fn wait_for_dad(lab: &RunningLab, node: &str, iface: &str) {
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

// ─── #108: a changed impairment is replaced, not torn down ───

/// The average round-trip time `from` measures to `to`, in ms.
pub fn avg_rtt_ms(lab: &RunningLab, from: &str, to: &str) -> f64 {
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
pub fn root_qdisc(lab: &RunningLab, node: &str, iface: &str) -> (String, u64) {
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

/// An image in the local podman store; nothing is pulled.
pub fn has_local_image(image: &str) -> bool {
    std::process::Command::new("podman")
        .args(["image", "exists", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `(exit code, elapsed)` of a TCP connect from `node` to `addr:port`.
pub fn tcp_connect(
    lab: &RunningLab,
    node: &str,
    addr: &str,
    port: u16,
) -> (i32, std::time::Duration) {
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
