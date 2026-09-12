//! `nlink-lab capture`.

use std::path::PathBuf;

use crate::ctx::{Ctx, require_root};
use crate::util::{compile_legacy_bpf_filter, parse_byte_size, parse_filter_cidr};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Endpoint (e.g., "router:eth0").
    pub endpoint: String,

    /// Write to pcap file (default: print summaries to stdout).
    #[arg(short, long)]
    pub write: Option<PathBuf>,

    /// Capture N packets then stop.
    #[arg(short, long)]
    pub count: Option<u64>,

    /// Legacy: full tcpdump filter expression (e.g., "tcp port
    /// 80"). Requires nlink-lab built with the
    /// `legacy-tcpdump-filter` feature *and* `tcpdump` on PATH.
    /// Default builds prefer the typed `--filter-*` flags below.
    #[arg(short, long)]
    pub filter: Option<String>,

    /// Match only TCP (sets ip_proto=6).
    #[arg(long = "filter-tcp")]
    pub filter_tcp: bool,

    /// Match only UDP (sets ip_proto=17).
    #[arg(long = "filter-udp")]
    pub filter_udp: bool,

    /// Match only ICMP (sets ip_proto=1).
    #[arg(long = "filter-icmp")]
    pub filter_icmp: bool,

    /// Match a specific IP protocol number (e.g. 47 for GRE).
    #[arg(long = "filter-ip-proto", value_name = "PROTO")]
    pub filter_ip_proto: Option<u8>,

    /// Restrict to IPv4 traffic.
    #[arg(long = "filter-ipv4")]
    pub filter_ipv4: bool,

    /// Restrict to IPv6 traffic.
    #[arg(long = "filter-ipv6")]
    pub filter_ipv6: bool,

    /// Match ARP frames (ethertype 0x0806).
    #[arg(long = "filter-arp")]
    pub filter_arp: bool,

    /// Match 802.1Q VLAN-tagged frames.
    #[arg(long = "filter-vlan")]
    pub filter_vlan: bool,

    /// Match a specific VLAN ID. Implies `--filter-vlan`.
    #[arg(long = "filter-vlan-id", value_name = "VID")]
    pub filter_vlan_id: Option<u16>,

    /// Match either source or destination IP address.
    #[arg(long = "filter-host", value_name = "ADDR")]
    pub filter_host: Option<std::net::IpAddr>,

    /// Match a specific source IP address.
    #[arg(long = "filter-src-host", value_name = "ADDR")]
    pub filter_src_host: Option<std::net::IpAddr>,

    /// Match a specific destination IP address.
    #[arg(long = "filter-dst-host", value_name = "ADDR")]
    pub filter_dst_host: Option<std::net::IpAddr>,

    /// Match either source or destination network (CIDR).
    #[arg(long = "filter-net", value_name = "CIDR")]
    pub filter_net: Option<String>,

    /// Match a source network (CIDR).
    #[arg(long = "filter-src-net", value_name = "CIDR")]
    pub filter_src_net: Option<String>,

    /// Match a destination network (CIDR).
    #[arg(long = "filter-dst-net", value_name = "CIDR")]
    pub filter_dst_net: Option<String>,

    /// Match either source or destination L4 port. Requires
    /// `--filter-tcp` or `--filter-udp`.
    #[arg(long = "filter-port", value_name = "PORT")]
    pub filter_port: Option<u16>,

    /// Match L4 source port.
    #[arg(long = "filter-src-port", value_name = "PORT")]
    pub filter_src_port: Option<u16>,

    /// Match L4 destination port.
    #[arg(long = "filter-dst-port", value_name = "PORT")]
    pub filter_dst_port: Option<u16>,

    /// Match any of these L4 ports (either source OR
    /// destination). Comma-separated, e.g. `80,443,8080`.
    /// Backed by netring 0.16's `BpfFilter::builder::ports()`
    /// multi-port shortcut — compiles to one BPF program
    /// branch per port. Requires `--filter-tcp` or
    /// `--filter-udp`.
    #[arg(long = "filter-ports", value_name = "PORTS", value_delimiter = ',')]
    pub filter_ports: Vec<u16>,

    /// Match any of these L4 source ports.
    #[arg(long = "filter-src-ports", value_name = "PORTS", value_delimiter = ',')]
    pub filter_src_ports: Vec<u16>,

    /// Match any of these L4 destination ports.
    #[arg(long = "filter-dst-ports", value_name = "PORTS", value_delimiter = ',')]
    pub filter_dst_ports: Vec<u16>,

    /// Negate the entire filter (capture everything that does
    /// NOT match the other `--filter-*` flags).
    #[arg(long = "filter-not")]
    pub filter_not: bool,

    /// Stop after N seconds.
    #[arg(long)]
    pub duration: Option<f64>,

    /// Snap length -- truncate packets to N bytes.
    #[arg(long, default_value = "262144")]
    pub snap_len: u32,

    /// Drop outgoing packets at the kernel via
    /// `PACKET_IGNORE_OUTGOING`. Use this when capturing on `lo`
    /// to halve the packet count: loopback otherwise reports each
    /// packet twice (once outgoing, once incoming). No effect on
    /// non-loopback interfaces. (Requires kernel >= 4.20.)
    #[arg(long)]
    pub dedupe_loopback: bool,

    /// Rotate the pcap file when the active segment exceeds this
    /// size (suffixes K/M/G accepted; e.g. `100M`). On rotation
    /// the active `<base>.pcap` becomes `<base>.pcap.1`, the
    /// previous `.1` becomes `.2`, etc. Requires `--write`.
    /// Round-5 §2.3.
    #[arg(long, value_name = "SIZE", value_parser = parse_byte_size)]
    pub max_size: Option<u64>,

    /// Rotate the pcap file every SECS seconds since the last
    /// rotation. Composes with `--max-size` (whichever threshold
    /// fires first triggers the rotation). Requires `--write`.
    #[arg(long, value_name = "SECS")]
    pub rotate: Option<f64>,

    /// Number of *rotated* segments to keep (`<base>.pcap.1`
    /// through `<base>.pcap.<keep>`). The active `<base>.pcap`
    /// is always retained and doesn't count. Default: 5.
    #[arg(long, default_value = "5", value_name = "N")]
    pub keep: usize,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        endpoint,
        write,
        count,
        filter,
        filter_tcp,
        filter_udp,
        filter_icmp,
        filter_ip_proto,
        filter_ipv4,
        filter_ipv6,
        filter_arp,
        filter_vlan,
        filter_vlan_id,
        filter_host,
        filter_src_host,
        filter_dst_host,
        filter_net,
        filter_src_net,
        filter_dst_net,
        filter_port,
        filter_src_port,
        filter_dst_port,
        filter_ports,
        filter_src_ports,
        filter_dst_ports,
        filter_not,
        duration,
        snap_len,
        dedupe_loopback,
        max_size,
        rotate,
        keep,
    } = args;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
        nlink_lab::Error::InvalidEndpoint {
            endpoint: endpoint.clone(),
        }
    })?;

    let ns_name = running.namespace_for(&ep.node)?.to_string();

    // Reject combining the legacy `--filter` expression with
    // any typed `--filter-*` flag — the two paths describe
    // the same logical thing and mixing them produces
    // surprising AND-of-AND semantics.
    let any_typed_filter = filter_tcp
        || filter_udp
        || filter_icmp
        || filter_ip_proto.is_some()
        || filter_ipv4
        || filter_ipv6
        || filter_arp
        || filter_vlan
        || filter_vlan_id.is_some()
        || filter_host.is_some()
        || filter_src_host.is_some()
        || filter_dst_host.is_some()
        || filter_net.is_some()
        || filter_src_net.is_some()
        || filter_dst_net.is_some()
        || filter_port.is_some()
        || filter_src_port.is_some()
        || filter_dst_port.is_some()
        || !filter_ports.is_empty()
        || !filter_src_ports.is_empty()
        || !filter_dst_ports.is_empty()
        || filter_not;
    if filter.is_some() && any_typed_filter {
        return Err(nlink_lab::Error::invalid_topology(
            "cannot combine --filter \"<expr>\" with typed --filter-* flags",
        ));
    }

    let bpf = if let Some(expr) = &filter {
        Some(compile_legacy_bpf_filter(expr)?)
    } else if any_typed_filter {
        let mut b = netring::BpfFilter::builder();
        if filter_ipv4 {
            b = b.ipv4();
        }
        if filter_ipv6 {
            b = b.ipv6();
        }
        if filter_arp {
            b = b.arp();
        }
        // `--filter-vlan-id` implies `--filter-vlan` so the
        // ethertype check + offset shift get emitted before
        // the ID match.
        if filter_vlan || filter_vlan_id.is_some() {
            b = b.vlan();
        }
        if let Some(vid) = filter_vlan_id {
            b = b.vlan_id(vid);
        }
        if filter_tcp {
            b = b.tcp();
        }
        if filter_udp {
            b = b.udp();
        }
        if filter_icmp {
            b = b.icmp();
        }
        if let Some(proto) = filter_ip_proto {
            b = b.ip_proto(proto);
        }
        if let Some(addr) = filter_host {
            b = b.host(addr);
        }
        if let Some(addr) = filter_src_host {
            b = b.src_host(addr);
        }
        if let Some(addr) = filter_dst_host {
            b = b.dst_host(addr);
        }
        if let Some(s) = &filter_net {
            b = b.net(parse_filter_cidr("--filter-net", s)?);
        }
        if let Some(s) = &filter_src_net {
            b = b.src_net(parse_filter_cidr("--filter-src-net", s)?);
        }
        if let Some(s) = &filter_dst_net {
            b = b.dst_net(parse_filter_cidr("--filter-dst-net", s)?);
        }
        if let Some(p) = filter_port {
            b = b.port(p);
        }
        if let Some(p) = filter_src_port {
            b = b.src_port(p);
        }
        if let Some(p) = filter_dst_port {
            b = b.dst_port(p);
        }
        // netring 0.16 multi-port shortcuts. Passing an
        // empty Vec is harmless (the builder treats it as
        // a no-op), so we just forward unconditionally.
        if !filter_ports.is_empty() {
            b = b.ports(filter_ports.iter().copied());
        }
        if !filter_src_ports.is_empty() {
            b = b.src_ports(filter_src_ports.iter().copied());
        }
        if !filter_dst_ports.is_empty() {
            b = b.dst_ports(filter_dst_ports.iter().copied());
        }
        if filter_not {
            b = b.negate();
        }
        Some(b.build().map_err(|e| {
            nlink_lab::Error::invalid_topology(format!("BPF filter build failed: {e}"))
        })?)
    } else {
        None
    };

    let config = nlink_lab::capture::CaptureConfig {
        interface: ep.iface.clone(),
        snap_len,
        count,
        duration: duration.map(std::time::Duration::from_secs_f64),
        bpf_filter: bpf,
        profile: netring::RingProfile::LowMemory,
        ignore_outgoing: dedupe_loopback,
    };

    static CAPTURE_SHUTDOWN: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    CAPTURE_SHUTDOWN.store(false, std::sync::atomic::Ordering::Relaxed);
    // Handle both SIGINT (Ctrl-C) and SIGTERM (`kill`, `timeout(1)`)
    // so the capture loop can exit cleanly and print the summary
    // line. SIGKILL is uncatchable; per-packet pcap flushes
    // (capture.rs) protect data integrity in that case.
    unsafe {
        extern "C" fn handler(_: libc::c_int) {
            CAPTURE_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let h = handler as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }

    // Build the output sink. --max-size or --rotate need
    // --write (rotation only makes sense for file output).
    if (max_size.is_some() || rotate.is_some()) && write.is_none() {
        return Err(nlink_lab::Error::invalid_topology(
            "--max-size and --rotate require --write",
        ));
    }
    let output = match write {
        Some(path) if max_size.is_some() || rotate.is_some() => {
            nlink_lab::capture::CaptureOutput::RotatingPcap {
                base: path,
                max_size,
                rotate_after: rotate.map(std::time::Duration::from_secs_f64),
                keep,
            }
        }
        Some(path) => nlink_lab::capture::CaptureOutput::pcap(&path)?,
        None => nlink_lab::capture::CaptureOutput::Summaries,
    };
    let result = nlink_lab::capture::run_capture(&ns_name, &config, output, &CAPTURE_SHUTDOWN)?;

    if !ctx.quiet {
        eprintln!(
            "\n{} packets captured ({} received by kernel, {} dropped)",
            result.packets_captured, result.stats.packets, result.stats.drops,
        );
    }
    Ok(())
}
