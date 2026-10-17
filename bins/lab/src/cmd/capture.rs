//! `nlink-lab capture`.

use std::path::PathBuf;

use crate::ctx::{Ctx, require_root};
use crate::util::{compile_legacy_bpf_filter, parse_filter_cidr};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Endpoint (e.g., "router:eth0").
    #[arg(value_parser = crate::args::endpoint, add = crate::ctx::endpoint_completer())]
    pub endpoint: String,

    /// Write a pcap file (default: print summaries to stdout).
    #[arg(short = 'o', long = "output", alias = "write", value_name = "FILE")]
    pub write: Option<PathBuf>,

    /// Capture N packets then stop.
    #[arg(short, long)]
    pub count: Option<u64>,

    /// Legacy: full tcpdump filter expression (e.g., "tcp port
    /// 80"). Requires nlink-lab built with the
    /// `legacy-tcpdump-filter` feature *and* `tcpdump` on PATH.
    /// Default builds prefer the typed `--filter-*` flags below.
    #[arg(long)]
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

    /// Stop after this long (`30s`).
    #[arg(long, value_name = "DURATION", value_parser = crate::args::duration)]
    pub duration: Option<std::time::Duration>,

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
    /// previous `.1` becomes `.2`, etc. Requires `--output`.
    /// Round-5 §2.3.
    #[arg(long, value_name = "SIZE", value_parser = crate::args::byte_size)]
    pub max_size: Option<u64>,

    /// Rotate the pcap file this long (`10m`) after the last
    /// rotation. Composes with `--max-size` (whichever threshold
    /// fires first triggers the rotation). Requires `--output`.
    #[arg(long, value_name = "DURATION", value_parser = crate::args::interval)]
    pub rotate: Option<std::time::Duration>,

    /// Number of *rotated* segments to keep (`<base>.pcap.1`
    /// through `<base>.pcap.<keep>`). The active `<base>.pcap`
    /// is always retained and doesn't count. Default: 5.
    #[arg(long, default_value = "5", value_name = "N")]
    pub keep: usize,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let bpf = build_filter(&args)?;
    let Args {
        lab,
        endpoint,
        write,
        count,
        duration,
        snap_len,
        dedupe_loopback,
        max_size,
        rotate,
        keep,
        ..
    } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
        nlink_lab::Error::InvalidEndpoint {
            endpoint: endpoint.clone(),
        }
    })?;

    let ns_name = running.namespace_for(&ep.node)?.to_string();

    let config = nlink_lab::capture::CaptureConfig {
        interface: ep.iface.clone(),
        snap_len,
        count,
        duration,
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
    // --output (rotation only makes sense for file output).
    if (max_size.is_some() || rotate.is_some()) && write.is_none() {
        return Err(nlink_lab::Error::invalid_argument(
            "--max-size and --rotate require --output",
        ));
    }
    let output = match write {
        Some(path) if max_size.is_some() || rotate.is_some() => {
            nlink_lab::capture::CaptureOutput::RotatingPcap {
                base: path,
                max_size,
                rotate_after: rotate,
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

/// The capture's BPF filter: the legacy `--filter` expression, or the
/// typed `--filter-*` flags built with netring's builder. `None` without
/// either.
fn build_filter(args: &Args) -> nlink_lab::Result<Option<netring::BpfFilter>> {
    let &Args {
        ref filter,
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
        ref filter_net,
        ref filter_src_net,
        ref filter_dst_net,
        filter_port,
        filter_src_port,
        filter_dst_port,
        ref filter_ports,
        ref filter_src_ports,
        ref filter_dst_ports,
        filter_not,
        ..
    } = args;
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
        return Err(nlink_lab::Error::invalid_argument(
            "cannot combine --filter \"<expr>\" with typed --filter-* flags",
        ));
    }

    Ok(if let Some(expr) = filter {
        Some(compile_legacy_bpf_filter(expr)?)
    } else if any_typed_filter {
        let net = filter_net
            .as_deref()
            .map(|s| parse_filter_cidr("--filter-net", s))
            .transpose()?;
        let src_net = filter_src_net
            .as_deref()
            .map(|s| parse_filter_cidr("--filter-src-net", s))
            .transpose()?;
        let dst_net = filter_dst_net
            .as_deref()
            .map(|s| parse_filter_cidr("--filter-dst-net", s))
            .transpose()?;
        // An IPv6 address needs the IPv6 ethertype check: without a family
        // flag the builder checks for IPv4, then reads IPv6 address
        // offsets, and the filter matched nothing (#209).
        let v6 = [filter_host, filter_src_host, filter_dst_host]
            .iter()
            .flatten()
            .any(|a| a.is_ipv6())
            || [net, src_net, dst_net]
                .iter()
                .flatten()
                .any(|n| n.is_ipv6());
        // Everything but the port lists, for each branch below.
        let base = |mut b: netring::BpfFilterBuilder| {
            if filter_ipv4 {
                b = b.ipv4();
            }
            if filter_ipv6 || (v6 && !filter_ipv4 && !filter_arp) {
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
            if let Some(n) = net {
                b = b.net(n);
            }
            if let Some(n) = src_net {
                b = b.src_net(n);
            }
            if let Some(n) = dst_net {
                b = b.dst_net(n);
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
            b
        };
        // One OR branch per combination of the port lists, each with the
        // whole chain. netring 0.30's `ports()` copies only the fragments
        // before it into its extra branches, so `--filter-src-ports 1,2
        // --filter-dst-ports 3,4` also matched source port 2 to any port
        // (#209; fixed in netring after 0.31.1).
        let each = |ports: &[u16]| -> Vec<Option<u16>> {
            if ports.is_empty() {
                vec![None]
            } else {
                ports.iter().copied().map(Some).collect()
            }
        };
        let mut combos = Vec::new();
        for p in each(filter_ports) {
            for sp in each(filter_src_ports) {
                for dp in each(filter_dst_ports) {
                    combos.push((p, sp, dp));
                }
            }
        }
        let with_ports =
            |mut b: netring::BpfFilterBuilder,
             (p, sp, dp): (Option<u16>, Option<u16>, Option<u16>)| {
                if let Some(p) = p {
                    b = b.port(p);
                }
                if let Some(p) = sp {
                    b = b.src_port(p);
                }
                if let Some(p) = dp {
                    b = b.dst_port(p);
                }
                b
            };
        let mut b = with_ports(base(netring::BpfFilter::builder()), combos[0]);
        for &combo in &combos[1..] {
            b = b.or(|branch| with_ports(base(branch), combo));
        }
        if filter_not {
            b = b.negate();
        }
        Some(b.build().map_err(|e| {
            nlink_lab::Error::invalid_argument(format!("BPF filter build failed: {e}"))
        })?)
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn filter(flags: &[&str]) -> netring::BpfFilter {
        let argv = ["capture", "a:eth0"].iter().chain(flags);
        build_filter(&Cli::parse_from(argv).args).unwrap().unwrap()
    }

    /// Ethernet + IPv4 (or IPv6) + the TCP ports.
    fn tcp_frame(src: std::net::IpAddr, sport: u16, dport: u16) -> Vec<u8> {
        let mut f = vec![0u8; 12];
        match src {
            std::net::IpAddr::V4(a) => {
                f.extend_from_slice(&[0x08, 0x00, 0x45, 0, 0, 40, 0, 0, 0, 0, 64, 6, 0, 0]);
                f.extend_from_slice(&a.octets());
                f.extend_from_slice(&[10, 0, 0, 2]);
            }
            std::net::IpAddr::V6(a) => {
                f.extend_from_slice(&[0x86, 0xdd, 0x60, 0, 0, 0, 0, 20, 6, 64]);
                f.extend_from_slice(&a.octets());
                f.extend_from_slice(&"fd00::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
            }
        }
        f.extend_from_slice(&sport.to_be_bytes());
        f.extend_from_slice(&dport.to_be_bytes());
        f.extend_from_slice(&[0u8; 16]);
        f
    }

    /// Two port lists are both required, for every pair. netring 0.30
    /// left the second list off the first list's extra branches: source
    /// port 2 matched with any destination (#209).
    #[test]
    fn port_lists_combine_as_a_product() {
        let f = filter(&[
            "--filter-tcp",
            "--filter-src-ports",
            "1,2",
            "--filter-dst-ports",
            "3,4",
        ]);
        let v4: std::net::IpAddr = "10.0.0.1".parse().unwrap();
        for (s, d) in [(1, 3), (1, 4), (2, 3), (2, 4)] {
            assert!(f.matches(&tcp_frame(v4, s, d)), "{s} -> {d}");
        }
        for (s, d) in [(2, 99), (1, 99), (99, 3)] {
            assert!(!f.matches(&tcp_frame(v4, s, d)), "{s} -> {d}");
        }
    }

    /// An IPv6 host matches IPv6 traffic without `--filter-ipv6` (#209).
    #[test]
    fn an_ipv6_host_filter_matches_ipv6() {
        let f = filter(&["--filter-host", "2001:db8::1"]);
        assert!(f.matches(&tcp_frame("2001:db8::1".parse().unwrap(), 5, 6)));
        assert!(!f.matches(&tcp_frame("2001:db8::9".parse().unwrap(), 5, 6)));
        assert!(!f.matches(&tcp_frame("10.0.0.1".parse().unwrap(), 5, 6)));
    }
}
