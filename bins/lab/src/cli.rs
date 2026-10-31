//! The `Commands` enum — one variant per subcommand. Doc comments and
//! `#[arg]`/`#[command]` attributes here and on each `cmd::*::Args`
//! struct are the help text (`docs/cli/*.md` is generated from them).

use clap::Subcommand;

use crate::cmd;

#[derive(Subcommand)]
pub enum Commands {
    /// Deploy a lab from a topology file (.nll).
    ///
    /// FILE is a topology (`.nll`) or a lab archive (`.nlz`).
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "schema_version": 1, "lab": str, "nodes": int, "links": int,
    ///     "duration_ms": int, "assertions"?: [...], "assertions_failed"?: bool }
    /// Schema: docs/json-schemas/deploy.schema.json
    ///
    /// Combined with `--unique`, `lab` is the chosen unique lab name
    /// (original name + PID suffix). Useful for scripted teardown.
    Deploy(cmd::deploy::Args),

    /// Apply topology changes to a running lab.
    ///
    /// Reconciles the live lab with a changed topology, issuing only the
    /// deltas. Without FILE, the file the lab was deployed from (or last
    /// applied) is read again, with its params. `--dry-run` shows the
    /// changes (`--json` for the machine-readable diff); `verify --file
    /// FILE` is the CI drift gate.
    ///
    /// JSON OUTPUT: { "schema_version": 1, "lab", "ops", "removed",
    ///   "applied", "duration_ms", "assertions"?, "assertions_failed"? }
    Apply(cmd::apply::Args),

    /// Tear down labs (several at once, or `--all`).
    ///
    /// Without a name, `-L`, `$NLINK_LAB` or `nlink-lab use` says which
    /// lab: never a guess at the only one. Destroying the current lab
    /// forgets it. `--orphans` reaps what crashed deploys left.
    Destroy(cmd::destroy::Args),

    /// Select the current lab: the one lab-scoped commands act on.
    ///
    /// A command's lab is `-L LAB` when given, else `$NLINK_LAB`, else
    /// the lab `use` selected (per user, `sudo` included), else the only
    /// running lab. Without LAB, prints the current lab and where it
    /// comes from; `--clear` forgets it.
    ///
    /// JSON OUTPUT: { "schema_version": 1, "lab": str | null,
    ///   "from": "flag" | "env" | "context" | "only" | null, "note"?: str }
    Use(cmd::use_lab::Args),

    /// One lab, all of it: nodes and their addresses, containers, links,
    /// impairments as they are now, partitions, processes and named
    /// firewall counters (read live as root).
    ///
    /// JSON OUTPUT: { "schema_version": 2, "lab", "created_at",
    ///   "source"?, "current", "nodes": [ { "node", "kind", "image"?,
    ///   "container"?: { "id", "pid" }, "addresses": { iface: CIDR list } } ],
    ///   "links", "networks", "impairments": [ { "endpoint",
    ///   "impairment", "runtime" } ], "partitions", "processes",
    ///   "counters"?, "host_resources": { "mgmt_bridge", "subnets" } }
    /// Schema: docs/json-schemas/status.schema.json
    Status(cmd::status::Args),

    /// List the labs on this host (`*` marks the current one).
    ///
    /// `--scan` also reports lab resources without a state file (orphans)
    /// and labs whose namespaces are gone (stale).
    ///
    /// JSON OUTPUT: { "schema_version": 1, "labs": [ { "lab", "nodes",
    ///   "created_at", "source"?, "current" } ], "orphans"?: { "bridges",
    ///   "veths", "netns", "stale" }, "legacy"?: [ str ] }
    /// Schema: docs/json-schemas/ls.schema.json. With `--quiet`, the names.
    Ls(cmd::ls::Args),

    /// Run a command in a lab node.
    Exec(cmd::exec::Args),

    /// Spawn a background process in a lab node.
    ///
    /// Stdout/stderr are captured to per-process log files at:
    ///
    ///   `/var/lib/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}`
    ///
    /// (under `$NLINK_LAB_STATE_DIR` when it is set). The
    /// path is stable; consumers can read it directly, or use
    /// `nlink-lab logs -L <lab> --pid <pid>`.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "command": str, "node": str, "pid": int, "host_pid": int }
    /// Schema: docs/json-schemas/spawn.schema.json
    ///
    /// `pid` and `host_pid` are aliases — equal values today because
    /// nlink-lab does not use `CLONE_NEWPID`. See ARCHITECTURE.md
    /// "Process & namespace model" for why.
    Spawn(cmd::spawn::Args),

    /// Validate a topology file without deploying.
    Validate(cmd::validate::Args),

    /// Format NLL files: canonical indentation and spacing, nothing else.
    ///
    /// A token-level formatter: comments, blank-line structure, quoting and
    /// statement order are kept; only whitespace changes, so the topology
    /// is guaranteed identical. `--check` for CI, `-w` to rewrite in place,
    /// `-` to format stdin.
    Fmt(cmd::fmt::Args),

    /// Style and portability advice for a topology file (never blocks deploy).
    ///
    /// Missing `validate` blocks, background processes without a
    /// healthcheck, one-sided impairments, disconnected node groups, no
    /// description. `--strict` exits 2 on any finding; `--allow RULE`
    /// silences one.
    Lint(cmd::lint::Args),

    /// Run a Language Server Protocol server for `.nll` files on stdio.
    ///
    /// Editors get the same diagnostics `validate` and `lint` print
    /// (errors, warnings and style hints, each carrying its rule id),
    /// plus document symbols, go-to-definition on `node:iface` endpoints
    /// and `: profile` references, hover showing a node's interfaces and
    /// addresses, keyword/name completion, and formatting through the
    /// same token-level formatter as `nlink-lab fmt`.
    ///
    /// `import` statements resolve against the document's own directory,
    /// so an unsaved buffer is checked exactly like a saved file.
    ///
    /// This subcommand reads and writes JSON-RPC on stdin/stdout; logs go
    /// to stderr. See docs/cli/lsp.md for editor configuration.
    Lsp(cmd::lsp::Args),

    /// Live terminal UI for a running lab (nodes, interfaces, flows).
    ///
    /// As root, collects a metrics snapshot every `--interval` itself,
    /// like `diagnose`. Without root (or with `--daemon`, or
    /// `NLINK_LAB_DAEMON` set) it reads them from the nlink-lab daemon,
    /// `nlink-labd`: no root needed. The write keys then need the
    /// `nlink-lab` group (an operator); a viewer only watches.
    ///
    /// KEYS
    ///   up/down or j/k  select        Tab  next pane      s  cycle sort
    ///   i  impair prompt (`delay 50ms loss 1%`)           c  clear
    ///   p  partition    h  heal       /  filter    ?  help    q  quit
    ///
    /// `--once` prints a single text frame and exits — no terminal
    /// control, so it is safe in pipes, CI and `watch -n1`. With `--json`
    /// it prints the raw snapshot
    /// (schema: docs/json-schemas/metrics-snapshot.schema.json).
    Top(cmd::top::Args),

    /// Check a running lab for drift against its topology (exit 2 on drift).
    ///
    /// Compares every node's live links/addresses/routes, nftables and
    /// WireGuard peers with what the topology declares — the same view `apply --dry-run` shows —
    /// and prints the differences. Meant for CI and monitoring: exit 0
    /// means the lab is exactly what its file says.
    Verify(cmd::verify::Args),

    /// Check this host for everything nlink-lab needs.
    ///
    /// Privileges, netlink, the binaries exec'd inside namespaces, kernel
    /// modules, a writable state dir, pending crash journals and orphaned
    /// lab resources. Exit 1 when a required check fails.
    Doctor(cmd::doctor::Args),

    /// Run topology tests: deploy, validate, destroy.
    Test(cmd::test::Args),

    /// Add, remove or restart a node of a running lab.
    Node(cmd::node::Args),

    /// Add, remove or change a link of a running lab (MTU, qdisc).
    Link(cmd::link::Args),

    /// Change an endpoint's impairment at runtime: `impair r1:eth0 delay
    /// 50ms loss 1%`, `--dir in|both` for what it receives, `--clear`,
    /// `--partition`/`--heal`; or a network's between two nodes with
    /// `--pair SRC DST`. `--show` lists what every endpoint has.
    ///
    /// The impairment replaces the endpoint's whole netem and is kept in
    /// the lab's state, so `apply`, `verify` and `snapshot` see it.
    ///
    /// JSON OUTPUT (with `--show --json`):
    ///   { "schema_version": 1, "lab": str, "endpoints": { "NODE:IFACE": { ... } | null } }
    /// Schema: docs/json-schemas/impair-show.schema.json
    Impair(cmd::impair::Args),

    /// Show a lab's lifecycle log (deploy, apply, spawn, kill, impair,
    /// partition, snapshot, …); `--follow` merges live drift (RTNETLINK /
    /// nftables) and runtime (process exit, link state) events, optionally
    /// fanned out on a unix socket.
    ///
    /// JSON OUTPUT (with `--json`): one record per line tagged
    /// `source: lifecycle | drift | runtime`. Schema: docs/json-schemas/events.schema.json
    Events(cmd::events::Args),

    /// Checkpoints of a running lab: `save`, `ls`, `rm`, `restore`.
    ///
    /// A snapshot holds the topology, the runtime impairments and the
    /// partitions; `restore` applies the topology and puts the rest back.
    Snapshot(cmd::snapshot::Args),

    /// Run a `scenario` block from a deployed lab's topology.
    ///
    /// When it ends, failed or cancelled too, the lab is put back as the
    /// scenario found it (impairments, partitions, pair impairments,
    /// interfaces it took down), unless the scenario says `keep`.
    ///
    /// JSON OUTPUT (with `--json`): the full `ScenarioResult` (steps,
    /// actions, assertion outcomes, timings, and `cleanup`: what was put
    /// back). Exit 2 when any step or the cleanup fails.
    Scenario(cmd::scenario::Args),

    /// Run a `benchmark` block from a deployed lab's topology.
    ///
    /// Collects the metrics the block asks for (`ping` latency/loss,
    /// `iperf3` throughput, and with `udp` jitter/loss) and evaluates
    /// each `assert` against them. A test that measured nothing says why.
    /// Omit the name to list the lab's benchmarks.
    ///
    /// JSON OUTPUT (with `--json`): the full `BenchmarkResult` (per test:
    /// the metric map, every assertion with its actual value, and the
    /// pass flag). Exit 2 when any assertion fails.
    Benchmark(cmd::benchmark::Args),

    /// Regenerate `docs/cli/*.md` from the clap definitions (maintainers).
    #[command(hide = true)]
    DocsGen(cmd::docs_gen::Args),

    /// A topology file with every loop, variable and import expanded: flat
    /// NLL, the topology as JSON (`--format json`), or a diagram
    /// (`--diagram dot|mermaid|ascii`).
    Render(cmd::render::Args),

    /// Open an interactive shell in a lab node.
    Shell(cmd::shell::Args),

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
    ///   { "schema_version": 2, "lab": str,
    ///     "processes": [ { "node": str, "pid": int, "host_pid": int,
    ///       "alive": bool, "exit_code": int | null,
    ///       "stdout_log": str | null, "stderr_log": str | null }, ... ] }
    /// Schema: docs/json-schemas/ps.schema.json. With `--quiet`, the PIDs,
    /// one per line.
    Ps(cmd::ps::Args),

    /// Stop tracked background processes (TERM, then KILL), or send them
    /// a signal (`-s STOP` freezes one in place).
    Kill(cmd::kill::Args),

    /// Run diagnostics on a lab.
    ///
    /// Per node: each interface's state, MTU, counters and issues, then the
    /// node's other issues; an interface is marked with its worst issue's
    /// severity (INFO, WARN, ERROR, CRITICAL). With `--json`, one object per
    /// node: `{node, interfaces: [{name, state, mtu, rx_bytes, tx_bytes,
    /// issues}], issues}`, each issue `{severity, message}`.
    Diagnose(cmd::diagnose::Args),

    /// Keep a lab's failure bundle: everything needed to debug it.
    ///
    /// `DIR/<lab>/` gets `summary.txt` (the seed and how to replay it),
    /// `topology.nll`, `status.json`, `events.ndjson`, and per node its
    /// addresses, routes, rules, nftables ruleset, qdiscs, conntrack table
    /// and process logs, as JSON from the host's `ip`, `nft` and `tc` run in
    /// the node's namespace. A failing `nlink-lab test` and `#[lab_test]`
    /// keep one by themselves.
    Bundle(cmd::bundle::Args),

    /// Capture packets on an interface using netring.
    Capture(cmd::capture::Args),

    /// Wait for a lab to be deployed, or for conditions in a node.
    ///
    /// `wait -L LAB` waits until LAB is deployed. `wait NODE --tcp 8080
    /// --exec 'curl -sf localhost/health'` waits until every condition
    /// holds in NODE, all within one `--timeout`; `--log`, `--fd-stable`
    /// (and optionally `--listening`) look at a spawned `--pid`.
    Wait(cmd::wait::Args),

    /// Show IP addresses assigned to a node.
    Ip(cmd::ip::Args),

    /// List a lab's tracked flows (its nodes' conntrack tables), or delete
    /// them with `--flush`.
    ///
    /// Filters select by the flow's original direction, as it began
    /// (`--dst` is the address before a DNAT); every filter given must
    /// hold. The NAT REPLY column shows where a translated flow's replies
    /// come from. Deleting a flow makes its next packet start a new one,
    /// through the rules as they are now: what `apply` does for the flows
    /// of the NAT rules it changes.
    ///
    /// JSON OUTPUT (with `--json`):
    ///   { "schema_version": 1, "lab": str,
    ///     "flows": [ { "node": str, "family": "ipv4" | "ipv6", "proto": str,
    ///       "orig": { "src", "dst", "sport", "dport" }, "reply": { … },
    ///       "state": str?, "status": [str], "timeout": int?, "mark": int?,
    ///       "zone": int? }, ... ],
    ///     "deleted": { NODE: int } }  (with --flush)
    /// Schema: docs/json-schemas/conntrack.schema.json. With `--quiet`, the
    /// number of flows listed or deleted.
    Conntrack(cmd::conntrack::Args),

    /// Compare two topology files and show differences.
    Diff(cmd::diff::Args),

    /// A running lab's topology as it is now (after `apply`, `node`, `link`):
    /// TOML, `--nll`, or JSON with `--format json`.
    Export(cmd::export::Args),

    /// Portable lab archives (`.nlz`): `create`, `extract`, `inspect`.
    ///
    /// An archive holds a manifest, the topology, its params, the rendered
    /// topology and checksums: a repro to share. `nlink-lab deploy
    /// FILE.nlz` deploys one.
    Archive(cmd::archive::Args),

    /// Show container logs or per-process logs from `nlink-lab spawn`.
    ///
    /// Without `--pid`: shows the container's stdout/stderr (node must be
    /// a container).  With `--pid`: shows the per-process log file written
    /// by `spawn`. Per-process log files live at:
    ///
    ///   `/var/lib/nlink-lab/labs/<lab>/logs/<node>-<basename>-<pid>.{stdout,stderr}`
    ///
    /// (under `$NLINK_LAB_STATE_DIR` when it is set). The
    /// path is stable; consumers can read it directly.
    Logs(cmd::logs::Args),

    /// Pre-pull all container images from a topology.
    Pull(cmd::pull::Args),

    /// What nodes use (cgroup v2: CPU time, memory, PIDs) — or with
    /// `--pid`, `--procs` or `--pidfile-glob`, what a node's processes use.
    ///
    /// Process samples read `/proc/<pid>/{stat,status,smaps_rollup}` and
    /// count `/proc/<pid>/fd/` (root: the fd directory is 0700); on a
    /// container node they are routed through the runtime. Sum `pss_kb`,
    /// not `rss_kb`, to get a node's memory: RSS counts every shared page
    /// once per process. CPU ticks are `sysconf(_SC_CLK_TCK)` units
    /// (usually 100 per second). `--watch` samples again until Ctrl-C.
    ///
    /// JSON OUTPUT: { "schema_version": 1, "lab", "nodes": [ { "node",
    ///   "kind", "cpu_usec", "memory_bytes", "memory_max", "cpu_max",
    ///   "pids" } ] }, or with process flags { "schema_version": 1, "lab",
    ///   "node", "processes": [ { "node", "host_pid", "ns_pid", "name"?,
    ///   "command", "uid", "rss_kb"?, "vsz_kb"?, "pss_kb"?, "fd_count",
    ///   "cpu_user_ticks", "cpu_kernel_ticks", "started_at_unix_micros",
    ///   "state" } ] }
    /// Schemas: docs/json-schemas/stats.schema.json, stats-procs.schema.json
    Stats(cmd::stats::Args),

    /// Print the shell completion script.
    ///
    /// The script calls back into nlink-lab, so it completes what is on
    /// the host: labs, the nodes and endpoints of the lab on the line (or
    /// the current one), snapshots, tracked PIDs, a topology's `--set`
    /// params, templates, rule ids. Install it with: bash
    /// `source <(nlink-lab completions bash)` (in ~/.bashrc), zsh
    /// `source <(nlink-lab completions zsh)`, fish
    /// `nlink-lab completions fish | source`. `--static` prints a script
    /// that knows only the commands and flags.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
        /// A script that does not call back: commands and flags only.
        #[arg(long = "static")]
        static_script: bool,
    },

    /// Create a topology file from a built-in template.
    Init(cmd::init::Args),
}

/// The top-level help's groups, in order: every visible command is in
/// exactly one (a unit test checks).
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "Lifecycle",
        &[
            "deploy", "apply", "destroy", "use", "ls", "status", "verify", "snapshot", "archive",
            "export",
        ],
    ),
    (
        "Run",
        &[
            "exec", "shell", "spawn", "wait", "ps", "kill", "logs", "stats",
        ],
    ),
    (
        "Network",
        &["impair", "node", "link", "ip", "conntrack", "capture"],
    ),
    ("Observe", &["events", "top", "diagnose"]),
    ("Test", &["test", "scenario", "benchmark", "bundle"]),
    (
        "Author",
        &["validate", "lint", "fmt", "render", "diff", "init", "pull"],
    ),
    ("Host", &["doctor", "completions", "lsp"]),
];

/// A command's support tier (`docs/SUPPORT.md`, #346): what may change, and
/// how it is announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Breaks only with a CHANGELOG Migration note and a release that
    /// still accepts the old form (a removed command or flag says what
    /// replaced it: `removed.rs`).
    Core,
    /// Breaks with a Migration note; tested in CI.
    Supported,
    /// May change or go in any minor release; marked in `--help`.
    Experimental,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Core => "Core",
            Tier::Supported => "Supported",
            Tier::Experimental => "Experimental",
        }
    }
}

/// The core commands: the lab lifecycle, running things in a node, and
/// `impair` (#346). Every other command is supported, unless listed in
/// [`EXPERIMENTAL`].
pub const CORE: &[&str] = &[
    "deploy", "apply", "destroy", "use", "ls", "status", "verify", "exec", "shell", "spawn",
    "wait", "ps", "kill", "logs", "impair", "validate",
];

/// The experimental commands, marked `(experimental)` in `--help`. None
/// today: what is experimental (the desktop app, Wi-Fi) is not a command.
pub const EXPERIMENTAL: &[&str] = &[];

/// A command's tier.
pub fn tier(name: &str) -> Tier {
    if CORE.contains(&name) {
        Tier::Core
    } else if EXPERIMENTAL.contains(&name) {
        Tier::Experimental
    } else {
        Tier::Supported
    }
}

/// A command's one-line summary: the first clause of its about text (what
/// the grouped help and the CLI reference's index show).
pub fn summary(cmd: &clap::Command, name: &str) -> String {
    let about = cmd
        .find_subcommand(name)
        .and_then(|c| c.get_about())
        .map(|a| a.to_string())
        .unwrap_or_default();
    about
        .split("; ")
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_string()
}

/// The top-level help template: the commands under their [`GROUPS`]
/// headings (clap lists subcommands in one block only).
pub fn grouped_help(cmd: &clap::Command) -> String {
    grouped_help_marking(cmd, EXPERIMENTAL)
}

/// [`grouped_help`], with `experimental` marked as such.
fn grouped_help_marking(cmd: &clap::Command, experimental: &[&str]) -> String {
    const MARK: &str = " (experimental)";
    let width = GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter())
        .map(|n| n.len())
        .max()
        .unwrap_or(0);
    // one line each: the first clause, cut to 100 columns
    let room = 100 - width - 4;
    let about = |name: &str| {
        let first = summary(cmd, name);
        let (mark, room) = if experimental.contains(&name) {
            (MARK, room - MARK.len())
        } else {
            ("", room)
        };
        if first.chars().count() <= room {
            format!("{first}{mark}")
        } else {
            let cut: String = first.chars().take(room - 1).collect();
            format!("{}…{mark}", cut.trim_end())
        }
    };
    let mut out = String::from("{before-help}{about-with-newline}\n{usage-heading} {usage}\n");
    for (group, names) in GROUPS {
        out.push_str(&format!("\n{group}:\n"));
        for name in *names {
            // a `{` in an about line would read as a template tag
            let about = about(name).replace('{', "(").replace('}', ")");
            out.push_str(&format!("  {name:<width$}  {about}\n"));
        }
    }
    out.push_str("\nOptions:\n{options}{after-help}");
    out
}

/// What each short flag means, for every command (a unit test checks):
/// one letter, one long name. `docs/cli/README.md` lists them too.
#[cfg(test)]
const SHORT_FLAGS: &[(char, &str)] = &[
    ('c', "count"),
    ('e', "env"),
    ('f', "follow"),
    ('h', "help"),
    ('i', "interval"),
    ('L', "lab"),
    ('o', "output"),
    ('q', "quiet"),
    ('s', "signal"),
    ('t', "timeout"),
    ('v', "verbose"),
    ('V', "version"),
    ('w', "write"),
];

#[cfg(test)]
mod tests {
    /// A short flag means one thing everywhere, and no command has a
    /// flag of a global one's name (it would shadow it).
    #[test]
    fn short_flags_mean_one_thing() {
        fn walk(cmd: &clap::Command, path: &str, globals: &[String], bad: &mut Vec<String>) {
            for a in cmd.get_arguments() {
                if a.is_global_set() && !path.is_empty() {
                    continue;
                }
                if let Some(short) = a.get_short() {
                    let long = a.get_long().unwrap_or("-");
                    match super::SHORT_FLAGS.iter().find(|(c, _)| *c == short) {
                        Some((_, meant)) if *meant == long => {}
                        Some((_, meant)) => bad.push(format!(
                            "`{path}` -{short} is --{long}; -{short} means --{meant}"
                        )),
                        None => bad.push(format!(
                            "`{path}` -{short} (--{long}) is not in SHORT_FLAGS"
                        )),
                    }
                }
                if !path.is_empty()
                    && let Some(long) = a.get_long()
                    && globals.iter().any(|g| g == long)
                {
                    bad.push(format!("`{path}` --{long} shadows the global --{long}"));
                }
            }
            for sub in cmd.get_subcommands() {
                walk(
                    sub,
                    format!("{path} {}", sub.get_name()).trim(),
                    globals,
                    bad,
                );
            }
        }
        let cmd = crate::command();
        let globals: Vec<String> = cmd
            .get_arguments()
            .filter(|a| a.is_global_set())
            .filter_map(|a| a.get_long().map(str::to_string))
            .collect();
        let mut bad = Vec::new();
        walk(&cmd, "", &globals, &mut bad);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn every_command_is_in_one_help_group() {
        let cmd = crate::command();
        let visible: Vec<&str> = cmd
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name())
            .collect();
        let grouped: Vec<&str> = super::GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        for name in &visible {
            let n = grouped.iter().filter(|g| *g == name).count();
            assert_eq!(n, 1, "`{name}` is in {n} help groups");
        }
        for name in &grouped {
            assert!(
                visible.contains(name),
                "group lists `{name}`, no such command"
            );
        }
        let help = cmd.clone().render_help().to_string();
        assert!(
            help.contains("Lifecycle:") && help.contains("  deploy "),
            "{help}"
        );
    }

    /// The tier lists name real commands, none twice (#346).
    #[test]
    fn tier_lists_name_commands() {
        let grouped: Vec<&str> = super::GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        for name in super::CORE.iter().chain(super::EXPERIMENTAL) {
            assert!(
                grouped.contains(name),
                "a tier lists `{name}`, no such command"
            );
        }
        for name in super::CORE {
            assert!(
                !super::EXPERIMENTAL.contains(name),
                "`{name}` is core and experimental"
            );
        }
        assert_eq!(super::tier("deploy"), super::Tier::Core);
        assert_eq!(super::tier("capture"), super::Tier::Supported);
    }

    /// `--help` marks an experimental command, and keeps its line in 100
    /// columns.
    #[test]
    fn help_marks_experimental_commands() {
        let cmd = crate::command();
        let help = super::grouped_help_marking(&cmd, &["lsp", "status"]);
        let line = |name: &str| {
            help.lines()
                .find(|l| l.trim_start().starts_with(&format!("{name} ")))
                .unwrap_or_else(|| panic!("no help line for {name}:\n{help}"))
                .to_string()
        };
        for name in ["lsp", "status"] {
            let l = line(name);
            assert!(l.ends_with(" (experimental)"), "{l}");
            assert!(
                l.chars().count() <= 100,
                "{} columns: {l}",
                l.chars().count()
            );
        }
        assert!(!line("deploy").contains("experimental"));
        if super::EXPERIMENTAL.is_empty() {
            assert!(!super::grouped_help(&cmd).contains("(experimental)"));
        }
    }

    /// Every JSON Schema under `docs/json-schemas/` must be valid JSON,
    /// and every generated one must match what `docs-gen --schemas`
    /// produces from the types (CI runs the same check via git diff).
    #[test]
    fn json_schemas_parse_and_generated_ones_are_current() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/json-schemas");
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                let text = std::fs::read_to_string(&path).unwrap();
                let _: serde_json::Value = serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                files += 1;
            }
        }
        assert!(files >= 10, "{files} schema files found");
        for (stem, schema) in crate::output::all_schemas() {
            let path = dir.join(format!("{stem}.schema.json"));
            let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "{}: {e} (run `nlink-lab docs-gen --schemas docs/json-schemas`)",
                    path.display()
                )
            });
            let generated = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
            assert_eq!(
                on_disk,
                generated,
                "{} is stale: run `nlink-lab docs-gen --schemas docs/json-schemas`",
                path.display()
            );
        }
    }
}
