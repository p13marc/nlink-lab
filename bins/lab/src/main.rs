#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
// The `run()` CLI dispatcher awaits the full deploy/apply future graph
// inline; its layout computation exceeds the default query-depth limit
// (compile-time only, no runtime effect).
#![recursion_limit = "256"]

use clap::{CommandFactory, Parser, Subcommand};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

// ─── Color helpers ───────────────────────────────────────

fn use_color() -> bool {
    std::env::var("NO_COLOR").is_err() && std::io::stderr().is_terminal()
}

fn green(s: &str) -> String {
    if use_color() {
        format!("\x1b[32m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn red(s: &str) -> String {
    if use_color() {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn yellow(s: &str) -> String {
    if use_color() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn bold(s: &str) -> String {
    if use_color() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Stream selector for `nlink-lab spawn --wait-log-stream`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum WaitLogStream {
    Stdout,
    Stderr,
    Both,
}

impl From<WaitLogStream> for nlink_lab::LogStream {
    fn from(s: WaitLogStream) -> Self {
        match s {
            WaitLogStream::Stdout => nlink_lab::LogStream::Stdout,
            WaitLogStream::Stderr => nlink_lab::LogStream::Stderr,
            WaitLogStream::Both => nlink_lab::LogStream::Both,
        }
    }
}

#[derive(Parser)]
#[command(name = "nlink-lab")]
#[command(about = "Network lab engine — create isolated network topologies using Linux namespaces")]
#[command(version)]
struct Cli {
    /// Output JSON instead of human-readable text (where supported).
    ///
    /// Supported by: `deploy`, `status`, `inspect`, `spawn`, `exec`, `ps`,
    /// `diagnose`, `render`. JSON Schemas for the high-traffic shapes
    /// (deploy/status/spawn/ps) live under `docs/json-schemas/`.
    #[arg(long, global = true)]
    json: bool,

    /// Verbose output (show deployment steps, tracing info).
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress informational output (errors still go to stderr).
    ///
    /// Recommended for scripted/automated use; the default human-readable
    /// output is intended for interactive shells.
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

/// Plan 159b — clap value-enum bridge for
/// [`nlink_lab::WatchFamily`].
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum WatchFamilyArg {
    Route,
    Nftables,
    Both,
}

impl From<WatchFamilyArg> for nlink_lab::WatchFamily {
    fn from(v: WatchFamilyArg) -> Self {
        match v {
            WatchFamilyArg::Route => nlink_lab::WatchFamily::Route,
            WatchFamilyArg::Nftables => nlink_lab::WatchFamily::Nftables,
            WatchFamilyArg::Both => nlink_lab::WatchFamily::Both,
        }
    }
}

/// `metrics --format` values.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum MetricsFormat {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy a lab from a topology file (.nll).
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "name": str, "nodes": int, "links": int, "deploy_time_ms": int }
    /// Schema: docs/json-schemas/deploy.schema.json
    ///
    /// Combined with `--unique`, the `name` field is the chosen unique
    /// lab name (original name + PID suffix). Useful for scripted
    /// teardown.
    Deploy {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Validate only, don't actually deploy.
        #[arg(long)]
        dry_run: bool,

        /// Destroy existing lab with same name before deploying.
        #[arg(long)]
        force: bool,

        /// Start the Zenoh backend daemon after deploying.
        #[arg(long)]
        daemon: bool,

        /// Skip validate block assertions after deploy.
        #[arg(long)]
        skip_validate: bool,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Append suffix to lab name (for parallel test safety).
        #[arg(long)]
        suffix: Option<String>,

        /// Fail (exit 2) when any `validate { … }` assertion fails after
        /// deploy. The lab stays deployed for inspection.
        #[arg(long)]
        strict: bool,

        /// Auto-generate unique lab name suffix (appends PID).
        #[arg(long)]
        unique: bool,
    },

    /// Apply topology changes to a running lab.
    ///
    /// Reconciles the live lab state to match an updated NLL,
    /// issuing only the deltas. Add `--check` to fail on any drift
    /// (a CI gate). Add `--json --dry-run` for machine-parseable
    /// diff output.
    Apply {
        /// Path to the updated topology file (.nll).
        topology: PathBuf,

        /// Set a `param` value (repeatable): --set wan_delay=50ms. Use the
        /// same values the lab was deployed with.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Show what would change without applying.
        #[arg(long)]
        dry_run: bool,

        /// Drift check — exit non-zero if the live lab differs from
        /// the NLL. Useful as a CI gate. Implies --dry-run.
        #[arg(long)]
        check: bool,
    },

    /// Tear down a running lab.
    Destroy {
        /// Lab name (omit with --all or --orphans).
        name: Option<String>,

        /// Continue cleanup even if some resources are already gone.
        #[arg(long)]
        force: bool,

        /// Destroy all running labs.
        #[arg(long)]
        all: bool,

        /// Also reap mgmt bridges / veths / namespaces with no state file
        /// (left behind by a crashed deploy). Implies best-effort cleanup;
        /// can be combined with --all or used on its own.
        #[arg(long)]
        orphans: bool,
    },

    /// Show running labs or details of a specific lab.
    ///
    /// JSON OUTPUT (with `--json`, no lab name):
    ///   [ { "name": str, "node_count": int, "created_at": str }, ... ]
    /// Schema: docs/json-schemas/status-list.schema.json
    ///
    /// JSON OUTPUT (with `--json --scan`):
    ///   { "labs": [ ... ],
    ///     "orphans": { "bridges": [str], "veths": [str], "netns": [str],
    ///                  "stale": [ { "name": str,
    ///                               "missing_namespaces": [str] } ] } }
    /// Schema: docs/json-schemas/status-scan.schema.json
    ///
    /// JSON OUTPUT (with `--json <lab>`):
    ///   topology object for the lab + an `addresses` field per node
    ///   + a `host_resources` block (mgmt bridge, declared subnets).
    ///
    /// Schema: docs/json-schemas/status-lab.schema.json
    Status {
        /// Lab name (omit to list all).
        name: Option<String>,

        /// Also scan the host for mgmt bridges / namespaces with no
        /// matching state file (orphans), and labs whose state file
        /// claims namespaces no longer present on the host (stale).
        #[arg(long)]
        scan: bool,
    },

    /// Run a command in a lab node.
    Exec {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Set environment variables (can be repeated: --env KEY=VALUE).
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,

        /// Working directory for the command. For namespace nodes this is
        /// `chdir()` on the host filesystem; for container nodes it's passed
        /// as `-w <path>` to docker/podman.
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,

        /// Maximum wall-clock time the command may run, in seconds. On
        /// expiry the child is sent SIGTERM, then SIGKILL after a 1s
        /// grace period. Exit code 124 on timeout (matches
        /// `coreutils timeout(1)`). Default: no timeout.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// Spawn a background process in a lab node.
    ///
    /// Stdout/stderr are captured to per-process log files at:
    ///
    ///   `$XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}`
    ///
    /// (defaults to `~/.local/state` if `XDG_STATE_HOME` is unset). The
    /// path is stable; consumers can read it directly, or use
    /// `nlink-lab logs <lab> --pid <pid>`.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "command": str, "node": str, "pid": int, "host_pid": int }
    /// Schema: docs/json-schemas/spawn.schema.json
    ///
    /// `pid` and `host_pid` are aliases — equal values today because
    /// nlink-lab does not use `CLONE_NEWPID`. See ARCHITECTURE.md
    /// "Process & namespace model" for why.
    Spawn {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Directory for stdout/stderr log files (default: lab state dir).
        #[arg(long)]
        log_dir: Option<PathBuf>,

        /// Set environment variables (can be repeated: --env KEY=VALUE).
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,

        /// Working directory for the spawned process (chdir before exec).
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,

        /// Wait for TCP port after spawn (e.g., "127.0.0.1:8080" or "8080").
        ///
        /// The probe runs inside the node's namespace, so `127.0.0.1:<port>`
        /// only matches a service that bound to the loopback interface. If
        /// your service binds to a specific node IP (e.g., the interface
        /// address), pass that address here instead of `127.0.0.1`.
        #[arg(long)]
        wait_tcp: Option<String>,

        /// Wait for a stdout/stderr line matching REGEX before returning.
        ///
        /// Useful for services that signal readiness via a log line
        /// rather than a port (e.g., `[STARTED] tunnel established`).
        /// Combinable with --wait-tcp; both must succeed before spawn
        /// returns. Fails the spawn on timeout.
        #[arg(long, value_name = "REGEX")]
        wait_log: Option<String>,

        /// Which stream to monitor for --wait-log: stdout, stderr, or
        /// both. Default: both.
        #[arg(long, value_name = "STREAM", default_value = "both")]
        wait_log_stream: WaitLogStream,

        /// Wait until the spawned process has a TCP listener on PORT
        /// inside its namespace. Reads `/proc/<pid>/net/tcp{,6}` —
        /// no actual `connect(2)` is attempted, so this works for
        /// services that bind to non-routable addresses or that
        /// would log connection-refused on probe attempts. Combinable
        /// with --wait-tcp / --wait-log (all AND-compose). Round-5
        /// §2.4.
        #[arg(long, value_name = "PORT")]
        wait_port: Option<u16>,

        /// Wait until the spawned process's open-fd count has been
        /// stable for SECS seconds. Heuristic — prefer --wait-log or
        /// --wait-port when a deterministic signal is available. A
        /// process can open more files later in its lifecycle.
        /// Round-5 §2.4.
        #[arg(long, value_name = "SECS")]
        wait_fd_stable: Option<f64>,

        /// Timeout for --wait-tcp / --wait-log / --wait-port /
        /// --wait-fd-stable in seconds (default: 30).
        #[arg(long, default_value = "30")]
        wait_timeout: u64,

        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// Validate a topology file without deploying.
    Validate {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Show resolved IP addresses for all interfaces.
        #[arg(long)]
        show_ips: bool,
    },

    /// Run topology tests: deploy, validate, destroy.
    Test {
        /// Topology file or directory of .nll files.
        path: PathBuf,

        /// Set a `param` value (repeatable) for every file: --set k=v.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Write JUnit XML results to file.
        #[arg(long)]
        junit: Option<PathBuf>,

        /// Write TAP output to stdout.
        #[arg(long)]
        tap: bool,

        /// Stop on first failure.
        #[arg(long)]
        fail_fast: bool,
    },

    /// Modify link impairment at runtime.
    ///
    /// JSON OUTPUT (with `--show --json`):
    ///   `{ "lab": str, "endpoints": { "<node>:<iface>": { ... } | null } }`
    /// Schema: docs/json-schemas/impair-show.schema.json
    ///
    /// Without `--show`, applies impairment changes; output is plain
    /// confirmation text.
    #[command(group = clap::ArgGroup::new("impair_mode").args(["show", "clear", "partition", "heal"]).multiple(false))]
    Impair {
        /// Lab name.
        lab: String,

        /// Endpoint (e.g., "router:eth0"). Not required with --show.
        endpoint: Option<String>,

        /// Show current impairments on all interfaces.
        #[arg(long)]
        show: bool,

        /// Delay (e.g., "10ms").
        #[arg(long)]
        delay: Option<String>,

        /// Jitter (e.g., "2ms").
        #[arg(long)]
        jitter: Option<String>,

        /// Packet loss (e.g., "0.1%").
        #[arg(long)]
        loss: Option<String>,

        /// Rate limit (e.g., "100mbit").
        #[arg(long)]
        rate: Option<String>,

        /// Remove impairment.
        #[arg(long)]
        clear: bool,

        /// Egress delay (applied to named endpoint).
        #[arg(long)]
        out_delay: Option<String>,

        /// Egress jitter.
        #[arg(long)]
        out_jitter: Option<String>,

        /// Egress packet loss.
        #[arg(long)]
        out_loss: Option<String>,

        /// Egress rate limit.
        #[arg(long)]
        out_rate: Option<String>,

        /// Ingress delay (applied to peer endpoint).
        #[arg(long)]
        in_delay: Option<String>,

        /// Ingress jitter.
        #[arg(long)]
        in_jitter: Option<String>,

        /// Ingress packet loss.
        #[arg(long)]
        in_loss: Option<String>,

        /// Ingress rate limit.
        #[arg(long)]
        in_rate: Option<String>,

        /// Simulate a network partition (save impairments, apply 100% loss).
        #[arg(long)]
        partition: bool,

        /// Restore pre-partition impairments.
        #[arg(long)]
        heal: bool,
    },

    /// Run a `scenario` block from a deployed lab's topology.
    ///
    /// JSON OUTPUT (with `--json`): the full `ScenarioResult` (steps,
    /// actions, assertion outcomes, timings). Exit 2 when any step fails.
    Scenario {
        /// Lab name (must be deployed).
        lab: String,

        /// Scenario name as declared in the topology (`scenario "name" { … }`).
        /// Omit to list the scenarios the lab defines.
        name: Option<String>,
    },

    /// Regenerate `docs/cli/*.md` from the clap definitions (maintainers).
    #[command(hide = true)]
    DocsGen {
        /// Output directory.
        #[arg(long, default_value = "docs/cli")]
        out: PathBuf,
    },

    /// Print topology as DOT graph.
    Graph {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Set a `param` value (repeatable): --set k=v.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,

        /// Emit a Mermaid `graph LR` block instead of DOT (renders inline
        /// in Forgejo/GitHub markdown).
        #[arg(long)]
        mermaid: bool,
    },

    /// Render a topology file with all loops, variables, and imports expanded.
    Render {
        /// Path to the topology file (.nll).
        topology: PathBuf,
        /// Output as DOT graph (for Graphviz).
        #[arg(long)]
        dot: bool,
        /// Output as ASCII diagram.
        #[arg(long)]
        ascii: bool,

        /// Set NLL parameters (can be repeated: --set key=value).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Open an interactive shell in a lab node.
    Shell {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Shell to use (default: /bin/sh).
        #[arg(long, default_value = "/bin/sh")]
        shell: String,
    },

    /// List background processes (alive and exited) tracked by `spawn`.
    ///
    /// Exited processes remain in the listing with `alive: false` so
    /// post-mortem inspection (which log files? when did they exit?) is
    /// possible. They are pruned only when the lab is destroyed. Consumers
    /// polling "is X still running?" must check the `alive` field, not
    /// just look up the PID — or pass `--alive-only` to filter dead
    /// entries out at the source.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   [ { "node": str, "pid": int, "host_pid": int, "alive": bool,
    ///       "stdout_log": str | null, "stderr_log": str | null }, ... ]
    /// Schema: docs/json-schemas/ps.schema.json
    Ps {
        /// Lab name.
        lab: String,

        /// Hide processes whose tracked PID has exited (alive == false).
        /// Useful for "is X still running?" polling loops where exited
        /// post-mortem entries would otherwise be misread as alive.
        #[arg(long)]
        alive_only: bool,
    },

    /// Kill a tracked background process.
    Kill {
        /// Lab name.
        lab: String,

        /// Process ID to kill.
        pid: u32,
    },

    /// Sample resource usage of a process inside a lab node.
    ///
    /// Reads `/proc/<pid>/{stat,status}` and counts entries in
    /// `/proc/<pid>/fd/` from inside the target namespace. Routes the
    /// reads through `nlink-lab exec` so `/proc/<pid>/fd/`
    /// (mode 0700, owned by root) is readable even from a non-root
    /// caller.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "host_pid": int, "command": str, "uid": int,
    ///     "rss_kb": int | null, "vsz_kb": int | null,
    ///     "fd_count": int,
    ///     "cpu_user_ticks": int, "cpu_kernel_ticks": int,
    ///     "started_at_unix_micros": int, "state": str }
    /// Schema: docs/json-schemas/proc-stat.schema.json
    ///
    /// CPU ticks are in `sysconf(_SC_CLK_TCK)` units (typically 100
    /// per second); convert by dividing.
    ProcStat {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Process ID (host PID — same as ns PID; see ARCHITECTURE.md).
        pid: u32,

        /// Sample every SECS seconds, emitting one record per
        /// sample. NDJSON (one JSON object per line) when combined
        /// with `--json`. Stops on Ctrl-C.
        #[arg(long, value_name = "SECS")]
        watch: Option<f64>,
    },

    /// Run diagnostics on a lab.
    Diagnose {
        /// Lab name.
        lab: String,

        /// Node name (omit to diagnose all).
        node: Option<String>,
    },

    /// Capture packets on an interface using netring.
    Capture {
        /// Lab name.
        lab: String,

        /// Endpoint (e.g., "router:eth0").
        endpoint: String,

        /// Write to pcap file (default: print summaries to stdout).
        #[arg(short, long)]
        write: Option<PathBuf>,

        /// Capture N packets then stop.
        #[arg(short, long)]
        count: Option<u64>,

        /// Legacy: full tcpdump filter expression (e.g., "tcp port
        /// 80"). Requires nlink-lab built with the
        /// `legacy-tcpdump-filter` feature *and* `tcpdump` on PATH.
        /// Default builds prefer the typed `--filter-*` flags below.
        #[arg(short, long)]
        filter: Option<String>,

        /// Match only TCP (sets ip_proto=6).
        #[arg(long = "filter-tcp")]
        filter_tcp: bool,

        /// Match only UDP (sets ip_proto=17).
        #[arg(long = "filter-udp")]
        filter_udp: bool,

        /// Match only ICMP (sets ip_proto=1).
        #[arg(long = "filter-icmp")]
        filter_icmp: bool,

        /// Match a specific IP protocol number (e.g. 47 for GRE).
        #[arg(long = "filter-ip-proto", value_name = "PROTO")]
        filter_ip_proto: Option<u8>,

        /// Restrict to IPv4 traffic.
        #[arg(long = "filter-ipv4")]
        filter_ipv4: bool,

        /// Restrict to IPv6 traffic.
        #[arg(long = "filter-ipv6")]
        filter_ipv6: bool,

        /// Match ARP frames (ethertype 0x0806).
        #[arg(long = "filter-arp")]
        filter_arp: bool,

        /// Match 802.1Q VLAN-tagged frames.
        #[arg(long = "filter-vlan")]
        filter_vlan: bool,

        /// Match a specific VLAN ID. Implies `--filter-vlan`.
        #[arg(long = "filter-vlan-id", value_name = "VID")]
        filter_vlan_id: Option<u16>,

        /// Match either source or destination IP address.
        #[arg(long = "filter-host", value_name = "ADDR")]
        filter_host: Option<std::net::IpAddr>,

        /// Match a specific source IP address.
        #[arg(long = "filter-src-host", value_name = "ADDR")]
        filter_src_host: Option<std::net::IpAddr>,

        /// Match a specific destination IP address.
        #[arg(long = "filter-dst-host", value_name = "ADDR")]
        filter_dst_host: Option<std::net::IpAddr>,

        /// Match either source or destination network (CIDR).
        #[arg(long = "filter-net", value_name = "CIDR")]
        filter_net: Option<String>,

        /// Match a source network (CIDR).
        #[arg(long = "filter-src-net", value_name = "CIDR")]
        filter_src_net: Option<String>,

        /// Match a destination network (CIDR).
        #[arg(long = "filter-dst-net", value_name = "CIDR")]
        filter_dst_net: Option<String>,

        /// Match either source or destination L4 port. Requires
        /// `--filter-tcp` or `--filter-udp`.
        #[arg(long = "filter-port", value_name = "PORT")]
        filter_port: Option<u16>,

        /// Match L4 source port.
        #[arg(long = "filter-src-port", value_name = "PORT")]
        filter_src_port: Option<u16>,

        /// Match L4 destination port.
        #[arg(long = "filter-dst-port", value_name = "PORT")]
        filter_dst_port: Option<u16>,

        /// Match any of these L4 ports (either source OR
        /// destination). Comma-separated, e.g. `80,443,8080`.
        /// Backed by netring 0.16's `BpfFilter::builder::ports()`
        /// multi-port shortcut — compiles to one BPF program
        /// branch per port. Requires `--filter-tcp` or
        /// `--filter-udp`.
        #[arg(long = "filter-ports", value_name = "PORTS", value_delimiter = ',')]
        filter_ports: Vec<u16>,

        /// Match any of these L4 source ports.
        #[arg(long = "filter-src-ports", value_name = "PORTS", value_delimiter = ',')]
        filter_src_ports: Vec<u16>,

        /// Match any of these L4 destination ports.
        #[arg(long = "filter-dst-ports", value_name = "PORTS", value_delimiter = ',')]
        filter_dst_ports: Vec<u16>,

        /// Negate the entire filter (capture everything that does
        /// NOT match the other `--filter-*` flags).
        #[arg(long = "filter-not")]
        filter_not: bool,

        /// Stop after N seconds.
        #[arg(long)]
        duration: Option<f64>,

        /// Snap length -- truncate packets to N bytes.
        #[arg(long, default_value = "262144")]
        snap_len: u32,

        /// Drop outgoing packets at the kernel via
        /// `PACKET_IGNORE_OUTGOING`. Use this when capturing on `lo`
        /// to halve the packet count: loopback otherwise reports each
        /// packet twice (once outgoing, once incoming). No effect on
        /// non-loopback interfaces. (Requires kernel >= 4.20.)
        #[arg(long)]
        dedupe_loopback: bool,

        /// Rotate the pcap file when the active segment exceeds this
        /// size (suffixes K/M/G accepted; e.g. `100M`). On rotation
        /// the active `<base>.pcap` becomes `<base>.pcap.1`, the
        /// previous `.1` becomes `.2`, etc. Requires `--write`.
        /// Round-5 §2.3.
        #[arg(long, value_name = "SIZE", value_parser = parse_byte_size)]
        max_size: Option<u64>,

        /// Rotate the pcap file every SECS seconds since the last
        /// rotation. Composes with `--max-size` (whichever threshold
        /// fires first triggers the rotation). Requires `--write`.
        #[arg(long, value_name = "SECS")]
        rotate: Option<f64>,

        /// Number of *rotated* segments to keep (`<base>.pcap.1`
        /// through `<base>.pcap.<keep>`). The active `<base>.pcap`
        /// is always retained and doesn't count. Default: 5.
        #[arg(long, default_value = "5", value_name = "N")]
        keep: usize,
    },

    /// Wait for a lab to be ready.
    Wait {
        /// Lab name.
        name: String,

        /// Timeout in seconds (default: 30).
        #[arg(short, long, default_value = "30")]
        timeout: u64,
    },

    /// Tail nftables + RTNETLINK drift events for a running lab.
    ///
    /// Subscribes to every node in the lab and prints one line
    /// per kernel mutation — useful for spotting hand-edits that
    /// bypass `nlink-lab apply`. `--json` emits NDJSON for
    /// piping to `jq`. Plan 159b.
    Watch {
        /// Lab name.
        lab: String,

        /// Event family: route, nftables, or both.
        #[arg(long, value_enum, default_value_t = WatchFamilyArg::Both)]
        family: WatchFamilyArg,

        /// Restrict subscription to a single node. Without this
        /// flag, every node in the lab is subscribed. Filter is
        /// pre-subscription — we don't open connections we don't
        /// need.
        #[arg(long)]
        node: Option<String>,

        /// Show resync replay frames after ENOBUFS recoveries.
        /// By default they're silenced — the user only sees
        /// live multicast deltas. With this flag, snapshot
        /// frames render with a `[snapshot]` marker.
        #[arg(long)]
        include_snapshot: bool,
    },

    /// Wait for a service or condition inside a lab node.
    WaitFor {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost).
        #[arg(long)]
        tcp: Option<String>,

        /// Wait for command to succeed (exit 0).
        #[arg(long = "exec")]
        exec_cmd: Option<String>,

        /// Wait for file to exist.
        #[arg(long)]
        file: Option<String>,

        /// Timeout in seconds (default: 30).
        #[arg(short, long, default_value = "30")]
        timeout: u64,

        /// Poll interval in milliseconds (default: 500).
        #[arg(long, default_value = "500")]
        interval: u64,
    },

    /// Show IP addresses assigned to a node.
    Ip {
        /// Lab name.
        lab: String,

        /// Node name.
        node: String,

        /// Filter by interface name.
        #[arg(long)]
        iface: Option<String>,

        /// Show CIDR notation (include prefix length).
        #[arg(long)]
        cidr: bool,
    },

    /// Compare two topology files and show differences.
    Diff {
        /// First topology file.
        a: PathBuf,

        /// Second topology file.
        b: PathBuf,

        /// Set a `param` value (repeatable) for both files: --set k=v.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Export a running lab's topology as serialized data.
    ///
    /// By default, dumps the rendered topology as TOML/JSON to stdout
    /// or `--output FILE`. With `--archive`, produces a portable
    /// `.nlz` lab archive (tar.gz with manifest + topology + params
    /// + rendered + checksums) suitable for sharing repros.
    Export {
        /// Lab name (or path to an .nll file with --archive).
        lab: String,

        /// Output file (default: stdout for plain export, `./<lab>.nlz` with `--archive`).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Produce a portable `.nlz` archive instead of plain TOML/JSON.
        #[arg(long)]
        archive: bool,

        /// (with --archive) Include live state (PIDs, ns names) for inspection.
        #[arg(long, requires = "archive")]
        include_running_state: bool,

        /// (with --archive) Skip the rendered.toml snapshot.
        #[arg(long, requires = "archive")]
        no_rendered: bool,

        /// (with --archive) NLL `param` overrides recorded in the archive.
        #[arg(long = "set", value_name = "KEY=VALUE", requires = "archive")]
        set_params: Vec<String>,
    },

    /// Import a `.nlz` lab archive.
    ///
    /// Verifies checksums, extracts to `./<lab-name>/` (or `-d DIR`),
    /// and validates the topology. Pass `--no-deploy` to extract +
    /// validate without deploying; `--no-reparse` to use the bundled
    /// rendered.toml directly (useful when the archive was produced
    /// by a newer nlink-lab whose NLL syntax we don't fully understand).
    Import {
        /// Path to a `.nlz` archive.
        archive: PathBuf,

        /// Extract to this directory. Default: `./<lab-name>/`
        #[arg(short = 'd', long)]
        dir: Option<PathBuf>,

        /// Extract + validate only; don't deploy.
        #[arg(long)]
        no_deploy: bool,

        /// Use the archive's rendered.toml as-is, skip re-parsing the NLL.
        #[arg(long)]
        no_reparse: bool,
    },

    /// Show comprehensive lab details, OR summarize a `.nlz` archive.
    ///
    /// If LAB is a path ending in `.nlz`, summarizes the archive
    /// (manifest + node/link/network counts) without extracting.
    /// Otherwise, behaves as before — runs against a deployed lab.
    Inspect {
        /// Lab name, or path to a `.nlz` archive.
        lab: String,
    },

    /// List container nodes in a running lab.
    Containers {
        /// Lab name.
        lab: String,
    },

    /// Show container logs or per-process logs from `nlink-lab spawn`.
    ///
    /// Without `--pid`: shows the container's stdout/stderr (node must be
    /// a container).  With `--pid`: shows the per-process log file written
    /// by `spawn`. Per-process log files live at:
    ///
    ///   `$XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}`
    ///
    /// (defaults to `~/.local/state` if `XDG_STATE_HOME` is unset). The
    /// path is stable; consumers can read it directly.
    Logs {
        /// Lab name.
        lab: String,
        /// Node name (for container logs).
        node: Option<String>,
        /// Process ID (for background process logs).
        #[arg(long)]
        pid: Option<u32>,
        /// Show stderr instead of stdout (with --pid).
        #[arg(long)]
        stderr: bool,
        /// Stream logs in tail -F style. Works for container nodes (via
        /// the runtime) and for tracked background processes (via
        /// `--pid`). Re-opens the file on rotation/truncation. Stops on
        /// Ctrl-C.
        #[arg(long)]
        follow: bool,
        /// Show last N lines.
        #[arg(long)]
        tail: Option<u32>,
    },

    /// Pre-pull all container images from a topology.
    Pull {
        /// Path to the topology file (.nll).
        topology: PathBuf,

        /// Set a `param` value (repeatable): --set k=v.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Show container resource usage.
    Stats {
        /// Lab name.
        lab: String,
    },

    /// Restart a container node.
    Restart {
        /// Lab name.
        lab: String,
        /// Node name (must be a container node).
        node: String,
    },

    /// Generate shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Start the Zenoh backend daemon for a running lab.
    Daemon {
        /// Lab name (must be deployed).
        lab: String,

        /// Metrics collection interval in seconds.
        #[arg(short, long, default_value = "2")]
        interval: u64,

        /// Zenoh mode: peer or client.
        #[arg(long, default_value = "peer")]
        zenoh_mode: String,

        /// Zenoh listen endpoint.
        #[arg(long)]
        zenoh_listen: Option<String>,

        /// Zenoh connect endpoint.
        #[arg(long)]
        zenoh_connect: Option<String>,
    },

    /// Stream live metrics from a lab via Zenoh (no root required).
    Metrics {
        /// Lab name.
        lab: String,

        /// Filter to specific node.
        #[arg(short, long)]
        node: Option<String>,

        /// Output format (`--json` selects json too).
        #[arg(short, long, value_enum, default_value_t = MetricsFormat::Table)]
        format: MetricsFormat,

        /// Number of samples then exit.
        #[arg(short, long)]
        count: Option<usize>,

        /// Zenoh connect endpoint.
        #[arg(long)]
        zenoh_connect: Option<String>,
    },

    /// Create a topology file from a built-in template.
    Init {
        /// Template name (e.g., "router", "spine-leaf"). Use --list to see all.
        template: Option<String>,

        /// List available templates.
        #[arg(long)]
        list: bool,

        /// Output directory (default: current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Override the lab name.
        #[arg(short, long)]
        name: Option<String>,

        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
}

/// Process exit status chosen by a subcommand that completed its work
/// but wants a non-zero status (a child's exit code, an assertion
/// failure, …). `run()` keeps returning `Result<()>`; arms call
/// [`set_exit_code`] instead of `std::process::exit` so buffered
/// output and destructors still run.
static EXIT_CODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Exit codes: 0 ok · 1 error · 2 validation / assertion / drift
/// failure · 124 timeout · `exec`/`shell` pass the child's code through.
const EXIT_FAILURE: u8 = 1;
const EXIT_VALIDATION: u8 = 2;
const EXIT_TIMEOUT: u8 = 124;

fn set_exit_code(code: u8) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
}

fn exit_code_for(err: &nlink_lab::Error) -> u8 {
    match err {
        nlink_lab::Error::Validation(_)
        | nlink_lab::Error::ValidationErrors(_)
        | nlink_lab::Error::InvalidTopology(_) => EXIT_VALIDATION,
        nlink_lab::Error::Timeout(_) => EXIT_TIMEOUT,
        _ => EXIT_FAILURE,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Set tracing level based on --verbose flag (default: warn, verbose: info)
    let env_filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new("info")
    } else {
        tracing_subscriber::EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Handle completions synchronously (no runtime needed)
    if let Commands::Completions { shell } = &cli.command {
        clap_complete::generate(
            *shell,
            &mut Cli::command(),
            "nlink-lab",
            &mut std::io::stdout(),
        );
        return ExitCode::SUCCESS;
    }

    // Plan 158b Phase 3 — when `--json` is on, surface terminal
    // errors as a structured envelope on stderr (still going to
    // stderr to keep stdout clean for tools piping JSON output).
    let want_json_errors = cli.json;

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::from(EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst)),
        Err(nlink_lab::Error::NllDiagnostic(diag)) => {
            // NLL diagnostics get their own rich miette renderer
            // even under --json; the structured envelope below is
            // for kernel/runtime errors.
            let report = miette::Report::new(*diag);
            eprintln!("{report:?}");
            ExitCode::from(EXIT_VALIDATION)
        }
        Err(e) if want_json_errors => {
            let envelope = render_error_json(&e);
            // Pretty-printing keeps the schema obvious in
            // interactive use; tools that want compact output can
            // re-serialize with serde_json::to_string.
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| format!("error: {e}"))
            );
            ExitCode::from(exit_code_for(&e))
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(exit_code_for(&e))
        }
    }
}

/// Plan 158b Phase 3 — render an `nlink_lab::Error` as a
/// structured JSON envelope for the `--json` paths. Walks the
/// `std::error::Error::source` chain to build `error_chain`,
/// and surfaces `errno` / `ext_ack` / `ext_ack_offset` from any
/// `nlink::Error::Kernel` / `KernelWithContext` found in the
/// chain (via the inherent accessors added in Plan 158b Phase 1).
fn render_error_json(err: &nlink_lab::Error) -> serde_json::Value {
    let mut chain: Vec<String> = Vec::new();
    let mut src: &dyn std::error::Error = err;
    loop {
        chain.push(src.to_string());
        match src.source() {
            Some(next) => src = next,
            None => break,
        }
    }
    let mut envelope = serde_json::json!({
        "error": err.to_string(),
        "error_chain": chain,
        "errno": err.errno(),
        "ext_ack": err.ext_ack(),
        "ext_ack_offset": err.ext_ack_offset(),
        "exit_code": exit_code_for(err),
    });
    // Validation failures carry the structured issues so `--json`
    // consumers do not have to scrape "see errors above".
    if let nlink_lab::Error::ValidationErrors(issues) = err {
        envelope["issues"] = serde_json::to_value(issues).unwrap_or(serde_json::Value::Null);
    }
    envelope
}

/// Turn a failed validation into the structured error (exit 2, issues
/// in the JSON envelope), printing the human-readable list first.
fn validation_failed(lab: &str, result: &nlink_lab::ValidationResult) -> nlink_lab::Error {
    eprintln!("Validation failed for {lab:?}:");
    for e in result.errors() {
        eprintln!("  {} {e}", red("ERROR"));
    }
    nlink_lab::Error::ValidationErrors(result.errors().cloned().collect())
}

/// Parse a topology file, optionally with CLI `--set` parameters.
fn parse_topology(
    path: &std::path::Path,
    params: &[String],
) -> nlink_lab::Result<nlink_lab::Topology> {
    let cli_params: Vec<(String, String)> = params
        .iter()
        .map(|p| {
            let (key, value) = p.split_once('=').ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "invalid --set format: '{p}' (expected KEY=VALUE)"
                ))
            })?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect::<nlink_lab::Result<Vec<_>>>()?;

    if cli_params.is_empty() {
        nlink_lab::parser::parse_file(path)
    } else {
        nlink_lab::parser::parse_file_with_params(path, &cli_params)
    }
}

async fn run(cli: Cli) -> nlink_lab::Result<()> {
    let json = cli.json;
    let quiet = cli.quiet;
    let verbose = cli.verbose;
    match cli.command {
        Commands::Deploy {
            topology,
            dry_run,
            force,
            daemon,
            skip_validate,
            params,
            strict,
            suffix,
            unique,
        } => {
            let mut topo = parse_topology(&topology, &params)?;
            if unique {
                topo.lab.name = format!("{}-{}", topo.lab.name, std::process::id());
            } else if let Some(ref sfx) = suffix {
                topo.lab.name = format!("{}-{sfx}", topo.lab.name);
            }
            if skip_validate {
                topo.assertions.clear();
            }
            let result = topo.validate();

            // Print warnings
            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }

            if result.has_errors() {
                return Err(validation_failed(&topo.lab.name, &result));
            }

            if dry_run {
                println!("Topology {:?} is valid", topo.lab.name);
                print_topology_summary(&topo);
                return Ok(());
            }

            // Handle --force: destroy existing lab first
            if force {
                if nlink_lab::state::exists(&topo.lab.name) {
                    let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
                    lab.destroy().await?;
                } else {
                    // Best-effort cleanup of orphaned resources (no state file)
                    force_cleanup(&topo.lab.name).await;
                }
            }

            require_root()?;

            let start = Instant::now();
            let lab = topo.deploy().await?;
            let elapsed = start.elapsed();
            let assertions_failed = lab.assertions_failed();

            if json {
                let mut report = serde_json::json!({
                    "name": topo.lab.name,
                    "nodes": topo.nodes.len(),
                    "links": topo.links.len(),
                    "deploy_time_ms": elapsed.as_millis() as u64,
                });
                if !lab.assertion_results().is_empty() {
                    report["assertions"] = serde_json::to_value(lab.assertion_results())?;
                    report["assertions_failed"] = serde_json::Value::Bool(assertions_failed);
                }
                println!("{report}");
            } else {
                println!(
                    "{} Lab {:?} deployed in {:.0?}",
                    green("OK"),
                    topo.lab.name,
                    elapsed
                );
                print_deploy_summary(&topo);

                if !quiet {
                    let first_node = topo
                        .nodes
                        .keys()
                        .next()
                        .map(|s| s.as_str())
                        .unwrap_or("node");
                    println!();
                    println!("Next steps:");
                    println!(
                        "  nlink-lab status {}          # inspect lab",
                        topo.lab.name
                    );
                    println!(
                        "  nlink-lab exec {} {} -- ip addr",
                        topo.lab.name, first_node
                    );
                    println!(
                        "  nlink-lab shell {} {}        # interactive shell",
                        topo.lab.name, first_node
                    );
                    println!("  nlink-lab destroy {}         # tear down", topo.lab.name);
                }
                for a in lab.assertion_results() {
                    if !a.passed {
                        eprintln!(
                            "  {} {}{}",
                            red("FAIL"),
                            a.description,
                            a.detail
                                .as_ref()
                                .map(|d| format!(": {d}"))
                                .unwrap_or_default()
                        );
                    }
                }
            }

            if assertions_failed {
                if strict {
                    return Err(nlink_lab::Error::Validation(format!(
                        "{} of {} assertion(s) failed (lab {:?} left deployed for inspection)",
                        lab.assertion_results().iter().filter(|a| !a.passed).count(),
                        lab.assertion_results().len(),
                        topo.lab.name
                    )));
                }
                eprintln!(
                    "  {} assertions failed; pass --strict to make this fatal",
                    yellow("WARN")
                );
            }

            if daemon {
                nlink_lab_backend::run(lab, nlink_lab_backend::BackendOpts::default()).await?;
            }
            Ok(())
        }

        Commands::Apply {
            topology,
            params,
            dry_run,
            check,
        } => {
            // --check implies --dry-run.
            let dry_run = dry_run || check;

            let desired = parse_topology(&topology, &params)?;
            let result = desired.validate();
            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }
            if result.has_errors() {
                return Err(validation_failed(&desired.lab.name, &result));
                #[allow(unreachable_code)]
                for e in result.errors() {
                    eprintln!("  {} {e}", red("ERROR"));
                }
                return Err(nlink_lab::Error::Validation("see errors above".into()));
            }

            // Load current topology from running lab state
            let lab_name = &desired.lab.name;
            if !nlink_lab::state::exists(lab_name) {
                return Err(nlink_lab::Error::NotFound {
                    name: format!("{lab_name} (deploy first, then apply changes)"),
                });
            }
            let mut running = nlink_lab::RunningLab::load(lab_name)?;
            let current = running.topology();

            let diff = nlink_lab::diff_topologies(current, &desired);

            // Plan 158f Phase 2 — when --check or --dry-run is set,
            // compute the layered diff (topology + per-namespace
            // NetworkConfig + NftablesConfig) so the user sees the
            // full set of kernel changes apply would commit, not
            // just the lab-graph subset.
            //
            // `compute_layered_diff` walks every node and runs a
            // dump round-trip per (node, protocol family). For a
            // 50-node lab that's ~100 dumps; in practice ms-scale
            // on a quiet host. The cost only applies on
            // --check/--dry-run paths; normal apply doesn't pay it.
            let layered_view = if check || dry_run {
                Some(nlink_lab::compute_layered_diff(&running, &desired).await?)
            } else {
                None
            };

            // JSON dry-run output for CI consumption.
            if json && dry_run {
                let layered = layered_view
                    .as_ref()
                    .expect("layered_view is Some when dry_run");
                #[derive(serde::Serialize)]
                struct DryRunReport<'a> {
                    /// Plan 159d — typed-shape schema marker.
                    /// `3` = v3 (this format). Downstream `jq`
                    /// consumers should branch on this. v3 dropped
                    /// the v1 `diff` / `layered_summary` /
                    /// `layered_summary_deprecated` fields (Plan
                    /// 160 / 0.7.0) — use `network`/`nftables`.
                    schema_version: u32,
                    lab: &'a str,
                    no_op: bool,
                    change_count: usize,
                    /// Plan 159d — typed per-namespace
                    /// `NetworkConfig` diff under
                    /// `nlink/serde`. Empty map elided.
                    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
                    network: &'a std::collections::HashMap<String, nlink_lab::diff::ConfigDiff>,
                    /// Plan 159d — typed per-namespace
                    /// `NftablesDiff`. Empty map elided.
                    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
                    nftables: &'a std::collections::HashMap<String, nlink_lab::diff::NftablesDiff>,
                }
                let report = DryRunReport {
                    schema_version: 3,
                    lab: lab_name,
                    no_op: layered.is_empty(),
                    change_count: layered.change_count(),
                    network: &layered.network,
                    nftables: &layered.nftables,
                };
                println!("{}", serde_json::to_string_pretty(&report)?);
                if check && !layered.is_empty() {
                    return Err(nlink_lab::Error::Validation(format!(
                        "drift detected: {} change(s) needed to converge",
                        layered.change_count(),
                    )));
                }
                return Ok(());
            }

            // Non-JSON path. For --check / --dry-run, render the
            // layered diff (richer than the lab-graph-only
            // TopologyDiff). For ordinary apply, the existing
            // TopologyDiff render is what gets printed.
            let layered_is_empty = layered_view.as_ref().map(|l| l.is_empty()).unwrap_or(true);

            if (check || dry_run) && layered_is_empty {
                if !quiet {
                    println!("No changes to apply.");
                }
                return Ok(());
            }
            if !(check || dry_run) && diff.is_empty() {
                if !quiet {
                    println!("No changes to apply.");
                }
                return Ok(());
            }

            // --check: exit non-zero if any layered drift.
            if check {
                let layered = layered_view
                    .as_ref()
                    .expect("layered_view is Some when check");
                if !quiet {
                    println!("Drift detected for lab '{lab_name}':");
                    print!("{layered}");
                    println!("{} change(s) needed to converge", layered.change_count());
                }
                return Err(nlink_lab::Error::Validation(format!(
                    "drift detected: {} change(s) needed to converge",
                    layered.change_count(),
                )));
            }

            if !quiet {
                println!("Changes for lab '{lab_name}':");
                if let Some(layered) = &layered_view {
                    print!("{layered}");
                    println!("{} change(s)", layered.change_count());
                } else {
                    print!("{diff}");
                    println!("{} change(s)", diff.change_count());
                }
            }

            if dry_run {
                if !quiet {
                    println!("\n(dry run — no changes applied)");
                }
                return Ok(());
            }

            require_root()?;
            let start = Instant::now();
            nlink_lab::apply_diff(&mut running, &desired, &diff).await?;
            let elapsed = start.elapsed();

            if !quiet {
                println!(
                    "\nApplied {} change(s) in {:.0?}",
                    diff.change_count(),
                    elapsed
                );
            }
            Ok(())
        }

        Commands::Destroy {
            name,
            force,
            all,
            orphans,
        } => {
            require_root()?;
            if all {
                let labs = nlink_lab::RunningLab::list()?;
                if labs.is_empty() && !orphans {
                    println!("No running labs.");
                    return Ok(());
                }
                for info in &labs {
                    match nlink_lab::RunningLab::load(&info.name) {
                        Ok(lab) => {
                            lab.destroy().await?;
                            println!("Destroyed '{}'", info.name);
                        }
                        Err(_) if force => {
                            force_cleanup(&info.name).await;
                            println!("Force-cleaned '{}'", info.name);
                        }
                        Err(e) => eprintln!("Failed to destroy '{}': {e}", info.name),
                    }
                }
                if !labs.is_empty() {
                    println!("{} lab(s) destroyed", labs.len());
                }
                if orphans {
                    reap_orphans(&labs).await;
                }
                return Ok(());
            }
            if orphans && name.is_none() {
                // `destroy --orphans` alone: reap without touching state-backed labs.
                let labs = nlink_lab::RunningLab::list()?;
                reap_orphans(&labs).await;
                return Ok(());
            }
            let name = name.ok_or_else(|| {
                nlink_lab::Error::deploy_failed("lab name required (or use --all/--orphans)")
            })?;
            match nlink_lab::RunningLab::load(&name) {
                Ok(lab) => {
                    let node_count = lab.namespace_count();
                    let topo = lab.topology();
                    let container_count = topo.nodes.values().filter(|n| n.image.is_some()).count();
                    let link_count = topo.links.len();
                    let process_count = lab.process_status().iter().filter(|p| p.alive).count();
                    lab.destroy().await?;
                    println!("Lab {name:?} destroyed:");
                    println!("  Nodes:       {node_count}");
                    if container_count > 0 {
                        println!("  Containers:  {container_count} stopped and removed");
                    }
                    println!("  Links:       {link_count}");
                    if process_count > 0 {
                        println!("  Processes:   {process_count} killed");
                    }
                }
                Err(e) if force => {
                    eprintln!("warning: state not found, attempting force cleanup: {e}");
                    force_cleanup(&name).await;
                    println!("Lab {name:?} force-cleaned");
                }
                Err(nlink_lab::Error::NotFound { .. }) => {
                    // Idempotent: destroying a non-existent lab is a no-op
                }
                Err(e) => return Err(e),
            }
            Ok(())
        }

        Commands::Status { name, scan } => match name {
            None => {
                let labs = nlink_lab::RunningLab::list()?;
                let orphans = if scan {
                    find_orphans(&labs).await
                } else {
                    Orphans::default()
                };
                if json {
                    if scan {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "labs": labs,
                                "orphans": orphans,
                            }))?
                        );
                    } else {
                        println!("{}", serde_json::to_string_pretty(&labs)?);
                    }
                } else if labs.is_empty() {
                    println!("No running labs.");
                } else {
                    println!("{:<18} {:<6} CREATED", "NAME", "NODES");
                    for info in labs {
                        println!(
                            "{:<18} {:<6} {}",
                            info.name, info.node_count, info.created_at
                        );
                    }
                }
                if scan && !json && !orphans.is_empty() {
                    let has_orphans = !orphans.bridges.is_empty()
                        || !orphans.veths.is_empty()
                        || !orphans.netns.is_empty();
                    if has_orphans {
                        println!();
                        println!("Orphans detected (no matching state file):");
                        for b in &orphans.bridges {
                            println!("  bridge {b}");
                        }
                        for v in &orphans.veths {
                            println!("  veth   {v}");
                        }
                        for n in &orphans.netns {
                            println!("  netns  {n}");
                        }
                        println!();
                        println!("Run `nlink-lab destroy --orphans` to clean up.");
                    }
                    if !orphans.stale.is_empty() {
                        println!();
                        println!("Stale labs detected (state file with missing resources):");
                        for s in &orphans.stale {
                            println!(
                                "  {}  (missing: {})",
                                s.name,
                                s.missing_namespaces.join(", ")
                            );
                        }
                        println!();
                        println!(
                            "Run `nlink-lab destroy <lab>` to clean up each stale state file."
                        );
                    }
                }
                if scan && !json && verbose && orphans.untagged_ignored > 0 {
                    println!();
                    println!(
                        "{} untagged namespace(s) ignored (not created by nlink-lab).",
                        orphans.untagged_ignored
                    );
                }
                Ok(())
            }
            Some(name) => {
                let lab = nlink_lab::RunningLab::load(&name)?;
                if json {
                    let mut output = serde_json::to_value(lab.topology())?;
                    // Add resolved addresses per node (including mgmt0)
                    if let Some(nodes) = output.get_mut("nodes")
                        && let Some(nodes_obj) = nodes.as_object_mut()
                    {
                        for node_name in nodes_obj.keys().cloned().collect::<Vec<_>>() {
                            if let Ok(addrs) = lab.node_addresses(&node_name)
                                && !addrs.is_empty()
                                && let Some(n) = nodes_obj.get_mut(&node_name)
                                && let Some(o) = n.as_object_mut()
                            {
                                o.insert("addresses".to_string(), serde_json::json!(addrs));
                            }
                        }
                    }
                    // Add host_resources — round-5 §1.2 bonus. Lets
                    // consumers detect parallel-lab collisions
                    // client-side (mgmt bridge name, declared subnets).
                    if let Some(o) = output.as_object_mut() {
                        o.insert("host_resources".to_string(), host_resources_json(&lab));
                    }
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    let topo = lab.topology();
                    println!("Lab: {}", lab.name());
                    println!(
                        "Nodes: {}  Links: {}  Impairments: {}",
                        lab.namespace_count(),
                        topo.links.len(),
                        topo.impairments.len()
                    );
                    println!();
                    println!("  {:<20} {:<12} IMAGE", "NODE", "TYPE");
                    let mut names: Vec<&String> = topo.nodes.keys().collect();
                    names.sort();
                    for name in names {
                        let node = &topo.nodes[name];
                        let kind = if node.image.is_some() {
                            "container"
                        } else {
                            "namespace"
                        };
                        let image = node.image.as_deref().unwrap_or("--");
                        println!("  {:<20} {:<12} {}", name, kind, image);
                    }
                }
                Ok(())
            }
        },

        Commands::Exec {
            lab,
            node,
            env_vars,
            workdir,
            timeout,
            cmd,
        } => {
            require_root()?;
            let env_pairs = parse_env_pairs(&env_vars)?;
            let env_refs: Vec<(&str, &str)> = env_pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let opts = nlink_lab::ExecOpts {
                workdir: workdir.as_deref(),
                env: &env_refs,
                timeout: timeout.map(std::time::Duration::from_secs),
            };

            if cli.json {
                // In JSON mode, wrap ALL errors as JSON output
                let result = (|| -> nlink_lab::Result<serde_json::Value> {
                    let running = nlink_lab::RunningLab::load(&lab)?;
                    let node_names: Vec<&str> = running.node_names().collect();
                    if !node_names.contains(&node.as_str()) {
                        return Err(nlink_lab::Error::NodeNotFound { name: node.clone() });
                    }
                    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
                    let start = Instant::now();
                    let output = running.exec_with_opts(&node, &cmd[0], &args, opts)?;
                    let duration_ms = start.elapsed().as_millis() as u64;
                    Ok(serde_json::json!({
                        "exit_code": output.exit_code,
                        "stdout": output.stdout,
                        "stderr": output.stderr,
                        "duration_ms": duration_ms,
                    }))
                })();
                match result {
                    Ok(json) => {
                        // The process exits with the child's code, exactly
                        // like the non-JSON path (#42); the envelope still
                        // carries it for consumers.
                        let code = json["exit_code"].as_i64().unwrap_or(0);
                        println!("{json}");
                        if code != 0 {
                            set_exit_code(u8::try_from(code.clamp(0, 255)).unwrap_or(EXIT_FAILURE));
                        }
                    }
                    Err(nlink_lab::Error::Timeout(d)) => {
                        // Timeout in --json: emit a structured error and
                        // exit 124 so scripts can distinguish "child
                        // exited 124" from "we timed out".
                        println!(
                            "{}",
                            serde_json::json!({
                                "error": format!("timed out after {d:?}"),
                                "exit_code": 124,
                                "stdout": "",
                                "stderr": "",
                                "duration_ms": d.as_millis() as u64,
                            })
                        );
                        set_exit_code(EXIT_TIMEOUT);
                    }
                    Err(e) => {
                        // Lab-level error (lab/node not found, exec failed):
                        // the same exec-shaped envelope on stdout so
                        // consumers keep one parser, plus the structured
                        // error envelope on stderr and a non-zero exit.
                        println!(
                            "{}",
                            serde_json::json!({
                                "error": e.to_string(),
                                "exit_code": null,
                                "stdout": "",
                                "stderr": "",
                                "duration_ms": 0,
                            })
                        );
                        return Err(e);
                    }
                }
                return Ok(());
            }

            // Non-JSON path: stream stdio live so long-running commands
            // (services, tail -f, ping) show output as it's produced.
            // Scripts that want captured/structured output should use
            // `--json`, which still buffers into the structured response.
            let running = nlink_lab::RunningLab::load(&lab)?;
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Available nodes: {}", node_names.join(", "));
                return Err(nlink_lab::Error::NodeNotFound { name: node });
            }
            let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
            match running.exec_attached_with_opts(&node, &cmd[0], &args, opts) {
                Ok(code) => {
                    if code != 0 {
                        set_exit_code(u8::try_from(code.clamp(0, 255)).unwrap_or(EXIT_FAILURE));
                    }
                    Ok(())
                }
                Err(nlink_lab::Error::Timeout(d)) => {
                    eprintln!("nlink-lab exec: command timed out after {d:?}");
                    set_exit_code(EXIT_TIMEOUT);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }

        Commands::Spawn {
            lab,
            node,
            log_dir,
            env_vars,
            workdir,
            wait_tcp,
            wait_log,
            wait_log_stream,
            wait_port,
            wait_fd_stable,
            wait_timeout,
            cmd,
        } => {
            require_root()?;
            let env_pairs = parse_env_pairs(&env_vars)?;
            let env_refs: Vec<(&str, &str)> = env_pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let mut running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            let args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let opts = nlink_lab::SpawnOpts {
                log_dir: log_dir.as_deref(),
                workdir: workdir.as_deref(),
                env: &env_refs,
            };
            let pid = running.spawn_with_logs_with_opts(&node, &args, opts)?;
            running.save_state()?;

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "pid": pid,
                        // Explicit alias for `pid`: equal today because nlink-lab
                        // doesn't use CLONE_NEWPID (host_pid == ns_pid). See
                        // ARCHITECTURE.md "Process & namespace model". Round-5 §2.1.
                        "host_pid": pid,
                        "node": node,
                        "command": cmd.join(" "),
                    })
                );
            } else {
                println!("PID: {pid}");
            }

            // Wait for readiness signal(s). --wait-tcp and --wait-log are
            // independent and AND-composed: both must succeed before
            // spawn returns. Either one can fail the spawn via timeout.
            let timeout = std::time::Duration::from_secs(wait_timeout);
            let interval = std::time::Duration::from_millis(500);
            if let Some(ref tcp_addr) = wait_tcp {
                let (ip, port) = if let Some((ip, port_str)) = tcp_addr.rsplit_once(':') {
                    (
                        ip.to_string(),
                        port_str.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                } else {
                    (
                        "127.0.0.1".to_string(),
                        tcp_addr.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                };
                running
                    .wait_for_tcp(&node, &ip, port, timeout, interval)
                    .await?;
            }
            if let Some(ref re_src) = wait_log {
                let pattern = regex::Regex::new(re_src).map_err(|e| {
                    nlink_lab::Error::invalid_topology(format!(
                        "invalid --wait-log regex {re_src:?}: {e}"
                    ))
                })?;
                running
                    .wait_for_log_line(pid, &pattern, wait_log_stream.into(), timeout, interval)
                    .await?;
            }
            if let Some(port) = wait_port {
                running
                    .wait_for_port(&node, pid, port, timeout, interval)
                    .await?;
            }
            if let Some(secs) = wait_fd_stable {
                let stable_for = std::time::Duration::from_secs_f64(secs);
                running
                    .wait_for_fd_stable(&node, pid, stable_for, timeout, interval)
                    .await?;
            }
            if (wait_tcp.is_some()
                || wait_log.is_some()
                || wait_port.is_some()
                || wait_fd_stable.is_some())
                && !cli.quiet
            {
                eprintln!("ready");
            }

            Ok(())
        }

        Commands::Validate {
            topology,
            params,
            show_ips,
        } => {
            let topo = parse_topology(&topology, &params)?;
            let result = topo.validate();

            if json {
                // One envelope for both outcomes; exit 2 on errors (#46).
                let issues: Vec<&nlink_lab::ValidationIssue> = result.issues().iter().collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "lab": topo.lab.name,
                        "valid": !result.has_errors(),
                        "nodes": topo.nodes.len(),
                        "links": topo.links.len(),
                        "networks": topo.networks.len(),
                        "errors": result.errors().count(),
                        "warnings": result.warnings().count(),
                        "issues": issues,
                    }))?
                );
                if result.has_errors() {
                    set_exit_code(EXIT_VALIDATION);
                }
                return Ok(());
            }

            for w in result.warnings() {
                eprintln!("  {} {w}", yellow("WARN"));
            }

            if result.has_errors() {
                return Err(validation_failed(&topo.lab.name, &result));
            }

            println!("Topology {:?} is valid", topo.lab.name);
            print_topology_summary(&topo);

            if show_ips {
                println!("\n  Addresses:");
                // From links
                for link in &topo.links {
                    if let Some(ref addrs) = link.addresses {
                        for (i, ep_str) in link.endpoints.iter().enumerate() {
                            println!("    {:<24} {} (link)", ep_str, addrs[i]);
                        }
                    }
                }
                // From network ports
                for (net_name, network) in &topo.networks {
                    for member in &network.members {
                        if let Some(ep) = nlink_lab::EndpointRef::parse(member) {
                            // Port keys can be either "node:iface" or "node"
                            let port = network
                                .ports
                                .get(member)
                                .or_else(|| network.ports.get(&ep.node));
                            if let Some(port) = port {
                                for addr in &port.addresses {
                                    println!(
                                        "    {:<24} {} (network {:?})",
                                        member, addr, net_name
                                    );
                                }
                            }
                        }
                    }
                }
                // From node interfaces (loopback, etc.)
                for (name, node) in &topo.nodes {
                    for (iface, cfg) in &node.interfaces {
                        for addr in &cfg.addresses {
                            println!("    {name}:{iface:<18} {addr} (interface)");
                        }
                    }
                }
            }
            Ok(())
        }

        Commands::Test {
            path,
            params,
            junit,
            tap,
            fail_fast,
        } => {
            require_root()?;
            let cli_params: Vec<(String, String)> = params
                .iter()
                .map(|p| {
                    p.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| {
                            nlink_lab::Error::invalid_topology(format!(
                                "invalid --set format: '{p}' (expected KEY=VALUE)"
                            ))
                        })
                })
                .collect::<nlink_lab::Result<Vec<_>>>()?;

            // Collect .nll files
            let files: Vec<PathBuf> = if path.is_dir() {
                let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "nll"))
                    .collect();
                entries.sort();
                entries
            } else {
                vec![path.clone()]
            };

            if files.is_empty() {
                eprintln!("No .nll files found in {}", path.display());
                return Ok(());
            }

            let mut all_results = Vec::new();
            let mut any_failed = false;

            for file in &files {
                eprint!("Testing {} ... ", file.display());
                match nlink_lab::test_runner::run_test_with_params(file, &cli_params).await {
                    Ok(result) => {
                        let pass_count = result.assertions.iter().filter(|a| a.passed).count();
                        let total = result.assertions.len();
                        if result.passed {
                            eprintln!(
                                "{} ({pass_count}/{total} assertions, {}ms)",
                                green("PASS"),
                                result.total_ms
                            );
                        } else {
                            eprintln!(
                                "{} ({pass_count}/{total} assertions, {}ms)",
                                red("FAIL"),
                                result.total_ms
                            );
                            for a in &result.assertions {
                                if !a.passed {
                                    eprintln!(
                                        "  {} {}{}",
                                        red("FAIL"),
                                        a.description,
                                        a.detail
                                            .as_ref()
                                            .map(|d| format!(": {d}"))
                                            .unwrap_or_default()
                                    );
                                }
                            }
                            any_failed = true;
                        }
                        all_results.push(result);
                    }
                    Err(e) => {
                        eprintln!("{}: {e}", red("ERROR"));
                        any_failed = true;
                        if fail_fast {
                            break;
                        }
                    }
                }
                if any_failed && fail_fast {
                    break;
                }
            }

            // Output formats
            if let Some(junit_path) = &junit {
                let xml = nlink_lab::test_runner::format_junit(&all_results);
                std::fs::write(junit_path, &xml)?;
                eprintln!("JUnit results written to {}", junit_path.display());
            }

            if tap {
                print!("{}", nlink_lab::test_runner::format_tap(&all_results));
            }

            if any_failed {
                set_exit_code(EXIT_VALIDATION);
            }
            Ok(())
        }

        Commands::Impair {
            lab,
            endpoint,
            show,
            delay,
            jitter,
            loss,
            rate,
            clear,
            out_delay,
            out_jitter,
            out_loss,
            out_rate,
            in_delay,
            in_jitter,
            in_loss,
            in_rate,
            partition,
            heal,
        } => {
            require_root()?;
            let mut running = nlink_lab::RunningLab::load(&lab)?;

            if show {
                if cli.json {
                    let endpoints = collect_impair_show(&running)?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "lab": running.name(),
                            "endpoints": endpoints,
                        }))?
                    );
                } else {
                    for node_name in running.node_names() {
                        let output = running.exec(node_name, "tc", &["qdisc", "show"])?;
                        if !output.stdout.trim().is_empty() {
                            println!("--- {node_name} ---");
                            println!("{}", output.stdout.trim());
                        }
                    }
                }
                return Ok(());
            }

            let endpoint = endpoint.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("endpoint required (use --show to inspect)")
            })?;

            let report = |action: &str, endpoint: &str, peer: Option<&str>| {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "lab": lab, "endpoint": endpoint, "action": action, "peer": peer })
                    );
                } else {
                    match peer {
                        Some(p) => println!("{action} {endpoint} (via {p})"),
                        None => println!("{action} {endpoint}"),
                    }
                }
            };
            if partition {
                running.partition(&endpoint).await?;
                report("partitioned", &endpoint, None);
            } else if heal {
                running.heal(&endpoint).await?;
                report("healed", &endpoint, None);
            } else if clear {
                running.clear_impairment(&endpoint).await?;
                report("cleared", &endpoint, None);
            } else {
                let has_directional = out_delay.is_some()
                    || out_jitter.is_some()
                    || out_loss.is_some()
                    || out_rate.is_some()
                    || in_delay.is_some()
                    || in_jitter.is_some()
                    || in_loss.is_some()
                    || in_rate.is_some();
                let has_symmetric =
                    delay.is_some() || jitter.is_some() || loss.is_some() || rate.is_some();

                if has_directional && has_symmetric {
                    return Err(nlink_lab::Error::invalid_topology(
                        "cannot mix --delay/--loss with --out-delay/--in-delay",
                    ));
                }

                if has_directional {
                    let egress = nlink_lab::Impairment {
                        delay: out_delay,
                        jitter: out_jitter,
                        loss: out_loss,
                        rate: out_rate,
                        ..Default::default()
                    };
                    let ingress = nlink_lab::Impairment {
                        delay: in_delay,
                        jitter: in_jitter,
                        loss: in_loss,
                        rate: in_rate,
                        ..Default::default()
                    };

                    if egress != nlink_lab::Impairment::default() {
                        running.set_impairment(&endpoint, &egress).await?;
                        report("updated egress impairment on", &endpoint, None);
                    }
                    if ingress != nlink_lab::Impairment::default() {
                        let peer = running.peer_endpoint(&endpoint)?;
                        running.set_impairment(&peer, &ingress).await?;
                        report("updated ingress impairment on", &endpoint, Some(&peer));
                    }
                } else {
                    let impairment = nlink_lab::Impairment {
                        delay,
                        jitter,
                        loss,
                        rate,
                        ..Default::default()
                    };
                    running.set_impairment(&endpoint, &impairment).await?;
                    report("updated impairment on", &endpoint, None);
                }
            }
            Ok(())
        }

        Commands::Scenario { lab, name } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let scenarios = &running.topology().scenarios;
            let Some(name) = name else {
                if json {
                    let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
                    println!("{}", serde_json::to_string_pretty(&names)?);
                } else if scenarios.is_empty() {
                    println!("Lab '{lab}' defines no scenarios.");
                } else {
                    for sc in scenarios {
                        println!("{}  ({} steps)", sc.name, sc.steps.len());
                    }
                }
                return Ok(());
            };
            let scenario = scenarios
                .iter()
                .find(|s| s.name == name)
                .cloned()
                .ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "lab '{lab}' has no scenario '{name}' (run without a name to list them)"
                    ))
                })?;
            require_root()?;
            let result = nlink_lab::scenario::run_scenario(&running, &scenario).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Scenario {:?}: {}",
                    scenario.name,
                    if result.passed {
                        green("PASS")
                    } else {
                        red("FAIL")
                    }
                );
                for (index, step) in result.steps.iter().enumerate() {
                    println!("  t={:>6}ms  step {index}", step.time_ms);
                    for action in &step.actions {
                        let mark = if action.ok { green("ok") } else { red("FAIL") };
                        let detail = action
                            .detail
                            .as_deref()
                            .map(|d| format!(" — {d}"))
                            .unwrap_or_default();
                        println!("    {mark}  {}{detail}", action.description);
                    }
                }
            }
            if !result.passed {
                set_exit_code(EXIT_VALIDATION);
            }
            Ok(())
        }

        Commands::DocsGen { out } => {
            let cmd = Cli::command();
            std::fs::create_dir_all(&out)?;
            let mut written = Vec::new();
            for sub in cmd.get_subcommands() {
                if sub.is_hide_set() {
                    continue;
                }
                let name = sub.get_name().to_string();
                let mut sub = sub.clone().name(clap::builder::Str::from(
                    format!("nlink-lab {name}").leak() as &'static str,
                ));
                let help = sub.render_long_help().to_string();
                // Hand-written prose above the markers is kept; only the
                // block between them is regenerated (CI diffs the result).
                const START: &str =
                    "<!-- cli-ref:start (generated by `nlink-lab docs-gen`; do not edit) -->";
                const END: &str = "<!-- cli-ref:end -->";
                let block = format!("{START}\n\n```text\n{}\n```\n\n{END}\n", help.trim_end());
                let path = out.join(format!("{name}.md"));
                let page = match std::fs::read_to_string(&path) {
                    Ok(existing) if existing.contains(START) && existing.contains(END) => {
                        let a = existing.find(START).unwrap_or(0);
                        let b = existing
                            .find(END)
                            .map(|i| i + END.len())
                            .unwrap_or(existing.len());
                        let tail = existing[b..].trim_start_matches('\n');
                        format!(
                            "{}{}{}",
                            &existing[..a],
                            block,
                            if tail.is_empty() {
                                String::new()
                            } else {
                                format!("\n{tail}")
                            }
                        )
                    }
                    Ok(existing) => format!("{}\n\n## Reference\n\n{block}", existing.trim_end()),
                    Err(_) => format!("# `nlink-lab {name}`\n\n## Reference\n\n{block}"),
                };
                std::fs::write(&path, page)?;
                written.push(name);
            }
            eprintln!("wrote {} pages to {}", written.len(), out.display());
            Ok(())
        }

        Commands::Graph {
            topology,
            params,
            mermaid,
        } => {
            let topo = parse_topology(&topology, &params)?;
            if mermaid {
                print!("{}", topology_to_mermaid(&topo));
            } else {
                print!("{}", topology_to_dot(&topo));
            }
            Ok(())
        }

        Commands::Render {
            topology,
            dot,
            ascii,
            params,
        } => {
            let topo = parse_topology(&topology, &params)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&topo)?);
            } else if dot {
                print!("{}", topology_to_dot(&topo));
            } else if ascii {
                print!("{}", topology_to_ascii(&topo));
            } else {
                print!("{}", nlink_lab::render::try_render(&topo)?);
            }
            Ok(())
        }

        Commands::Shell { lab, node, shell } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            if let Some(container) = running.container_for(&node) {
                let rt = running.runtime_binary().unwrap_or("docker");
                let status = std::process::Command::new(rt)
                    .args(["exec", "-it", &container.id, &shell])
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("exec failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            } else {
                let ns = running.namespace_for(&node)?;
                let args = nsenter_shell_args(ns, &shell);
                let status = std::process::Command::new("nsenter")
                    .args(&args)
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("nsenter failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Commands::Ps { lab, alive_only } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let procs = if alive_only {
                running.process_status_alive_only()
            } else {
                running.process_status()
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&procs)?);
            } else if procs.is_empty() {
                println!("No tracked processes.");
            } else {
                println!("{:<12} {:<8} STATUS", "NODE", "PID");
                for p in &procs {
                    let status = if p.alive { "running" } else { "dead" };
                    println!("{:<12} {:<8} {}", p.node, p.pid, status);
                }
            }
            Ok(())
        }

        Commands::Kill { lab, pid } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            running.kill_process(pid)?;
            println!("Killed process {pid}");
            Ok(())
        }

        Commands::ProcStat {
            lab,
            node,
            pid,
            watch,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            // Single sample = one shot then exit. --watch = NDJSON
            // (or text) stream until Ctrl-C.
            let interval = watch.map(std::time::Duration::from_secs_f64);
            loop {
                let stat = running.proc_stat(&node, pid)?;
                if cli.json {
                    println!("{}", serde_json::to_string(&stat)?);
                } else {
                    println!(
                        "pid={} cmd={} state={} rss={}kB vsz={}kB fds={} \
                         user_ticks={} kernel_ticks={}",
                        stat.host_pid,
                        stat.command,
                        stat.state,
                        stat.rss_kb
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        stat.vsz_kb
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        stat.fd_count,
                        stat.cpu_user_ticks,
                        stat.cpu_kernel_ticks,
                    );
                }
                match interval {
                    Some(d) => tokio::time::sleep(d).await,
                    None => break,
                }
            }
            Ok(())
        }

        Commands::Diagnose { lab, node } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let results = running.diagnose(node.as_deref()).await?;
            if json {
                let json_results: Vec<serde_json::Value> = results.iter().map(|diag| {
                    serde_json::json!({
                        "node": diag.node,
                        "interfaces": diag.interfaces.iter().map(|iface| {
                            serde_json::json!({
                                "name": iface.name,
                                "state": format!("{:?}", iface.state),
                                "mtu": iface.mtu,
                                "rx_bytes": iface.stats.rx_bytes(),
                                "tx_bytes": iface.stats.tx_bytes(),
                                "issues": iface.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                            })
                        }).collect::<Vec<_>>(),
                        "issues": diag.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json_results)?);
            } else {
                for diag in &results {
                    println!("── {} ──", diag.node);
                    for iface in &diag.interfaces {
                        let status = if iface.issues.is_empty() {
                            "OK"
                        } else {
                            "WARN"
                        };
                        println!(
                            "  [{status:<4}] {:<12} state={:<6} mtu={:<5} rx={} tx={}",
                            iface.name,
                            format!("{:?}", iface.state),
                            iface.mtu.unwrap_or(0),
                            iface.stats.rx_bytes(),
                            iface.stats.tx_bytes(),
                        );
                        for issue in &iface.issues {
                            println!("         {issue}");
                        }
                    }
                    for issue in &diag.issues {
                        println!("  [WARN] {issue}");
                    }
                }
            }
            Ok(())
        }

        Commands::Capture {
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
        } => {
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
            let result =
                nlink_lab::capture::run_capture(&ns_name, &config, output, &CAPTURE_SHUTDOWN)?;

            if !cli.quiet {
                eprintln!(
                    "\n{} packets captured ({} received by kernel, {} dropped)",
                    result.packets_captured, result.stats.packets, result.stats.drops,
                );
            }
            Ok(())
        }

        Commands::Diff { a, b, params } => {
            let topo_a = parse_topology(&a, &params)?;
            let topo_b = parse_topology(&b, &params)?;
            let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
            if json {
                // For JSON, output a simple summary
                println!(
                    "{}",
                    serde_json::json!({
                        "nodes_added": diff.nodes_added,
                        "nodes_removed": diff.nodes_removed,
                        "links_added": diff.links_added.len(),
                        "links_removed": diff.links_removed.len(),
                        "impairments_changed": diff.impairments_changed.len(),
                        "impairments_added": diff.impairments_added.len(),
                        "impairments_removed": diff.impairments_removed.len(),
                        "total_changes": diff.change_count(),
                    })
                );
            } else if diff.is_empty() {
                println!("No differences.");
            } else {
                println!("Diff: {} → {}", a.display(), b.display());
                print!("{diff}");
                println!("\n{} change(s)", diff.change_count());
            }
            Ok(())
        }

        Commands::Export {
            lab,
            output,
            archive,
            include_running_state,
            no_rendered,
            set_params,
        } => {
            if archive {
                use nlink_lab::portability::{ArchiveSource, ExportOptions, export_archive};
                let lab_path = std::path::Path::new(&lab);
                let source = if lab_path.extension().and_then(|s| s.to_str()) == Some("nll")
                    || lab_path.exists()
                {
                    ArchiveSource::Nll {
                        path: lab_path.into(),
                    }
                } else {
                    ArchiveSource::Lab { name: lab.clone() }
                };

                let params: Vec<(String, String)> = set_params
                    .iter()
                    .map(|p| {
                        let (k, v) = p.split_once('=').ok_or_else(|| {
                            nlink_lab::Error::invalid_topology(format!(
                                "invalid --set format: '{p}' (expected KEY=VALUE)"
                            ))
                        })?;
                        Ok((k.to_string(), v.to_string()))
                    })
                    .collect::<nlink_lab::Result<Vec<_>>>()?;

                let out_path = output.unwrap_or_else(|| {
                    let basename = match &source {
                        ArchiveSource::Lab { name } => name.clone(),
                        ArchiveSource::Nll { path } => path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("lab")
                            .to_string(),
                    };
                    PathBuf::from(format!("{basename}.nlz"))
                });

                let opts = ExportOptions {
                    include_running_state,
                    no_rendered,
                    params,
                };
                export_archive(source, &out_path, opts)?;
                if !quiet {
                    eprintln!("Archive written to {}", out_path.display());
                }
            } else {
                let running = nlink_lab::RunningLab::load(&lab)?;
                let content = if json {
                    serde_json::to_string_pretty(running.topology())?
                } else {
                    toml::to_string_pretty(running.topology()).map_err(|e| {
                        nlink_lab::Error::invalid_topology(format!("serialize: {e}"))
                    })?
                };
                match output {
                    Some(path) => {
                        std::fs::write(&path, &content)?;
                        if !quiet {
                            eprintln!("Exported to {}", path.display());
                        }
                    }
                    None => print!("{content}"),
                }
            }
            Ok(())
        }

        Commands::Import {
            archive,
            dir,
            no_deploy,
            no_reparse,
        } => {
            use nlink_lab::portability::import_archive;
            let report = import_archive(&archive, dir.as_deref(), no_reparse)?;
            if !quiet {
                eprintln!(
                    "Extracted lab '{}' to {} (format v{}, exported by {})",
                    report.manifest.lab_name,
                    report.extracted_to.display(),
                    report.manifest.format_version,
                    report.manifest.exported_by,
                );
            }
            if no_deploy {
                if !quiet {
                    eprintln!("(--no-deploy: skipping deploy)");
                }
                return Ok(());
            }
            // Deploy the imported topology. We re-read the extracted
            // topology.nll so the import path matches what `deploy`
            // would do for a regular file.
            let topology_path = report.extracted_to.join("topology.nll");
            let topo = nlink_lab::parser::parse_file(&topology_path)?;
            let lab = topo.deploy().await?;
            if !quiet {
                eprintln!("Deployed lab '{}'", lab.name());
            }
            Ok(())
        }

        Commands::Daemon {
            lab,
            interval,
            zenoh_mode,
            zenoh_listen,
            zenoh_connect,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            // The real backend (nlink-lab-backend as a library): RPC
            // queryables, events, sockdiag flows — the CLI used to run a
            // private fork that ignored every flag below (#43).
            let opts = nlink_lab_backend::BackendOpts {
                interval: std::time::Duration::from_secs(interval),
                zenoh_mode: zenoh_mode.parse()?,
                zenoh_listen: zenoh_listen.into_iter().collect(),
                zenoh_connect: zenoh_connect.into_iter().collect(),
            };
            if !quiet {
                println!(
                    "Starting Zenoh backend for lab '{}' ({} nodes, every {}s)",
                    lab,
                    running.namespace_count(),
                    interval
                );
            }
            nlink_lab_backend::run(running, opts).await?;
            Ok(())
        }

        Commands::Metrics {
            lab,
            node,
            format: fmt,
            count,
            zenoh_connect,
        } => {
            let mut zenoh_config = zenoh::Config::default();
            if let Some(connect) = &zenoh_connect {
                zenoh_config
                    .insert_json5("connect/endpoints", &format!(r#"["{connect}"]"#))
                    .map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("bad zenoh config: {e}"))
                    })?;
            }

            let session = zenoh::open(zenoh_config).await.map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("failed to open Zenoh session: {e}"))
            })?;

            let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
            let subscriber = session.declare_subscriber(&topic).await.map_err(|e| {
                nlink_lab::Error::deploy_failed(format!("subscribe to '{topic}': {e}"))
            })?;

            eprintln!("Subscribing to metrics for lab '{lab}'... (Ctrl-C to stop)");

            let mut samples = 0usize;
            loop {
                tokio::select! {
                    Ok(sample) = subscriber.recv_async() => {
                        let payload = sample.payload().to_bytes();
                        if let Ok(snapshot) = serde_json::from_slice::<nlink_lab_shared::metrics::MetricsSnapshot>(&payload) {
                            samples += 1;

                            if fmt == MetricsFormat::Json || json {
                                println!("{}", serde_json::to_string(&snapshot).unwrap_or_default());
                            } else {
                                // Clear screen for table mode
                                print!("\x1B[2J\x1B[H");
                                println!(
                                    "lab: {}  |  nodes: {}  |  sample: #{}",
                                    snapshot.lab_name,
                                    snapshot.nodes.len(),
                                    samples,
                                );
                                println!();
                                println!(
                                    "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>8}",
                                    "NODE", "IFACE", "STATE", "RX rate", "TX rate", "ERRORS", "DROPS"
                                );
                                println!("{}", "─".repeat(78));

                                let mut node_names: Vec<&String> = snapshot.nodes.keys().collect();
                                node_names.sort();
                                for node_name in node_names {
                                    if let Some(filter) = &node
                                        && node_name != filter { continue; }
                                    let metrics = &snapshot.nodes[node_name];
                                    for iface in &metrics.interfaces {
                                        let errors = iface.rx_errors + iface.tx_errors;
                                        let drops = iface.rx_dropped + iface.tx_dropped + iface.tc_drops;
                                        let drop_warn = if drops > 0 { " !" } else { "" };
                                        println!(
                                            "{:<12} {:<10} {:<6} {:>12} {:>12} {:>8} {:>7}{}",
                                            node_name,
                                            iface.name,
                                            iface.state,
                                            nlink_lab_shared::metrics::format_rate(iface.rx_bps),
                                            nlink_lab_shared::metrics::format_rate(iface.tx_bps),
                                            errors,
                                            drops,
                                            drop_warn,
                                        );
                                    }
                                    for issue in &metrics.issues {
                                        println!("  [WARN] {node_name}: {issue}");
                                    }
                                    // Top TCP flows by goodput (nlink 0.24 sockdiag),
                                    // published by the backend collector.
                                    for s in &metrics.sockets {
                                        println!(
                                            "  {:<20} {} -> {}  tx {}  rx {}{}",
                                            format!("{} (pid {})", s.comm, s.pid.map_or_else(|| "-".to_string(), |p| p.to_string())),
                                            s.local,
                                            s.remote,
                                            nlink_lab_shared::metrics::format_rate(s.tx_bytes_per_sec * 8),
                                            nlink_lab_shared::metrics::format_rate(s.rx_bytes_per_sec * 8),
                                            if s.retrans_ratio > 0.0 {
                                                format!("  retr {:.1}%", s.retrans_ratio * 100.0)
                                            } else {
                                                String::new()
                                            },
                                        );
                                    }
                                }
                            }

                            if let Some(max) = count
                                && samples >= max {
                                    break;
                                }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            }
            Ok(())
        }

        Commands::Init {
            template,
            list,
            output,
            name,
            force,
        } => {
            if list || template.is_none() {
                println!(
                    "{:<15} {:<5} {:<5} DESCRIPTION",
                    "TEMPLATE", "NODES", "LINKS"
                );
                println!("{}", "─".repeat(70));
                for t in nlink_lab::templates::list() {
                    println!(
                        "{:<15} {:<5} {:<5} {}",
                        t.name, t.node_count, t.link_count, t.description
                    );
                }
                return Ok(());
            }

            let template_name = template.unwrap();
            let t = nlink_lab::templates::get(&template_name).ok_or_else(|| {
                nlink_lab::Error::invalid_topology(format!(
                    "unknown template '{template_name}'. Use --list to see available templates"
                ))
            })?;

            let nll_content = nlink_lab::templates::render(t, name.as_deref());
            let out_dir = output.unwrap_or_else(|| PathBuf::from("."));
            let lab_name = name.as_deref().unwrap_or(t.name);

            let path = out_dir.join(format!("{lab_name}.nll"));
            if path.exists() && !force {
                return Err(nlink_lab::Error::AlreadyExists {
                    name: format!("{} (use --force to overwrite)", path.display()),
                });
            }
            std::fs::write(&path, &nll_content)?;
            println!(
                "Created {} ({} nodes, {} links)",
                path.display(),
                t.node_count,
                t.link_count
            );

            Ok(())
        }

        Commands::Watch {
            lab,
            family,
            node,
            include_snapshot,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let opts = nlink_lab::WatchOpts {
                family: family.into(),
                json,
                node,
                include_snapshot,
            };
            nlink_lab::watch_loop(&running, opts).await?;
            Ok(())
        }

        Commands::Wait { name, timeout } => {
            let start = Instant::now();
            let deadline = start + std::time::Duration::from_secs(timeout);
            eprint!("Waiting for lab '{name}'...");
            loop {
                if nlink_lab::state::exists(&name) {
                    eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    eprintln!(" timeout after {timeout}s");
                    return Err(nlink_lab::Error::invalid_topology(format!(
                        "timeout waiting for lab '{name}' after {timeout}s"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        Commands::WaitFor {
            lab,
            node,
            tcp,
            exec_cmd,
            file,
            timeout,
            interval,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let timeout = std::time::Duration::from_secs(timeout);
            let interval = std::time::Duration::from_millis(interval);

            let result = if let Some(ref tcp_addr) = tcp {
                let (ip, port) = if let Some((ip, port_str)) = tcp_addr.rsplit_once(':') {
                    (
                        ip.to_string(),
                        port_str.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                } else {
                    (
                        "127.0.0.1".to_string(),
                        tcp_addr.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                };
                running
                    .wait_for_tcp(&node, &ip, port, timeout, interval)
                    .await
            } else if let Some(ref cmd) = exec_cmd {
                running.wait_for_exec(&node, cmd, timeout, interval).await
            } else if let Some(ref path) = file {
                running.wait_for_file(&node, path, timeout, interval).await
            } else {
                return Err(nlink_lab::Error::invalid_topology(
                    "one of --tcp, --exec, or --file is required".to_string(),
                ));
            };

            match result {
                Ok(()) => {
                    if !cli.quiet {
                        eprintln!("ready");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }

        Commands::Ip {
            lab,
            node,
            iface,
            cidr,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let addrs = running.node_addresses(&node)?;

            if let Some(ref iface_name) = iface {
                let iface_addrs = addrs.get(iface_name).ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "interface '{iface_name}' not found on node '{node}'"
                    ))
                })?;

                if json {
                    println!("{}", serde_json::to_string_pretty(&iface_addrs)?);
                } else if let Some(first) = iface_addrs.first() {
                    if cidr {
                        println!("{first}");
                    } else {
                        println!("{}", first.split('/').next().unwrap_or(first));
                    }
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&addrs)?);
            } else {
                for (iface_name, iface_addrs) in &addrs {
                    for addr in iface_addrs {
                        if cidr {
                            println!("{iface_name}: {addr}");
                        } else {
                            println!("{iface_name}: {}", addr.split('/').next().unwrap_or(addr));
                        }
                    }
                }
            }
            Ok(())
        }

        Commands::Inspect { lab } => {
            // If the argument looks like a `.nlz` archive path, do
            // archive inspection instead of lab inspection.
            let lab_path = std::path::Path::new(&lab);
            if lab.ends_with(".nlz") || (lab_path.exists() && !nlink_lab::state::exists(&lab)) {
                use nlink_lab::portability::inspect_archive;
                let summary = inspect_archive(lab_path)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                } else {
                    let m = &summary.manifest;
                    println!("Archive:       {}", lab_path.display());
                    println!("Lab:           {}", m.lab_name);
                    println!("Format:        v{}", m.format_version);
                    println!("Exported by:   {} on {}", m.exported_by, m.exported_at);
                    println!(
                        "Platform:      {} {} / {}",
                        m.platform.os, m.platform.kernel, m.platform.arch,
                    );
                    println!("State:         {:?}", m.deploy_state);
                    if let Some(n) = summary.node_count {
                        println!("Nodes:         {n}");
                    }
                    if let Some(n) = summary.link_count {
                        println!("Links:         {n}");
                    }
                    if let Some(n) = summary.network_count {
                        println!("Networks:      {n}");
                    }
                    println!("Files:");
                    println!("  topology:    {}", m.files.topology);
                    if let Some(f) = &m.files.params {
                        println!("  params:      {f}");
                    }
                    if let Some(f) = &m.files.rendered {
                        println!("  rendered:    {f}");
                    }
                    if let Some(f) = &m.files.state {
                        println!("  state:       {f}");
                    }
                }
                return Ok(());
            }

            let running = nlink_lab::RunningLab::load(&lab)?;
            let topo = running.topology();

            if json {
                println!("{}", serde_json::to_string_pretty(topo)?);
                return Ok(());
            }

            // Header
            println!("{}", bold(&format!("Lab: {}", running.name())));
            println!(
                "Nodes: {}  Links: {}  Impairments: {}",
                running.namespace_count(),
                topo.links.len(),
                topo.impairments.len()
            );

            // Node table
            println!(
                "\n  {:<20} {:<12} {}",
                bold("NODE"),
                bold("TYPE"),
                bold("IMAGE")
            );
            let mut names: Vec<&String> = topo.nodes.keys().collect();
            names.sort();
            for name in &names {
                let node = &topo.nodes[*name];
                let kind = if node.image.is_some() {
                    "container"
                } else {
                    "namespace"
                };
                let image = node.image.as_deref().unwrap_or("--");
                println!("  {:<20} {:<12} {}", name, kind, image);
            }

            // Links
            if !topo.links.is_empty() {
                println!("\n  {:<40} {}", bold("LINK"), bold("ADDRESSES"));
                for link in &topo.links {
                    let addrs = link
                        .addresses
                        .as_ref()
                        .map(|a| format!("{} -- {}", a[0], a[1]))
                        .unwrap_or_else(|| "--".to_string());
                    println!(
                        "  {:<40} {}",
                        format!("{} -- {}", link.endpoints[0], link.endpoints[1]),
                        addrs
                    );
                }
            }

            // Impairments
            if !topo.impairments.is_empty() {
                println!("\n  {}", bold("IMPAIRMENTS"));
                for (ep, imp) in &topo.impairments {
                    let mut parts = Vec::new();
                    if let Some(d) = &imp.delay {
                        parts.push(format!("delay={d}"));
                    }
                    if let Some(j) = &imp.jitter {
                        parts.push(format!("jitter={j}"));
                    }
                    if let Some(l) = &imp.loss {
                        parts.push(format!("loss={l}"));
                    }
                    if let Some(r) = &imp.rate {
                        parts.push(format!("rate={r}"));
                    }
                    println!("  {:<24} {}", ep, parts.join("  "));
                }
            }

            // Processes
            let procs: Vec<_> = running
                .process_status()
                .into_iter()
                .filter(|p| p.alive)
                .collect();
            if !procs.is_empty() {
                println!("\n  {}", bold("PROCESSES"));
                for p in &procs {
                    println!("  {:<16} pid={}", p.node, p.pid);
                }
            }

            Ok(())
        }

        Commands::Containers { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if json {
                // `[]` when there are none — never prose under --json (#46)
                let mut data: Vec<serde_json::Value> = containers.iter().map(|(name, state)| {
                    serde_json::json!({ "node": name, "image": state.image, "id": state.id, "pid": state.pid })
                }).collect();
                data.sort_by_key(|v| v["node"].as_str().unwrap_or_default().to_string());
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else if containers.is_empty() {
                println!("No container nodes in lab '{lab}'.");
            } else {
                println!(
                    "  {:<16} {:<20} {:<14} PID",
                    "NODE", "IMAGE", "CONTAINER ID"
                );
                let mut entries: Vec<_> = containers.iter().collect();
                entries.sort_by_key(|(name, _)| (*name).clone());
                for (name, state) in entries {
                    let short_id = if state.id.len() > 12 {
                        &state.id[..12]
                    } else {
                        &state.id
                    };
                    println!(
                        "  {:<16} {:<20} {:<14} {}",
                        name, state.image, short_id, state.pid
                    );
                }
            }
            Ok(())
        }

        Commands::Logs {
            lab,
            node,
            pid,
            stderr,
            follow,
            tail,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;

            // Process logs mode (--pid)
            if let Some(pid) = pid {
                let (stdout_path, stderr_path) = running.log_paths(pid).ok_or_else(|| {
                    nlink_lab::Error::deploy_failed(format!("no log files found for PID {pid}"))
                })?;
                let path = std::path::Path::new(if stderr { stderr_path } else { stdout_path });
                // `--tail N` reads only the tail of the file (a service log
                // can be gigabytes); without it the whole file is streamed.
                let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let initial: String = match tail {
                    Some(n) => tail_lines(path, n as usize).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
                    })?,
                    None => std::fs::read_to_string(path).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
                    })?,
                };
                if !initial.is_empty() {
                    print!("{initial}");
                    if !initial.ends_with('\n') {
                        println!();
                    }
                }
                if follow {
                    // tail -F semantics: resume reading from current EOF,
                    // poll, and reopen if the file is rotated/truncated.
                    tail_follow(path, file_len)?;
                }
                return Ok(());
            }

            // Container logs mode (node name)
            let node = node.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("either a node name or --pid is required")
            })?;
            let container = running.container_for(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' is not a container. Logs are only available for container nodes."
                ))
            })?;
            let rt = running.runtime_binary().unwrap_or("docker");
            let mut args = vec!["logs".to_string()];
            if follow {
                args.push("--follow".to_string());
            }
            if let Some(n) = tail {
                args.push("--tail".to_string());
                args.push(n.to_string());
            }
            args.push(container.id.clone());
            let status = std::process::Command::new(rt)
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("logs failed: {e}")))?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }

        Commands::Pull { topology, params } => {
            let topo = parse_topology(&topology, &params)?;
            let images: std::collections::BTreeSet<&str> = topo
                .nodes
                .values()
                .filter_map(|n| n.image.as_deref())
                .collect();
            if images.is_empty() {
                println!("No container images in topology.");
            } else {
                let rt = nlink_lab::container::Runtime::detect()?;
                for image in &images {
                    eprint!("Pulling {image}...");
                    rt.pull_image(image)?;
                    eprintln!(" done");
                }
                println!("{} image(s) pulled", images.len());
            }
            Ok(())
        }

        Commands::Stats { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if containers.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("No container nodes in lab '{lab}'.");
                }
                return Ok(());
            }
            let rt = running.runtime_binary().unwrap_or("docker");
            let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
            let format = if json {
                "{{json .}}"
            } else {
                "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}"
            };
            let output = std::process::Command::new(rt)
                .args(["stats", "--no-stream", "--format", format])
                .args(&ids)
                .output()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("{rt} stats failed: {e}")))?;
            if !output.status.success() {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "{rt} stats exited with {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            if json {
                // docker/podman emit one JSON object per line; wrap as an array
                let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<std::result::Result<_, _>>()?;
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            Ok(())
        }

        Commands::Restart { lab, node } => {
            require_root()?;
            // Same per-lab flock deploy/destroy take: the PID refresh
            // below rewrites state.json and must not race an `apply`.
            let _lock = nlink_lab::state::lock(&lab)?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let container = running
                .container_for(&node)
                .cloned()
                .ok_or_else(|| {
                    nlink_lab::Error::deploy_failed(format!(
                        "node '{node}' is not a container. Restart is only available for container nodes."
                    ))
                })?;
            // Issue #31: `docker restart` recreates the container's
            // network namespace, so every veth the deployer moved into
            // it is gone afterwards and nothing here can put it back.
            // Refuse up front instead of leaving a half-broken node.
            let links = node_link_count(running.topology(), &node);
            if links > 0 {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' has {links} link(s); restarting would drop its veths \
                     — destroy and redeploy, or use apply (issue #31)"
                )));
            }
            let rt = nlink_lab::container::Runtime::with_binary(
                running.runtime_binary().unwrap_or("docker"),
            );
            eprint!("Restarting '{node}'...");
            let status = std::process::Command::new(rt.binary())
                .args(["restart", &container.id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("restart failed: {e}")))?;
            if !status.success() {
                eprintln!(" failed");
                std::process::exit(1);
            }
            // The persisted init PID died with the old container process;
            // re-read it (a `.State.Pid` of 0 is rejected by `inspect_pid`)
            // so later `/proc/<pid>/ns/net` references stay valid.
            let pid = rt.inspect_pid(&container.id).map_err(|e| {
                eprintln!(" failed");
                nlink_lab::Error::deploy_failed(format!(
                    "'{node}' was restarted but is not running: {e}"
                ))
            })?;
            // `RunningLab::save_state` persists only pids / impairments /
            // process logs and the container map has no public mutator,
            // so the new PID goes through the state module directly.
            let (mut lab_state, topo) = nlink_lab::state::load(&lab)?;
            let entry = lab_state.containers.get_mut(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "state.json for lab '{lab}' no longer lists container '{node}'"
                ))
            })?;
            let old_pid = entry.pid;
            entry.pid = pid;
            nlink_lab::state::save(&lab_state, &topo)?;
            eprintln!(" done (pid {old_pid} -> {pid})");
            Ok(())
        }

        Commands::Completions { .. } => {
            // Already handled before async runtime
            Ok(())
        }
    }
}

fn print_topology_summary(topo: &nlink_lab::Topology) {
    println!("  Nodes:       {}", topo.nodes.len());
    println!("  Links:       {}", topo.links.len());
    println!("  Profiles:    {}", topo.profiles.len());
    println!("  Networks:    {}", topo.networks.len());
    println!("  Impairments: {}", topo.impairments.len());
    println!("  Rate limits: {}", topo.rate_limits.len());
}

fn print_deploy_summary(topo: &nlink_lab::Topology) {
    let node_names: Vec<&str> = topo.nodes.keys().map(|s| s.as_str()).collect();
    println!("  Nodes:       {}", node_names.join(", "));
    println!("  Links:       {} point-to-point", topo.links.len());
    if !topo.impairments.is_empty() {
        println!("  Impairments: {}", topo.impairments.len());
    }
    let bg_count: usize = topo
        .nodes
        .values()
        .flat_map(|n| &n.exec)
        .filter(|e| e.background)
        .count();
    if bg_count > 0 {
        println!("  Processes:   {} background", bg_count);
    }
}

fn topology_to_dot(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;

    let mut out = format!("graph {:?} {{\n", topo.lab.name);
    out += "  rankdir=LR;\n";
    out += "  node [shape=box];\n";

    for link in &topo.links {
        let (Some(a), Some(b)) = (
            EndpointRef::parse(&link.endpoints[0]),
            EndpointRef::parse(&link.endpoints[1]),
        ) else {
            continue;
        };

        let mut label_parts = Vec::new();
        if let Some(addrs) = &link.addresses {
            label_parts.push(format!("{} / {}", addrs[0], addrs[1]));
        }
        if let Some(mtu) = link.mtu {
            label_parts.push(format!("MTU {mtu}"));
        }
        // Check for impairment
        if let Some(imp) = topo.impairments.get(&link.endpoints[0]) {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss={l}"));
            }
            if !parts.is_empty() {
                label_parts.push(parts.join(" "));
            }
        }

        let label = label_parts.join("\\n");
        if label.is_empty() {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface
            );
        } else {
            out += &format!(
                "  \"{}\" -- \"{}\" [taillabel=\"{}\", headlabel=\"{}\", label=\"{}\"];\n",
                a.node, b.node, a.iface, b.iface, label
            );
        }
    }

    // Bridge networks: one ellipse per network, an edge per member,
    // per-pair impairments as dashed labelled edges (#47).
    let mut net_names: Vec<&String> = topo.networks.keys().collect();
    net_names.sort();
    for name in net_names {
        let net = &topo.networks[name];
        let label = match &net.subnet {
            Some(sn) => format!("{name}\\n{sn}"),
            None => name.clone(),
        };
        out += &format!("  \"net:{name}\" [shape=ellipse, label=\"{label}\"];\n");
        for member in &net.members {
            if let Some(ep) = EndpointRef::parse(member) {
                let addr = net
                    .ports
                    .get(member)
                    .and_then(|p| p.addresses.first())
                    .map(|a| format!(", label=\"{a}\""))
                    .unwrap_or_default();
                out += &format!(
                    "  \"{}\" -- \"net:{name}\" [taillabel=\"{}\"{addr}];\n",
                    ep.node, ep.iface
                );
            }
        }
        for imp in &net.impairments {
            let mut parts = Vec::new();
            if let Some(d) = &imp.impairment.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(l) = &imp.impairment.loss {
                parts.push(format!("loss={l}"));
            }
            if let Some(r) = &imp.rate_cap {
                parts.push(format!("cap={r}"));
            }
            out += &format!(
                "  \"{}\" -> \"{}\" [style=dashed, color=gray, label=\"{}\"];\n",
                imp.src,
                imp.dst,
                parts.join(" ")
            );
        }
    }

    out += "}\n";
    out
}

/// Mermaid `graph LR` rendering (renders inline in Forgejo/GitHub markdown).
fn topology_to_mermaid(topo: &nlink_lab::Topology) -> String {
    use nlink_lab::EndpointRef;
    fn id(s: &str) -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
    let mut out = String::from("graph LR\n");
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    for n in nodes {
        let kind = if topo.nodes[n].image.is_some() {
            "[["
        } else {
            "["
        };
        let close = if topo.nodes[n].image.is_some() {
            "]]"
        } else {
            "]"
        };
        out += &format!("  {}{kind}\"{n}\"{close}\n", id(n));
    }
    for link in &topo.links {
        let (Some(a), Some(b)) = (
            EndpointRef::parse(&link.endpoints[0]),
            EndpointRef::parse(&link.endpoints[1]),
        ) else {
            continue;
        };
        let mut label = format!("{} — {}", a.iface, b.iface);
        if let Some(addrs) = &link.addresses {
            label = format!("{label}<br/>{} / {}", addrs[0], addrs[1]);
        }
        if let Some(imp) = topo.impairments.get(&link.endpoints[0]) {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay {d}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss {l}"));
            }
            if !parts.is_empty() {
                label = format!("{label}<br/>{}", parts.join(", "));
            }
        }
        out += &format!("  {} ---|\"{label}\"| {}\n", id(&a.node), id(&b.node));
    }
    let mut net_names: Vec<&String> = topo.networks.keys().collect();
    net_names.sort();
    for name in net_names {
        let net = &topo.networks[name];
        let label = match &net.subnet {
            Some(sn) => format!("{name}<br/>{sn}"),
            None => name.clone(),
        };
        out += &format!("  net_{}((\"{label}\"))\n", id(name));
        for member in &net.members {
            if let Some(ep) = EndpointRef::parse(member) {
                out += &format!(
                    "  {} ---|\"{}\"| net_{}\n",
                    id(&ep.node),
                    ep.iface,
                    id(name)
                );
            }
        }
        for imp in &net.impairments {
            let mut parts = Vec::new();
            if let Some(d) = &imp.impairment.delay {
                parts.push(format!("delay {d}"));
            }
            if let Some(l) = &imp.impairment.loss {
                parts.push(format!("loss {l}"));
            }
            out += &format!(
                "  {} -.->|\"{}\"| {}\n",
                id(&imp.src),
                parts.join(", "),
                id(&imp.dst)
            );
        }
    }
    out
}

fn topology_to_ascii(topo: &nlink_lab::Topology) -> String {
    use std::collections::HashSet;

    let mut out = String::new();
    out.push_str(&format!("Lab: {}\n", topo.lab.name));
    if let Some(desc) = &topo.lab.description {
        out.push_str(&format!("  {desc}\n"));
    }
    out.push('\n');

    out.push_str("Nodes:\n");
    let mut nodes: Vec<&String> = topo.nodes.keys().collect();
    nodes.sort();
    for name in &nodes {
        let node = &topo.nodes[*name];
        let kind = if node.image.is_some() {
            " [container]"
        } else {
            ""
        };
        out.push_str(&format!("  {name}{kind}\n"));
    }

    out.push_str("\nLinks:\n");
    let mut shown: HashSet<String> = HashSet::new();
    for link in &topo.links {
        let key = format!("{} -- {}", link.endpoints[0], link.endpoints[1]);
        if shown.insert(key.clone()) {
            let mut parts = vec![format!("  {}", key)];
            if let Some(addrs) = &link.addresses {
                parts.push(format!("{} -- {}", addrs[0], addrs[1]));
            }
            if let Some(mtu) = link.mtu {
                parts.push(format!("mtu={mtu}"));
            }
            out.push_str(&format!("{}\n", parts.join("  ")));
        }
    }

    if !topo.networks.is_empty() {
        out.push_str("\nNetworks:\n");
        let mut names: Vec<&String> = topo.networks.keys().collect();
        names.sort();
        for name in names {
            let net = &topo.networks[name];
            let subnet = net
                .subnet
                .as_deref()
                .map(|s| format!("  subnet={s}"))
                .unwrap_or_default();
            out.push_str(&format!("  {name}{subnet}\n"));
            for member in &net.members {
                let addr = net
                    .ports
                    .get(member)
                    .and_then(|p| p.addresses.first())
                    .map(|a| format!("  {a}"))
                    .unwrap_or_default();
                out.push_str(&format!("    {member}{addr}\n"));
            }
            for imp in &net.impairments {
                let mut parts = Vec::new();
                if let Some(d) = &imp.impairment.delay {
                    parts.push(format!("delay={d}"));
                }
                if let Some(l) = &imp.impairment.loss {
                    parts.push(format!("loss={l}"));
                }
                if let Some(r) = &imp.rate_cap {
                    parts.push(format!("rate-cap={r}"));
                }
                out.push_str(&format!(
                    "    {} -> {}  {}\n",
                    imp.src,
                    imp.dst,
                    parts.join(" ")
                ));
            }
        }
    }

    if !topo.assertions.is_empty() {
        out.push_str("\nAssertions:\n");
        for a in &topo.assertions {
            match a {
                nlink_lab::types::Assertion::Reach { from, to } => {
                    out.push_str(&format!("  reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::NoReach { from, to } => {
                    out.push_str(&format!("  no-reach {from} -> {to}\n"));
                }
                nlink_lab::types::Assertion::TcpConnect {
                    from,
                    to,
                    port,
                    timeout,
                    retries,
                    interval,
                } => {
                    let t = timeout
                        .as_deref()
                        .map(|t| format!(" timeout {t}"))
                        .unwrap_or_default();
                    let r = retries.map(|r| format!(" retries {r}")).unwrap_or_default();
                    let i = interval
                        .as_deref()
                        .map(|i| format!(" interval {i}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  tcp-connect {from} -> {to}:{port}{t}{r}{i}\n"));
                }
                nlink_lab::types::Assertion::LatencyUnder {
                    from,
                    to,
                    max,
                    samples,
                } => {
                    let s = samples.map(|s| format!(" samples {s}")).unwrap_or_default();
                    out.push_str(&format!("  latency-under {from} -> {to} < {max}{s}\n"));
                }
                nlink_lab::types::Assertion::RouteHas {
                    node,
                    destination,
                    via,
                    dev,
                } => {
                    let v = via
                        .as_deref()
                        .map(|v| format!(" via {v}"))
                        .unwrap_or_default();
                    let d = dev
                        .as_deref()
                        .map(|d| format!(" dev {d}"))
                        .unwrap_or_default();
                    out.push_str(&format!("  route-has {node} {destination}{v}{d}\n"));
                }
                nlink_lab::types::Assertion::DnsResolves {
                    from,
                    name,
                    expected_ip,
                } => {
                    out.push_str(&format!("  dns-resolves {from} {name} -> {expected_ip}\n"));
                }
            }
        }
    }

    out
}

const CAP_NET_ADMIN: u64 = 12;
const CAP_SYS_ADMIN: u64 = 21;

/// Whether a `CapEff` bitmask (hex, as in `/proc/self/status`) grants
/// both capabilities nlink-lab needs.
fn cap_eff_has_net_and_sys_admin(hex: &str) -> bool {
    u64::from_str_radix(hex.trim(), 16)
        .map(|bits| bits & (1 << CAP_NET_ADMIN) != 0 && bits & (1 << CAP_SYS_ADMIN) != 0)
        .unwrap_or(false)
}

/// Fail fast, before any namespace/netlink work, unless this process is
/// root or holds CAP_NET_ADMIN *and* CAP_SYS_ADMIN. The old check only
/// warned and accepted any capability bit at all (#44).
fn require_root() -> nlink_lab::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }
    let ok = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("CapEff:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .map(cap_eff_has_net_and_sys_admin)
            })
        })
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(nlink_lab::Error::deploy_failed(
            "this command needs root (sudo), a SUID binary, or CAP_NET_ADMIN+CAP_SYS_ADMIN \
             (see `just install-caps`)",
        ))
    }
}

/// Best-effort cleanup when state is missing: delete this lab's namespaces
/// and its root-namespace mgmt bridge/veth peers.
///
/// Namespaces are matched by the `{name}-` prefix *and* must not carry a
/// `netns_tag` for a different lab — `destroy simple --force` must not take
/// "simple-2-router" down with it. Untagged prefix matches are still reaped
/// here (unlike `--orphans`) because the caller named the lab explicitly and
/// deploys that predate tagging left nothing else to go on.
async fn force_cleanup(name: &str) {
    let prefix = format!("{name}-");
    match nlink::netlink::namespace::list() {
        Ok(all) => {
            for ns in all.iter().filter(|ns| ns.starts_with(&prefix)) {
                if let Some(owner) = nlink_lab::netns_tag::lab_of(ns)
                    && owner != name
                {
                    eprintln!("  skipped namespace '{ns}' (owned by lab '{owner}')");
                    continue;
                }
                match nlink::netlink::namespace::delete(ns) {
                    Ok(()) => {
                        nlink_lab::netns_tag::untag(ns);
                        eprintln!("  deleted namespace '{ns}'");
                    }
                    Err(e) => eprintln!("  warning: failed to delete namespace '{ns}': {e}"),
                }
            }
        }
        Err(e) => eprintln!("  warning: failed to list namespaces: {e}"),
    }

    // Clean up root-namespace mgmt veth peers first (may be orphaned if bridge
    // was already deleted or namespaces were deleted before the bridge).
    // Veth peers are named nm{hash8}{idx} — same hash as the bridge
    // (`nl{hash8}`), so strip the "nl" prefix.
    let bridge_name = nlink_lab::mgmt_bridge_name_for(name);
    let veth_prefix = format!("nm{}", &bridge_name[2..]);
    match nlink::Connection::<nlink::Route>::new() {
        Ok(conn) => {
            for ifname in list_ip_links_with(&conn).await {
                if ifname.starts_with(veth_prefix.as_str()) {
                    let _ = conn.del_link_if_exists(ifname.as_str()).await;
                }
            }
            if let Ok(true) = conn.del_link_if_exists(bridge_name.as_str()).await {
                eprintln!("  deleted mgmt bridge '{bridge_name}'");
            }
        }
        Err(e) => eprintln!("  warning: cannot open netlink socket: {e}"),
    }

    // Also clean up state directory
    let _ = nlink_lab::state::remove(name);
}

/// Resources on the host that look like lab-owned state but have no matching
/// `state.json` — usually left behind by a crashed deploy.
#[derive(Debug, Default, serde::Serialize)]
struct Orphans {
    /// Root-namespace mgmt bridges (`nl{hash8}`).
    bridges: Vec<String>,
    /// Root-namespace mgmt veth peers (`nm{hash8}{idx}`).
    veths: Vec<String>,
    /// Named network namespaces tagged by nlink-lab (`netns_tag`) whose
    /// owning lab is not registered, or is registered but no longer
    /// claims them in `state.json`.
    netns: Vec<String>,
    /// Namespaces on the host without an nlink-lab ownership tag. Never
    /// listed and never touched (#29 — they belong to libvirt, podman,
    /// CNI, ...); surfaced only as a count for `status --scan -v`.
    #[serde(skip_serializing_if = "is_zero")]
    untagged_ignored: usize,
    /// Labs whose state file claims namespaces that no longer exist on the
    /// host — the mirror case of the above (state with no resources). Most
    /// commonly caused by a reboot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stale: Vec<StaleLab>,
}

/// A state-backed lab with one or more namespaces missing from the host.
#[derive(Debug, Clone, serde::Serialize)]
struct StaleLab {
    /// Lab name.
    name: String,
    /// Namespaces claimed by `state.json` that are absent from `ip netns list`.
    missing_namespaces: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Orphans {
    /// True when nothing is reportable. `untagged_ignored` is
    /// informational only and does not count.
    fn is_empty(&self) -> bool {
        self.bridges.is_empty()
            && self.veths.is_empty()
            && self.netns.is_empty()
            && self.stale.is_empty()
    }
}

/// Scan the host for lab-owned resources without a matching state file, and
/// state-backed labs whose resources are gone from the host.
///
/// Detection rules for *orphans* (resource with no state):
/// - Interfaces matching `^nl[0-9a-f]{8}$` are mgmt bridges; orphan if the
///   hash doesn't match any known lab's `mgmt_bridge_name_for`.
/// - Interfaces starting with `nm` + 8 hex + digits are mgmt veth peers;
///   orphan if the hash portion doesn't match any known lab.
/// - Named netns are orphans only when they carry an nlink-lab ownership
///   tag (`netns_tag`) *and* the tagged lab is not registered, or is
///   registered but its `state.json` no longer lists that namespace.
///   Untagged namespaces are never reported (#29) — name heuristics
///   cannot tell a crashed lab from libvirt/podman/CNI.
///
/// Detection rule for *stale* (state with no resources): for each known lab,
/// compare the namespaces it claims in `state.json` against the host's
/// current namespace list. Any missing namespace marks the lab stale.
async fn find_orphans(known: &[nlink_lab::state::LabInfo]) -> Orphans {
    let ifnames: Vec<String> = list_ip_links().await;
    let netns: Vec<String> = list_netns();
    let tagged: Vec<(String, Option<String>)> =
        netns.iter().map(|ns| (ns.clone(), tag_of(ns))).collect();

    let lab_namespaces: Vec<(String, Vec<String>)> = known
        .iter()
        .filter_map(|info| {
            nlink_lab::state::load_namespace_names(&info.name)
                .ok()
                .map(|ns| (info.name.clone(), ns))
        })
        .collect();
    let mut orphans = classify_orphans(&ifnames, &tagged, known, &lab_namespaces);
    orphans.stale = classify_stale(&lab_namespaces, &netns);
    orphans
}

/// Ownership tag of a namespace as the classifier sees it: `None` for an
/// untagged namespace, `Some(lab)` otherwise. A tag file that exists but
/// is empty (crash mid-write) still counts as tagged — by a lab nobody
/// knows — so it stays reapable.
fn tag_of(ns: &str) -> Option<String> {
    nlink_lab::netns_tag::is_tagged(ns)
        .then(|| nlink_lab::netns_tag::lab_of(ns).unwrap_or_default())
}

/// Pure stale-lab classifier.
///
/// For each `(lab_name, claimed_namespaces)` pair, return a [`StaleLab`] if
/// any claimed namespace is absent from `netns_present`. Labs with all
/// namespaces present are omitted. Claimed namespaces are deduplicated and
/// sorted in the output so results are stable for tests.
fn classify_stale(labs: &[(String, Vec<String>)], netns_present: &[String]) -> Vec<StaleLab> {
    let present: std::collections::HashSet<&str> =
        netns_present.iter().map(|s| s.as_str()).collect();
    let mut out = Vec::new();
    for (name, claimed) in labs {
        let mut missing: Vec<String> = claimed
            .iter()
            .filter(|ns| !present.contains(ns.as_str()))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            out.push(StaleLab {
                name: name.clone(),
                missing_namespaces: missing,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Interface names in the root namespace (RTM_GETLINK dump).
async fn list_ip_links() -> Vec<String> {
    match nlink::Connection::<nlink::Route>::new() {
        Ok(conn) => list_ip_links_with(&conn).await,
        Err(e) => {
            tracing::warn!("cannot open netlink socket: {e}");
            Vec::new()
        }
    }
}

async fn list_ip_links_with(conn: &nlink::Connection<nlink::Route>) -> Vec<String> {
    match conn.get_links().await {
        Ok(links) => links
            .iter()
            .filter_map(|l| l.name().map(str::to_string))
            .collect(),
        Err(e) => {
            tracing::warn!("failed to list links: {e}");
            Vec::new()
        }
    }
}

/// Named network namespaces on the host (`/var/run/netns`).
fn list_netns() -> Vec<String> {
    nlink::netlink::namespace::list().unwrap_or_else(|e| {
        tracing::warn!("failed to list namespaces: {e}");
        Vec::new()
    })
}

/// Pure classification — given host state and known labs, emit orphans.
///
/// `netns` pairs every namespace name with its `netns_tag` owner (`None`
/// = untagged). `claimed` lists, per registered lab, the namespaces its
/// `state.json` currently records; a registered lab missing from
/// `claimed` (state unreadable) is treated as still owning everything it
/// tagged, so nothing is reaped on a corrupt state file.
fn classify_orphans(
    ifnames: &[String],
    netns: &[(String, Option<String>)],
    known: &[nlink_lab::state::LabInfo],
    claimed: &[(String, Vec<String>)],
) -> Orphans {
    use std::collections::{HashMap, HashSet};

    let mut known_bridges: HashSet<String> = HashSet::new();
    let mut known_hashes: HashSet<String> = HashSet::new();
    let mut known_names: HashSet<&str> = HashSet::new();
    for info in known {
        let bridge = nlink_lab::mgmt_bridge_name_for(&info.name);
        if bridge.len() > 2 {
            known_hashes.insert(bridge[2..].to_string());
        }
        known_bridges.insert(bridge);
        known_names.insert(info.name.as_str());
    }
    let claimed_by: HashMap<&str, HashSet<&str>> = claimed
        .iter()
        .map(|(lab, nss)| (lab.as_str(), nss.iter().map(String::as_str).collect()))
        .collect();

    let mut orphans = Orphans::default();
    for ifname in ifnames {
        // Mgmt bridge: `nl` + 8 hex chars, total 10.
        if ifname.len() == 10
            && ifname.starts_with("nl")
            && ifname[2..].chars().all(|c| c.is_ascii_hexdigit())
            && !known_bridges.contains(ifname)
        {
            orphans.bridges.push(ifname.clone());
            continue;
        }
        // Mgmt veth peer: `nm` + 8 hex + 1+ digits.
        if ifname.len() >= 11
            && ifname.starts_with("nm")
            && ifname[2..10].chars().all(|c| c.is_ascii_hexdigit())
            && ifname[10..].chars().all(|c| c.is_ascii_digit())
            && !known_hashes.contains(&ifname[2..10])
        {
            orphans.veths.push(ifname.clone());
        }
    }

    for (ns, tag) in netns {
        let Some(owner) = tag else {
            orphans.untagged_ignored += 1;
            continue;
        };
        let orphaned = match claimed_by.get(owner.as_str()) {
            // Registered lab with readable state: orphan iff it no
            // longer claims this namespace.
            Some(claims) => !claims.contains(ns.as_str()),
            // Registered but state unreadable: leave it alone.
            None if known_names.contains(owner.as_str()) => false,
            // Tagged by a lab that is not registered at all.
            None => true,
        };
        if orphaned {
            orphans.netns.push(ns.clone());
        }
    }

    orphans
}

/// Best-effort cleanup of orphan resources found by [`find_orphans`].
/// Only tagged namespaces ever reach here (see [`classify_orphans`]).
async fn reap_orphans(known: &[nlink_lab::state::LabInfo]) {
    let orphans = find_orphans(known).await;
    if orphans.is_empty() {
        println!("No orphans detected.");
        return;
    }
    // Netns first: deleting a namespace reaps the veths inside it.
    for ns in &orphans.netns {
        match nlink::netlink::namespace::delete(ns) {
            Ok(()) => {
                nlink_lab::netns_tag::untag(ns);
                println!("  deleted namespace '{ns}'");
            }
            Err(e) => eprintln!("  warning: failed to delete namespace '{ns}': {e}"),
        }
    }
    if orphans.veths.is_empty() && orphans.bridges.is_empty() {
        return;
    }
    let conn = match nlink::Connection::<nlink::Route>::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  warning: cannot open netlink socket: {e}");
            return;
        }
    };
    for v in &orphans.veths {
        match conn.del_link_if_exists(v.as_str()).await {
            Ok(true) => println!("  deleted veth '{v}'"),
            // Already gone — its peer went with a namespace above.
            Ok(false) => {}
            Err(e) => eprintln!("  warning: failed to delete veth '{v}': {e}"),
        }
    }
    for b in &orphans.bridges {
        match conn.del_link_if_exists(b.as_str()).await {
            Ok(true) => println!("  deleted mgmt bridge '{b}'"),
            Ok(false) => {}
            Err(e) => eprintln!("  warning: failed to delete mgmt bridge '{b}': {e}"),
        }
    }
}

/// Number of veth attachments a node has in the topology: point-to-point
/// links, bridge network memberships, and the mgmt veth when the lab has a
/// management network. All are moved into the node's namespace at deploy
/// time and vanish when a container is restarted (#31).
fn node_link_count(topo: &nlink_lab::Topology, node: &str) -> usize {
    let is_node = |endpoint: &str| {
        endpoint == node || nlink_lab::EndpointRef::parse(endpoint).is_some_and(|r| r.node == node)
    };
    let links = topo
        .links
        .iter()
        .filter(|l| l.endpoints.iter().any(|e| is_node(e)))
        .count();
    let members = topo
        .networks
        .values()
        .flat_map(|n| n.members.iter())
        .filter(|m| is_node(m))
        .count();
    let mgmt = usize::from(topo.lab.mgmt_subnet.is_some());
    links + members + mgmt
}

/// Follow `path` from `start_offset`, writing each new chunk to `out`
/// until `should_continue()` returns false or an I/O error occurs.
/// Handles file truncation/rotation by reopening from offset 0 when the
/// file shrinks below the last-read position.
///
/// Production callers use `|| true` for `should_continue` and exit on
/// SIGINT (Ctrl-C terminates the process as usual). Tests can pass a
/// closure that stops after a deterministic number of iterations.
fn tail_follow_to<W: std::io::Write>(
    path: &std::path::Path,
    start_offset: u64,
    out: &mut W,
    should_continue: impl Fn() -> bool,
) -> nlink_lab::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("failed to open log file: {e}")))?;
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("seek on log file: {e}")))?;
    let mut pos = start_offset;
    let mut buf = [0u8; 8192];
    while should_continue() {
        match file.read(&mut buf) {
            Ok(0) => {
                let meta = std::fs::metadata(path).ok();
                if let Some(m) = meta
                    && m.len() < pos
                {
                    file = std::fs::File::open(path).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("reopen log file: {e}"))
                    })?;
                    pos = 0;
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(n) => {
                out.write_all(&buf[..n]).ok();
                out.flush().ok();
                pos += n as u64;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "read from log file: {e}"
                )));
            }
        }
    }
    Ok(())
}

/// Wrapper used by the CLI: runs forever (until Ctrl-C) and writes to
/// stdout.
/// Last `n` lines of a file, read backwards in 64 KiB chunks so a
/// multi-gigabyte log is never loaded into memory (#46).
fn tail_lines(path: &std::path::Path, n: usize) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    if n == 0 {
        return Ok(String::new());
    }
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut end = len;
    let mut buf: Vec<u8> = Vec::new();
    const CHUNK: u64 = 64 * 1024;
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        end = start;
        // count newlines, ignoring a single trailing one
        let body = if buf.last() == Some(&b'\n') {
            &buf[..buf.len() - 1]
        } else {
            &buf[..]
        };
        if body.iter().filter(|&&b| b == b'\n').count() >= n {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].join("\n"))
}

fn tail_follow(path: &std::path::Path, start_offset: u64) -> nlink_lab::Result<()> {
    let mut stdout = std::io::stdout();
    tail_follow_to(path, start_offset, &mut stdout, || true)
}

/// Parse a byte-size CLI argument with optional K/M/G suffix.
/// Decimal-SI units (1K = 1000, 1M = 1_000_000) — same convention as
/// `tcpdump -C`. Round-5 §2.3.
/// Parse a CIDR string (`10.0.0.0/24`, `2001:db8::/32`) into the
/// netring `IpNet` type used by the typed BPF filter builder.
/// Surfaces the originating flag name in the error so the user can
/// tell which `--filter-*-net` was malformed.
fn parse_filter_cidr(flag: &str, s: &str) -> nlink_lab::Result<netring::IpNet> {
    s.parse::<netring::IpNet>()
        .map_err(|e| nlink_lab::Error::invalid_topology(format!("invalid {flag} value {s:?}: {e}")))
}

/// Legacy `--filter "<tcpdump expr>"` path. Default builds reject
/// at parse time with a migration suggestion; opting into the
/// `legacy-tcpdump-filter` feature reinstates the `tcpdump -dd`
/// shell-out.
#[cfg(feature = "legacy-tcpdump-filter")]
fn compile_legacy_bpf_filter(expr: &str) -> nlink_lab::Result<netring::BpfFilter> {
    nlink_lab::capture::compile_bpf_filter(expr)
}

#[cfg(not(feature = "legacy-tcpdump-filter"))]
fn compile_legacy_bpf_filter(_expr: &str) -> nlink_lab::Result<netring::BpfFilter> {
    Err(nlink_lab::Error::invalid_topology(
        "--filter \"<tcpdump expr>\" requires nlink-lab built with the \
         `legacy-tcpdump-filter` feature (which shells out to `tcpdump -dd`). \
         Default builds use the typed --filter-tcp / --filter-dst-port / \
         --filter-host / --filter-src-net flags instead.",
    ))
}

fn parse_byte_size(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    let (num_str, mul) = if let Some(rest) = s.strip_suffix(['G', 'g']) {
        (rest, 1_000_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['M', 'm']) {
        (rest, 1_000_000u64)
    } else if let Some(rest) = s.strip_suffix(['K', 'k']) {
        (rest, 1_000u64)
    } else {
        (s, 1u64)
    };
    let n: u64 = num_str
        .parse()
        .map_err(|e| format!("invalid byte size {s:?}: {e}"))?;
    Ok(n * mul)
}

/// Parse repeated `--env KEY=VALUE` strings into `(key, value)` pairs.
///
/// Returns an error on entries missing `=`. The parsed pairs are
/// applied to the child process via `Command::env(k, v)` (additive on
/// top of the inherited environment) — *not* by wrapping the command
/// in `/usr/bin/env`. The wrapper approach was the previous behaviour
/// and silently broke the per-process logfile naming convention,
/// because `argv[0]` ended up being `env` rather than the user's
/// binary. See round-3 feedback §3.1.
fn parse_env_pairs(env_vars: &[String]) -> nlink_lab::Result<Vec<(String, String)>> {
    env_vars
        .iter()
        .map(|s| {
            s.split_once('=')
                .ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "invalid --env {s:?} (expected KEY=VALUE)"
                    ))
                })
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Build the `host_resources` block for `status --json <lab>`.
///
/// Reports the host-side artefacts a lab installs, so consumers can
/// detect cross-lab collisions client-side without netlink:
///
/// - `mgmt_bridge`: the `nl{hash8}` bridge in the host network ns
///   (always present for namespace-only labs).
/// - `subnets`: every declared subnet in the topology (links +
///   networks). Useful to verify two labs don't claim the same
///   private range.
///
/// Round-5 §1.2 bonus.
fn host_resources_json(lab: &nlink_lab::RunningLab) -> serde_json::Value {
    let topo = lab.topology();
    let mgmt_bridge = nlink_lab::mgmt_bridge_name_for(lab.name());

    let mut subnets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for link in &topo.links {
        if let Some(addrs) = &link.addresses {
            for a in addrs {
                if let Some((_, prefix)) = a.split_once('/')
                    && let Some(network) = subnet_of(a)
                {
                    subnets.insert(format!("{network}/{prefix}"));
                }
            }
        }
    }
    for network in topo.networks.values() {
        if let Some(s) = &network.subnet {
            subnets.insert(s.clone());
        }
    }

    serde_json::json!({
        "mgmt_bridge": mgmt_bridge,
        "subnets": subnets.into_iter().collect::<Vec<_>>(),
    })
}

/// Compute the network-address portion of a CIDR (e.g.
/// `"10.0.0.5/24"` → `"10.0.0.0"`). Returns None on malformed input.
fn subnet_of(cidr: &str) -> Option<String> {
    let (ip_str, prefix_str) = cidr.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip_str.parse().ok()?;
    let prefix: u32 = prefix_str.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 {
        0u32
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(ip) & mask;
    Some(std::net::Ipv4Addr::from(network).to_string())
}

/// One row of `nlink-lab impair --show --json` output. `ImpairShow`'s
/// fields (qdisc, delay_ms, jitter_ms, loss_pct, rate_bps) flatten in
/// next to a `partition` flag pulled from `RunningLab::is_partitioned`.
#[derive(serde::Serialize)]
struct ImpairShowEntry {
    #[serde(flatten)]
    fields: nlink_lab::impair_parse::ImpairShow,
    /// `true` iff the endpoint is in `partition` state (i.e. its
    /// pre-partition impairment is in `saved_impairments`). A user who
    /// installed `--loss 100%` directly is *not* partitioned by this
    /// definition — the flag tracks the partition/heal lifecycle.
    partition: bool,
}

/// Walk every endpoint in the topology, exec `tc qdisc show dev <iface>`
/// inside the right namespace, and parse the result. Endpoints with no
/// impairment installed serialize as `null`. Used by `--show --json`.
fn collect_impair_show(
    running: &nlink_lab::RunningLab,
) -> nlink_lab::Result<std::collections::BTreeMap<String, Option<ImpairShowEntry>>> {
    use nlink_lab::EndpointRef;
    let mut out = std::collections::BTreeMap::new();
    for ep_str in nlink_lab::impair_parse::topology_endpoints(running.topology()) {
        let ep = EndpointRef::parse(&ep_str).ok_or_else(|| {
            nlink_lab::Error::invalid_topology(format!("malformed endpoint {ep_str:?} in topology"))
        })?;
        // Skip endpoints whose node isn't a known namespace (e.g.
        // container-only nodes — `tc qdisc show` via `running.exec`
        // would route through docker/podman and is meaningless).
        if running.namespace_for(&ep.node).is_err() {
            continue;
        }
        let tc = running.exec(&ep.node, "tc", &["qdisc", "show", "dev", &ep.iface])?;
        let parsed = nlink_lab::impair_parse::parse_tc_qdisc_show(&tc.stdout);
        let entry = parsed.map(|fields| ImpairShowEntry {
            fields,
            partition: running.is_partitioned(&ep_str),
        });
        out.insert(ep_str, entry);
    }
    Ok(out)
}

/// Build the argv to pass to `nsenter` for entering a lab node's network
/// namespace and exec'ing a shell.
///
/// Must emit `--net=<path>` as a single argument; splitting it into two
/// (`--net`, `<path>`) makes nsenter treat `--net` as "enter target's netns"
/// and then look for a target it never got, failing with
/// "neither filename nor target pid supplied for ns/net".
fn nsenter_shell_args(ns: &str, shell: &str) -> Vec<String> {
    vec![
        format!("--net=/var/run/netns/{ns}"),
        "--".into(),
        shell.into(),
    ]
}

#[cfg(test)]
mod tests {

    #[test]
    fn cap_eff_requires_both_admin_bits() {
        // CAP_NET_ADMIN (12) + CAP_SYS_ADMIN (21)
        assert!(cap_eff_has_net_and_sys_admin("0000000000201000"));
        // full root set
        assert!(cap_eff_has_net_and_sys_admin("000001ffffffffff"));
        // only CAP_NET_ADMIN
        assert!(!cap_eff_has_net_and_sys_admin("0000000000001000"));
        // only CAP_CHOWN — used to pass the old `!= 0` check
        assert!(!cap_eff_has_net_and_sys_admin("0000000000000001"));
        assert!(!cap_eff_has_net_and_sys_admin("garbage"));
    }

    #[test]
    fn tail_lines_reads_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let body: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, &body).unwrap();
        assert_eq!(
            tail_lines(&p, 3).unwrap(),
            "line 9997\nline 9998\nline 9999"
        );
        assert_eq!(tail_lines(&p, 0).unwrap(), "");
        assert_eq!(tail_lines(&p, 20_000).unwrap().lines().count(), 10_000);
        // file without trailing newline, spanning several 64 KiB chunks
        let big: String = (0..200_000).map(|i| format!("{i:08}\n")).collect();
        std::fs::write(&p, big.trim_end()).unwrap();
        assert_eq!(tail_lines(&p, 2).unwrap(), "00199998\n00199999");
    }

    #[test]
    fn exit_code_policy() {
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Validation("x".into())),
            EXIT_VALIDATION
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::Timeout(std::time::Duration::from_secs(
                1
            ))),
            EXIT_TIMEOUT
        );
        assert_eq!(
            exit_code_for(&nlink_lab::Error::NotFound { name: "l".into() }),
            EXIT_FAILURE
        );
    }
    use super::*;
    use nlink_lab::state::LabInfo;

    #[test]
    fn parse_env_pairs_accepts_valid() {
        let inputs = vec![
            "FOO=bar".to_string(),
            "EMPTY=".to_string(),
            "X=y=z".to_string(),
        ];
        let pairs = parse_env_pairs(&inputs).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("EMPTY".to_string(), "".to_string()),
                // `split_once` splits on the *first* `=`, so the value gets
                // `y=z`. Keeps `--env DSN=postgres://u@h/db?x=y` working.
                ("X".to_string(), "y=z".to_string()),
            ]
        );
    }

    #[test]
    fn parse_env_pairs_rejects_missing_equals() {
        let inputs = vec!["NOEQ".to_string()];
        let err = parse_env_pairs(&inputs).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("NOEQ"), "error should name the bad entry: {s}");
    }

    /// Each JSON Schema under `docs/json-schemas/` must be valid JSON.
    /// Catches accidental hand-edit corruption (trailing comma, etc.) at
    /// CI time — we don't validate the schema language itself, just
    /// parseability. Keep the file list in sync when adding schemas.
    #[test]
    fn json_schemas_parse() {
        let schemas = [
            include_str!("../../../docs/json-schemas/deploy.schema.json"),
            include_str!("../../../docs/json-schemas/status-list.schema.json"),
            include_str!("../../../docs/json-schemas/status-scan.schema.json"),
            include_str!("../../../docs/json-schemas/spawn.schema.json"),
            include_str!("../../../docs/json-schemas/ps.schema.json"),
            include_str!("../../../docs/json-schemas/impair-show.schema.json"),
            include_str!("../../../docs/json-schemas/proc-stat.schema.json"),
            include_str!("../../../docs/json-schemas/status-lab.schema.json"),
        ];
        for s in schemas {
            let _: serde_json::Value = serde_json::from_str(s)
                .expect("JSON Schema file failed to parse — see file list above");
        }
    }

    /// `topology_endpoints` must collect from every place a topology
    /// can declare an endpoint — `links`, `networks.members`, and
    /// declared impairments. Round-4 follow-up: the first `--show
    /// --json` impl only walked `links` and emitted `endpoints: {}`
    /// for any topology built around bridge networks.
    #[test]
    fn topology_endpoints_collects_links_networks_and_impairments() {
        let nll = r#"
lab "tep"

node a
node b
node c

# point-to-point link → endpoints visible in `links`
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

# bridge network → endpoints visible in `networks.members`
network lan {
  members [b:eth1, c:eth0]
  subnet 10.1.0.0/24
}

# top-level impair → endpoint visible in `impairments`
impair c:lo delay 5ms
"#;
        let topo = nlink_lab::parser::parse(nll).unwrap();
        let endpoints = nlink_lab::impair_parse::topology_endpoints(&topo);

        // BTreeSet ordering is alphabetical.
        assert_eq!(
            endpoints,
            vec![
                "a:eth0".to_string(),
                "b:eth0".to_string(),
                "b:eth1".to_string(),
                "c:eth0".to_string(),
                "c:lo".to_string(),
            ],
            "must collect link endpoints + network members + impairment keys"
        );
    }

    /// Network-only topology must produce a non-empty endpoint list.
    /// Specifically guards against the round-4 follow-up regression
    /// where `--show --json` returned `endpoints: {}` for the harness
    /// team's 3-machine topology (built around networks, no `link`
    /// declarations).
    #[test]
    fn topology_endpoints_handles_network_only_topology() {
        let nll = r#"
lab "net-only"

node router
node site_a
node site_b

network lan_a {
  members [router:eth0, site_a:eth0]
  subnet 10.0.0.0/24
}
network lan_b {
  members [router:eth1, site_b:eth0]
  subnet 10.1.0.0/24
}
"#;
        let topo = nlink_lab::parser::parse(nll).unwrap();
        let endpoints = nlink_lab::impair_parse::topology_endpoints(&topo);
        assert!(
            endpoints.contains(&"router:eth0".to_string()),
            "missing router:eth0: {endpoints:?}"
        );
        assert!(
            endpoints.contains(&"site_a:eth0".to_string()),
            "missing site_a:eth0: {endpoints:?}"
        );
        assert!(
            endpoints.contains(&"site_b:eth0".to_string()),
            "missing site_b:eth0 (was the regression trigger): {endpoints:?}"
        );
    }

    #[test]
    fn nsenter_shell_args_uses_equals_form() {
        let args = nsenter_shell_args("mylab-router", "/bin/bash");
        assert_eq!(
            args,
            vec![
                "--net=/var/run/netns/mylab-router".to_string(),
                "--".to_string(),
                "/bin/bash".to_string(),
            ]
        );
        // Guard against the split `--net` regression: no argument may be
        // exactly `--net`, which nsenter interprets as the flag alone.
        assert!(
            !args.iter().any(|a| a == "--net"),
            "bare --net would be misparsed by nsenter"
        );
    }

    fn info(name: &str) -> LabInfo {
        LabInfo {
            name: name.to_string(),
            node_count: 0,
            created_at: String::new(),
        }
    }

    #[test]
    fn classify_orphans_reports_unknown_mgmt_bridge() {
        let known = vec![info("keep")];
        let keep_bridge = nlink_lab::mgmt_bridge_name_for("keep");
        let orphan_bridge = nlink_lab::mgmt_bridge_name_for("gone");
        let ifnames = vec![keep_bridge.clone(), orphan_bridge.clone(), "eth0".into()];
        let orphans = classify_orphans(&ifnames, &[], &known, &[]);
        assert_eq!(orphans.bridges, vec![orphan_bridge]);
        assert!(orphans.veths.is_empty());
    }

    #[test]
    fn classify_orphans_skips_known_mgmt_veths() {
        let known = vec![info("keep")];
        let keep_hash = &nlink_lab::mgmt_bridge_name_for("keep")[2..];
        let gone_hash = &nlink_lab::mgmt_bridge_name_for("gone")[2..];
        let ifnames = vec![
            format!("nm{keep_hash}0"),
            format!("nm{gone_hash}0"),
            format!("nm{gone_hash}42"),
            "lo".into(),
            "eth0".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &known, &[]);
        assert_eq!(orphans.veths.len(), 2);
        assert!(orphans.veths.iter().all(|v| v.contains(gone_hash)));
    }

    fn untagged(ns: &str) -> (String, Option<String>) {
        (ns.to_string(), None)
    }

    fn tagged(ns: &str, lab: &str) -> (String, Option<String>) {
        (ns.to_string(), Some(lab.to_string()))
    }

    #[test]
    fn classify_orphans_ignores_system_netns() {
        // Untagged system netns are never flagged, only counted.
        let netns = vec![untagged("default"), untagged("init")];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty());
        assert_eq!(orphans.untagged_ignored, 2);
        assert!(
            orphans.is_empty(),
            "untagged count must not make it non-empty"
        );
    }

    #[test]
    fn classify_orphans_reports_tagged_unregistered_netns() {
        // Tagged by a lab with no state file at all → orphan (#29).
        let known = vec![info("keep")];
        let claimed = vec![(
            "keep".to_string(),
            vec!["keep-router".to_string(), "keep-mgmt".to_string()],
        )];
        let netns = vec![
            tagged("keep-router", "keep"),
            tagged("keep-mgmt", "keep"),
            tagged("stale-mgmt", "stale"),
            tagged("stale-node1", "stale"),
        ];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert_eq!(orphans.netns.len(), 2);
        assert!(orphans.netns.contains(&"stale-mgmt".to_string()));
        assert!(orphans.netns.contains(&"stale-node1".to_string()));
        assert_eq!(orphans.untagged_ignored, 0);
    }

    #[test]
    fn classify_orphans_keeps_tagged_registered_netns() {
        // Tagged and claimed by a registered lab → not an orphan, even
        // though the name would have matched nothing by prefix.
        let known = vec![info("keep")];
        let claimed = vec![("keep".to_string(), vec!["r1".to_string()])];
        let netns = vec![tagged("r1", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
    }

    #[test]
    fn classify_orphans_reports_tagged_but_unclaimed_netns() {
        // Registered lab whose state.json no longer lists the namespace
        // (e.g. an `apply` removed the node but the delete failed).
        let known = vec![info("keep")];
        let claimed = vec![("keep".to_string(), vec!["keep-a".to_string()])];
        let netns = vec![tagged("keep-a", "keep"), tagged("keep-b", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &claimed);
        assert_eq!(orphans.netns, vec!["keep-b".to_string()]);
    }

    #[test]
    fn classify_orphans_keeps_tagged_netns_when_state_unreadable() {
        // Registered lab absent from `claimed` (state.json unreadable):
        // be conservative and leave its namespaces alone.
        let known = vec![info("keep")];
        let netns = vec![tagged("keep-a", "keep")];
        let orphans = classify_orphans(&[], &netns, &known, &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
    }

    #[test]
    fn classify_orphans_never_touches_untagged_dashed_names() {
        // The exact #29 failure: lab-shaped names with hyphens that
        // nlink-lab did not create. No registered labs at all.
        let netns = vec![
            untagged("my-lab-router"),
            untagged("stale-mgmt"),
            untagged("ns-with-dashes"),
        ];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
        assert_eq!(orphans.untagged_ignored, 3);
    }

    #[test]
    fn classify_orphans_ignores_libvirt_and_cni_style_netns() {
        // Names other tools create on a shared host; none are tagged.
        let netns = vec![
            untagged("qemu-1-vm1"),
            untagged("cni-2f3a1b7c-9d4e-4a1b-8c2d-0e1f2a3b4c5d"),
            untagged("netns-podman-abcdef"),
            untagged("mininet-h1"),
            untagged("vrf-blue"),
        ];
        let orphans = classify_orphans(&[], &netns, &[], &[]);
        assert!(orphans.netns.is_empty(), "{orphans:?}");
        assert_eq!(orphans.untagged_ignored, 5);
    }

    #[test]
    fn classify_orphans_empty_tag_counts_as_tagged_by_nobody() {
        // A tag file that exists but is empty (crash mid-write) maps to
        // `Some("")`, which no registered lab matches → reapable.
        let netns = vec![tagged("half-written", "")];
        let orphans = classify_orphans(&[], &netns, &[info("keep")], &[]);
        assert_eq!(orphans.netns, vec!["half-written".to_string()]);
    }

    #[test]
    fn node_link_count_counts_links_members_and_mgmt() {
        use nlink_lab::{Link, Network, Topology};
        let mut topo = Topology::default();
        topo.links.push(Link {
            endpoints: ["r1:eth0".into(), "h1:eth0".into()],
            addresses: None,
            mtu: None,
        });
        topo.links.push(Link {
            endpoints: ["r1:eth1".into(), "h2:eth0".into()],
            addresses: None,
            mtu: None,
        });
        topo.networks.insert(
            "lan".into(),
            Network {
                members: vec!["h1:eth1".into(), "h3:eth0".into()],
                ..Default::default()
            },
        );
        assert_eq!(node_link_count(&topo, "r1"), 2);
        assert_eq!(node_link_count(&topo, "h1"), 2);
        assert_eq!(node_link_count(&topo, "h3"), 1);
        assert_eq!(node_link_count(&topo, "lonely"), 0);

        // A management network adds one veth to every node.
        topo.lab.mgmt_subnet = Some("10.99.0.0/24".into());
        assert_eq!(node_link_count(&topo, "lonely"), 1);
        assert_eq!(node_link_count(&topo, "r1"), 3);
    }

    #[test]
    fn tail_follow_reads_appended_data() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"initial\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Append after a short delay from a background thread.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_w)
                .unwrap();
            f.write_all(b"appended\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            // Stop after roughly 1 second of polling (4×250ms sleeps).
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 6
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("appended"),
            "expected appended content, got: {captured:?}"
        );
        assert!(
            !captured.contains("initial"),
            "should not re-read data before start_offset: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_follow_handles_truncation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nlink-lab-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");

        std::fs::write(&path, b"old content that will be truncated\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();

        // Truncate then write fresh content.
        let path_w = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut f = std::fs::File::create(&path_w).unwrap(); // truncates
            f.write_all(b"fresh\n").unwrap();
        });

        let counter = std::sync::atomic::AtomicUsize::new(0);
        let mut out = Vec::new();
        tail_follow_to(&path, start, &mut out, || {
            let c = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            c < 8
        })
        .unwrap();

        let captured = String::from_utf8(out).unwrap();
        assert!(
            captured.contains("fresh"),
            "expected post-truncation content, got: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_stale_flags_lab_with_missing_namespace() {
        let labs = vec![(
            "des-3m".into(),
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ],
        )];
        // Host sees nothing — classic WSL-restart case.
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "des-3m");
        assert_eq!(
            stale[0].missing_namespaces,
            vec![
                "des-3m-router".to_string(),
                "des-3m-site_a".to_string(),
                "des-3m-site_b".to_string(),
            ]
        );
    }

    #[test]
    fn classify_stale_ignores_healthy_labs() {
        let labs = vec![(
            "healthy".into(),
            vec!["healthy-a".to_string(), "healthy-b".to_string()],
        )];
        let present = vec!["healthy-a".into(), "healthy-b".into()];
        assert!(classify_stale(&labs, &present).is_empty());
    }

    #[test]
    fn classify_stale_reports_partial_loss() {
        let labs = vec![(
            "partial".into(),
            vec!["partial-a".to_string(), "partial-b".to_string()],
        )];
        let present = vec!["partial-a".into()];
        let stale = classify_stale(&labs, &present);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].missing_namespaces, vec!["partial-b".to_string()]);
    }

    #[test]
    fn classify_stale_orders_deterministically() {
        // Two stale labs provided out of alphabetical order — output should
        // be sorted so test assertions and `--json` output are stable.
        let labs = vec![
            ("zebra".into(), vec!["zebra-a".to_string()]),
            ("alpha".into(), vec!["alpha-a".to_string()]),
        ];
        let stale = classify_stale(&labs, &[]);
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].name, "alpha");
        assert_eq!(stale[1].name, "zebra");
    }

    #[test]
    fn classify_orphans_ignores_non_lab_interfaces() {
        // Random interface names should not trigger detection.
        let ifnames = vec![
            "eth0".into(),
            "docker0".into(),
            "br-abc".into(),
            "wlp3s0".into(),
            // nl-prefixed but wrong length / non-hex — not a mgmt bridge.
            "nlmonitor".into(),
            "nl1234".into(),
            // nm-prefixed but no trailing digits.
            "nmabcdef01".into(),
        ];
        let orphans = classify_orphans(&ifnames, &[], &[], &[]);
        assert!(
            orphans.bridges.is_empty() && orphans.veths.is_empty(),
            "false positives: {orphans:?}"
        );
    }
}
