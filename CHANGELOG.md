# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
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
