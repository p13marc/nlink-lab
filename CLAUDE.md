# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Project Overview

nlink-lab is a Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. It is built on top of the [nlink](https://github.com/p13marc/nlink)
library for netlink operations.

**Key design decisions:**
- Library-first architecture — `crates/nlink-lab` is the core, `bins/lab` is a thin CLI
- NLL DSL for topology definitions + programmatic Rust builder DSL
- NLL supports loops, variables, imports for composable topologies
- Pure Linux namespaces — no Docker/container runtime needed (containers optional)
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
nlink-lab validate examples/simple.nll

# Deploy a lab (requires root)
sudo nlink-lab deploy examples/simple.nll

# Destroy a lab
sudo nlink-lab destroy simple

# Execute in a lab node
sudo nlink-lab exec simple router -- ip addr

# Show running labs
nlink-lab status

# Expand loops/variables and print flat NLL
nlink-lab render examples/spine-leaf.nll
```

## Architecture

```
crates/nlink-lab/src/
  lib.rs            # Public API and re-exports
  types.rs          # Topology types (Topology, Node, Link, Impairment, etc.)
  parser/
    mod.rs          # Parse entry points (parse, parse_file)
    nll/
      mod.rs        # NLL public API: parse(), parse_file_with_imports()
      lexer.rs      # logos-based lexer (typed tokens: CIDR, Duration, Rate, etc.)
      ast.rs        # AST types (imports, statements, before lowering)
      parser.rs     # Recursive-descent parser → AST
      lower.rs      # AST → Topology (imports, loops, variables, lowering)
  error.rs          # Error types (includes NllDiagnostic for miette)
  validator.rs      # Topology validation (20 rules)
  render.rs         # Topology → NLL serializer (for `render` command)
  dns.rs            # DNS /etc/hosts generation, injection, removal
  test_runner.rs    # CI test runner (deploy→validate→destroy) with JUnit/TAP output
  scenario.rs       # Timed scenario execution engine (fault injection + validation)
  deploy.rs         # Deployer — 18-step deployment sequence
  running.rs        # RunningLab — interact with deployed lab
  state.rs          # State persistence (~/.nlink-lab/)
  builder.rs        # Rust builder DSL
  templates/        # Built-in topology templates for `nlink-lab init`

bins/lab/src/
  main.rs           # CLI binary (clap)

examples/
  *.nll             # NLL topology examples (28 files)
  imports/          # Import composition and parametric module examples
```

## Key Types

| Type | Description |
|------|-------------|
| `Topology` | Top-level container — parsed from NLL or built programmatically |
| `LabConfig` | Lab metadata (name, description, prefix, version, author, tags, dns) |
| `DnsMode` | DNS resolution mode (Off, Hosts) |
| `Profile` | Reusable node template (sysctls, firewall) |
| `Node` | Network namespace or container definition |
| `Link` | Point-to-point veth connection |
| `Network` | Shared L2 bridge segment |
| `Impairment` | Netem config (delay, jitter, loss, rate) |
| `RateLimit` | Per-interface traffic shaping |
| `FirewallConfig` | nftables rules (with src/dst matching) |
| `ExecConfig` | Process to spawn in namespace |
| `EndpointRef` | Parsed "node:interface" reference |
| `VrfConfig` | VRF routing table configuration |
| `WireguardConfig` | WireGuard tunnel configuration |
| `RouteConfig` | Route entry (via, dev, metric) |
| `MacvlanConfig` | macvlan interface (name, parent, mode, addresses) |
| `IpvlanConfig` | ipvlan interface (name, parent, mode, addresses) |
| `ContainerRuntime` | Docker/Podman selection (auto, docker, podman) |

## NLL Topology Format

NLL (nlink-lab Language) is the topology DSL for nlink-lab.

```nll
lab "simple"

profile router { forward ipv4 }

node router : router
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}
```

NLL supports `for` loops (integer ranges and list iteration), `let` variables,
`${interpolation}` (compound arithmetic, modulo, ternary conditionals),
`import` for composition (with parametric modules), `defaults` blocks,
`/* block comments */`, subnet auto-assignment, named subnet pools
(`pool fabric 10.0.0.0/16 /30`), topology patterns (`mesh`, `ring`, `star`),
cross-references (`${node.iface}`), multi-profile inheritance,
inline impairments (`->` / `<-`), profiles, firewall (with `src`/`dst`
matching), VRF, WireGuard, VXLAN, containers (with cpu/memory limits,
capabilities, health checks with `interval`/`timeout`/`retries`,
depends-on, config injection, overlay), reachability assertions
(`validate { reach a b }`), management network (`mgmt` in lab block),
DNS resolution (`dns hosts` auto-generates `/etc/hosts` entries),
macvlan/ipvlan (attach nodes to host physical interfaces),
rich validation assertions (`tcp-connect`, `latency-under`, `route-has`,
`dns-resolves`), timed scenarios for fault injection (`scenario` block
with `at`, `down`, `up`, `clear`, `validate`), and network (bridge) blocks.

Nested interpolation works: `${leaf${i}.eth0}` resolves inner `${i}` first.
Pool exhaustion is detected and errors at parse time.
State locking via flock prevents concurrent deploy/destroy on the same lab.

CLI commands (29 total): `deploy`, `destroy` (with `--all`), `apply`,
`status`, `exec`, `shell` (interactive TTY), `validate`, `test`
(`--junit`, `--tap`, `--fail-fast`), `render`
(`--json`, `--dot`, `--ascii`), `inspect` (combined view), `impair`,
`graph`, `diagnose` (`--json`), `capture`, `diff`, `export`, `wait`,
`ps`, `kill`, `init`, `completions`, `daemon`, `metrics`,
`containers`, `logs` (`--follow`, `--tail`), `pull`, `stats`, `restart`.

Global flags: `--json`, `--verbose`, `--quiet`, `--skip-validate`.

See `docs/NLL_DSL_DESIGN.md` for the full language specification.

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
15b. Inject /etc/hosts entries (if `dns hosts`)
16. Spawn background processes
17. Run validation (connectivity checks)
18. Write state file
```

## Dependencies

- **nlink** — Linux netlink library (namespaces, links, TC, nftables, routing)
- **serde + toml** — State serialization (toml for state files, not topology input)
- **logos** — NLL lexer (derive-macro lexer with typed tokens)
- **miette** — Rich error diagnostics for NLL parse errors
- **clap + clap_complete** — CLI argument parsing and shell completions
- **tokio** — Async runtime
- **thiserror** — Error types
- **x25519-dalek + getrandom** — WireGuard key generation
- **time** — ISO 8601 timestamps

## Design Documents

- `docs/NLL_DSL_DESIGN.md` — NLL language specification and examples
- `docs/NLINK_LAB.md` — Full design document (topology DSL, architecture, roadmap)
- `docs/plans/` — Active implementation plans
