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
