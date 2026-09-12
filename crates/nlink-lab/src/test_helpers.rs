//! Helpers for `#[lab_test]` integration tests.
//!
//! This module ships the runtime pieces the `#[lab_test]` proc macro
//! expands to, so a consumer crate only needs `nlink-lab` as a
//! dev-dependency:
//!
//! - [`root_gate`] — the privilege check (fail by default, skip when
//!   [`SKIP_ROOT_TESTS_ENV`] is set).
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
/// **skip** for `#[lab_test]` functions. Set it to any value other
/// than `""`, `"0"` or `"false"` (conventionally `1`).
pub const SKIP_ROOT_TESTS_ENV: &str = "NLINK_LAB_SKIP_ROOT_TESTS";

/// True when the effective UID is 0.
pub fn is_root() -> bool {
    // SAFETY: geteuid(2) has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
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
    if is_root() {
        return RootGate::Proceed;
    }
    if skip_root_tests_requested() {
        eprintln!(
            "\n*** SKIPPING #[lab_test] '{test_name}' — not root and \
             {SKIP_ROOT_TESTS_ENV} is set ***"
        );
        return RootGate::Skip;
    }
    RootGate::Fail(root_required_message(test_name))
}

/// True when [`SKIP_ROOT_TESTS_ENV`] is set to an opt-in value.
pub fn skip_root_tests_requested() -> bool {
    std::env::var(SKIP_ROOT_TESTS_ENV)
        .map(|v| !matches!(v.trim(), "" | "0" | "false"))
        .unwrap_or(false)
}

/// The message a `#[lab_test]` panics with when it is run without
/// root and [`SKIP_ROOT_TESTS_ENV`] is unset.
pub fn root_required_message(test_name: &str) -> String {
    format!(
        "#[lab_test] '{test_name}' needs root/CAP_NET_ADMIN; run under sudo, \
         or set {SKIP_ROOT_TESTS_ENV}=1 to skip"
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
///    - hwsim configs / module when the topology has Wi-Fi nodes,
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

    if crate::wifi::count_wifi_nodes(topology) > 0 {
        crate::wifi::cleanup_configs(&lab_name);
        crate::wifi::unload_hwsim();
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
    let handle = thread::Builder::new()
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
/// (~200 ms), so [`LabCapture::stop`] returns within a bounded time
/// even on a completely idle link. On `persist_on_failure`, the
/// pcaps are moved from the temp dir to a discoverable location only
/// if the caller flagged failure; otherwise the temp dir is wiped on
/// `drop`.
///
/// Designed for use inside the `#[lab_test]` macro's `capture =
/// true` form.
pub struct LabCapture {
    /// node-name → temp pcap path
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
    /// Each capture writes to `<temp>/<node>.pcap`. Captures run
    /// until the helper is dropped.
    pub fn start(targets: &[(String, String)]) -> Result<Self> {
        let temp = tempfile::tempdir()
            .map_err(|e| Error::invalid_topology(format!("create temp dir for captures: {e}")))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut pcaps: HashMap<String, PathBuf> = HashMap::new();
        let mut handles: Vec<(String, CaptureHandle)> = Vec::new();

        for (ns_name, iface) in targets {
            let pcap_path = temp.path().join(format!("{ns_name}.pcap"));
            pcaps.insert(ns_name.clone(), pcap_path.clone());

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
        for (ns, src) in &self.pcaps {
            let dst = dest_dir.join(format!("{ns}.pcap"));
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
