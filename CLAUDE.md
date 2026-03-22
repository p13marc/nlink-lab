# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Project Overview

nlink-lab is a Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. It is built on top of the [nlink](https://github.com/p13marc/nlink)
library for netlink operations.

**Key design decisions:**
- Library-first architecture — `crates/nlink-lab` is the core, `bins/lab` is a thin CLI
- Declarative TOML topology files + programmatic Rust builder DSL
- Pure Linux namespaces — no Docker/container runtime needed
- Deep networking control via nlink (TC, nftables, WireGuard, VRF, etc.)
- Rust edition 2024

## Build Commands

```bash
cargo build                       # Build everything
cargo build -p nlink-lab          # Build the library
cargo build -p nlink-lab-cli      # Build the CLI binary
cargo test -p nlink-lab --lib     # Run library unit tests
cargo test                        # Run all tests
```

## Usage

```bash
# Validate a topology file
nlink-lab validate examples/simple.toml

# Deploy a lab (requires root)
sudo nlink-lab deploy examples/simple.toml

# Destroy a lab
sudo nlink-lab destroy simple

# Execute in a lab node
sudo nlink-lab exec simple router -- ip addr

# Show running labs
nlink-lab status
```

## Architecture

```
crates/nlink-lab/src/
  lib.rs            # Public API and re-exports
  types.rs          # Topology types (Topology, Node, Link, Impairment, etc.)
  parser.rs         # TOML parser (toml::from_str → Topology)
  error.rs          # LabError type
  validator.rs      # Topology validation (TODO)
  deploy.rs         # Deployer — 18-step deployment sequence (TODO)
  running.rs        # RunningLab — interact with deployed lab (TODO)
  state.rs          # State persistence (~/.nlink-lab/) (TODO)
  builder.rs        # Rust builder DSL (TODO)

bins/lab/src/
  main.rs           # CLI binary (clap)

examples/
  simple.toml       # Minimal 2-node topology
```

## Key Types

| Type | Description |
|------|-------------|
| `Topology` | Top-level container — deserialized from TOML |
| `LabConfig` | Lab metadata (name, description, prefix) |
| `Profile` | Reusable node template (sysctls, firewall) |
| `Node` | Network namespace definition |
| `Link` | Point-to-point veth connection |
| `Network` | Shared L2 bridge segment |
| `Impairment` | Netem config (delay, jitter, loss, rate) |
| `RateLimit` | Per-interface traffic shaping |
| `FirewallConfig` | nftables rules |
| `ExecConfig` | Process to spawn in namespace |
| `EndpointRef` | Parsed "node:interface" reference |

## TOML Topology Format

```toml
[lab]
name = "my-lab"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.r1]
profile = "router"

[nodes.h1]

[nodes.h1.routes]
default = { via = "10.0.0.1" }

[[links]]
endpoints = ["r1:eth0", "h1:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]

[impairments."r1:eth0"]
delay = "10ms"
jitter = "2ms"
```

## Deployment Sequence

The deployer executes these steps in order:

```
 1. Parse topology file → Topology
 2. Validate (bail on errors)
 3. Create namespaces
 4. Create bridge networks (if any)
 5. Create veth pairs spanning namespaces
 6. Create additional interfaces (vxlan, bond, vlan, wireguard)
 7. Assign interfaces to bridges/bonds
 8. Configure VLANs on bridge ports
 9. Set interface addresses
10. Bring interfaces up
11. Apply sysctls per namespace
12. Add routes per namespace
13. Apply nftables rules per namespace
14. Apply TC qdiscs/impairments per interface
15. Apply rate limits
16. Spawn background processes
17. Run validation (connectivity checks)
18. Write state file
```

## Dependencies

- **nlink** — Linux netlink library (namespaces, links, TC, nftables, routing)
- **serde + toml** — TOML parsing
- **clap** — CLI argument parsing
- **tokio** — Async runtime
- **thiserror** — Error types

## Design Documents

- `docs/NLINK_LAB.md` — Full design document (topology DSL, architecture, roadmap)
- `docs/NLINK_LAB_READINESS_REPORT.md` — nlink library readiness assessment
- `docs/plans/` — Implementation plans with progress tracking
