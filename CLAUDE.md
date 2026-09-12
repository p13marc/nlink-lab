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

# Deploy with CLI parameters
sudo nlink-lab deploy topology.nll --set wan_delay=50ms --set wan_loss=0.1%

# Destroy a lab
sudo nlink-lab destroy simple

# Reap host resources left by a crashed deploy (mgmt bridges/veths/ns
# with no state file). Can also be combined with --all.
sudo nlink-lab destroy --orphans

# List running labs; --scan also reports orphans detected on the host
nlink-lab status
nlink-lab status --scan

# Execute in a lab node (stdio streams live — good for services, tail -f, ping)
sudo nlink-lab exec simple router -- ip addr

# Execute with JSON output (buffered; exit_code, stdout, stderr, duration_ms)
sudo nlink-lab exec --json simple router -- ip addr

# Spawn a background process (tracked by ps/kill)
sudo nlink-lab spawn simple server -- /usr/bin/my-service --port 8080

# Wait for a service to be ready
sudo nlink-lab wait-for simple server --tcp 127.0.0.1:8080 --timeout 30

# Show node IP addresses (including mgmt0 for host-reachable labs)
nlink-lab ip simple server --iface eth0
nlink-lab ip simple server --iface mgmt0

# Deploy with JSON output (machine-parseable)
sudo nlink-lab deploy --json examples/simple.nll

# Show running labs
nlink-lab status

# Validate with resolved IP addresses
nlink-lab validate --show-ips examples/multi-site.nll

# Expand loops/variables and print flat NLL
nlink-lab render examples/spine-leaf.nll

# Capture packets on an interface (uses netring, zero-copy AF_PACKET)
sudo nlink-lab capture simple router:eth0
sudo nlink-lab capture simple router:eth0 -w trace.pcap
sudo nlink-lab capture simple router:eth0 --filter-tcp --filter-port 80 -c 100 --duration 30

# Show process logs (captured automatically for all background processes)
nlink-lab logs simple --pid 12345
nlink-lab logs simple --pid 12345 --stderr --tail 50
nlink-lab logs simple --pid 12345 --follow          # tail -F a spawned process
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
  validator.rs      # Topology validation (40 rules with stable ids, see RULE_IDS)
  render.rs         # Topology → NLL serializer (for `render` command)
  dns.rs            # DNS /etc/hosts generation, injection, removal
  test_runner.rs    # CI test runner (deploy→validate→destroy) with JUnit/TAP output
  scenario.rs       # Timed scenario execution engine (fault injection + validation)
  benchmark.rs      # Benchmark execution engine (ping/iperf3 with metric assertions)
  capture.rs        # Packet capture using netring (pcap output, BPF filters)
  wifi.rs           # Wi-Fi emulation (hostapd/wpa_supplicant config gen, hwsim mgmt)
  deploy.rs         # Deployer — 18-step deployment sequence
  running.rs        # RunningLab — interact with deployed lab
  state.rs          # State persistence ($XDG_STATE_HOME/nlink-lab/labs, schema 2, flock in .locks/)
  netns_tag.rs      # Ownership tag on namespaces nlink-lab created (orphan reaper trusts only these)
  builder.rs        # Rust builder DSL
  templates/        # Built-in topology templates for `nlink-lab init`

bins/lab/src/
  main.rs           # entry: Cli, tracing, runtime, exit-code mapping (~120 lines)
  cli.rs            # Commands enum (help text lives here — docs/cli is generated from it)
  ctx.rs            # Ctx {json, quiet, verbose}, parse_topology(--set), require_root
  output.rs         # exit-code policy (0/1/2/124/child)
  cmd/<name>.rs     # one handler per subcommand (Args + run)
  render/           # dot / ascii / mermaid topology renderers
  host_scan.rs      # orphan/stale detection + reaper (tag-gated, nlink-based)
  util.rs           # tail, env pairs, byte sizes, BPF glue

examples/
  *.nll             # NLL topology examples (34 top-level; 43 incl. cookbook/ and imports/)
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
| `Network` | Shared L2 bridge segment (with optional per-pair impairments) |
| `Impairment` | Netem config (delay, jitter, loss, rate) |
| `NetworkImpairment` | Per-pair impairment on a shared network (src/dst nodes + netem + optional rate-cap) |
| `RateLimit` | Per-interface traffic shaping |
| `FirewallConfig` | nftables rules (with src/dst matching) |
| `NatConfig` | NAT rules (masquerade, snat, dnat, translate) |
| `ExecConfig` | Process to spawn in namespace |
| `EndpointRef` | Parsed "node:interface" reference |
| `VrfConfig` | VRF routing table configuration |
| `WireguardConfig` | WireGuard tunnel configuration |
| `RouteConfig` | Route entry (via, dev, metric) |
| `MacvlanConfig` | macvlan interface (name, parent, mode, addresses) |
| `IpvlanConfig` | ipvlan interface (name, parent, mode, addresses) |
| `WifiConfig` | Wi-Fi interface (name, mode, ssid, channel, passphrase) |
| `WifiMode` | Wi-Fi mode (Ap, Station, Mesh) |
| `Scenario` | Timed fault-injection test (steps with down/up/clear/validate) |
| `ScenarioStep` | Single timed step at a time offset |
| `Benchmark` | Performance test (ping/iperf3 with metric assertions) |
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
matching), NAT (`nat` block with `masquerade`/`snat`/`dnat`/`translate`),
link type profiles (`defaults radio { delay 15ms }` + `: radio` on links),
route groups (`route [a, b, c] via gw`),
VRF, WireGuard, VXLAN, containers (with cpu/memory limits,
capabilities, health checks with `interval`/`timeout`/`retries`,
depends-on, config injection, overlay), reachability assertions
(`validate { reach a b }`), management network (`mgmt` in lab block,
with optional `host-reachable` for root-namespace bridge),
DNS resolution (`dns hosts` auto-generates `/etc/hosts` entries),
macvlan/ipvlan (attach nodes to host physical interfaces),
rich validation assertions (`tcp-connect` with `retries`/`interval`,
`latency-under`, `route-has`, `dns-resolves`),
timed scenarios for fault injection (`scenario` block
with `at`, `down`, `up`, `clear`, `validate`), performance benchmarks
(`benchmark` block with `ping`/`iperf3` and `assert` thresholds),
Wi-Fi emulation (`wifi` block with `mode ap`/`station`/`mesh`,
mac80211_hwsim), site grouping (`site name { ... }` auto-prefixes
node names), IP computation functions (`subnet()`, `host()`),
conditional logic (`if` blocks with `==`/`!=`/`<`/`>`/`&&`/`||`),
`for` loops inside node/nat/network blocks, loopback pool allocation
(`lo pool name`), auto-routing (`routing auto` computes static routes
from topology graph), fleet `for_each` imports (instantiate templates
N times), glob patterns in network members (`*-black:fo`),
`param` declarations with CLI `--set` for parameterized topologies,
network (bridge) blocks, and per-pair impairment matrices inside
`network` blocks (`impair NODE -- NODE { delay … loss … rate-cap … }`
— one HTB+netem+flower tree per source interface, built via nlink's
`PerPeerImpairer` helper).

Nested interpolation works: `${leaf${i}.eth0}` resolves inner `${i}` first.
Pool exhaustion is detected and errors at parse time.
State locking via flock prevents concurrent deploy/destroy on the same lab.

CLI commands (34 total): `deploy` (with `--set`, `--unique`, `--suffix`, `--json`),
`destroy` (with `--all`, `--orphans`), `apply`,
`status` (with `--scan`, reports orphans + stale labs),
`exec` (`--json`, `--env`, `--workdir`),
`spawn` (`--log-dir`, `--env`, `--workdir`, `--wait-tcp`),
`shell` (interactive TTY), `validate` (with `--set`, `--show-ips`), `test`
(`--junit`, `--tap`, `--fail-fast`), `render`
(`--json`, `--dot`, `--ascii`, `--set`), `inspect` (combined view),
`impair` (`--out-*`/`--in-*`, `--partition`/`--heal`),
`graph`, `diagnose` (`--json`), `capture`, `diff`, `export`, `import`,
`watch` (`--family route|nftables|both`, `--json` for NDJSON),
`wait`, `wait-for` (`--tcp`, `--exec`, `--file`),
`ip` (`--iface`, `--cidr`),
`ps`, `kill`, `proc-stat`, `init`, `completions`, `daemon`, `metrics`,
`containers`, `logs` (`--follow`, `--tail`, `--pid`, `--stderr`),
`pull`, `stats`, `restart`.

Global flags: `--json`, `--verbose`, `--quiet`, `--skip-validate`.

See `docs/NLL_DSL_DESIGN.md` for the full language specification.

## Deployment Sequence

Since Plan 161 (the deep-analysis series, wave 5) deploy is **plan +
execute**:

```
 deploy(t)          = execute(plan(t))
 apply(cur, des)    = execute(Plan::diff(plan(cur), plan(des)))   # with purge
```

`crates/nlink-lab/src/deploy/`:

```
 mod.rs        deploy(), apply(), compute_layered_diff(), plan_for(); the
               per-node stack appliers (network / nftables / WireGuard)
 op.rs         NsRef (Root | Named | Container), Stage, Op, Plan::diff
 plan/         PURE planners — nothing here touches the kernel:
   topology.rs   namespaces, containers, hwsim, mgmt bridge, bridge
                 networks + member veths + VLANs, p2p veths, host-side
                 macvlan/ipvlan, links-up, sysctls, DNS overlays
   network.rs    topology_to_network_config (links/addresses/routes incl.
                 VRF-table routes via RouteBuilder::table)
   nftables.rs   topology_to_nftables_config (firewall + NAT, one table)
   wireguard.rs  key material + WireguardConfig per node
   qdisc.rs      build_netem
   process.rs    depends_on order, container create options
 apply.rs      the ONLY kernel/host-touching module: execute(plan) runs
               ops in stage order and journals every inverse
 rollback.rs   Undo + Journal (persisted as journal.json; unwound on
               error, on the next deploy after a crash, and by
               destroy --orphans)
```

Stages, in order (`Stage`): Namespaces → Hwsim → MgmtBridge → Networks →
Links → HostLinks → LinksUp → Sysctls → Stack → Tc → Dns → Processes →
Wifi. Removals in an apply run first, in reverse stage order. Every
netlink resource still commits through nlink's declarative
`NetworkConfig` / `NftablesConfig` / `WireguardConfig` reconcile paths
(zero kernel calls when unchanged); in apply mode the network layer uses
`ApplyOptions::with_purge(true)`. Namespaces created by nlink-lab carry
an ownership tag (`/run/nlink-lab/netns/<ns>`, `netns_tag.rs`) and
`destroy --orphans` only ever reaps tagged ones. `deploy --dry-run`
prints the plan; `validate { … }` assertions run after the state file is
written and ride on `RunningLab::assertion_results()`.

`apply_diff` (live reconcile) shares the declarative builders
with the initial-deploy path. `compute_layered_diff(running,
desired)` (Plan 158f Phase 2) is the dry-run preview that walks
every node, builds the same per-namespace `NetworkConfig` /
`NftablesConfig`, and emits a single `LayeredDiff` bundle for
`apply --check` / `apply --dry-run`. Plan 159d ships
`apply --check --json` with typed `network` + `nftables` fields;
Plan 160 (0.7.0) bumped it to **schema v3**, removing the v1
`diff` / `layered_summary` / `layered_summary_deprecated`
fallback fields after their one-release deprecation window.

`nlink-lab watch <lab>` (Plan 159b) subscribes to every node's
nftables + RTNETLINK multicast and prints one line per drift —
useful for spotting hand-edits that bypass `apply`. Powered by
0.19's `Connection<P>::subscribe_all_with_resync`.

## Dependencies

- **nlink** — Linux netlink library (namespaces, links, TC, nftables, routing)
- **netring** — Zero-copy packet capture (AF_PACKET TPACKET_V3 ring buffers)
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
