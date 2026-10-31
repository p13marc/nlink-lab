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
cargo build -p nlink-lab-gui      # Build the desktop app (iced)
cargo test -p nlink-lab --lib     # Run library unit tests
cargo test                        # Run all tests (root tests fail without root; NLINK_LAB_SKIP_ROOT_TESTS=1 skips them)
just test-integration             # Root suite under sudo: one binary per area, in parallel
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
nlink-lab ls
nlink-lab ls --scan

# Pick the lab later commands act on (or pass -L LAB to each)
nlink-lab use simple
nlink-lab status                  # nodes, addresses, impairments, processes

# Execute in a lab node (stdio streams live — good for services, tail -f, ping)
sudo nlink-lab exec -L simple router -- ip addr

# Execute with JSON output (buffered; exit_code, stdout, stderr, duration_ms)
sudo nlink-lab exec --json -L simple router -- ip addr

# Spawn a background process (tracked by ps/kill)
sudo nlink-lab spawn -L simple server -- /usr/bin/my-service --port 8080

# Wait for a service to be ready
sudo nlink-lab wait -L simple server --tcp 127.0.0.1:8080 --timeout 30s

# Show node IP addresses (including mgmt0 for host-reachable labs)
nlink-lab ip -L simple server:eth0
nlink-lab ip -L simple server:mgmt0

# Impair a link at runtime (one way, or --dir in|both); cut it off
sudo nlink-lab impair -L simple router:eth0 delay 50ms loss 1%
sudo nlink-lab impair -L simple router:eth0 --partition

# Deploy with JSON output (machine-parseable); NDJSON progress events too
sudo nlink-lab deploy --json examples/simple.nll
sudo nlink-lab --format ndjson deploy examples/simple.nll

# Validate with resolved IP addresses
nlink-lab validate --show-ips examples/multi-site.nll

# Expand loops/variables and print flat NLL
nlink-lab render examples/spine-leaf.nll

# Capture packets on an interface (uses netring, zero-copy AF_PACKET)
sudo nlink-lab capture -L simple router:eth0
sudo nlink-lab capture -L simple router:eth0 -o trace.pcap
sudo nlink-lab capture -L simple router:eth0 --filter-tcp --filter-port 80 -c 100 --duration 30s

# Show process logs (captured automatically for all background processes)
nlink-lab logs -L simple --pid 12345
nlink-lab logs -L simple --pid 12345 --stderr --tail 50
nlink-lab logs -L simple --pid 12345 --follow       # tail -F a spawned process
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
      value.rs      # Val<T>: typed + spanned literals/deferred values (Duration, Percent, Rate, Size, …)
      lower.rs      # AST → Topology (imports, loops, variables, lowering)
  error.rs          # Error types (includes NllDiagnostic for miette)
  validator.rs      # Topology validation (<!-- count:validator-rules -->83<!-- /count --> rules with stable ids, see RULE_IDS)
  render.rs         # Topology → NLL serializer (for `render` command)
  dns.rs            # DNS /etc/hosts generation, injection, removal
  test_runner.rs    # CI test runner (deploy→validate→destroy) with JUnit/TAP output
  scenario.rs       # Timed scenario execution engine (fault injection + validation)
  seed.rs           # Netem seeds: the lab seed, derive(), the kernel probe (#337)
  benchmark.rs      # Benchmark execution engine (ping/iperf3 with metric assertions)
  capture.rs        # Packet capture using netring (pcap output, BPF filters)
  bundle.rs         # Failure bundles: logs, status, events, node state, pcaps (#338)
  wifi.rs           # Wi-Fi emulation (hostapd/wpa_supplicant config gen, hwsim radios over netlink)
  deploy/           # Deployer — plan + execute (see "Deployment Sequence")
  running.rs        # RunningLab — interact with deployed lab
  state.rs          # State persistence (/var/lib/nlink-lab/labs or $NLINK_LAB_STATE_DIR, group nlink-lab, schema 2, flock in .locks/; snapshots/<name>/ checkpoints; legacy per-user roots + migrate_lab; context/<uid>)
  events.rs         # Lifecycle event log (events.ndjson per lab; `events` command, backend republish)
  frr.rs            # FRR routing daemons: pathspaces, per-daemon config generation, neighbour resolution
  netns_tag.rs      # Ownership tag on namespaces nlink-lab created (orphan reaper trusts only these)
  builder.rs        # Rust builder DSL
  ops/              # OpCtx: progress (OpEvent) + cancellation of deploy/apply/destroy/…;
                    #   view (LabStatus, LabSummary, impairments, processes, events with
                    #   a cursor — what status/ps/stats and the daemon share), read (the
                    #   API's read calls), mutate (every change: deploy from a source
                    #   bundle, apply, check, restore, impair, spawn, restart, … —
                    #   what the daemon's jobs run and the CLI shares)
  host.rs           # orphan/stale detection + reaper (tag-gated), reported as Cleanups
  host_registry.rs  # what labs claim in the host namespace (host-reachable mgmt subnets) (#332)
  metrics.rs        # MetricsCollector: interface rates/states, top TCP flows, process exits
  logs.rs           # process log tail / cat / follow
  edit.rs           # Edit model: live topology edits (nodes, links, impairments, qdiscs, pairs, MTU)
  analysis/         # one pass over NLL text: lex, parse, validate, lint -> diagnostics
                    #   as byte ranges (the LSP's and the app's editor's); locate
                    #   (validator location -> span), symbols (token queries)
  wait.rs           # WaitCondition + RunningLab::wait_all (one deadline, progress, cancel)
  pty.rs            # pseudo-terminals: Pty::open/attach (setsid + controlling tty), resize
crates/nlink-lab-client/src/   # client of the daemon API: Endpoint (unix, ssh dial-stdio,
                               #   cmd, zenoh — a bridge to the frame protocol, feature
                               #   `zenoh`), Client (handshake, multiplexed calls), Call
bins/nlink-labd/src/           # the host daemon: config (/etc/nlink-lab/daemon.toml),
                               #   listen (socket activation, bind 0666, sd_notify),
                               #   authz (SO_PEERCRED/SO_PEERGROUPS -> Operator/Viewer),
                               #   server (handshake, per-call tasks, cancel), calls
                               #   (dispatch), hub (state-root scan, events tail,
                               #   subscriptions, per-lab metrics threads; collect_all for
                               #   telemetry), zenoh (every lab's topics on the old layout,
                               #   unixsock 0660 viewer group, scouting off; requests on
                               #   nlink-lab/@rpc/<host>/{viewer,operator}; [zenoh.tls]
                               #   = mTLS + generated access_control; feature `zenoh`),
                               #   http (/metrics, /api/v2), jobs (one
                               #   queue per lab on its own thread, detachable jobs:
                               #   attach_op/list_ops/cancel_op), exec (pipes or a pty,
                               #   shell), sessions (logs, capture, wait_for, export),
                               #   audit
dist/                          # systemd units (socket-activated nlink-labd), sysusers,
                               #   tmpfiles — `just install-daemon`; install.sh and
                               #   install-gui.sh, the release tarballs' installers
flatpak/                       # the app's manifest, desktop entry, metainfo (releases:
                               #   `just release-prep VERSION`), media type, icon
crates/nlink-lab-shared/src/
  api/              # daemon API (docs/API.md): Request (+ meta roles), Response,
                    #   StreamItem/Notification, OpEvent progress, ApiError, DTOs,
                    #   unix-socket Frame codec, API_VERSION
  metrics.rs        # MetricsSnapshot and friends (daemon, top, desktop app)
  templates/        # Built-in topology templates for `nlink-lab init`
crates/nlink-lab-view/src/      # what the desktop app draws, renderer-free (pure, tested):
  graph.rs          # TopoGraph: nodes by kind, links/ports/tunnels/host NICs/mgmt, site regions
  layout.rs         # layered (multi-root for fabrics, wide layers wrapped), circle (DFS
                    #   order), force (Fruchterman–Reingold), auto; pins kept; extend
                    #   (new nodes beside the placed ones); bounds; natural order
  metrics.rs        # MetricsStore: per-interface rates + history, error heat, staleness
  impair.rs         # effective_impairments (shared with `top`)
  scene.rs          # Scene: shapes, badges, parallel-edge curves, end labels beside the
                    #   line (untangled, left out without room), hit test (Hit)
  svg.rs            # Scene -> SVG
  diagram/          # DOT / Mermaid / ASCII of a Topology (`render --diagram`, exports)

bins/lab/src/
  main.rs           # entry: Cli (global --format/--json/--color/-v/-q), usage errors
                    #   (exit 64, removed-command hints), tracing, runtime, completions
  cli.rs            # Commands enum (help text lives here — docs/cli is generated
                    #   from it), GROUPS for the grouped help, the short-flag policy test
  ctx.rs            # Ctx, LabArg + resolve_lab (-L > NLINK_LAB > `use` > only lab),
                    #   scoped completers, parse_topology(--set), require_root
  args.rs           # clap value parsers (durations need units, KEY=VALUE, endpoints, …)
  output/           # exit codes, the --json error document (ApiError), styles
                    #   (per stream), tables, Report + emit, JSON schemas
  progress.rs       # live progress of OpEvents (terminal / pipe / NDJSON), Ctrl-C cancel
  removed.rs        # removed commands and flags -> what replaced them (exit 64)
  docs_gate.rs      # test: every `nlink-lab …` line in the docs and example comments parses
  cmd/<name>.rs     # one handler per subcommand (Args + run); cmd/common.rs shared
                    #   printing/lookup, cmd/edit.rs the Edit executor for node/link
  cmd/lsp/          # `lsp`: the protocol over nlink_lab::analysis —
                    #   analyze (its diagnostics as LSP ranges), position
                    #   (byte <-> UTF-16)
  cmd/top/          # `top`: app (pure state + keys), ui (widgets +
                    #   `--once` text frame), mod (source, event loop)
  render/           # validate/deploy summaries (diagrams: nlink_lab_view::diagram)
  host_scan.rs      # prints nlink_lab::host cleanups
  util.rs           # env pairs, byte sizes, BPF glue, impair --show lines

bins/gui/src/       # nlink-lab-gui, the desktop app (iced 0.14, experimental):
  model.rs          #   App (route, tabs, toasts, conn, labs), Tab (Design: source,
                    #   params, problems | Live: lab, revision, impairments, events),
                    #   TopoView (graph, positions, pins, camera, selection, metrics, scene)
  backend.rs        #   Backend: the daemon's client or the demo; subscribe -> Feed
                    #   (polls where the daemon streams nothing: Zenoh)
  fake.rs           #   --demo: made-up labs, metrics, impairments, events
  live.rs           #   lifecycle/runtime events -> timeline lines (+ what to refetch)
  control.rs        #   changing a lab: impairment/pair editors, presets, Action
                    #   (toast, refetch), undo requests, Confirm (typed-name destroy)
  spark.rs          #   sparkline canvas
  session.rs        #   console runs, logs, captures: lines (cap, \r), state, cancel
  term.rs           #   terminal host: iced_term widgets (feature embedded-terminal)
                    #   or an external emulator, both running `pty-bridge`
  bridge.rs         #   `nlink-lab-gui pty-bridge`: raw pty <-> the daemon's `shell`
  job.rs            #   long operations (deploy/apply/scenario/…) from OpEvents:
                    #   stage, plan-wide steps, waits, lines, outcome, cancel
  message.rs        #   Message, CanvasMsg, Shortcut
  reduce.rs         #   reduce(app, msg) -> Vec<Effect>: every behaviour, pure, tested
  effect.rs         #   Effect + run: loads, layouts (off the UI thread), rfd dialogs,
                    #   exports, settings/pins writes, connect, fetches
  doc.rs            #   Source (file | unsaved template), load: nlink_lab::analysis ->
                    #   problems with spans (the file, or the editor's text), marks
  highlight.rs      #   the editor's NLL highlighter (line at a time, block comments
                    #   carried over, problems marked)
  commands.rs       #   the command palette: entries for the tab in front, filter
  canvas.rs         #   canvas::Program painting the Scene in cached layers (+ a
                    #   minimap when it does not fit); no keys
  keys.rs           #   shortcuts from keyboard::listen() (never while typing)
  settings.rs       #   gui.toml (theme, layout, toggles, recent), per-file pins
  export.rs         #   SVG / PNG (resvg) / DOT / Mermaid
  palette.rs        #   the canvas's colours (tones, node kinds) per theme
  view/             #   home, workspace (pane_grid: canvas, source editor, inspector,
                    #   dock), inspector, dock, modals (panel, confirm, palette)
  ui_tests.rs       #   headless frames (iced_test), every example drawn;
                    #   NLINK_LAB_GUI_SNAPSHOTS=DIR keeps them as PNGs
  daemon_tests.rs   #   against an in-process nlink-labd (root ones deploy labs)
bins/gui/tests/packaging.rs    # desktop entry / metainfo / manifest / release agree

examples/
  *.nll             # NLL topology examples (<!-- count:examples -->48<!-- /count --> here; <!-- count:examples-all -->57<!-- /count --> incl. cookbook/ and imports/)
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
| `Impairment` | Netem config (delay, jitter, loss, rate, corrupt, reorder, duplicate, correlations, limit) |
| `NetworkImpairment` | Per-pair impairment on a shared network (src/dst nodes + netem + optional rate-cap) |
| `RateLimit` | Per-interface traffic shaping |
| `QdiscConfig` / `QdiscKind` | Non-netem root qdisc (tbf, fq_codel, sfq, prio) |
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
| `FrrConfig` / `OspfConfig` / `BgpConfig` | FRR daemons for a node (`frr { ospf … bgp … }`, `routing frr`) |
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
inline impairments (`->` / `<-`), profiles, firewall (input/forward/output
chains; `src`/`dst`, `iif`/`oif`, `!=` and orders (`< > <= >=` on ports, marks,
`ct mark`, `ct zone`, `l4proto`), port ranges, `tcp flags` (`syn / syn,ack`), `ct status`/
`direction`/`zone`, `pkttype`, `l4proto`; accept/drop/reject
with `reject with tcp-reset`; named sets with `@set` lookups, interval and
concatenated keys; `limit` (packets or bytes)/`counter`/`counter name`/`log … level` statements and a
`validate { counter … }` assertion; `rpf` anti-spoofing, `!=` port ranges, mark ranges; `mark set`/`ct mark set|save|restore`,
`classify` into `prio` bands, `clamp mss`), NAT (`nat` block with
`masquerade`/`snat`/`dnat`/`redirect`/`translate`, `iif`/`oif`/`tcp dport` qualifiers,
address and port ranges, `random`/`fully-random`/`persistent`),
link type profiles (`defaults radio { delay 15ms }` + `: radio` on links),
route groups (`route [a, b, c] via gw`),
VRF, WireGuard, VXLAN, containers (with cpu/memory limits,
capabilities, health checks with `interval`/`timeout`/`retries`,
depends-on, config injection, overlay), reachability assertions
(`validate { reach a b }`, with `retries`/`interval` on the block for
labs that converge), management network (`mgmt` in lab block,
with optional `host-reachable` for root-namespace bridge),
DNS resolution (`dns hosts` auto-generates `/etc/hosts` entries),
macvlan/ipvlan (attach nodes to host physical interfaces),
rich validation assertions (`tcp-connect` with `retries`/`interval`,
`latency-under`, `route-has`, `dns-resolves`, `counter`),
timed scenarios for fault injection (`scenario` block
with `at`, `down`, `up`, `clear`, `validate`), performance benchmarks
(`benchmark` block with `ping`/`iperf3` and `assert` thresholds),
Wi-Fi emulation (`wifi` block with `mode ap`/`station`/`mesh`,
mac80211_hwsim), site grouping (`site name { ... }` auto-prefixes
node names), IP computation functions (`subnet()`, `host()`),
conditional logic (`if` blocks with `==`/`!=`/`<`/`>`/`&&`/`||`),
`for` loops inside node/nat/network blocks, loopback pool allocation
(`lo pool name`), auto-routing (`routing auto` computes static routes
from topology graph), dynamic routing with FRR (`routing frr { ospf }`,
per-node `frr { ospf … bgp … }`), fleet `for_each` imports (instantiate templates
N times), glob patterns in network members (`*-black:fo`),
`param` declarations with CLI `--set` for parameterized topologies,
first-class IPv6 (dual-stack `port` addresses, `ip6` firewall matches,
NAT66, IPv6 `mgmt`, `subnet()`/`host()`/pools on u128, per-family
`routing auto`, `dnat … to [addr]:port`),
`qdisc NODE:IFACE tbf|fq_codel|sfq|prio { … }` root qdiscs,
`bond NAME { members [...] mode 802.3ad … }` and `vlan NAME { parent … id … protocol 802.1ad }` blocks,
network (bridge) blocks, and per-pair impairment matrices inside
`network` blocks (`impair NODE -- NODE { delay … loss … rate-cap … }`
— one HTB+netem+flower tree per source interface, built via nlink's
`PerPeerImpairer` helper).

Nested interpolation works: `${leaf${i}.eth0}` resolves inner `${i}` first.
Pool exhaustion is detected and errors at parse time.
State locking via flock prevents concurrent deploy/destroy on the same lab.

CLI commands (<!-- count:cli-commands -->41<!-- /count -->, grouped in `--help`; lab-scoped ones take `-L LAB`, else `$NLINK_LAB`, `use`, or the only lab):
`deploy` (`.nll` or `.nlz`, with `--set`, `--unique`, `--suffix`, `--json`), `use` (`--clear`),
`destroy` (`LAB…`, `--all`, `--orphans`), `apply` (`[FILE]`, default the recorded source; `--reset-impairments`),
`ls` (`--scan`, reports orphans + stale labs), `status [LAB]` (nodes, addresses, containers,
impairments, partitions, processes, counters),
`snapshot save|ls|rm|restore` (`--dry-run`), `archive create|extract|inspect`,
`events` (`--follow`, `--source lifecycle|drift|runtime`, `--family`, `--node`, `--since`, `--kind`, `--socket`),
`exec` (`--json`, `--env`, `--workdir`),
`spawn` (`--log-dir`, `--env`, `--workdir`, `--wait-tcp`),
`shell` (interactive TTY), `validate` (with `--set`, `--show-ips`), `test`
(`--scenarios`, `--benchmarks`, `--keep`, `--junit`, `--tap`, `--fail-fast`), `render`
(`--diagram dot|mermaid|ascii`, `--set`),
`impair EP SPEC…` (`--dir out|in|both`, `--clear`, `--partition`/`--heal`, `--pair SRC DST`, `--show`),
`diagnose` (`--json`), `bundle` (`-o`, `--archive`: a failure bundle on demand), `capture`, `diff`, `export` (`--nll`),
`wait [NODE]` (`--tcp`, `--exec`, `--file`, `--listening`, `--log`, `--fd-stable`),
`ip` (`--iface`, `--cidr`), `conntrack [NODE]` (`--proto`, `--src`/`--dst`,
`--sport`/`--dport`, `--mark`, `--zone`, `--status`, `--flush`),
`ps [NODE]`, `kill PID…`, `stats [NODE]` (`--pid`, `--procs`, `--pidfile-glob`, `--watch`), `init`, `completions`,
`logs` (`--follow`, `--tail`, `--pid`, `--stderr`),
`pull`, `node add|rm|restart`, `link add|rm|set` (`--mtu`, `--qdisc`), `fmt` (`--check`, `-w`, `-`), `lint`
(`--strict`, `--allow`), `doctor` (`--fix`), `verify` (`--file`, the CI drift gate),
`scenario`, `benchmark` (ping/iperf3 with metric asserts),
`lsp` (LSP server over stdio, issue #56),
`top` (`--interval`, `--once`, `--daemon`; ratatui TUI, issue #63; without
root it reads the daemon's metrics).

Global flags: `--format text|json|ndjson` (`--json`), `--color auto|always|never`,
`-v`/`-vv`, `-q`. Lab-scoped commands take `-L LAB`. Durations need a unit
(`30s`); short flags mean one thing (`-c -e -f -i -L -o -s -t -w`). Usage
errors exit 64.

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
               per-node Stack (network + nftables + WireGuard, one nlink Stack)
 op.rs         NsRef (Root | Named | Container), Stage, Op, Plan::diff
 plan/         PURE planners — nothing here touches the kernel:
   topology.rs   namespaces, containers, Wi-Fi radios, mgmt bridge, bridge
                 networks + member veths + VLANs, p2p veths, host-side
                 macvlan/ipvlan, links-up, sysctls, DNS overlays
   network.rs    topology_to_network_config (links — one declaration each,
                 carrying its master — addresses, routes incl. VRF tables)
   nftables.rs   topology_to_nftables_config (firewall + NAT, one table)
   wireguard.rs  key material + WireguardConfig per node
   qdisc.rs      build_netem, build_qdisc (QdiscSpec)
   tc.rs         declare_node_tc: netem, qdisc blocks, RateLimiter::declare,
                 PerPeerImpairer::declare, partitions — into the Stack
   process.rs    depends_on order, container create options
 apply.rs      the ONLY kernel/host-touching module: execute(plan) runs
               ops in stage order and journals every inverse
 rollback.rs   Undo + Journal (persisted as journal.json; unwound on
               error, on the next deploy after a crash, and by
               destroy --orphans)
```

Stages, in order (`Stage`): Namespaces → Hwsim → MgmtBridge → Networks →
Links → HostLinks → LinksUp → Sysctls → Stack → Dns →
RoutingDaemons → Processes → Wifi. Removals in an apply run first, in reverse stage order.
A declared link whose kind parameters change (VLAN id, VNI, bond mode, VRF
table…) is recreated by nlink inside Stack, which puts its declared Tc and
routes back (and keeps undeclared Tc, `preserve_undeclared_tc`); `apply` runs a
read-only preflight so a recreate nlink would refuse fails before anything
changes, and another of each reconciled node's nftables so what nlink 0.33's
`NftablesDiff::blocked` refuses (a set a hand-added rule uses, recreated)
fails before the first stage too (`preflight_nftables`). Every
netlink resource still commits through nlink's declarative
`NetworkConfig` / `NftablesConfig` / `WireguardConfig`, planned and
applied per node as one `nlink::facade::Stack` (`apply_reconcile_in`;
zero kernel calls when unchanged). In apply mode the network layer purges
and plans against `previous` (the running lab's plan, runtime impairments
and partitions included), so a declared link, qdisc or Tc tree the topology
no longer has is deleted; a declared link's per-interface sysctls are
devconf on its declaration. Traffic control is declared too
(`plan/tc.rs`): a runtime `impair`/`--partition`/`--heal` reconciles the
node's Tc-only config (`deploy::reconcile_node_tc`); a partition is a
clsact matchall on both hooks, under whatever root the endpoint has: a drop
on ingress, a steal (TC_ACT_STOLEN) on egress, so the sender's stack sees a
black hole, not a local drop (#347). Namespaces created by nlink-lab carry
an ownership tag (`/run/nlink-lab/netns/<ns>`, `netns_tag.rs`) and
`destroy --orphans` only ever reaps tagged ones. `deploy --dry-run`
prints the plan; `validate { … }` assertions run after the state file is
written and ride on `RunningLab::assertion_results()`.

`apply_diff` (live reconcile) shares the declarative builders
with the initial-deploy path. `compute_layered_diff(running,
desired)` (Plan 158f Phase 2) is the dry-run preview that walks
every node, plans the same `Stack` apply would (`Stack::plan_in`), and
emits a single `LayeredDiff` bundle (network, nftables, WireGuard) for
`verify` / `apply --dry-run`. Plan 159d ships
`verify --json` with typed `network` + `nftables` (+ `wireguard`) fields;
Plan 160 (0.7.0) bumped it to **schema v3**, removing the v1
`diff` / `layered_summary` / `layered_summary_deprecated`
fallback fields after their one-release deprecation window.

`nlink-lab events -f --source drift` (Plan 159b) subscribes to every node's
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
- `docs/INSTALL.md` — Installation (binaries, source, capabilities, flatpak app)
- `docs/GUI.md` — The daemon, `top`, the experimental desktop app (`nlink-lab-gui`)
- `docs/plans/NLINK_LAB-original-design.md` — The original design proposal (historical)
- `docs/plans/` — Active implementation plans
