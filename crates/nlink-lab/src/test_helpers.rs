//! Helpers for `#[lab_test]` integration tests.
//!
//! Today this module ships [`LabCapture`] — the engine behind the
//! macro's `capture = true` form. On failure, every capture pcap
//! is persisted to a discoverable directory; on success, captures
//! are discarded.
//!
//! More helpers (typed `wait_for_route`, `wait_for_tcp`, `ping`,
//! `iperf3`) ship in subsequent Plan 154 polish PRs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crate::capture::{run_capture, CaptureConfig};
use netring::RingProfile;
use crate::error::{Error, Result};

/// A live packet-capture session covering one or more lab interfaces.
///
/// Captures run in dedicated threads (entering each namespace
/// once, then sticking there). Captures are stopped via a shared
/// `AtomicBool`. On `persist_on_failure`, the pcaps are moved
/// from the temp dir to a discoverable location only if the
/// caller flagged failure; otherwise the temp dir is wiped on
/// `drop`.
///
/// Designed for use inside the `#[lab_test]` macro's `capture =
/// true` form.
pub struct LabCapture {
    /// node-name → temp pcap path
    pcaps: HashMap<String, PathBuf>,
    /// Signal threads to stop.
    shutdown: Arc<AtomicBool>,
    /// Capture-thread join handles. Emptied on `stop`.
    handles: Vec<thread::JoinHandle<()>>,
    /// Temp dir owning the pcap files. Dropped on success.
    temp_dir: Option<tempfile::TempDir>,
    /// True if `stop` has been called. Prevents double-stop.
    stopped: bool,
}

impl LabCapture {
    /// Start one capture per `(node-namespace-name, iface)` entry.
    ///
    /// Each capture writes to `<temp>/<node>.pcap`. Captures run
    /// until [`stop`] is called or the helper is dropped.
    pub fn start(targets: &[(String, String)]) -> Result<Self> {
        let temp = tempfile::tempdir().map_err(|e| {
            Error::invalid_topology(format!("create temp dir for captures: {e}"))
        })?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut pcaps: HashMap<String, PathBuf> = HashMap::new();
        let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();

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
            };
            let shutdown_thread = Arc::clone(&shutdown);
            let ns_name_thread = ns_name.clone();
            let handle = thread::spawn(move || {
                let f = match std::fs::File::create(&pcap_path) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(
                            "lab_capture: failed to open pcap for '{ns_name_thread}': {e}"
                        );
                        return;
                    }
                };
                if let Err(e) = run_capture(
                    &ns_name_thread,
                    &cfg,
                    Some(f),
                    &shutdown_thread,
                ) {
                    tracing::warn!(
                        "lab_capture: '{ns_name_thread}' aborted: {e}"
                    );
                }
            });
            handles.push(handle);
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
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.shutdown.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
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
            Error::invalid_topology(format!(
                "create capture dir {}: {e}",
                dest_dir.display(),
            ))
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
