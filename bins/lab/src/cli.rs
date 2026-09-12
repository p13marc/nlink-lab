//! The `Commands` enum — one variant per subcommand. Doc comments and
//! `#[arg]`/`#[command]` attributes here and on each `cmd::*::Args`
//! struct are the help text (`docs/cli/*.md` is generated from them).

use clap::Subcommand;

use crate::cmd;

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
    Exec(cmd::exec::Args),

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
    Spawn(cmd::spawn::Args),

    /// Validate a topology file without deploying.
    Validate(cmd::validate::Args),

    /// Edit a running lab in place: add/remove nodes and links, set impairments.
    ///
    /// The edits are applied to the lab's stored topology and reconciled
    /// with `apply`, so the state file stays the source of truth; use
    /// `render`/`export` afterwards to get the updated NLL.
    Edit(cmd::edit::Args),

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

    /// Check a running lab for drift against its topology (exit 2 on drift).
    ///
    /// Compares every node's live links/addresses/routes/nftables with
    /// what the topology declares — the same view `apply --check` shows —
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

    /// Modify link impairment at runtime.
    ///
    /// JSON OUTPUT (with `--show --json`):
    ///   `{ "lab": str, "endpoints": { "<node>:<iface>": { ... } | null } }`
    /// Schema: docs/json-schemas/impair-show.schema.json
    ///
    /// Without `--show`, applies impairment changes; output is plain
    /// confirmation text.
    #[command(group = clap::ArgGroup::new("impair_mode").args(["show", "clear", "partition", "heal"]).multiple(false))]
    Impair(cmd::impair::Args),

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
    ///   [ { "node": str, "pid": int, "host_pid": int, "alive": bool,
    ///       "stdout_log": str | null, "stderr_log": str | null }, ... ]
    /// Schema: docs/json-schemas/ps.schema.json
    Ps(cmd::ps::Args),

    /// Kill a tracked background process.
    Kill(cmd::kill::Args),

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
    ProcStat(cmd::proc_stat::Args),

    /// Run diagnostics on a lab.
    Diagnose(cmd::diagnose::Args),

    /// Capture packets on an interface using netring.
    Capture(cmd::capture::Args),

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
    WaitFor(cmd::wait_for::Args),

    /// Show IP addresses assigned to a node.
    Ip(cmd::ip::Args),

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
    Containers(cmd::containers::Args),

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
    Logs(cmd::logs::Args),

    /// Pre-pull all container images from a topology.
    Pull(cmd::pull::Args),

    /// Show container resource usage.
    Stats(cmd::stats::Args),

    /// Restart a container node.
    Restart(cmd::restart::Args),

    /// Generate shell completions.
    ///
    /// Static completions for the given shell. For completions that know
    /// deployed lab, node and rule names, source the dynamic form instead:
    /// bash `source <(COMPLETE=bash nlink-lab)`, zsh
    /// `source <(COMPLETE=zsh nlink-lab)`, fish
    /// `COMPLETE=fish nlink-lab | source`.
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
