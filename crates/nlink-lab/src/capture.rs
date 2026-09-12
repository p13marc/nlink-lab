//! Packet capture using netring with pcap file output.
//!
//! Enters a lab node's network namespace, creates an AF_PACKET capture via
//! netring, and either writes packets as pcap or prints one-line summaries.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(feature = "legacy-tcpdump-filter")]
use netring::BpfInsn;
use netring::{BpfFilter, Capture, CaptureStats, RingProfile};
use nlink::netlink::namespace;

use crate::error::{Error, Result};

// ── Pcap writer ───────────────────────────────────────────────────────────

/// Nanosecond-resolution pcap magic (supported by Wireshark, tshark, tcpdump).
const PCAP_MAGIC_NS: u32 = 0xa1b2_3c4d;
const PCAP_VERSION_MAJOR: u16 = 2;
const PCAP_VERSION_MINOR: u16 = 4;
/// LINKTYPE_ETHERNET
const LINKTYPE_ETHERNET: u32 = 1;

/// Minimal pcap file writer (nanosecond timestamp variant).
///
/// Writes are unbuffered — every `write_packet` flushes the bytes to the
/// underlying `W` directly, so a SIGKILL or abrupt termination still leaves
/// a complete pcap up to the last fully-written packet. Matches `tcpdump
/// -U`. Capture in this tool runs at debugging packet rates, not line-rate,
/// so the syscall cost is irrelevant.
struct PcapWriter<W: Write> {
    writer: W,
    snap_len: u32,
}

impl<W: Write> PcapWriter<W> {
    /// Create a new pcap writer, immediately writing the 24-byte global header.
    fn new(mut writer: W, snap_len: u32) -> io::Result<Self> {
        // Global header: magic, version, thiszone, sigfigs, snaplen, network
        writer.write_all(&PCAP_MAGIC_NS.to_le_bytes())?;
        writer.write_all(&PCAP_VERSION_MAJOR.to_le_bytes())?;
        writer.write_all(&PCAP_VERSION_MINOR.to_le_bytes())?;
        writer.write_all(&0i32.to_le_bytes())?; // thiszone
        writer.write_all(&0u32.to_le_bytes())?; // sigfigs
        writer.write_all(&snap_len.to_le_bytes())?;
        writer.write_all(&LINKTYPE_ETHERNET.to_le_bytes())?;
        writer.flush()?;
        Ok(Self { writer, snap_len })
    }

    /// Write a single packet record (16-byte header + data) and flush.
    fn write_packet(
        &mut self,
        ts: netring::Timestamp,
        data: &[u8],
        orig_len: u32,
    ) -> io::Result<()> {
        let incl_len = (data.len() as u32).min(self.snap_len);
        self.writer.write_all(&ts.sec.to_le_bytes())?;
        self.writer.write_all(&ts.nsec.to_le_bytes())?;
        self.writer.write_all(&incl_len.to_le_bytes())?;
        self.writer.write_all(&orig_len.to_le_bytes())?;
        self.writer.write_all(&data[..incl_len as usize])?;
        self.writer.flush()
    }
}

// ── Rotating pcap writer ──────────────────────────────────────────────────

/// Rotating pcap file sink. Wraps a `PcapWriter<File>` and an
/// optional rotation policy (size and/or time-based). When the
/// active segment crosses the threshold, the file is closed,
/// existing segments are renamed to make room (`base.pcap` →
/// `base.pcap.1`, `.1` → `.2`, etc., dropping anything past `keep`),
/// and a new segment is started with a fresh pcap global header.
///
/// Per-packet writes are unbuffered (inherited from `PcapWriter`),
/// so a SIGKILL between rotations still leaves all completed
/// segments intact and the active segment complete up to the last
/// fully-written packet. Round-5 §2.3.
pub struct RotatingPcapWriter {
    base: PathBuf,
    max_size: Option<u64>,
    rotate_after: Option<Duration>,
    /// Maximum number of *rotated* segments to keep (i.e. `.pcap.1`
    /// through `.pcap.keep`). The active `.pcap` doesn't count
    /// against this. `usize::MAX` means unlimited.
    keep: usize,
    snap_len: u32,
    writer: Option<PcapWriter<File>>,
    bytes_written: u64,
    rotated_at: Instant,
}

impl RotatingPcapWriter {
    /// Create a new rotating sink writing to `base`. The active
    /// segment is created immediately with a pcap global header.
    pub fn new(
        base: PathBuf,
        max_size: Option<u64>,
        rotate_after: Option<Duration>,
        keep: usize,
        snap_len: u32,
    ) -> io::Result<Self> {
        let file = File::create(&base)?;
        let writer = PcapWriter::new(file, snap_len)?;
        Ok(Self {
            base,
            max_size,
            rotate_after,
            keep,
            snap_len,
            writer: Some(writer),
            bytes_written: PCAP_GLOBAL_HEADER_BYTES,
            rotated_at: Instant::now(),
        })
    }

    /// Write a packet, rotating first if the active segment has
    /// crossed the configured threshold.
    pub fn write_packet(
        &mut self,
        ts: netring::Timestamp,
        data: &[u8],
        orig_len: u32,
    ) -> io::Result<()> {
        let pkt_size = PCAP_RECORD_HEADER_BYTES + (data.len() as u64).min(self.snap_len as u64);
        if self.should_rotate(pkt_size) {
            self.rotate()?;
        }
        if let Some(ref mut w) = self.writer {
            w.write_packet(ts, data, orig_len)?;
        }
        self.bytes_written += pkt_size;
        Ok(())
    }

    fn should_rotate(&self, next_pkt_size: u64) -> bool {
        if let Some(max) = self.max_size
            && self.bytes_written + next_pkt_size > max
        {
            return true;
        }
        if let Some(after) = self.rotate_after
            && self.rotated_at.elapsed() >= after
        {
            return true;
        }
        false
    }

    /// Close the active segment, shift older segments by one index,
    /// drop anything past `keep`, and start a fresh segment.
    fn rotate(&mut self) -> io::Result<()> {
        // Drop the writer first so its File is closed before we
        // rename it.
        self.writer = None;

        // Drop the oldest segment if we're at the keep limit. `keep`
        // is the *max* number of rotated segments; if user passed
        // keep=3, files are .pcap.1, .pcap.2, .pcap.3, and we drop
        // .pcap.4 before shifting everything up.
        if self.keep != usize::MAX {
            let oldest = self.segment_path(self.keep + 1);
            let _ = std::fs::remove_file(&oldest);
        }

        // Shift `.pcap.<keep>` → `.pcap.<keep+1>` if no keep limit;
        // otherwise shift `.pcap.<keep-1>` → `.pcap.<keep>`. Walk
        // from oldest existing index down to 1.
        let max_idx = self.keep.min(usize::MAX - 1);
        for i in (1..=max_idx).rev() {
            let from = self.segment_path(i);
            let to = self.segment_path(i + 1);
            if from.exists() && i < self.keep {
                let _ = std::fs::rename(&from, &to);
            } else if from.exists() {
                let _ = std::fs::remove_file(&from);
            }
        }

        // Move the active segment to .pcap.1.
        if self.base.exists() && self.keep >= 1 {
            let _ = std::fs::rename(&self.base, self.segment_path(1));
        } else if self.keep == 0 {
            // keep=0 means "never retain rotated segments". Just
            // drop the active segment when rotating.
            let _ = std::fs::remove_file(&self.base);
        }

        // Open new active segment with a fresh global header.
        let file = File::create(&self.base)?;
        self.writer = Some(PcapWriter::new(file, self.snap_len)?);
        self.bytes_written = PCAP_GLOBAL_HEADER_BYTES;
        self.rotated_at = Instant::now();
        Ok(())
    }

    fn segment_path(&self, idx: usize) -> PathBuf {
        let mut s = self.base.as_os_str().to_os_string();
        s.push(format!(".{idx}"));
        PathBuf::from(s)
    }
}

const PCAP_GLOBAL_HEADER_BYTES: u64 = 24;
const PCAP_RECORD_HEADER_BYTES: u64 = 16;

// ── BPF filter compilation ────────────────────────────────────────────────

/// Compile a tcpdump filter expression into a [`BpfFilter`].
///
/// Shells out to `tcpdump -dd` which outputs C-style BPF bytecode.
/// Requires `tcpdump` (and `libpcap`) installed on the system.
///
/// **This is the legacy path.** Prefer [`netring::BpfFilter::builder`]
/// (re-exported as `netring::BpfFilter`) for typed, dependency-free
/// filter construction. Available only when nlink-lab is built with
/// the `legacy-tcpdump-filter` feature.
#[cfg(feature = "legacy-tcpdump-filter")]
pub fn compile_bpf_filter(expression: &str) -> Result<BpfFilter> {
    let output = std::process::Command::new("tcpdump")
        .args(["-dd", expression])
        .output()
        .map_err(|e| Error::Capture(format!("failed to run tcpdump for BPF compilation: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Capture(format!(
            "tcpdump filter compilation failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut insns = Vec::new();
    for line in stdout.lines() {
        // Lines look like: { 0x28, 0, 0, 0x0000000c },
        let trimmed = line
            .trim()
            .trim_start_matches('{')
            .trim_end_matches([',', '}', ' ']);
        let parts: Vec<&str> = trimmed.split(',').map(|s| s.trim()).collect();
        if parts.len() == 4 {
            let code = parse_hex_or_dec(parts[0])?;
            let jt = parse_hex_or_dec(parts[1])? as u8;
            let jf = parse_hex_or_dec(parts[2])? as u8;
            let k = parse_hex_or_dec(parts[3])? as u32;
            insns.push(BpfInsn {
                code: code as u16,
                jt,
                jf,
                k,
            });
        }
    }

    if insns.is_empty() {
        return Err(Error::Capture(
            "tcpdump produced no BPF instructions".into(),
        ));
    }

    BpfFilter::new(insns).map_err(|e| Error::Capture(format!("invalid BPF filter: {e}")))
}

#[cfg(feature = "legacy-tcpdump-filter")]
fn parse_hex_or_dec(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map_err(|e| Error::Capture(format!("invalid BPF value '{s}': {e}")))
    } else {
        s.parse::<u64>()
            .map_err(|e| Error::Capture(format!("invalid BPF value '{s}': {e}")))
    }
}

// ── Capture config & result ───────────────────────────────────────────────

/// Configuration for a packet capture session.
pub struct CaptureConfig {
    /// Network interface to capture on.
    pub interface: String,
    /// Maximum bytes per packet (default: 262144).
    pub snap_len: u32,
    /// Stop after N packets.
    pub count: Option<u64>,
    /// Stop after this duration.
    pub duration: Option<Duration>,
    /// Compiled BPF filter. Build via [`netring::BpfFilter::builder`]
    /// for typed, dependency-free construction. Set to `None` to
    /// capture every packet on the interface.
    pub bpf_filter: Option<BpfFilter>,
    /// Ring buffer profile.
    pub profile: RingProfile,
    /// When true, set `PACKET_IGNORE_OUTGOING` on the AF_PACKET socket
    /// so the kernel skips outgoing packets. The intended use case is
    /// loopback (`lo`) capture, where every packet otherwise appears
    /// twice — once with `PACKET_OUTGOING` (send-side BPF tap) and
    /// once with `PACKET_HOST` (receive-side). Default: false.
    /// (Round-5 §2.6.)
    pub ignore_outgoing: bool,
}

/// Result of a completed capture session.
#[derive(Debug)]
pub struct CaptureResult {
    /// Number of packets captured.
    pub packets_captured: u64,
    /// Kernel-reported statistics (packets seen, drops, freezes).
    pub stats: CaptureStats,
    /// Why the loop ended.
    pub stop_reason: StopReason,
}

/// Where captured packets go. Selects between summary printing,
/// single-file pcap output, and rotating-pcap output. Constructed
/// by the CLI based on `--write` / `--max-size` / `--rotate` flags.
pub enum CaptureOutput {
    /// Print a one-line summary per packet to stdout.
    Summaries,
    /// Write all packets to a single pcap. Closed when the loop exits.
    Pcap(File),
    /// Write rotating pcap segments. See [`RotatingPcapWriter`].
    RotatingPcap {
        base: PathBuf,
        max_size: Option<u64>,
        rotate_after: Option<Duration>,
        keep: usize,
    },
}

impl CaptureOutput {
    /// Convenience constructor: `--write <path>` with no rotation.
    pub fn pcap(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(CaptureOutput::Pcap(File::create(path.as_ref())?))
    }
}

/// Internal packet sink used by [`run_capture`]. Erased over the
/// public `CaptureOutput` enum so the inner loop is monomorphic.
enum PcapSink {
    None,
    Single(PcapWriter<File>),
    Rotating(RotatingPcapWriter),
}

impl PcapSink {
    fn write_packet(
        &mut self,
        ts: netring::Timestamp,
        data: &[u8],
        orig_len: u32,
    ) -> io::Result<()> {
        match self {
            PcapSink::None => Ok(()),
            PcapSink::Single(w) => w.write_packet(ts, data, orig_len),
            PcapSink::Rotating(w) => w.write_packet(ts, data, orig_len),
        }
    }
}

// ── Main capture loop ─────────────────────────────────────────────────────

/// How long a single blocking poll on the ring may last before the loop
/// re-checks the `shutdown` flag and the `--duration` deadline.
///
/// netring's `Packets::next_packet` retries its internal `poll(2)`
/// indefinitely on timeout, so it can never observe an external stop
/// request on an idle interface (issue #33). We drive
/// `Capture::next_batch_blocking` ourselves with this quantum instead;
/// the flag and deadline are therefore honoured within roughly this
/// interval even when no traffic arrives.
pub const POLL_QUANTUM: Duration = Duration::from_millis(200);

/// One captured packet, decoupled from netring's lending `Packet` so the
/// capture loop can be exercised by a fake source in unit tests.
#[derive(Debug, Clone, Copy)]
pub struct PacketRecord<'a> {
    /// Capture timestamp.
    pub ts: netring::Timestamp,
    /// Captured bytes (already truncated to the snap length by the kernel).
    pub data: &'a [u8],
    /// Original on-wire length.
    pub orig_len: u32,
}

/// Why the capture loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The `shutdown` flag was raised (Ctrl-C / SIGTERM / `LabCapture::stop`).
    Shutdown,
    /// `CaptureConfig::duration` elapsed.
    Deadline,
    /// `CaptureConfig::count` packets were captured.
    CountReached,
}

/// Stop conditions for [`drive_capture_loop`]. Mirrors the `count` /
/// `duration` fields of [`CaptureConfig`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureLimits {
    /// Stop after N packets.
    pub count: Option<u64>,
    /// Stop after this duration (measured from loop entry).
    pub duration: Option<Duration>,
}

impl From<&CaptureConfig> for CaptureLimits {
    fn from(c: &CaptureConfig) -> Self {
        Self {
            count: c.count,
            duration: c.duration,
        }
    }
}

/// Callback that receives each packet of one poll. Return `Ok(false)` to
/// stop consuming the current batch (the loop then exits).
pub type PacketSink<'s> = dyn FnMut(PacketRecord<'_>) -> Result<bool> + 's;

/// The capture loop, factored out of [`run_capture`] so its stop logic is
/// testable without root or a ring.
///
/// `source(timeout, sink)` must block for **at most** `timeout`, then hand
/// every packet that arrived to `sink` (stopping early when `sink` returns
/// `Ok(false)`), and return `Ok(())` — also when nothing arrived. The
/// production source wraps `Capture::next_batch_blocking`; tests use
/// closures.
///
/// Between polls the loop checks `shutdown` and the deadline, and each
/// poll is bounded by `min(POLL_QUANTUM, time-to-deadline)`, so both
/// conditions are honoured within ~[`POLL_QUANTUM`] on an idle interface.
/// `on_packet` is invoked for each packet; its error aborts the loop.
///
/// Returns the number of packets delivered to `on_packet` and why the
/// loop stopped.
pub fn drive_capture_loop<S, F>(
    limits: CaptureLimits,
    shutdown: &AtomicBool,
    mut source: S,
    mut on_packet: F,
) -> Result<(u64, StopReason)>
where
    S: FnMut(Duration, &mut PacketSink<'_>) -> Result<()>,
    F: FnMut(PacketRecord<'_>) -> Result<()>,
{
    let deadline = limits.duration.map(|d| Instant::now() + d);
    let mut count: u64 = 0;

    // `--count 0` is "stop immediately" — don't wait for a packet we'd
    // discard anyway.
    if limits.count == Some(0) {
        return Ok((0, StopReason::CountReached));
    }

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok((count, StopReason::Shutdown));
        }
        let timeout = match deadline {
            Some(d) => {
                let remaining = d.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok((count, StopReason::Deadline));
                }
                remaining.min(POLL_QUANTUM)
            }
            None => POLL_QUANTUM,
        };

        let mut stop: Option<StopReason> = None;
        source(timeout, &mut |rec: PacketRecord<'_>| {
            on_packet(rec)?;
            count += 1;
            if let Some(max) = limits.count
                && count >= max
            {
                stop = Some(StopReason::CountReached);
                return Ok(false);
            }
            // Re-check the flag between packets of a large batch so a
            // stop request during a flood is still prompt.
            if shutdown.load(Ordering::Relaxed) {
                stop = Some(StopReason::Shutdown);
                return Ok(false);
            }
            Ok(true)
        })?;

        if let Some(reason) = stop {
            return Ok((count, reason));
        }
    }
}

/// Enter `ns_name`, open the ring, and run the capture loop **on the
/// calling thread**.
///
/// `namespace::enter` calls `setns(CLONE_NEWNET)`, which retargets the
/// *calling thread* — so this must only ever run on a thread nobody
/// else schedules work onto. [`run_capture`] and [`spawn_capture`] are
/// the public entry points; both create that thread. The guard is
/// dropped as soon as the socket exists (the fd stays bound to the
/// target namespace), but the thread is still consumed by the blocking
/// loop for the whole capture.
fn capture_in_namespace(
    ns_name: &str,
    config: &CaptureConfig,
    output: CaptureOutput,
    shutdown: &AtomicBool,
) -> Result<CaptureResult> {
    let guard = namespace::enter(ns_name)?;

    let mut builder = Capture::builder()
        .interface(&config.interface)
        .profile(config.profile)
        .snap_len(config.snap_len)
        .ignore_outgoing(config.ignore_outgoing);

    if let Some(ref filter) = config.bpf_filter {
        builder = builder.bpf_filter(filter.clone());
    }

    let mut capture = builder
        .build()
        .map_err(|e| Error::Capture(format!("netring: {e}")))?;

    // Restore namespace — the socket fd remains bound to the target namespace.
    drop(guard);

    // Set up output
    let mut pcap = match output {
        CaptureOutput::Summaries => PcapSink::None,
        CaptureOutput::Pcap(file) => PcapSink::Single(PcapWriter::new(file, config.snap_len)?),
        CaptureOutput::RotatingPcap {
            base,
            max_size,
            rotate_after,
            keep,
        } => PcapSink::Rotating(RotatingPcapWriter::new(
            base,
            max_size,
            rotate_after,
            keep,
            config.snap_len,
        )?),
    };

    // Bounded poll source over the ring. Each `PacketBatch` is a zero-copy
    // view of one ring block (netring 0.30); it is released back to the
    // kernel when the batch drops at the end of the closure, so no packet
    // borrow escapes. Scoped so the `&mut capture` borrow ends before
    // `capture.stats()`.
    let (count, stop_reason) = {
        let source = |timeout: Duration, sink: &mut PacketSink<'_>| -> Result<()> {
            let batch = capture
                .next_batch_blocking(timeout)
                .map_err(|e| Error::Capture(format!("netring: {e}")))?;
            let Some(batch) = batch else {
                return Ok(());
            };
            for pkt in batch.iter() {
                let rec = PacketRecord {
                    ts: pkt.timestamp(),
                    data: pkt.data(),
                    orig_len: pkt.original_len() as u32,
                };
                if !sink(rec)? {
                    break;
                }
            }
            Ok(())
        };

        drive_capture_loop(
            CaptureLimits::from(config),
            shutdown,
            source,
            |rec| match &mut pcap {
                PcapSink::None => {
                    println!(
                        "{}.{:09}  {} bytes",
                        rec.ts.sec,
                        rec.ts.nsec,
                        rec.data.len()
                    );
                    Ok(())
                }
                _ => Ok(pcap.write_packet(rec.ts, rec.data, rec.orig_len)?),
            },
        )?
    };

    // No trailing flush needed — `PcapWriter::write_packet` already flushes
    // per-packet, so a SIGKILL between the loop body and this point still
    // leaves a complete pcap.
    drop(pcap);

    let stats = capture.stats().unwrap_or_default();

    Ok(CaptureResult {
        packets_captured: count,
        stats,
        stop_reason,
    })
}

/// Map a thread-panic payload into a capture error.
fn panic_to_error(payload: Box<dyn std::any::Any + Send>) -> Error {
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into());
    Error::Capture(format!("capture thread panicked: {msg}"))
}

/// Run a packet capture in the given namespace and block until it ends.
///
/// The namespace is entered **on a dedicated (scoped) thread** — never
/// the caller's. `setns(CLONE_NEWNET)` is per-thread, so entering it on
/// a tokio worker would silently run every task later scheduled onto
/// that worker inside the lab namespace (issue #33). The calling thread
/// only blocks on the join.
///
/// The loop ends when `shutdown` is raised, `config.duration` elapses,
/// or `config.count` packets were seen — each honoured within
/// ~[`POLL_QUANTUM`] even on an idle interface. `output` selects
/// pcap-vs-summary and single-vs-rotating.
///
/// For a non-blocking start (e.g. from async code, or for several
/// captures at once) use [`spawn_capture`].
pub fn run_capture(
    ns_name: &str,
    config: &CaptureConfig,
    output: CaptureOutput,
    shutdown: &AtomicBool,
) -> Result<CaptureResult> {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name(format!("capture-{ns_name}"))
            .spawn_scoped(s, || {
                capture_in_namespace(ns_name, config, output, shutdown)
            })
            .map_err(|e| Error::Capture(format!("spawn capture thread: {e}")))?;
        worker.join().unwrap_or_else(|p| Err(panic_to_error(p)))
    })
}

/// A capture running on its own dedicated thread. Obtained from
/// [`spawn_capture`].
///
/// Dropping the handle without joining raises the shutdown flag so the
/// detached thread exits within ~[`POLL_QUANTUM`]; the result is lost.
/// Note the flag is shared with whoever else holds the `Arc`, so several
/// captures started with one flag all stop together.
pub struct CaptureHandle {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<CaptureResult>>>,
}

impl CaptureHandle {
    /// The flag this capture polls. Store `true` to request a stop.
    pub fn shutdown_flag(&self) -> &Arc<AtomicBool> {
        &self.shutdown
    }

    /// True once the capture thread has exited (for any reason).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Raise the shutdown flag and wait for the thread. Returns within
    /// ~[`POLL_QUANTUM`] on an idle interface.
    pub fn stop(mut self) -> Result<CaptureResult> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.join_inner()
    }

    /// Wait for the capture to end on its own (`count`, `duration`, or a
    /// flag raised elsewhere). Blocks — call from a blocking context.
    pub fn join(mut self) -> Result<CaptureResult> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> Result<CaptureResult> {
        match self.thread.take() {
            Some(t) => t.join().unwrap_or_else(|p| Err(panic_to_error(p))),
            None => Err(Error::Capture("capture already joined".into())),
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }
}

/// Start a capture on a dedicated thread and return immediately.
///
/// Same semantics as [`run_capture`] (namespace entered on the new
/// thread only; flag/deadline honoured within ~[`POLL_QUANTUM`]), but
/// the caller keeps running. Stop or await it via the returned
/// [`CaptureHandle`]. Async callers should wrap the (blocking) join in
/// `tokio::task::spawn_blocking`.
pub fn spawn_capture(
    ns_name: String,
    config: CaptureConfig,
    output: CaptureOutput,
    shutdown: Arc<AtomicBool>,
) -> Result<CaptureHandle> {
    let flag = Arc::clone(&shutdown);
    let thread = std::thread::Builder::new()
        .name(format!("capture-{ns_name}"))
        .spawn(move || capture_in_namespace(&ns_name, &config, output, &flag))
        .map_err(|e| Error::Capture(format!("spawn capture thread: {e}")))?;
    Ok(CaptureHandle {
        shutdown,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// `Write` impl that pushes into a shared `Vec<u8>` so the test can
    /// observe what's been written *while* the writer is still alive (i.e.
    /// without relying on an explicit flush or drop).
    struct SharedSink(Rc<RefCell<Vec<u8>>>);
    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// `PcapWriter::write_packet` must flush per-packet. Guards against
    /// accidental reintroduction of `BufWriter`, which caused 0-byte pcaps
    /// when the capture process was killed by SIGTERM/SIGKILL.
    #[test]
    fn pcap_writer_flushes_each_packet() {
        let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = SharedSink(Rc::clone(&buf));
        let mut w = PcapWriter::new(sink, 256).unwrap();

        // Constructor wrote the 24-byte global header.
        assert_eq!(buf.borrow().len(), 24, "global header should be present");

        let ts = netring::Timestamp {
            sec: 7,
            nsec: 1_000,
        };
        let payload = [0xab, 0xcd, 0xef];
        w.write_packet(ts, &payload, payload.len() as u32).unwrap();

        // Without dropping or flushing `w`, the packet bytes must already be
        // visible. 24 (header) + 16 (record header) + 3 (payload).
        assert_eq!(buf.borrow().len(), 24 + 16 + 3);

        let captured = buf.borrow();
        let magic = u32::from_le_bytes(captured[..4].try_into().unwrap());
        assert_eq!(magic, PCAP_MAGIC_NS);
        // Packet's `incl_len` at offset 24 + 8 = 32.
        let incl_len = u32::from_le_bytes(captured[32..36].try_into().unwrap());
        assert_eq!(incl_len, 3);
    }

    /// Drive `RotatingPcapWriter` through enough bytes to trigger
    /// rotation and verify (a) the active segment + N rotated
    /// segments exist, (b) no segments past `keep` are kept, and
    /// (c) each rotated segment opens with the pcap global header.
    #[test]
    fn rotating_writer_rotates_at_size_and_keeps_n() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("cap.pcap");

        // Each packet writes 16 (record header) + 100 (payload, capped
        // by snap_len=128) = 116 bytes. With max_size=400 and a 24-
        // byte global header, the first 3 packets fit (24 + 3*116 =
        // 372), the 4th forces rotation.
        let snap_len = 128;
        let mut w =
            RotatingPcapWriter::new(base.clone(), Some(400), None, /*keep=*/ 2, snap_len).unwrap();

        let ts = netring::Timestamp { sec: 0, nsec: 0 };
        let payload = vec![0xAB; 100];
        // Write 8 packets — should produce active + .1 + .2 (with .3
        // dropped due to keep=2).
        for _ in 0..8 {
            w.write_packet(ts, &payload, payload.len() as u32).unwrap();
        }
        drop(w);

        assert!(base.exists(), "active segment must remain");
        assert!(
            dir.path().join("cap.pcap.1").exists(),
            ".pcap.1 must exist (rotated once)"
        );
        assert!(
            dir.path().join("cap.pcap.2").exists(),
            ".pcap.2 must exist (rotated twice)"
        );
        assert!(
            !dir.path().join("cap.pcap.3").exists(),
            ".pcap.3 must NOT exist with keep=2"
        );

        // Each segment must start with the pcap global header magic —
        // proves rotation re-emits the header rather than letting the
        // new file start mid-packet.
        for name in ["cap.pcap", "cap.pcap.1", "cap.pcap.2"] {
            let bytes = std::fs::read(dir.path().join(name)).unwrap();
            assert!(bytes.len() >= 24, "{name}: pcap header missing");
            let magic = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            assert_eq!(magic, PCAP_MAGIC_NS, "{name}: bad pcap magic");
        }
    }

    /// `keep = 0` means "no rotated segments retained" — when the
    /// active segment is rotated out it just gets deleted.
    #[test]
    fn rotating_writer_keep_zero_drops_old_segments() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("cap.pcap");
        let snap_len = 128;
        let mut w =
            RotatingPcapWriter::new(base.clone(), Some(200), None, /*keep=*/ 0, snap_len).unwrap();

        let ts = netring::Timestamp { sec: 0, nsec: 0 };
        let payload = vec![0xAB; 100];
        for _ in 0..5 {
            w.write_packet(ts, &payload, payload.len() as u32).unwrap();
        }
        drop(w);

        assert!(base.exists(), "active segment must remain");
        assert!(!dir.path().join("cap.pcap.1").exists());
        assert!(!dir.path().join("cap.pcap.2").exists());
    }

    /// No `--max-size` and no `--rotate` → no rotation ever, even on
    /// arbitrary write volume. Sanity check that the rotation logic
    /// is opt-in.
    #[test]
    fn rotating_writer_no_policy_never_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("cap.pcap");
        let mut w = RotatingPcapWriter::new(base.clone(), None, None, 5, 128).unwrap();

        let ts = netring::Timestamp { sec: 0, nsec: 0 };
        let payload = vec![0xAB; 100];
        for _ in 0..50 {
            w.write_packet(ts, &payload, payload.len() as u32).unwrap();
        }
        drop(w);

        assert!(base.exists());
        assert!(!dir.path().join("cap.pcap.1").exists());
    }

    /// Smoke-test that `CaptureConfig.bpf_filter` accepts a typed
    /// `netring::BpfFilter` built via the chain DSL — i.e. the
    /// `Plan 156` migration off the `tcpdump -dd` shell-out is
    /// wired up end-to-end on the type level. The bytecode itself
    /// is exhaustively covered by netring's own tests; we just
    /// guard the integration boundary.
    #[test]
    fn capture_config_accepts_typed_bpf_filter() {
        let filter = netring::BpfFilter::builder()
            .ipv4()
            .tcp()
            .dst_port(80)
            .build()
            .expect("builder produces a valid filter for tcp dst port 80");
        assert!(!filter.is_empty(), "compiled filter must be non-empty");

        let cfg = CaptureConfig {
            interface: "eth0".into(),
            snap_len: 65536,
            count: None,
            duration: None,
            bpf_filter: Some(filter),
            profile: RingProfile::Default,
            ignore_outgoing: false,
        };
        assert!(cfg.bpf_filter.is_some());
    }

    /// netring 0.16 adoption — `BpfFilter::builder().ports([...])`
    /// multi-port OR shortcut. nlink-lab exposes this via
    /// `nlink-lab capture --filter-ports 80,443,8080` (versus
    /// the single `--filter-port`). Smoke-test that the typed
    /// builder accepts the multi-port form.
    #[test]
    fn capture_config_accepts_multi_port_bpf_filter() {
        let filter = netring::BpfFilter::builder()
            .ipv4()
            .tcp()
            .ports([80u16, 443, 8080])
            .build()
            .expect("builder produces a valid filter for tcp ports 80/443/8080");
        assert!(!filter.is_empty(), "compiled filter must be non-empty");

        let cfg = CaptureConfig {
            interface: "eth0".into(),
            snap_len: 65536,
            count: None,
            duration: None,
            bpf_filter: Some(filter),
            profile: RingProfile::Default,
            ignore_outgoing: false,
        };
        assert!(cfg.bpf_filter.is_some());
    }

    /// netring 0.16 adoption — combined v4+v6 ICMP filter via
    /// the chain DSL. nlink-lab's `--filter-icmp` flag now
    /// covers both IP versions through the existing `.icmp()`
    /// shortcut (which 0.16 made smarter about v6 handling).
    #[test]
    fn capture_config_accepts_icmp_chain_filter() {
        let filter = netring::BpfFilter::builder()
            .icmp()
            .build()
            .expect("builder produces a valid filter for any ICMP");
        assert!(!filter.is_empty(), "compiled filter must be non-empty");

        let cfg = CaptureConfig {
            interface: "eth0".into(),
            snap_len: 65536,
            count: None,
            duration: None,
            bpf_filter: Some(filter),
            profile: RingProfile::Default,
            ignore_outgoing: false,
        };
        assert!(cfg.bpf_filter.is_some());
    }

    // ── Capture loop (issue #33) — rootless, via fake sources ─────────

    /// A source that never yields anything: simulates an idle
    /// interface by sleeping for the full poll timeout, like
    /// `poll(2)` timing out on the ring fd.
    fn idle_source(timeout: Duration, _sink: &mut PacketSink<'_>) -> Result<()> {
        std::thread::sleep(timeout);
        Ok(())
    }

    /// A source that yields `per_poll` packets on every call without
    /// blocking — a flood.
    fn flood_source(per_poll: usize) -> impl FnMut(Duration, &mut PacketSink<'_>) -> Result<()> {
        move |_timeout, sink| {
            let payload = [0u8; 64];
            for _ in 0..per_poll {
                let rec = PacketRecord {
                    ts: netring::Timestamp { sec: 1, nsec: 2 },
                    data: &payload,
                    orig_len: 64,
                };
                if !sink(rec)? {
                    break;
                }
            }
            Ok(())
        }
    }

    /// Generous upper bound on how long a stop should take: one poll
    /// quantum plus scheduling slack. Loose enough for slow CI, tight
    /// enough to catch the pre-fix "never returns" behaviour.
    const STOP_BUDGET: Duration = Duration::from_millis(1500);

    /// The pre-fix loop only looked at `shutdown` after a packet
    /// arrived, so `LabCapture::stop` deadlocked on an idle link. The
    /// flag must now be honoured within about one poll quantum.
    #[test]
    fn loop_honours_shutdown_on_idle_source() {
        let flag = AtomicBool::new(false);
        let started = Instant::now();
        let (count, reason) = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                flag.store(true, Ordering::Relaxed);
            });
            drive_capture_loop(CaptureLimits::default(), &flag, idle_source, |_| Ok(()))
        })
        .unwrap();
        assert_eq!(count, 0);
        assert_eq!(reason, StopReason::Shutdown);
        assert!(
            started.elapsed() < STOP_BUDGET,
            "shutdown took {:?}, expected < {:?}",
            started.elapsed(),
            STOP_BUDGET
        );
    }

    /// `--duration` on an idle interface must end the capture; the
    /// last poll is clipped to the remaining time so we don't
    /// overshoot by a whole quantum.
    #[test]
    fn loop_honours_duration_on_idle_source() {
        let flag = AtomicBool::new(false);
        let limits = CaptureLimits {
            count: None,
            duration: Some(Duration::from_millis(120)),
        };
        let started = Instant::now();
        let (count, reason) = drive_capture_loop(limits, &flag, idle_source, |_| Ok(())).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(count, 0);
        assert_eq!(reason, StopReason::Deadline);
        assert!(
            elapsed >= Duration::from_millis(120),
            "ended early: {elapsed:?}"
        );
        assert!(elapsed < STOP_BUDGET, "deadline overshoot: {elapsed:?}");
    }

    /// Every poll must be bounded by `POLL_QUANTUM` and by the time
    /// left until the deadline — otherwise a stop request could be
    /// delayed by an arbitrarily long `poll(2)`.
    #[test]
    fn loop_bounds_every_poll_timeout() {
        let flag = AtomicBool::new(false);
        let limits = CaptureLimits {
            count: None,
            duration: Some(Duration::from_millis(450)),
        };
        let mut timeouts: Vec<Duration> = Vec::new();
        let source = |timeout: Duration, _sink: &mut PacketSink<'_>| -> Result<()> {
            timeouts.push(timeout);
            std::thread::sleep(timeout);
            Ok(())
        };
        let (_, reason) = drive_capture_loop(limits, &flag, source, |_| Ok(())).unwrap();
        assert_eq!(reason, StopReason::Deadline);
        assert!(
            timeouts.len() >= 3,
            "expected several polls, got {timeouts:?}"
        );
        assert!(timeouts.iter().all(|t| *t <= POLL_QUANTUM), "{timeouts:?}");
        assert!(timeouts.iter().all(|t| !t.is_zero()), "{timeouts:?}");
        // The final poll was clipped to the remainder, not a full quantum.
        assert!(
            *timeouts.last().unwrap() < POLL_QUANTUM,
            "last poll not clipped to the deadline: {timeouts:?}"
        );
    }

    /// `--count N` stops mid-batch — the sink must not be fed the
    /// rest of the batch once the limit is hit.
    #[test]
    fn loop_stops_at_count_mid_batch() {
        let flag = AtomicBool::new(false);
        let limits = CaptureLimits {
            count: Some(5),
            duration: None,
        };
        let mut seen = 0u64;
        let (count, reason) = drive_capture_loop(limits, &flag, flood_source(100), |_| {
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((count, seen), (5, 5));
        assert_eq!(reason, StopReason::CountReached);
    }

    /// `--count 0` means "capture nothing" and must not block waiting
    /// for a first packet on an idle interface.
    #[test]
    fn loop_count_zero_returns_immediately() {
        let flag = AtomicBool::new(false);
        let limits = CaptureLimits {
            count: Some(0),
            duration: None,
        };
        let started = Instant::now();
        let (count, reason) = drive_capture_loop(limits, &flag, idle_source, |_| Ok(())).unwrap();
        assert_eq!((count, reason), (0, StopReason::CountReached));
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    /// A stop requested during a flood is honoured between packets of
    /// the same batch, not only at the next poll boundary.
    #[test]
    fn loop_honours_shutdown_mid_batch() {
        let flag = AtomicBool::new(false);
        let mut seen = 0u64;
        let (count, reason) =
            drive_capture_loop(CaptureLimits::default(), &flag, flood_source(1_000), |_| {
                seen += 1;
                if seen == 3 {
                    flag.store(true, Ordering::Relaxed);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!((count, seen), (3, 3));
        assert_eq!(reason, StopReason::Shutdown);
    }

    /// Ring I/O errors surface as `Error::Capture`, never as a short
    /// but "successful" capture.
    #[test]
    fn loop_propagates_source_error() {
        let flag = AtomicBool::new(false);
        let source = |_t: Duration, _s: &mut PacketSink<'_>| -> Result<()> {
            Err(Error::Capture("netring: boom".into()))
        };
        let err =
            drive_capture_loop(CaptureLimits::default(), &flag, source, |_| Ok(())).unwrap_err();
        assert!(
            matches!(err, Error::Capture(ref m) if m.contains("boom")),
            "{err}"
        );
    }

    /// A failing sink (e.g. pcap write to a full disk) aborts the
    /// loop with that error instead of silently dropping packets.
    #[test]
    fn loop_propagates_sink_error() {
        let flag = AtomicBool::new(false);
        let err = drive_capture_loop(CaptureLimits::default(), &flag, flood_source(10), |_| {
            Err(Error::Capture("disk full".into()))
        })
        .unwrap_err();
        assert!(
            matches!(err, Error::Capture(ref m) if m == "disk full"),
            "{err}"
        );
    }

    /// `run_capture` must not enter the namespace on the caller's
    /// thread. We can't create a namespace rootless, but we can prove
    /// the work happens elsewhere: a bogus namespace fails inside the
    /// worker and the error is relayed — and the calling thread's name
    /// is untouched, i.e. we were never the `capture-*` thread.
    #[test]
    fn run_capture_runs_on_dedicated_thread_and_relays_errors() {
        let cfg = CaptureConfig {
            interface: "lo".into(),
            snap_len: 256,
            count: Some(1),
            duration: Some(Duration::from_millis(10)),
            bpf_filter: None,
            profile: RingProfile::LowMemory,
            ignore_outgoing: false,
        };
        let flag = AtomicBool::new(false);
        let err = run_capture(
            "nlink-lab-issue33-does-not-exist",
            &cfg,
            CaptureOutput::Summaries,
            &flag,
        )
        .expect_err("entering a nonexistent namespace must fail");
        // Whatever the failure (ENOENT on the ns, EPERM rootless), it
        // must come back as an error, not a panic or a hang.
        let _ = err.to_string();
        assert_ne!(
            std::thread::current().name(),
            Some("capture-nlink-lab-issue33-does-not-exist"),
        );
    }

    /// Same contract for the non-blocking entry point: `stop()` on a
    /// capture that failed to start returns the error promptly and
    /// never hangs.
    #[test]
    fn spawn_capture_stop_returns_promptly() {
        let cfg = CaptureConfig {
            interface: "lo".into(),
            snap_len: 256,
            count: None,
            duration: None,
            bpf_filter: None,
            profile: RingProfile::LowMemory,
            ignore_outgoing: false,
        };
        let flag = Arc::new(AtomicBool::new(false));
        let handle = spawn_capture(
            "nlink-lab-issue33-does-not-exist".into(),
            cfg,
            CaptureOutput::Summaries,
            Arc::clone(&flag),
        )
        .unwrap();
        let started = Instant::now();
        let res = handle.stop();
        assert!(res.is_err(), "nonexistent namespace must not capture");
        assert!(flag.load(Ordering::Relaxed), "stop() must raise the flag");
        assert!(started.elapsed() < STOP_BUDGET);
    }
}
