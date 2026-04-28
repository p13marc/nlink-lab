# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
