# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

(empty — entries land here as the next release accumulates)

## [0.3.1] - 2026-05-03

Patch release for one bug in 0.3.0. No API changes.

### Fixed
- `nlink-lab impair --show --json` returned `endpoints: {}` for any
  topology built around bridge networks. The first cut only walked
  `topology.links`, so `network { members [...] }`-style endpoints
  were invisible. `collect_impair_show` now collects from
  `links` + `networks.members` + declared `impairments` keys via a
  new pure helper `nlink_lab::impair_parse::topology_endpoints`.
  Two unit tests cover the multi-source collection and a
  network-only topology; a root-gated integration test
  (`impair_show_includes_network_members`) deploys
  `examples/vlan-trunk.nll` and verifies end-to-end that a
  partitioned bridge member's qdisc is visible. Regression guard
  for the harness team's 3-machine config.
  (round-4 §3 follow-up)

## [0.3.0] - 2026-05-03

The "round-4 harness feedback" release — three small PRs from Plan
156 fixing the partition-cycle silent no-op, adding `exec --timeout`,
and adding `impair --show --json`. Together they let the
`des-test-harness` team revert their `--loss 100%` workaround and
their host-side `Command + child.kill()` deadline plumbing.

### Added
- `nlink-lab exec --timeout SECS` — bound the wall-clock time a command
  may run. On expiry the child is sent SIGTERM, then SIGKILL after a
  1-second grace period. Exit code 124 on timeout (matches
  `coreutils timeout(1)`). The CLI prints
  `nlink-lab exec: command timed out after Ns` to stderr. New
  `ExecOpts::timeout: Option<Duration>` field plumbs the value
  through `exec_with_opts` and `exec_attached_with_opts`. New
  `Error::Timeout(Duration)` variant for library consumers.
  (Plan 156 PR B — round-4 §2)
- `nlink-lab impair --show --json` — structured per-endpoint view of
  installed netem state, replacing grep-against-`tc`-text for harness
  consumers. One row per endpoint declared in the topology;
  endpoints with no qdisc serialize as `null`. Each row carries
  `qdisc`, `delay_ms`, `jitter_ms`, `loss_pct`, `rate_bps` (omitted
  when not set), plus a `partition` flag tracking the partition/heal
  lifecycle (distinct from a user installing `--loss 100%`
  directly). New library helper
  `RunningLab::is_partitioned(endpoint)` and pure parser
  `nlink_lab::impair_parse::parse_tc_qdisc_show`. Schema:
  `docs/json-schemas/impair-show.schema.json`.
  (Plan 156 PR C — round-4 §3)
  > **Known bug in 0.3.0**: `endpoints` always returned `{}` for
  > topologies built around bridge networks (the harness team's
  > 3-machine config). The collector only walked `topology.links`
  > and missed `network { members [...] }`-style endpoints
  > entirely. Use 0.3.1 or later for this feature.

### Fixed
- `nlink-lab impair --partition` is no longer a silent no-op on the
  second invocation after `--clear`. `clear_impairment` now prunes
  the endpoint's entry from `saved_impairments` (so the next
  `partition` doesn't short-circuit on the stale "is partitioned"
  flag) and persists state. It is also now idempotent on
  `QdiscNotFound` from the kernel — a missing qdisc is treated as
  "already cleared" instead of erroring. Together this makes
  partition→clear→partition→clear cycles work reliably; previously
  cycle 2's `partition` printed success but installed nothing, and
  cycle 2's `clear` crashed. (Plan 156 PR A — round-4 §1)

### Changed
- **Library API**: `RunningLab::clear_impairment` is now `&mut self`
  (was `&self`) — necessary for the partition-cycle fix above. All
  in-tree callers were already passing `mut RunningLab`; external
  callers (none we know of) need to pass `mut`.

## [0.2.0] - 2026-04-30

The "documentation + reconcile" release. Two big arcs landed since
0.1.0:

1. **Documentation overhaul (Plans 150–154).** README rewritten
   to lead with the wedge, 11 cookbook recipes paired with runnable
   `examples/cookbook/*.nll`, full CLI reference, `COMPARISON.md`
   (vs containerlab), `ARCHITECTURE.md` (contributor on-ramp),
   60-minute USER_GUIDE walkthrough, TROUBLESHOOTING expanded
   193→416 LOC, doc-CI gate.
2. **`apply` reconcile completeness.** Editing any non-process
   topology field (per-endpoint impair, network-level per-pair
   impair, routes, sysctls, rate-limits, nftables, NAT) now
   converges in place via `nlink-lab apply` — no destroy + redeploy
   for non-structural edits. Backed by nlink 0.15.1's
   `PerPeerImpairer::reconcile()`. New `--check` drift gate exits
   non-zero if live state differs from NLL; new `--json` structured
   diff for CI consumption.

Plus: lab portability (`.nlz` archives, Plan 153), library-first
testing polish (`#[lab_test]` `set` / `timeout` / `capture = true`,
Plan 154), per-pair network impair (`impair A -- B { … }` inside
`network`, Plan 128), `for` loops inside network blocks, plus the
round-3 polish from Plan 155 (workdir, status --scan stale
detection, destroy --orphans, spawn --wait-log, ps --alive-only,
ExecOpts/SpawnOpts, JSON schemas).

### Notable bug fixes (this release)

- **Bridge naming collision** (hash-based `nb{hash8}` replaces
  `{prefix}-{net_name}[..15]`). The old truncation silently
  collided whenever the lab prefix grew long enough — surfaced
  by the `#[lab_test]` macro's name-rewriting.
- **Zombie processes treated as alive**: `process_status` now
  reads `/proc/<pid>/stat` and treats state `Z` as not-alive.
  `kill(pid, 0)` returns 0 for zombies; before this fix,
  quick-exiting children stayed "alive" forever from the lab's
  POV.
- **Builder `.port(node, |p| p.interface("eth0"))`** now
  auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, builder-DSL labs silently produced empty
  `members` and a missing veth at deploy time.

### Fixed
- `nlink-lab spawn --env KEY=VALUE` no longer changes the per-process
  log file basename to `env`. Previously the CLI implemented `--env` by
  prepending `/usr/bin/env K=V` to the user's command; the log basename
  is derived from `argv[0]`, so consumers that reconstructed log paths
  from the binary name silently broke. Env vars are now applied via
  `Command::env(k, v)` directly. (Plan 155 PR B — round-3 §3.1)
- `nlink-lab capture -w <pcap>` no longer produces a 0-byte pcap when
  the capture process is terminated by SIGTERM (e.g., `timeout(1)`'s
  default signal) or SIGKILL. The pcap writer now flushes after every
  packet, matching `tcpdump -U`. The CLI also installs a SIGTERM
  handler alongside the existing SIGINT handler so the loop exits
  cleanly and prints the summary line. (Plan 155 PR A — round-3 §2.1)

### Added

#### From Plans 150–154 (this session)

- **Per-pair network impairment**: `impair A -- B { delay … loss …
  rate-cap … }` inside `network { }` blocks, modeling
  distance-dependent radio/satellite/multipoint paths on a shared
  L2. Built on nlink 0.15.1's `PerPeerImpairer`. Deploy step 14b
  builds one HTB+netem+flower TC tree per source interface.
  (Plan 128)
- **`for` loops inside `network { }`** with full arithmetic
  (`${(i+1) % 12}`), modulo, and nested loops (Cartesian product
  expansion). The 12-node satellite-mesh cookbook example uses
  this to generate 32 directional impair rules from ~25 lines of
  NLL.
- **`apply` reconcile completeness** (Plan 152): network-level
  per-pair impair (Phase A), per-node static routes (B/1),
  per-node sysctls (B/2), per-endpoint rate-limits (B/3), per-node
  nftables firewall + NAT (B/4 — atomic flush + rebuild). Phase C
  added `apply --check` (drift gate) and `apply --json --dry-run`
  (structured diff for CI).
- **`.nlz` lab archive** for repros and sharing (Plan 153):
  - `nlink-lab export --archive <lab|.nll> [-o file.nlz]` with
    `--include-running-state`, `--no-rendered`, `--set`.
  - `nlink-lab import file.nlz` — verifies SHA-256 checksums,
    extracts, validates, deploys.
  - `nlink-lab inspect FILE.nlz` — manifest + node/link/network
    counts without extracting.
  - Format: gzipped tarball with `manifest.json` + `topology.nll`
    + optional `params.json` / `rendered.toml` / `state.json`.
    `format_version = 1`.
- **`#[lab_test]` macro polish** (Plan 154):
  - `set { key = "value" }` — apply NLL `param` overrides.
  - `timeout = N` — wrap test body in `tokio::time::timeout`.
  - `capture = true` — start parallel pcaps on every (namespace,
    iface). On panic, persist to
    `target/lab_test_captures/<test>-<pid>/`. On success, discard.
  - `nlink_lab::test_helpers::LabCapture` helper drives the
    implementation.
- **Documentation overhaul** (Plan 150):
  - README rewritten leading with the wedge.
  - `docs/COMPARISON.md` (honest vs containerlab) and
    `docs/ARCHITECTURE.md` (contributor on-ramp).
  - `docs/cookbook/` with 11 recipes: satellite-mesh,
    multi-tenant-wan, vrf-multitenant, wireguard-mesh,
    macvlan-host-bridge, nftables-firewall, bridge-vlan-trunk,
    p2p-partition, iperf3-benchmark, healthcheck-depends-on,
    parametric-imports, ci-matrix-sweep, lab-portability,
    rust-integration-test.
  - `docs/cli/` with 29 reference pages (8 hand-crafted +
    21 auto-stubs).
  - `docs/USER_GUIDE.md` 60-minute guided walkthrough that builds
    one realistic site-to-site WAN progressively (786→1160 LOC).
  - `docs/TROUBLESHOOTING.md` expanded 193→416 LOC with apply,
    archive, scenario, library-test, and common-misconfig
    sections.
  - Doc-CI gate: `every_nll_snippet_in_docs_parses` and
    `internal_doc_links_resolve` lib tests catch drift on every
    PR.

#### From Plan 155 (round-3 polish)

- `nlink-lab spawn --wait-log <REGEX>` — block the spawn until a line
  matching REGEX appears in the spawned process's captured
  stdout/stderr, mirroring `--wait-tcp` for services that signal
  readiness via a log line rather than a port. `--wait-log-stream`
  selects which stream to watch (`stdout` / `stderr` / `both`,
  default `both`). `--wait-log` and `--wait-tcp` AND-compose: both
  must succeed before spawn returns. Library: new
  `RunningLab::wait_for_log_line(pid, regex, LogStream, timeout,
  interval)`. (Plan 155 PR E — round-3 §4.2)
- `nlink-lab ps --alive-only` flag (and library helper
  `RunningLab::process_status_alive_only`) that filters out tracked
  processes whose PID has exited. Useful for "is X still running?"
  polling loops where the default retention behaviour (exited entries
  remain in the listing with `alive: false`) is a footgun. The default
  `ps` behaviour is unchanged. (Plan 155 PR C — round-3 §3.2)
- `nlink_lab::ExecOpts` and `nlink_lab::SpawnOpts` — borrow-based
  option structs for `RunningLab::exec_with_opts`,
  `exec_attached_with_opts`, and `spawn_with_logs_with_opts`. Carry
  `workdir` and `env` (plus `log_dir` for spawn). Existing `exec`,
  `exec_in`, `exec_attached`, `exec_attached_in`, `spawn_with_logs`,
  `spawn_with_logs_in` methods are now thin wrappers over these — no
  caller break. (Plan 155 PR B)
- JSON output schemas for the four high-traffic shapes under
  `docs/json-schemas/`: `deploy`, `status` (list + scan variants),
  `spawn`, `ps`. Hand-written draft-07 schemas; the source of truth
  remains the code. Linked from `--json` `--help`. (Plan 155 PR D —
  round-3 §5.1)
- `nlink-lab exec --workdir <dir>` and `nlink-lab spawn --workdir <dir>`
  — set the working directory of the child. For namespace nodes this is
  `chdir()` on the host filesystem (namespace nodes share the host mount
  namespace); for container nodes it's passed as `-w` to the runtime.
  Library: new `exec_in`, `exec_attached_in`, `spawn_with_logs_in`
  methods on `RunningLab`; the existing zero-workdir methods delegate
  to these with `None`.
- `nlink-lab status --scan` now also reports **stale** labs — state files
  claiming namespaces that no longer exist on the host (typical after a
  reboot or WSL restart). Human output lists missing namespaces and
  suggests `destroy <lab>`; `--json` adds a `stale` array alongside
  `bridges`/`veths`/`netns`.
- `nlink-lab destroy --orphans` — reap host resources (mgmt bridges,
  veth peers, named namespaces) that match the lab naming scheme but
  have no `state.json`. Left behind by crashed deploys. Composes with
  `--all` (clean state-backed labs + orphans) or runs standalone.
- `nlink-lab status --scan` — scan the host for the same set and report
  anything unaccounted for. Prints nothing when clean; otherwise names
  each resource and suggests `destroy --orphans`. `--json` emits
  `{ labs, orphans }` instead of the labs list alone.
- NLL DSL as sole topology format (TOML removed)
- `InterfaceKind` enum replacing string-based interface types
- Shell completions for bash, zsh, fish, powershell
- `--json` global flag for machine-readable CLI output
- `--dry-run` flag on deploy command
- `Lab::build_validated()` method for early error detection
- `validate_interface_name()` helper for Linux IFNAMSIZ enforcement
- 4 new validation rules: interface-name-length, wireguard-peer-exists,
  vrf-table-unique, duplicate-link-endpoint (18 rules total)
- NLL: `burst` on rate limits, `env`/`volumes`/`runtime` for containers,
  multiple addresses on WG/dummy/vxlan, spaceless interpolation `${i+1}`
- Duplicate node/network name detection in NLL lowering
- Asymmetric impairment example
- `getrandom` for safe WireGuard key generation (no more panic)
- `time` crate for ISO 8601 timestamps
- Atomic state file writes (temp + rename)

### Changed

#### From Plans 150–154 (this session)

- **Upgraded `nlink` 0.13.0 → 0.15.1.** Mostly additive; the typed
  `*Config::parse_params` rollout, new
  `nlink::netlink::impair::PerPeerImpairer` helper (which Plan 128
  consumes), legacy `nlink::tc::builders::*` deletion (we never
  used it). MSRV bumped to 1.85 (already required by edition 2024).
- **Bridge naming**: shared L2 bridges now use
  `network_bridge_name_for(net_name) → "nb{hash8}"` (10 chars,
  always within IFNAMSIZ). Replaces the previous
  `{prefix}-{net_name}[..15]` truncation that silently collided
  whenever the lab prefix grew long enough. Mirrors the existing
  Plan-149 fix for veth peer names. Internal — no caller-visible
  change.
- **Process liveness check** now treats zombie state (`Z` in
  `/proc/<pid>/stat`) as not-alive. Previously `kill(pid, 0) == 0`
  reported zombies as alive forever, since `spawn_with_logs`
  drops its `Child` without `wait()`-ing. Affects
  `RunningLab::process_status` and `process_status_alive_only`.
- **Builder DSL**: `.port(node, |p| p.interface("eth0").address(...))`
  now auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, callers had to write both `.member(...)` and
  `.port(...)` separately — and forgetting `.member` produced
  empty members and a missing veth at deploy time. The deploy
  step's address-application pass also handles bare-`node` port
  keys now (using `port.interface` for the iface name) so both
  builder and NLL keying styles work.
- **`for` loops inside `network` blocks** use the same
  `interpolate()` engine as the lower stage (now `pub(crate)`),
  so arithmetic and nested vars work consistently across loop
  forms.

#### From Plan 155 + earlier (existing Unreleased)

- Upgraded `nlink` dependency from 0.12.2 to 0.13.0. Internal only —
  no behavioural change. `NetemConfig::rate_bps(u64)` was removed in
  favour of `rate(Rate)`; `NetemConfig::{loss,corrupt,reorder}` and
  `RateLimiter::{egress,ingress}` now take typed `Percent`/`Rate`
  wrappers instead of `f64`/`&str`. `del_qdisc`/`change_qdisc` take
  `TcHandle` (use `TcHandle::ROOT` in place of `"root"`).
- `nlink-lab logs --pid <pid> --follow` now actually follows. Previously
  `--follow` was silently dropped on the `--pid` path (container logs
  were the only case it worked for). The CLI now implements `tail -F`
  semantics: print the existing tail, then poll the log file for new
  bytes, reopening from offset 0 if truncation/rotation is detected.
  `--tail N` is honoured for the initial dump as before.
- `nlink-lab exec` (non-JSON mode) now streams stdio live. Previously it
  captured the full stdout/stderr into buffers and printed them only
  after the child exited, which made it unusable for services,
  `tail -f`, `ping`, and any other long-running command. `--json` still
  returns structured `{ exit_code, stdout, stderr, duration_ms }`.
  `RunningLab::exec_attached(node, cmd, args)` exposes the streaming
  path for library callers.

### Fixed
- `nlink-lab shell` no longer fails with
  `nsenter: neither filename nor target pid supplied for ns/net` — the
  nsenter invocation now passes `--net=<path>` as a single argv entry
  instead of two entries, which nsenter was misparsing as the bare
  `--net` flag with a stray command argument.
- Bridge-network peer names no longer collide when two networks share a
  4-char prefix (e.g. `lan_a` / `lan_b`). Previously the mgmt-side veth
  peer was named `br{net_name[..4]}p{idx}`, which collapsed both names
  to `brlan_p{idx}` and failed the second `add_link` with EEXIST. Peer
  names are now `np{hash8}{idx}`, derived from a DJB2 hash of the
  network name — deterministic, within the 15-char IFNAMSIZ budget, and
  exposed as `nlink_lab::network_peer_name_for`.
- Veth-creation errors for bridge networks now name the mgmt-side peer
  interface as well as the node-side endpoint, so an EEXIST is no
  longer misattributed to whichever name the user typed.
- Rate limiting now applies to both link endpoints (was left-only)
- Bare integer tokens rejected as node names
- Division by zero in interpolation now logs error
- Firewall: unrecognized match expressions now error instead of silently passing
- Removed no-op `replace()` call in NLL diagnostics
- All actionable compiler warnings resolved

## [0.1.0] - 2026-03-22

Initial release.

- Core lab engine: parse, validate, deploy, destroy
- NLL and TOML topology formats
- 14 validation rules
- 18-step deployment sequence
- VRF, WireGuard, bond, VLAN, VXLAN, bridge support
- `#[lab_test]` proc macro for integration testing
- 12 built-in templates via `nlink-lab init`
- Runtime impairment modification, diagnostics, packet capture
- DOT graph output, process management
- Container node support (Docker/Podman)
