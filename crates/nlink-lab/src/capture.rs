//! Packet capture using netring with pcap file output.
//!
//! Enters a lab node's network namespace, creates an AF_PACKET capture via
//! netring, and either writes packets as pcap or prints one-line summaries.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use netring::{BpfFilter, Capture, CaptureStats, RingProfile};
#[cfg(feature = "legacy-tcpdump-filter")]
use netring::BpfInsn;
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
pub struct CaptureResult {
    /// Number of packets captured.
    pub packets_captured: u64,
    /// Kernel-reported statistics (packets seen, drops, freezes).
    pub stats: CaptureStats,
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

/// Run a packet capture in the given namespace.
///
/// Enters the namespace on a dedicated thread (to avoid affecting the tokio
/// runtime), creates the AF_PACKET socket there, then runs the capture loop.
/// `output` selects pcap-vs-summary and single-vs-rotating.
pub fn run_capture(
    ns_name: &str,
    config: &CaptureConfig,
    output: CaptureOutput,
    shutdown: &AtomicBool,
) -> Result<CaptureResult> {
    // Enter namespace and create capture socket.
    // We do this on the current thread since capture is a blocking operation
    // and the CLI doesn't have async context running.
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

    let start = Instant::now();
    let mut count: u64 = 0;

    for pkt in capture.packets() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        if let Some(max_duration) = config.duration
            && start.elapsed() >= max_duration
        {
            break;
        }

        let ts = pkt.timestamp();
        let data = pkt.data();
        let orig_len = pkt.original_len() as u32;

        match &mut pcap {
            PcapSink::None => {
                println!("{}.{:09}  {} bytes", ts.sec, ts.nsec, data.len(),);
            }
            _ => {
                pcap.write_packet(ts, data, orig_len)?;
            }
        }

        count += 1;

        if let Some(max_count) = config.count
            && count >= max_count
        {
            break;
        }
    }

    // No trailing flush needed — `PcapWriter::write_packet` already flushes
    // per-packet, so a SIGKILL between the loop body and this point still
    // leaves a complete pcap.
    drop(pcap);

    let stats = capture.stats().unwrap_or_default();

    Ok(CaptureResult {
        packets_captured: count,
        stats,
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
}
