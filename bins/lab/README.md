# nlink-lab CLI (`nlink-lab-cli`)

Binary crate that provides the `nlink-lab` CLI — a 28-subcommand interface for
creating, managing, and inspecting isolated network topologies on Linux.

> **Package name:** `nlink-lab-cli` (in `Cargo.toml`)
> **Binary name:** `nlink-lab` (what users run)
> Use `cargo build -p nlink-lab-cli` / `cargo clippy -p nlink-lab-cli`.

## Code Structure

```
bins/lab/src/
├── main.rs              # Entry point (~50 lines): tracing, tokio runtime, error display
├── cli.rs               # Clap CLI definitions (Cli struct + Commands enum) — zero logic
├── color.rs             # Terminal color helpers: use_color(), green(), red(), yellow(), bold()
├── output.rs            # Shared output: print_topology_summary(), print_deploy_summary()
├── rendering.rs         # Topology visualization: topology_to_dot(), topology_to_ascii()
├── util.rs              # Utilities: check_root(), force_cleanup(), now_unix()
├── daemon.rs            # Zenoh backend daemon: run_daemon_inline(), diags_to_snapshot()
└── commands/
    ├── mod.rs           # dispatch(cli) — routes Commands variants to handlers
    ├── deploy.rs        # deploy: validate + deploy + optional daemon
    ├── destroy.rs       # destroy: tear down lab(s), namespace cleanup
    ├── apply.rs         # apply: diff + apply topology changes to running lab
    ├── status.rs        # status: list all labs or show one (JSON)
    ├── validate.rs      # validate: parse & validate .nll without deploying
    ├── exec.rs          # exec: run command in a lab node namespace
    ├── shell.rs         # shell: interactive shell in a node
    ├── test.rs          # test: run topology tests with JUnit/TAP output
    ├── impair.rs        # impair: runtime link impairment (delay, jitter, loss, rate)
    ├── graph.rs         # graph + render: DOT / ASCII topology visualization
    ├── diff.rs          # diff: compare two topology files
    ├── export.rs        # export: dump running lab topology (JSON/NLL)
    ├── inspect.rs       # inspect: comprehensive lab details
    ├── capture.rs       # capture: packet capture via tcpdump
    ├── process.rs       # ps + kill: background process management
    ├── diagnose.rs      # diagnose: interface/node diagnostics (JSON)
    ├── containers.rs    # containers, logs, pull, stats, restart
    ├── init.rs          # init: generate topology from built-in templates
    ├── wait.rs          # wait: poll lab status until ready
    ├── daemon_cmd.rs    # daemon: start Zenoh metrics backend
    └── metrics.rs       # metrics: stream live metrics via Zenoh
```

## Module Dependency Graph

```
main.rs
├── cli.rs              (no internal deps)
├── color.rs            (no internal deps)
├── util.rs             (no internal deps)
├── output.rs           (no internal deps)
├── rendering.rs        (no internal deps)
├── daemon.rs           ──> util
└── commands/
    ├── mod.rs          ──> cli
    ├── deploy.rs       ──> color, output, util, daemon
    ├── apply.rs        ──> color, util
    ├── destroy.rs      ──> util
    ├── test.rs         ──> color, util
    ├── diagnose.rs     ──> util
    ├── graph.rs        ──> rendering
    ├── inspect.rs      ──> color
    ├── containers.rs   ──> util
    ├── daemon_cmd.rs   ──> util, daemon
    ├── validate.rs     ──> color, output
    ├── exec.rs         ──> util
    ├── shell.rs        ──> util
    ├── capture.rs      ──> util
    ├── impair.rs       ──> util
    ├── metrics.rs      (no internal deps)
    ├── process.rs      ──> util
    └── (others)        (no internal deps)
```

## Command Properties

| Command | Async | Root | JSON | Handler |
|---------|:-----:|:----:|:----:|---------|
| `deploy` | ✅ | ✅ | — | `deploy::run` |
| `destroy` | ✅ | ✅ | — | `destroy::run` |
| `apply` | ✅ | ✅ | — | `apply::run` |
| `status` | — | — | ✅ | `status::run` |
| `validate` | — | — | — | `validate::run` |
| `exec` | — | ✅ | — | `exec::run` |
| `shell` | — | ✅ | — | `shell::run` |
| `test` | ✅ | ✅ | — | `test::run` |
| `impair` | ✅ | ✅ | — | `impair::run` |
| `graph` | — | — | — | `graph::run_graph` |
| `render` | — | — | ✅ | `graph::run_render` |
| `diagnose` | ✅ | ✅ | ✅ | `diagnose::run` |
| `capture` | — | ✅ | — | `capture::run` |
| `diff` | — | — | ✅ | `diff::run` |
| `export` | — | — | ✅ | `export::run` |
| `inspect` | — | — | ✅ | `inspect::run` |
| `ps` | — | — | ✅ | `process::run_ps` |
| `kill` | — | ✅ | — | `process::run_kill` |
| `containers` | — | — | ✅ | `containers::run_list` |
| `logs` | — | — | — | `containers::run_logs` |
| `pull` | — | — | — | `containers::run_pull` |
| `stats` | — | — | — | `containers::run_stats` |
| `restart` | — | ✅ | — | `containers::run_restart` |
| `daemon` | ✅ | ✅ | — | `daemon_cmd::run` |
| `metrics` | ✅ | — | — | `metrics::run` |
| `init` | — | — | — | `init::run` |
| `wait` | ✅ | — | — | `wait::run` |
| `completions` | — | — | — | Handled directly in `main.rs` |

## Conventions

- **One entry point per command file:** `pub(crate) [async] fn run(...)`.
  Grouped commands (graph/render, ps/kill, container ops) use `run_<variant>()`.
- **Parameters are destructured fields** from `Commands` variants plus global
  flags (`json`, `quiet`) passed from `dispatch()`.
- **`pub(crate)` visibility** on all items in every module.
- **`check_root()`** is called at the start of commands that need root/`CAP_NET_ADMIN`.
- **`process::exit()`** is used in some handlers (`exec`, `shell`, `capture`,
  `restart`, `logs`) for process replacement — this is intentional.

## Future Improvements

These improvements were identified in [#1](https://github.com/p13marc/nlink-lab/issues/1)
as follow-up work after the initial refactoring:

### `OutputContext` struct

Many commands receive `json`, `quiet`, and `verbose` as individual parameters.
A shared context struct would reduce parameter lists:

```rust
pub(crate) struct OutputContext {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
}
```

### Shared validation helper

The parse → validate → report-warnings → fail-on-errors pattern is duplicated
across `deploy`, `apply`, and `validate`. Extract into a single helper:

```rust
// util.rs or output.rs
pub(crate) fn validate_topology(topo: &Topology) -> nlink_lab::Result<()>;
```

### Shared node-existence check

`exec` and `shell` both validate that a node exists in a lab with identical
code. Extract as a shared helper in `util.rs`.

### Replace `process::exit()` with proper errors

Several handlers use `std::process::exit(1)` instead of returning errors.
Replacing these would improve composability, but requires careful behavioral
review since some (`exec`, `shell`) use `exec()` syscall for process replacement.

### Unit tests

The CLI modules now have `pub(crate)` boundaries that enable unit testing.
Priority candidates:
- `rendering.rs` — DOT/ASCII output for known topologies
- `color.rs` — color function behavior with/without `NO_COLOR`
- `output.rs` — summary formatting
- `util.rs` — timestamp, root check behavior
