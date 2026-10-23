//! Helpers for `#[lab_test]` integration tests.
//!
//! This module ships the runtime pieces the `#[lab_test]` proc macro
//! expands to, so a consumer crate only needs `nlink-lab` as a
//! dev-dependency:
//!
//! - [`root_gate`] — the privilege check (fail by default, skip when
//!   [`SKIP_ROOT_TESTS_ENV`] is set); [`privileged`] is the same gate for
//!   a hand-written test, and [`feature`] gates on a kernel feature or a
//!   tool, failing instead of skipping where [`STRICT_FEATURES_ENV`]
//!   names it.
//! - [`cleanup_lab`] / [`cleanup_lab_blocking`] — full host teardown
//!   for a lab, used by the macro's panic-path guard. Pure nlink /
//!   std, no `ip(8)` shell-outs.
//! - [`LabCapture`] — the engine behind the macro's `capture = true`
//!   form. On failure, every capture pcap is persisted to a
//!   discoverable directory; on success, captures are discarded.
//! - [`__macro_support`] — crate re-exports for the macro expansion
//!   (not part of the public API).
//!
//! More helpers (typed `wait_for_route`, `wait_for_tcp`, `ping`,
//! `iperf3`) ship in subsequent Plan 154 polish PRs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nlink::netlink::namespace;
use nlink::{Connection, Route};

use crate::capture::{CaptureConfig, CaptureHandle, CaptureOutput, spawn_capture};
use crate::error::{Error, Result};
use crate::running::RunningLab;
use crate::types::Topology;
use netring::RingProfile;

/// Crates the `#[lab_test]` expansion refers to by absolute path.
///
/// The macro emits `::nlink_lab::test_helpers::__macro_support::tokio::…`
/// so a consumer crate does not have to add `tokio` (or `libc`) to
/// its own `[dev-dependencies]` for the generated code to compile.
/// Not a stable API — user code should depend on `tokio` directly.
#[doc(hidden)]
pub mod __macro_support {
    pub use tokio;
}

/// Environment variable that turns a missing-root **failure** into a
/// **skip** for `#[lab_test]` functions and [`privileged`] tests. Off when
/// unset, empty, `0`, `false`, `no` or `off` (in any case); any other value
/// (conventionally `1`) is on, so a typo skips loudly rather than failing
/// quietly the other way.
pub const SKIP_ROOT_TESTS_ENV: &str = "NLINK_LAB_SKIP_ROOT_TESTS";

/// Environment variable naming the kernel features and tools a run must
/// have: a [`feature`] gate it names **fails** instead of skipping. A
/// comma-separated list of the names the tests use (`nftables,vrf`), or
/// `1`/`all` for every one (see [`feature_required`]). nlink's
/// `NLINK_TEST_STRICT_MODULES` is the same switch: a lane that lost a
/// module otherwise passes every test that needs it.
pub const STRICT_FEATURES_ENV: &str = "NLINK_LAB_STRICT_FEATURES";

/// True when the effective UID is 0.
pub fn is_root() -> bool {
    // SAFETY: geteuid(2) has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Root, or CAP_NET_ADMIN + CAP_SYS_ADMIN: what a lab deploy needs, and
/// what the CLI accepts. The gate checked [`is_root`] only, so a test
/// binary granted the capabilities always failed (#222).
pub fn has_privileges() -> bool {
    crate::privileges::has_privileges()
}

/// Outcome of the `#[lab_test]` privilege check. See [`root_gate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootGate {
    /// Running as root — deploy the lab and run the test.
    Proceed,
    /// Not root, but [`SKIP_ROOT_TESTS_ENV`] is set — return early
    /// without failing. The skip notice has already been printed.
    Skip,
    /// Not root and no opt-out — the test must fail with this message.
    Fail(String),
}

/// Decide whether a privileged test may run.
///
/// The default for a non-root process is [`RootGate::Fail`]: a
/// privileged test that silently returns green on a developer
/// laptop reports coverage that does not exist. Set
/// [`SKIP_ROOT_TESTS_ENV`]`=1` to get the old skip-and-pass
/// behaviour (the skip is printed to stderr so it is visible in
/// `cargo test -- --nocapture` / CI logs).
pub fn root_gate(test_name: &str) -> RootGate {
    let gate = gate_for(
        has_privileges(),
        std::env::var(SKIP_ROOT_TESTS_ENV).ok().as_deref(),
        test_name,
    );
    if gate == RootGate::Skip {
        eprintln!(
            "\n*** SKIPPING test '{test_name}' — not root and {SKIP_ROOT_TESTS_ENV} is set ***"
        );
    }
    gate
}

/// The decision [`root_gate`] makes, from the privileges and the value
/// of [`SKIP_ROOT_TESTS_ENV`].
pub fn gate_for(has_privileges: bool, skip_env: Option<&str>, test_name: &str) -> RootGate {
    if has_privileges {
        RootGate::Proceed
    } else if skip_env.is_some_and(env_flag) {
        RootGate::Skip
    } else {
        RootGate::Fail(root_required_message(test_name))
    }
}

/// True when [`SKIP_ROOT_TESTS_ENV`] is set to an opt-in value.
pub fn skip_root_tests_requested() -> bool {
    std::env::var(SKIP_ROOT_TESTS_ENV).is_ok_and(|v| env_flag(&v))
}

/// An on/off environment value, as nlink reads its test switches: off
/// for an empty value, `0`, `false`, `no` or `off`, in any case; on for
/// anything else.
fn env_flag(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// The privilege gate for a test written without `#[lab_test]`:
/// `if !privileged("my_test") { return; }`. True with root or
/// CAP_NET_ADMIN + CAP_SYS_ADMIN; false (a loud skip) without them when
/// [`SKIP_ROOT_TESTS_ENV`] is set; a panic otherwise, so a test that needs
/// root never passes without running.
pub fn privileged(test_name: &str) -> bool {
    match root_gate(test_name) {
        RootGate::Proceed => true,
        RootGate::Skip => false,
        RootGate::Fail(msg) => panic!("{msg}"),
    }
}

/// The gate on a kernel feature or a tool a test needs:
/// `if !feature("my_test", "vrf", has_vrf()) { return; }`. True when it is
/// `present`; otherwise a loud skip, or a panic when
/// [`STRICT_FEATURES_ENV`] names `what` (or is on for every feature).
pub fn feature(test_name: &str, what: &str, present: bool) -> bool {
    if present {
        return true;
    }
    let strict = std::env::var(STRICT_FEATURES_ENV).ok();
    if feature_required(strict.as_deref(), what) {
        panic!("'{test_name}' needs {what}, and {STRICT_FEATURES_ENV} requires it");
    }
    eprintln!("\n*** SKIPPING test '{test_name}' — {what} is not available here ***");
    false
}

/// Whether [`STRICT_FEATURES_ENV`]'s value requires `what`: `1`, `true`,
/// `yes`, `on` or `all` require every feature; `0`, `false`, `no`, `off`
/// or nothing, none; anything else is a comma-separated list of names.
pub fn feature_required(strict_env: Option<&str>, what: &str) -> bool {
    let Some(value) = strict_env.map(str::trim) else {
        return false;
    };
    match value.to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => false,
        "1" | "true" | "yes" | "on" | "all" => true,
        _ => value.split(',').map(str::trim).any(|name| name == what),
    }
}

/// The message a `#[lab_test]` (or a [`privileged`] test) panics with
/// when it is run without root and [`SKIP_ROOT_TESTS_ENV`] is off.
pub fn root_required_message(test_name: &str) -> String {
    format!(
        "test '{test_name}' needs root or CAP_NET_ADMIN+CAP_SYS_ADMIN; run under \
         sudo, or set {SKIP_ROOT_TESTS_ENV}=1 to skip"
    )
}

/// What [`cleanup_lab`] did. Every step is best-effort; anything
/// that went wrong lands in `warnings` instead of aborting the sweep.
#[derive(Debug, Default, Clone)]
pub struct CleanupReport {
    /// `RunningLab::load(..).destroy()` succeeded, so processes,
    /// containers, mgmt peers, hwsim and `/etc/hosts` were handled
    /// by the regular destroy path. The manual sweep still ran
    /// afterwards (it is idempotent) to catch anything destroy missed.
    pub destroyed_via_state: bool,
    /// Namespaces removed by the manual sweep (prefix match).
    pub namespaces_removed: Vec<String>,
    /// Root-namespace links (mgmt bridge / veth peers) removed by
    /// the manual sweep.
    pub links_removed: Vec<String>,
    /// Containers removed by name by the manual sweep.
    pub containers_removed: Vec<String>,
    /// Non-fatal problems encountered along the way.
    pub warnings: Vec<String>,
}

/// Tear down everything a (possibly half-deployed) lab may have left
/// on the host. Used by the `#[lab_test]` guard on the panic path
/// and after a failed `destroy`; also handy for ad-hoc tests that
/// deploy without the macro.
///
/// Order:
///
/// 1. If a state file exists, `RunningLab::load(name)?.destroy()`.
///    That is the authoritative path (kills spawned processes,
///    removes containers, mgmt peers, hwsim, `/etc/hosts` entries).
///    A load or destroy failure is recorded and the sweep continues.
/// 2. Manual sweep from the topology alone (no state needed):
///    - every namespace named `<prefix>-*` via
///      `nlink::netlink::namespace::{list, delete}` (plus its
///      `/etc/netns/<ns>` directory),
///    - the host-reachable mgmt bridge and its veth peers,
///    - containers named `<prefix>-<node>`,
///    - hostapd / wpa_supplicant configs when the topology has Wi-Fi nodes,
///    - `/etc/hosts` section, subnet-pool allocations, state dir.
///
/// Requires root; without it the sweep simply reports warnings.
pub async fn cleanup_lab(topology: &Topology) -> CleanupReport {
    let mut report = CleanupReport::default();
    let lab_name = topology.lab.name.clone();
    let prefix = topology.lab.prefix().to_string();

    // 1. Preferred path: the regular destroy, driven by the state file.
    if crate::state::exists(&lab_name) {
        match RunningLab::load(&lab_name) {
            Ok(lab) => match lab.destroy().await {
                Ok(()) => report.destroyed_via_state = true,
                Err(e) => report
                    .warnings
                    .push(format!("RunningLab::destroy('{lab_name}') failed: {e}")),
            },
            Err(e) => report
                .warnings
                .push(format!("RunningLab::load('{lab_name}') failed: {e}")),
        }
    }

    // 2. Manual sweep — idempotent, so it always runs.
    let ns_prefix = format!("{prefix}-");
    match namespace::list() {
        Ok(names) => {
            for ns in names.iter().filter(|n| n.starts_with(&ns_prefix)) {
                match namespace::delete(ns) {
                    Ok(()) => {
                        crate::dns::remove_netns_etc(ns);
                        crate::netns_tag::untag(ns);
                        report.namespaces_removed.push(ns.clone());
                    }
                    Err(e) => report
                        .warnings
                        .push(format!("delete namespace '{ns}': {e}")),
                }
            }
        }
        Err(e) => report.warnings.push(format!("list namespaces: {e}")),
    }

    if topology.lab.mgmt_host_reachable {
        match Connection::<Route>::new() {
            Ok(conn) => {
                let mut links: Vec<String> = (0..topology.nodes.len())
                    .map(|idx| topology.lab.mgmt_peer_name(idx))
                    .collect();
                links.push(topology.lab.mgmt_bridge_name());
                for link in links {
                    match conn.del_link_if_exists(link.as_str()).await {
                        Ok(true) => report.links_removed.push(link),
                        Ok(false) => {}
                        Err(e) => report
                            .warnings
                            .push(format!("delete root link '{link}': {e}")),
                    }
                }
            }
            Err(e) => report
                .warnings
                .push(format!("open rtnetlink for mgmt sweep: {e}")),
        }
    }

    let container_nodes: Vec<&String> = topology
        .nodes
        .iter()
        .filter(|(_, n)| n.image.is_some())
        .map(|(name, _)| name)
        .collect();
    if !container_nodes.is_empty() {
        let runtime = match &topology.lab.runtime {
            Some(rt) => crate::container::Runtime::new(rt),
            None => crate::container::Runtime::detect(),
        };
        match runtime {
            Ok(rt) => {
                for node in container_nodes {
                    let name = format!("{prefix}-{node}");
                    if rt.exists(&name) {
                        rt.remove(&name);
                        report.containers_removed.push(name);
                    }
                }
            }
            Err(e) => report.warnings.push(format!(
                "container runtime unavailable, skipping container sweep: {e}"
            )),
        }
    }

    // The lab's radios went with its namespaces (#176).
    if crate::wifi::count_wifi_nodes(topology) > 0 {
        crate::wifi::cleanup_configs(&lab_name);
    }

    if let Err(e) = crate::dns::remove_hosts(&lab_name) {
        report
            .warnings
            .push(format!("remove /etc/hosts entries: {e}"));
    }
    if let Err(e) = crate::subnet_pool::free_for_lab(&lab_name) {
        report
            .warnings
            .push(format!("free subnet pool entries: {e}"));
    }
    if let Err(e) = crate::state::remove(&lab_name) {
        report.warnings.push(format!("remove state dir: {e}"));
    }
    // …and its lock file, or `.locks/` grows by one per test (issue #103).
    crate::state::remove_lock_if_unheld(&lab_name);

    report
}

/// Synchronous wrapper around [`cleanup_lab`] that is safe to call
/// from a `Drop` impl — including while unwinding inside a tokio
/// runtime, where `block_on` on the current thread would panic with
/// "Cannot start a runtime from within a runtime". The sweep runs on
/// a fresh thread with its own current-thread runtime; this call
/// blocks until it finishes.
pub fn cleanup_lab_blocking(topology: &Topology) -> CleanupReport {
    let topology = topology.clone();
    let lab_name = topology.lab.name.clone();
    let failed = |msg: String| CleanupReport {
        warnings: vec![msg],
        ..CleanupReport::default()
    };
    let handle = std::thread::Builder::new()
        .name(format!("lab-cleanup-{lab_name}"))
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(cleanup_lab(&topology)),
                Err(e) => failed(format!("build cleanup runtime: {e}")),
            }
        });
    match handle {
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| failed(format!("cleanup thread for '{lab_name}' panicked"))),
        Err(e) => failed(format!("spawn cleanup thread for '{lab_name}': {e}")),
    }
}

/// A live packet-capture session covering one or more lab interfaces.
///
/// Each capture runs on its own dedicated thread via
/// [`spawn_capture`] — the same implementation the CLI's
/// `run_capture` uses — so the namespace `setns` never touches the
/// test's tokio workers. All captures share one `AtomicBool` stop
/// flag, which the loop polls every [`crate::capture::POLL_QUANTUM`]
/// (~200 ms), so `LabCapture::stop` returns within a bounded time
/// even on a completely idle link. On `persist_on_failure`, the
/// pcaps are moved from the temp dir to a discoverable location only
/// if the caller flagged failure; otherwise the temp dir is wiped on
/// `drop`.
///
/// Designed for use inside the `#[lab_test]` macro's `capture =
/// true` form.
pub struct LabCapture {
    /// `{namespace}-{iface}` → temp pcap path
    pcaps: HashMap<String, PathBuf>,
    /// Signal threads to stop.
    shutdown: Arc<AtomicBool>,
    /// `(ns_name, handle)` per running capture. Emptied on `stop`.
    handles: Vec<(String, CaptureHandle)>,
    /// Temp dir owning the pcap files. Dropped on success.
    temp_dir: Option<tempfile::TempDir>,
    /// True if `stop` has been called. Prevents double-stop.
    stopped: bool,
}

impl LabCapture {
    /// Start one capture per `(node-namespace-name, iface)` entry.
    ///
    /// Each capture writes to `<temp>/<namespace>-<iface>.pcap`: a node's
    /// interfaces all wrote `<namespace>.pcap`, truncating each other's
    /// (#222). Captures run until the helper is dropped.
    pub fn start(targets: &[(String, String)]) -> Result<Self> {
        let temp = tempfile::tempdir()
            .map_err(|e| Error::invalid_topology(format!("create temp dir for captures: {e}")))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut pcaps: HashMap<String, PathBuf> = HashMap::new();
        let mut handles: Vec<(String, CaptureHandle)> = Vec::new();

        for (ns_name, iface) in targets {
            let stem = format!("{ns_name}-{iface}");
            let pcap_path = temp.path().join(format!("{stem}.pcap"));
            pcaps.insert(stem, pcap_path.clone());

            let cfg = CaptureConfig {
                interface: iface.clone(),
                snap_len: 65536,
                bpf_filter: None,
                profile: RingProfile::Default,
                count: None,
                duration: None,
                ignore_outgoing: false,
            };
            let output = match CaptureOutput::pcap(&pcap_path) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!("lab_capture: failed to open pcap for '{ns_name}': {e}");
                    continue;
                }
            };
            match spawn_capture(ns_name.clone(), cfg, output, Arc::clone(&shutdown)) {
                Ok(h) => handles.push((ns_name.clone(), h)),
                Err(e) => tracing::warn!("lab_capture: failed to start '{ns_name}': {e}"),
            }
        }

        Ok(Self {
            pcaps,
            shutdown,
            handles,
            temp_dir: Some(temp),
            stopped: false,
        })
    }

    /// Stop all running captures. Idempotent.
    ///
    /// Raises the shared flag first so every thread starts winding
    /// down concurrently, then joins each one; total wait is bounded
    /// by ~one [`crate::capture::POLL_QUANTUM`] on an idle link.
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.shutdown.store(true, Ordering::Relaxed);
        for (ns_name, handle) in self.handles.drain(..) {
            if let Err(e) = handle.stop() {
                tracing::warn!("lab_capture: '{ns_name}' aborted: {e}");
            }
        }
    }

    /// Move pcaps to `dest_dir` and consume the helper.
    ///
    /// Use when the test failed and you want the artifacts to
    /// survive cleanup. `dest_dir` is created if needed.
    /// Returns the destination paths (one per capture).
    pub fn persist_to(mut self, dest_dir: &Path) -> Result<Vec<PathBuf>> {
        self.stop();
        std::fs::create_dir_all(dest_dir).map_err(|e| {
            Error::invalid_topology(format!("create capture dir {}: {e}", dest_dir.display(),))
        })?;
        let mut out = Vec::with_capacity(self.pcaps.len());
        for (stem, src) in &self.pcaps {
            let dst = dest_dir.join(format!("{stem}.pcap"));
            if src.exists() {
                if let Err(e) = std::fs::copy(src, &dst) {
                    tracing::warn!(
                        "lab_capture: copy {} → {}: {e}",
                        src.display(),
                        dst.display(),
                    );
                    continue;
                }
                out.push(dst);
            }
        }
        // The TempDir's drop wipes the source files.
        self.temp_dir.take();
        Ok(out)
    }

    /// Conditionally persist: if `failure` is true, move pcaps
    /// to `dest_dir`; otherwise discard.
    ///
    /// Designed for the `#[lab_test]` macro's `capture = true`
    /// path: the macro detects panic via `std::panic::catch_unwind`
    /// then calls this with the result.
    pub fn persist_on_failure_in(
        self,
        failure: bool,
        dest_dir: &Path,
    ) -> Result<Option<Vec<PathBuf>>> {
        if failure {
            Ok(Some(self.persist_to(dest_dir)?))
        } else {
            // self drops, temp_dir wipes the pcaps.
            Ok(None)
        }
    }
}

impl Drop for LabCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Where `#[lab_test(capture = true)]` keeps a failed test's pcaps:
/// `$CARGO_TARGET_DIR/lab_test_captures/<test>-<pid>/`, else the
/// workspace's `target/`. It was a `target` relative to the test's working
/// directory, which cargo sets to the package's, so in a workspace the
/// pcaps went to `crates/<pkg>/target/` (#222).
pub fn capture_dir(workspace_root: &str, test: &str) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(workspace_root).join("target"))
        .join("lab_test_captures")
        .join(format!("{test}-{}", std::process::id()))
}

/// The `capture = true` state of a `#[lab_test]`, shared by the thread that
/// runs the test and the one that enforces its `timeout`: whichever
/// finishes the capture first decides whether the pcaps are kept.
#[derive(Clone)]
pub struct CaptureSlot {
    cap: Arc<std::sync::Mutex<Option<LabCapture>>>,
    dest: PathBuf,
}

impl CaptureSlot {
    /// An empty slot whose pcaps would be kept in `dest`.
    pub fn new(dest: PathBuf) -> Self {
        Self {
            cap: Arc::default(),
            dest,
        }
    }

    /// Capture every interface of `lab` ([`RunningLab::capture_targets`]).
    pub fn start(&self, lab: &crate::RunningLab) {
        match LabCapture::start(&lab.capture_targets()) {
            Ok(cap) => *self.cap.lock().unwrap_or_else(|p| p.into_inner()) = Some(cap),
            Err(e) => eprintln!("lab_capture: failed to start: {e}"),
        }
    }

    /// Stop capturing; keep the pcaps when `failed`, else discard them.
    /// Only the first call does anything.
    pub fn finish(&self, failed: bool) {
        let Some(cap) = self.cap.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return;
        };
        match cap.persist_on_failure_in(failed, &self.dest) {
            Ok(Some(paths)) => {
                eprintln!(
                    "lab_capture: persisted {} pcap(s) to {}",
                    paths.len(),
                    self.dest.display(),
                );
                for p in paths {
                    eprintln!("  - {}", p.display());
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("lab_capture: persist failed: {e}"),
        }
    }

    /// A guard that finishes the capture when dropped, keeping the pcaps
    /// when the thread is panicking.
    pub fn guard(&self) -> CaptureGuard {
        CaptureGuard(self.clone())
    }
}

/// See [`CaptureSlot::guard`].
pub struct CaptureGuard(CaptureSlot);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.finish(std::thread::panicking());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without privileges a test fails unless the skip variable is on;
    /// `no` and `off` are off, as nlink reads them (they used to turn the
    /// skip on).
    #[test]
    fn the_root_gate_fails_unless_the_skip_is_on() {
        assert_eq!(gate_for(true, None, "t"), RootGate::Proceed);
        assert_eq!(gate_for(true, Some("1"), "t"), RootGate::Proceed);
        for on in ["1", "true", "yes", "YES", "on", "x"] {
            assert_eq!(gate_for(false, Some(on), "t"), RootGate::Skip, "{on:?}");
        }
        for off in [
            None,
            Some(""),
            Some(" "),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("Off"),
            Some("FALSE"),
        ] {
            assert!(
                matches!(gate_for(false, off, "t"), RootGate::Fail(m) if m.contains("'t'")),
                "{off:?}"
            );
        }
    }

    #[test]
    fn strict_features_name_a_list_or_all() {
        assert!(!feature_required(None, "vrf"));
        for none in ["", "0", "off", "no", "False"] {
            assert!(!feature_required(Some(none), "vrf"), "{none:?}");
        }
        for all in ["1", "all", "ON", "true"] {
            assert!(feature_required(Some(all), "vrf"), "{all:?}");
        }
        assert!(feature_required(Some("nftables, vrf"), "vrf"));
        assert!(!feature_required(Some("nftables,vrf"), "wireguard"));
        assert!(!feature_required(Some("vrfs"), "vrf"));
    }

    #[test]
    fn a_present_feature_runs_and_a_missing_one_skips_unless_required() {
        assert!(feature("t", "vrf", true));
        // the variable is unset in the unit tests' environment
        if std::env::var_os(STRICT_FEATURES_ENV).is_none() {
            assert!(!feature("t", "vrf", false));
        }
    }
}
