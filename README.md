# nlink-lab

A Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. Deep control over L2/L3/L4 networking
(TC, nftables, WireGuard, VRF, VXLAN) through the NLL topology DSL or a
programmatic Rust API.

## Quick Start

```bash
# Install system-wide (with just)
just install                # builds + installs SUID root (full feature support)
just install-caps           # alternative: capabilities only (no SUID)

# Create a topology from a template
nlink-lab init router

# Validate and deploy
nlink-lab validate router.nll
sudo nlink-lab deploy router.nll

# Interact with lab nodes
sudo nlink-lab exec router host -- ping -c1 10.0.0.1
sudo nlink-lab shell router host         # interactive shell
nlink-lab inspect router                 # full lab overview
nlink-lab status router                  # node table

# Container management
nlink-lab containers mylab               # list container nodes
nlink-lab logs mylab web --follow        # stream container logs
nlink-lab stats mylab                    # live CPU/memory usage
sudo nlink-lab restart mylab web         # restart a container node
nlink-lab pull topology.nll              # pre-pull all images

# CI/CD testing (deploy, validate, destroy in one shot)
sudo nlink-lab test topology.nll         # single topology
sudo nlink-lab test tests/               # all .nll files in directory
sudo nlink-lab test --junit results.xml tests/  # JUnit XML output
sudo nlink-lab test --tap tests/         # TAP output

# Tear down
sudo nlink-lab destroy router            # single lab
sudo nlink-lab destroy --all             # all labs
```

## NLL — nlink-lab Language

A purpose-built DSL with typed literals, visual link syntax, loops, variables,
cross-references, block comments, and composable modules:

```nll
lab "datacenter" {
  version "2.0"
  tags [fabric, l3]
  mgmt 172.20.0.0/24     /* management network for all nodes */
}

defaults link { mtu 9000 }

profile router { forward ipv4 }
node router : router
node host { route default via ${router.eth0} }  /* cross-reference */

link router:eth0 -- host:eth0 {
  subnet 10.0.0.0/30   /* auto-assign: .1 and .2 */
  delay 10ms jitter 2ms
}
```

### Loops and Variables

```nll
/* Integer ranges */
for i in 1..4 {
  node leaf${i} : router { lo 10.255.1.${i}/32 }
}

/* List iteration */
for role in [web, api, db] {
  node ${role} { route default via 10.0.0.1 }
}

/* Compound expressions and modulo */
for i in 0..7 {
  link leaf${i}:up -- spine${i % 2}:eth${i} { 10.${i % 2}.${i}.0/31 }
}
```

### Parametric Imports

Compose topologies from reusable, parameterized modules:

```nll
import "spine-leaf.nll" as dc(spines=4, leaves=8)

lab "extended"
node monitor
link dc-spine1:mon0 -- monitor:eth0 { 172.16.0.0/30 }
```

Fleet management with `for_each` — import the same template N times:

```nll
import "imports/site.nll" for_each {
  site1(id=1)
  site2(id=2)
  site3(id=3)
}
```

### Glob Patterns in Networks

Network members auto-match nodes using `*` wildcards:

```nll
network wan {
  members [gateway:wan, *-router:wan]    # matches site1-router, site2-router, ...
  subnet 172.16.0.0/24
}
```

Adding a new site only needs one import line — networks auto-adapt.

### Containers

```nll
node web image "nginx" {
  cpu "1"
  memory "512m"
  cap-add [NET_ADMIN, NET_RAW]
  healthcheck "curl -f http://localhost"
  depends-on [db]
}
```

### Subnet Pools

Automatic address allocation eliminates manual IP planning:

```nll
pool fabric 10.0.0.0/16 /30
pool access 10.1.0.0/16 /24

link s1:e1 -- l1:e1 { pool fabric }   # auto: 10.0.0.1/30 -- 10.0.0.2/30
link s1:e2 -- l2:e1 { pool fabric }   # auto: 10.0.0.5/30 -- 10.0.0.6/30
```

### Topology Patterns

Generate common topologies in a single statement:

```nll
mesh cluster { node [a, b, c, d]; pool links }        # full mesh (6 links)
ring backbone { count 6; pool backbone }                # ring (6 links)
star campus { hub router; spokes [s1, s2, s3, s4] }    # hub-and-spoke
```

### Multi-Profile Inheritance

```nll
profile router { forward ipv4 }
profile monitored { sysctl "net.core.rmem_max" "16777216" }
node core : router, monitored
```

### DNS Resolution

Auto-generate `/etc/hosts` so lab nodes can resolve each other by name:

```nll
lab "my-lab" {
  dns hosts
}
```

After deploy, `ping server` works from any node instead of `ping 10.0.2.2`.
Multi-homed nodes get per-interface aliases (e.g., `router-eth0`, `router-eth1`).

### NAT (Network Address Translation)

Declarative NAT rules for routers and firewalls:

```nll
node firewall {
  forward ipv4
  nat {
    masquerade src 10.0.0.0/16
    dnat dst 144.18.0.0/16 to 172.100.1.18
  }
}
```

### Link Type Profiles

Named impairment presets for different transport types:

```nll
defaults radio { delay 15ms jitter 10ms loss 2% }
defaults satellite { delay 300ms jitter 50ms }

link fw:radio -- remote:radio : radio { ... }
```

### macvlan / ipvlan

Attach lab nodes directly to host physical interfaces:

```nll
node gateway {
  macvlan eth0 parent "enp3s0" mode bridge { 192.168.1.100/24 }
  route default via 192.168.1.1
}
```

Supports macvlan modes (`bridge`, `private`, `vepa`, `passthru`) and ipvlan
modes (`l2`, `l3`, `l3s`).

### Validation Assertions

Post-deploy connectivity and network state checks:

```nll
validate {
    reach host1 host2              # ICMP ping
    no-reach host1 host3           # firewall blocks this path
    tcp-connect client server 80   # TCP port reachable
    tcp-connect client server 8080 timeout 5s retries 10 interval 1s  # with retry
    latency-under client server 50ms samples 10
    route-has router default via 10.0.0.1
    dns-resolves client "server" "10.0.2.2"
}
```

Use `--skip-validate` to disable assertion execution at deploy time.

### Timed Scenarios (Fault Injection)

Declarative chaos engineering — timed fault injection with validation:

```nll
scenario "failover-test" {
  at 0s  { validate { reach client server } }
  at 2s  { down router:eth0 }
  at 4s  { validate { no-reach client server } }
  at 8s  { up router:eth0 }
  at 10s { validate { reach client server } }
}
```

Actions: `down`, `up`, `clear` (remove impairments), `validate`, `exec`, `log`.

### Performance Benchmarks

Declarative performance testing with assertions:

```nll
benchmark "latency" {
  ping client server {
    count 100
    assert avg below 5ms
    assert loss below 1%
  }
}
```

Supports `ping` (always available) and `iperf3` (requires iperf3 installed).

### Wi-Fi Emulation

Virtual Wi-Fi topologies using the kernel's `mac80211_hwsim` module:

```nll
node ap {
  wifi wlan0 mode ap {
    ssid "labnet"
    channel 6
    wpa2 "testpassword"
    10.0.0.1/24
  }
}

node sta1 {
  wifi wlan0 mode station {
    ssid "labnet"
    wpa2 "testpassword"
  }
}
```

Modes: `ap` (hostapd), `station` (wpa_supplicant), `mesh` (802.11s).
Requires: `mac80211_hwsim` kernel module.

### Route Groups

Multiple routes to the same gateway in one line:

```nll
node dcs {
  route [144.18.1.0/24, 144.18.2.0/24, 144.18.3.0/24] via 10.2.2.2
}
```

### IP Computation Functions

Computed addressing with `subnet()` and `host()` — no more manual IPs:

```nll
let base = subnet("10.0.0.0/8", 16, 2)        # 10.2.0.0/16
let lan = subnet(${base}, 24, 1)                # 10.2.1.0/24

node server { route default via host(${lan}, 1) }
link a:eth0 -- b:eth0 { host(${lan}, 1)/24 -- host(${lan}, 2)/24 }
```

### Conditional Logic

Include nodes/links conditionally:

```nll
let simplified = 0
if ${simplified} == 0 {
  node red : router
  node black : router
}
```

Operators: `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`.

### Auto-Routing

Compute static routes from topology graph — no manual route statements:

```nll
lab "example" { routing auto }

node router : router
node host

link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
# host automatically gets: route default via 10.0.0.1
# router knows host subnet is directly connected
```

Stub nodes get default routes. Transit routers get shortest-path routes to
all remote subnets. Manual routes are preserved and not overridden.

### Loopback Pool Allocation

Auto-assign loopback addresses from a pool:

```nll
pool loopbacks 10.255.0.0/24 /32
node r1 { lo pool loopbacks }     # 10.255.0.0/32
node r2 { lo pool loopbacks }     # 10.255.0.1/32
```

### Site Grouping

Group nodes by physical location with automatic name prefixing:

```nll
site dc1 {
  node router : router
  node server { route default via 10.1.0.1 }
  link router:eth0 -- server:eth0 { subnet 10.1.0.0/24 }
}

# Cross-site link uses prefixed names
link dc1-router:wan -- dc2-router:wan { 172.16.0.1/30 -- 172.16.0.2/30 }
```

### Render and Inspect

```bash
nlink-lab render topology.nll          # expanded flat NLL
nlink-lab render --json topology.nll   # JSON
nlink-lab render --dot topology.nll    # Graphviz DOT
nlink-lab render --ascii topology.nll  # text diagram
```

See [`docs/NLL_DSL_DESIGN.md`](docs/NLL_DSL_DESIGN.md) for the full language specification.

## Rust API

```rust
use nlink_lab::{Lab, parser};

// From file (with import support)
let topo = parser::parse_file("datacenter.nll")?;

// Or build programmatically
let topo = Lab::new("my-lab")
    .node("server", |n| n)
    .node("client", |n| n.route("default", "10.0.0.1"))
    .link("server:eth0", "client:eth0", |l| {
        l.addresses("10.0.0.1/24", "10.0.0.2/24")
    })
    .build();

// Deploy
let lab = topo.deploy().await?;

// Interact
let output = lab.exec("client", "ping", &["-c1", "10.0.0.1"])?;
assert_eq!(output.exit_code, 0);

// Teardown
lab.destroy().await?;
```

## Integration Testing

The `#[lab_test]` macro auto-deploys and destroys topologies around test functions:

```rust
use nlink_lab::lab_test;

#[lab_test("examples/simple.nll")]
async fn test_connectivity(lab: RunningLab) {
    let out = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
    assert_eq!(out.exit_code, 0);
}
```

Tests automatically skip when not running as root.

Run with: `sudo cargo test -p nlink-lab --test integration`

### CLI-Based Integration Testing

For testing external applications (any language), use the CLI:

```bash
# Deploy with host-reachable management network
sudo nlink-lab deploy topology.nll

# Spawn services in controlled order
sudo nlink-lab spawn my-lab server -- /usr/bin/my-service --port 8080
sudo nlink-lab wait-for my-lab server --tcp 127.0.0.1:8080 --timeout 30

# Run client against the service
sudo nlink-lab exec --json my-lab client -- curl http://172.20.0.2:8080/health

# Query node IPs dynamically (including mgmt0 for host-reachable labs)
ADDR=$(nlink-lab ip my-lab server --iface eth0)
MGMT=$(nlink-lab ip my-lab server --iface mgmt0)

# Simulate network partitions
sudo nlink-lab impair my-lab router:wan0 --partition
# ... test failure detection ...
sudo nlink-lab impair my-lab router:wan0 --heal

# Asymmetric impairments (satellite/mobile simulation)
sudo nlink-lab impair my-lab router:wan0 --out-delay 50ms --in-delay 200ms

# Capture packets (native zero-copy via netring, writes pcap)
sudo nlink-lab capture my-lab router:eth0 -w trace.pcap
sudo nlink-lab capture my-lab router:eth0 -f "tcp port 80" -c 100

# Check process logs on failure (captured automatically for all background processes)
nlink-lab logs my-lab --pid 12345 --tail 50

# Parameterize topologies for different test scenarios
sudo nlink-lab deploy wan.nll --set latency=50ms --set loss=0.1%

# Tear down
sudo nlink-lab destroy my-lab
```

Management network with `host-reachable` creates a bridge in the root namespace,
allowing test processes to connect directly to services inside lab nodes:

```nll
lab "my-lab" {
    mgmt 172.20.0.0/24 host-reachable
}
```

## Examples

| Example | Description |
|---------|-------------|
| `simple` | 2 nodes, 1 link, impairment |
| `router` | Router between two subnets |
| `spine-leaf` | Datacenter fabric with defaults, subnet auto-assign, cross-refs |
| `datacenter-fabric` | 4-spine 8-leaf Clos with loops, modulo, block comments |
| `wan-impairment` | WAN with delay, loss, rate limiting |
| `firewall` | Stateful nftables firewall with src/dst matching |
| `multi-profile` | Compose router + monitored + secured profiles |
| `list-iteration` | Named service nodes with `for x in [...]` |
| `subnet-pools` | Named pools with auto-allocation + validate block |
| `pattern-mesh` | Full-mesh via `mesh` pattern |
| `pattern-ring` | Ring via `ring` pattern |
| `pattern-star` | Hub-and-spoke via `star` pattern |
| `mesh` | Manual full mesh (4 nodes, 6 links) |
| `ipv6-simple` | IPv6-only topology |
| `vxlan-overlay` | VXLAN tunnel between VTEPs |
| `vrf-multitenant` | VRF tenant isolation |
| `wireguard-vpn` | Site-to-site WireGuard VPN |
| `iperf-benchmark` | Throughput testing with rate limiting |
| `vlan-trunk` | Bridge with VLAN filtering |
| `container` | Docker/Podman container nodes |
| `container-advanced` | Resource limits, capabilities, labels, exec |
| `container-lifecycle` | Health checks, depends-on, startup-delay |
| `asymmetric` | Directional impairments (`->` / `<-`) |
| `imports/composed` | Topology composition via imports |
| `imports/parametric-ring` | Parametric module with `param` declarations |
| `imports/use-ring` | Parametric import with custom count |
| `dns` | DNS resolution via `/etc/hosts` injection |
| `macvlan` | macvlan: attach lab node to physical host NIC |
| `ipvlan` | ipvlan: shared-MAC attachment to physical host NIC |
| `scenario` | Timed fault injection with validation checkpoints |
| `benchmark` | Performance testing with ping/iperf3 assertions |
| `wifi` | Wi-Fi AP + stations via mac80211_hwsim |
| `nat` | NAT: masquerade + DNAT firewall |
| `site-grouping` | Multi-site topology with auto name prefixing |
| `multi-site` | Multi-site infrastructure with NAT, modem links, parametric imports |
| `management-network` | OOB management bridge with `mgmt` subnet |
| `integration-testing` | Host-reachable mgmt, healthcheck, depends-on, tcp-connect retry |
| `imports/base-network` | Reusable base network module |

All examples use the `.nll` format. Use `nlink-lab init --list` to create from templates.

## Comparison with containerlab

| Feature | **nlink-lab** | **containerlab** |
|---|---|---|
| **Abstraction** | Linux namespaces + optional containers | Docker/podman containers |
| **Topology format** | NLL DSL (loops, variables, imports, typed literals) | YAML (+ Go templates for generation) |
| **Inline impairments** | `delay 10ms jitter 2ms` on links, asymmetric `->` / `<-` | Post-deploy only (`tools netem`) |
| **Address management** | Inline on links, subnet auto-assign, named pools | Manual (via startup-config scripts) |
| **Topology patterns** | `mesh`, `ring`, `star` generators with pool integration | `generate` for CLOS fabrics |
| **Firewall** | Native nftables with `src`/`dst` matching | Depends on NOS |
| **VRF / VXLAN / WireGuard** | First-class NLL syntax | Depends on NOS image |
| **DNS resolution** | `dns hosts` auto-generates `/etc/hosts` | `/etc/hosts` injection on mgmt network |
| **Cross-references** | `route via ${router.eth0}` | None |
| **Hot-reload** | `apply` with topology diff | Redeploy required |
| **Diagnostics** | `diagnose`, `capture`, `metrics` (Zenoh) | None built-in |
| **Reachability assertions** | `validate { reach a b }` in topology | None |
| **Programmatic API** | Rust builder DSL + `#[lab_test]` macro | Go library |
| **Vendor NOS support** | None (pure Linux networking) | 80+ kinds (SR Linux, cEOS, vMX, etc.) |
| **Startup config** | Container `exec` + `config` mounts | SSH/NETCONF provisioning per NOS |
| **Dependencies** | Linux kernel only | Docker/podman runtime |
| **Startup time** | Milliseconds | Seconds (image pull + boot) |
| **Best for** | Application network testing, simulation, CI | Multi-vendor NOS labs |

## Requirements

- Linux (kernel 4.19+)
- Root, SUID, or capabilities (`CAP_NET_ADMIN` + `CAP_SYS_ADMIN`; `CAP_DAC_OVERRIDE` for DNS; `CAP_SYS_MODULE` for WiFi)
- Rust 1.85+ (edition 2024)

## Editor Support

NLL files (`.nll`) have editor support for syntax highlighting, bracket matching,
comment toggling, and code folding.

### VS Code

Install the extension from the `editors/vscode-nll/` directory:

```bash
cd editors/vscode-nll
code --install-extension .
```

### Neovim (tree-sitter)

Add to your nvim-treesitter config:

```lua
local parser_config = require("nvim-treesitter.parsers").get_parser_configs()
parser_config.nll = {
  install_info = {
    url = "/path/to/nlink-lab/editors/tree-sitter-nll",
    files = { "src/parser.c" },
  },
  filetype = "nll",
}
vim.filetype.add({ extension = { nll = "nll" } })
```

Then run `:TSInstall nll`. Copy `editors/tree-sitter-nll/queries/highlights.scm`
to `~/.config/nvim/after/queries/nll/highlights.scm`.

### Helix

Add to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "nll"
scope = "source.nll"
file-types = ["nll"]
comment-token = "#"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 2, unit = "  " }

[[grammar]]
name = "nll"
source = { path = "/path/to/nlink-lab/editors/tree-sitter-nll" }
```

Then `hx --grammar fetch && hx --grammar build`. Copy query files to
`~/.config/helix/runtime/queries/nll/`.

### Zed

Install as a dev extension:

```bash
ln -s /path/to/nlink-lab/editors/zed-nll ~/.local/share/zed/extensions/installed/nll
```

Then restart Zed. NLL files will have syntax highlighting, bracket matching,
comment toggling, and code folding.

## License

MIT OR Apache-2.0
