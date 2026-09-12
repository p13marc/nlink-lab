//! The `Commands` enum — one variant per subcommand. Doc comments and
//! `#[arg]`/`#[command]` attributes here and on each `cmd::*::Args`
//! struct are the help text (`docs/cli/*.md` is generated from them).

use clap::Subcommand;
use std::path::PathBuf;

use crate::cmd;
use crate::util::parse_byte_size;

/// Stream selector for `nlink-lab spawn --wait-log-stream`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum WaitLogStream {
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

#[derive(Subcommand)]
pub enum Commands {
    /// Deploy a lab from a topology file (.nll).
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "name": str, "nodes": int, "links": int, "deploy_time_ms": int }
    /// Schema: docs/json-schemas/deploy.schema.json
    ///
    /// Combined with `--unique`, the `name` field is the chosen unique
    /// lab name (original name + PID suffix). Useful for scripted
    /// teardown.
    Deploy(cmd::deploy::Args),

    /// Apply topology changes to a running lab.
    ///
    /// Reconciles the live lab state to match an updated NLL,
    /// issuing only the deltas. Add `--check` to fail on any drift
    /// (a CI gate). Add `--json --dry-run` for machine-parseable
    /// diff output.
    Apply(cmd::apply::Args),

    /// Tear down a running lab.
    Destroy(cmd::destroy::Args),

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
    Status(cmd::status::Args),

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
    Validate(cmd::validate::Args),

    /// Run topology tests: deploy, validate, destroy.
    Test(cmd::test::Args),

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
    Scenario(cmd::scenario::Args),

    /// Regenerate `docs/cli/*.md` from the clap definitions (maintainers).
    #[command(hide = true)]
    DocsGen(cmd::docs_gen::Args),

    /// Print topology as DOT graph.
    Graph(cmd::graph::Args),

    /// Render a topology file with all loops, variables, and imports expanded.
    Render(cmd::render::Args),

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
    Wait(cmd::wait::Args),

    /// Tail nftables + RTNETLINK drift events for a running lab.
    ///
    /// Subscribes to every node in the lab and prints one line
    /// per kernel mutation — useful for spotting hand-edits that
    /// bypass `nlink-lab apply`. `--json` emits NDJSON for
    /// piping to `jq`. Plan 159b.
    Watch(cmd::watch::Args),

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
    Diff(cmd::diff::Args),

    /// Export a running lab's topology as serialized data.
    ///
    /// By default, dumps the rendered topology as TOML/JSON to stdout
    /// or `--output FILE`. With `--archive`, produces a portable
    /// `.nlz` lab archive (tar.gz with manifest + topology + params
    /// + rendered + checksums) suitable for sharing repros.
    Export(cmd::export::Args),

    /// Import a `.nlz` lab archive.
    ///
    /// Verifies checksums, extracts to `./<lab-name>/` (or `-d DIR`),
    /// and validates the topology. Pass `--no-deploy` to extract +
    /// validate without deploying; `--no-reparse` to use the bundled
    /// rendered.toml directly (useful when the archive was produced
    /// by a newer nlink-lab whose NLL syntax we don't fully understand).
    Import(cmd::import::Args),

    /// Show comprehensive lab details, OR summarize a `.nlz` archive.
    ///
    /// If LAB is a path ending in `.nlz`, summarizes the archive
    /// (manifest + node/link/network counts) without extracting.
    /// Otherwise, behaves as before — runs against a deployed lab.
    Inspect(cmd::inspect::Args),

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
    Pull(cmd::pull::Args),

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
    Daemon(cmd::daemon::Args),

    /// Stream live metrics from a lab via Zenoh (no root required).
    Metrics(cmd::metrics::Args),

    /// Create a topology file from a built-in template.
    Init(cmd::init::Args),
}

#[cfg(test)]
mod tests {
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
}
