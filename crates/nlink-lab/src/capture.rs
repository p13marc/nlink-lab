//! Packet capture using netring with pcap file output.
//!
//! Enters a lab node's network namespace, creates an AF_PACKET capture via
//! netring, and either writes packets as pcap or prints one-line summaries.

use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use netring::{BpfInsn, Capture, CaptureStats, RingProfile};
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
struct PcapWriter<W: Write> {
    writer: BufWriter<W>,
    snap_len: u32,
}

impl<W: Write> PcapWriter<W> {
    /// Create a new pcap writer, immediately writing the 24-byte global header.
    fn new(writer: W, snap_len: u32) -> io::Result<Self> {
        let mut w = BufWriter::new(writer);
        // Global header: magic, version, thiszone, sigfigs, snaplen, network
        w.write_all(&PCAP_MAGIC_NS.to_le_bytes())?;
        w.write_all(&PCAP_VERSION_MAJOR.to_le_bytes())?;
        w.write_all(&PCAP_VERSION_MINOR.to_le_bytes())?;
        w.write_all(&0i32.to_le_bytes())?; // thiszone
        w.write_all(&0u32.to_le_bytes())?; // sigfigs
        w.write_all(&snap_len.to_le_bytes())?;
        w.write_all(&LINKTYPE_ETHERNET.to_le_bytes())?;
        Ok(Self { writer: w, snap_len })
    }

    /// Write a single packet record (16-byte header + data).
    fn write_packet(&mut self, ts: netring::Timestamp, data: &[u8], orig_len: u32) -> io::Result<()> {
        let incl_len = (data.len() as u32).min(self.snap_len);
        self.writer.write_all(&ts.sec.to_le_bytes())?;
        self.writer.write_all(&ts.nsec.to_le_bytes())?;
        self.writer.write_all(&incl_len.to_le_bytes())?;
        self.writer.write_all(&orig_len.to_le_bytes())?;
        self.writer.write_all(&data[..incl_len as usize])?;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

// ── BPF filter compilation ────────────────────────────────────────────────

/// Compile a tcpdump filter expression into BPF instructions.
///
/// Shells out to `tcpdump -dd` which outputs C-style BPF bytecode.
/// Requires tcpdump to be installed on the system.
pub fn compile_bpf_filter(expression: &str) -> Result<Vec<BpfInsn>> {
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
        let trimmed = line.trim().trim_start_matches('{').trim_end_matches([',', '}', ' ']);
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

    Ok(insns)
}

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
    /// Compiled BPF filter instructions.
    pub bpf_filter: Option<Vec<BpfInsn>>,
    /// Ring buffer profile.
    pub profile: RingProfile,
}

/// Result of a completed capture session.
pub struct CaptureResult {
    /// Number of packets captured.
    pub packets_captured: u64,
    /// Kernel-reported statistics (packets seen, drops, freezes).
    pub stats: CaptureStats,
}

// ── Main capture loop ─────────────────────────────────────────────────────

/// Run a packet capture in the given namespace.
///
/// Enters the namespace on a dedicated thread (to avoid affecting the tokio
/// runtime), creates the AF_PACKET socket there, then runs the capture loop.
/// If `pcap_output` is `Some`, writes pcap format; otherwise prints summaries.
pub fn run_capture<W: Write + Send + 'static>(
    ns_name: &str,
    config: &CaptureConfig,
    pcap_output: Option<W>,
    shutdown: &AtomicBool,
) -> Result<CaptureResult> {
    // Enter namespace and create capture socket.
    // We do this on the current thread since capture is a blocking operation
    // and the CLI doesn't have async context running.
    let guard = namespace::enter(ns_name)?;

    let mut builder = Capture::builder()
        .interface(&config.interface)
        .profile(config.profile)
        .snap_len(config.snap_len);

    if let Some(ref insns) = config.bpf_filter {
        builder = builder.bpf_filter(insns.clone());
    }

    let mut capture = builder
        .build()
        .map_err(|e| Error::Capture(format!("netring: {e}")))?;

    // Restore namespace — the socket fd remains bound to the target namespace.
    drop(guard);

    // Set up output
    let mut pcap = match pcap_output {
        Some(w) => Some(PcapWriter::new(w, config.snap_len)?),
        None => None,
    };

    let start = Instant::now();
    let mut count: u64 = 0;

    for pkt in capture.packets() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        if let Some(max_duration) = config.duration {
            if start.elapsed() >= max_duration {
                break;
            }
        }

        let ts = pkt.timestamp();
        let data = pkt.data();
        let orig_len = pkt.original_len() as u32;

        if let Some(ref mut w) = pcap {
            w.write_packet(ts, data, orig_len)?;
        } else {
            println!(
                "{}.{:09}  {} bytes",
                ts.sec, ts.nsec,
                data.len(),
            );
        }

        count += 1;

        if let Some(max_count) = config.count {
            if count >= max_count {
                break;
            }
        }
    }

    if let Some(ref mut w) = pcap {
        w.flush()?;
    }

    let stats = capture.stats().unwrap_or_default();

    Ok(CaptureResult {
        packets_captured: count,
        stats,
    })
}
