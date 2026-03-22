# nlink-lab: Network Lab Engine Built on nlink

*Design document for a containerlab-like network simulation tool powered by nlink*

## 1. Executive Summary

**nlink-lab** is a proposed Rust-based network lab engine for creating isolated, reproducible
network topologies using Linux network namespaces. Unlike containerlab (which focuses on
container orchestration with basic networking), nlink-lab is **networking-first**: it provides
deep control over L2/L3/L4 topology, traffic control, firewalling, and diagnostics — all
through a declarative topology file.

**Primary use case:** Testing network-dependent applications in realistic, reproducible
environments with LAN segments, VLANs, bridges, routers, netem impairment, firewalls,
and traffic shaping.

## 2. Comparison with containerlab

| Capability | containerlab | nlink-lab (proposed) |
|---|---|---|
| **Topology format** | YAML | TOML (type-safe, better errors) |
| **Node types** | 40+ vendor NOS images | Linux namespaces + processes |
| **Link types** | veth, macvlan, vxlan | veth, macvlan, vxlan, bridge, vlan, bond, gre, wireguard, vti |
| **Link impairment** | netem (delay, loss, jitter) | Full TC: netem + bandwidth + corruption + reorder + rate limiting + HTB hierarchies |
| **Firewall** | None | nftables per-node (NAT, filtering, sets) |
| **Traffic shaping** | None | HTB/HFSC/TBF/DRR per-link with classes and filters |
| **Routing** | Static only (node config) | Policy routing, MPLS, SRv6, nexthop groups, FIB |
| **Bridge features** | Basic | VLAN filtering, FDB management, STP, VLAN tunneling |
| **Diagnostics** | None | Built-in: packet loss detection, bottleneck analysis, connectivity checks |
| **Event monitoring** | None | Real-time netlink events across all namespaces |
| **Batch operations** | Sequential | Batched netlink for fast setup |
| **Runtime** | Docker/podman required | No container runtime needed (pure namespaces) |
| **Process execution** | Container entrypoint | Direct process spawn in namespace |
| **Reproducibility** | Container image dependent | Kernel-level, no image pulls |
| **Startup time** | Seconds-minutes (image pulls) | Milliseconds (namespace + netlink) |
| **Dependencies** | Docker/podman + vendor images | Linux kernel only |
| **Language** | Go (vishvananda/netlink) | Rust (nlink) |

## 3. nlink Gap Analysis

### 3.1 Gaps — All Resolved

All five originally-identified gaps have been addressed. Three were already implemented
when the gap analysis was written; the remaining two (sysctl, namespace spawn) were
implemented in commit `e18c602`.

#### Gap 1: Sysctl Management — ✅ IMPLEMENTED

```rust
use nlink::netlink::{sysctl, namespace};

// Local namespace
sysctl::get("net.ipv4.ip_forward")?;
sysctl::set("net.ipv4.ip_forward", "1")?;
sysctl::set_many(&[
    ("net.ipv4.ip_forward", "1"),
    ("net.ipv6.conf.all.forwarding", "1"),
])?;

// Named namespace (enters via setns, reads /proc/sys/, restores)
namespace::set_sysctl("myns", "net.ipv4.ip_forward", "1")?;
namespace::get_sysctl("myns", "net.ipv4.ip_forward")?;
namespace::set_sysctls("myns", &[
    ("net.ipv4.ip_forward", "1"),
    ("net.ipv6.conf.all.forwarding", "1"),
])?;
```

Key: `sysctl::get/set/set_many` for local, `namespace::get_sysctl/set_sysctl/set_sysctls`
for namespace-aware operations. Path traversal validation prevents abuse.

#### Gap 2: Public Namespace Process Execution — ✅ IMPLEMENTED

```rust
use nlink::netlink::namespace;
use std::process::Command;

// Spawn in named namespace (pre_exec + setns, parent unaffected)
let mut cmd = Command::new("iperf3");
cmd.arg("-s");
let mut child = namespace::spawn("myns", cmd)?;

// Spawn and collect output
let mut cmd = Command::new("ip");
cmd.arg("addr");
let output = namespace::spawn_output("myns", cmd)?;

// Via NamespaceSpec
let spec = NamespaceSpec::Named("myns");
let child = spec.spawn(Command::new("nginx"))?;
```

Uses `CommandExt::pre_exec()` + `libc::setns()` — the child switches namespace between
`fork()` and `exec()`. Parent is never affected. Also: `spawn_path()`, `spawn_output_path()`
for path-based namespaces.

#### Gap 3: NetworkConfig Namespace Awareness — DEFERRED (not needed)

The lab engine will be its own multi-namespace orchestration layer, using per-namespace
`Connection<Route>` instances and the existing nlink APIs directly. `NetworkConfig` stays
single-namespace by design.

#### Gap 4: VRF Table Assignment — ✅ ALREADY WORKED

`VrfLink::new("vrf-red", 100)` + `conn.set_link_master("eth0", "vrf-red")` — fully
implemented with integration test.

#### Gap 5: Interface Rename — ✅ ALREADY IMPLEMENTED

`conn.set_link_name("old", "new")` / `conn.set_link_name_by_index(idx, "new")` — fully
implemented with integration test and CLI support.

### 3.2 Gaps for the Lab Tool (not nlink library)

These are features needed in the lab tool itself, not in nlink:

| Gap | Description | Priority |
|-----|-------------|----------|
| Topology parser | TOML/YAML → internal graph | Critical |
| Lifecycle management | Create/destroy/status of labs | Critical |
| Process manager | Spawn/monitor/kill processes in nodes | Critical |
| Topology validation | Check for IP conflicts, missing routes, loops | High |
| State persistence | Track running labs for cleanup | High |
| Templating | Reusable node/link profiles | Medium |
| Traffic generation | Built-in iperf3/ping orchestration | Medium |
| Packet capture | tcpdump integration per-link | Medium |
| Web dashboard | Live topology visualization | Low |
| DHCP/DNS | Auto-assign addresses, resolve names | Low |

### 3.3 What nlink Covers (complete)

All networking primitives needed for the lab engine are available:

- **All link types:** veth, bridge, vlan, vxlan, macvlan, bond, gre, gretap, ipip, sit,
  vti, wireguard, geneve, bareudp, netkit, dummy, ifb, macvtap, ipvlan, vrf
- **Namespace lifecycle:** create, delete, list, exists, connection_for
- **Namespace process spawning:** `namespace::spawn()`, `spawn_output()`, `NamespaceSpec::spawn()`
- **Sysctl management:** `sysctl::get/set/set_many`, `namespace::get_sysctl/set_sysctl/set_sysctls`
- **Interface rename:** `set_link_name()`, `set_link_name_by_index()`
- **Cross-namespace links:** `VethLink::peer_netns_fd()`, `set_link_netns_fd/pid()`
- **Full TC:** 19 qdisc types, 4 class types, 9 filter types, 12 action types
- **nftables:** tables, chains, rules, sets, NAT, atomic transactions
- **Routing:** IPv4/IPv6, policy rules, nexthop groups, MPLS, SRv6
- **Bridge:** VLAN filtering, FDB, STP, VLAN tunneling
- **Addresses:** IPv4/IPv6 CRUD with namespace-safe `*_by_index` variants
- **Neighbors:** ARP/NDP management, proxy ARP
- **Monitoring:** Event streams across multiple namespaces via `StreamMap`
- **Diagnostics:** Scan, bottleneck detection, connectivity checks
- **Batch operations:** Multiple netlink ops in single syscall
- **Rate limiting:** `RateLimiter`, `PerHostLimiter` high-level APIs

## 4. Topology DSL Design

### 4.1 Why TOML

| Format | Pros | Cons |
|--------|------|------|
| YAML | Familiar (containerlab uses it) | Whitespace-sensitive, type coercion bugs, security concerns |
| JSON | Universal | Verbose, no comments |
| TOML | Explicit types, comments, no indentation hell | Less familiar for networking people |
| Rust DSL | Type-safe, IDE completion | Requires recompilation |
| HCL | Good for infra (Terraform) | Another dependency |

TOML wins for a Rust project: it has native serde support, explicit typing, inline tables
for compact definitions, and comments for documentation. The topology file is a data
declaration, not a program — TOML fits this perfectly.

**The Rust builder DSL is also supported** for programmatic topology construction (tests,
dynamic topologies). The TOML file deserializes into the same structures.

### 4.2 Core Concepts

```
Lab
├── Nodes (network namespaces)
│   ├── Interfaces (created by links or explicitly)
│   ├── Addresses
│   ├── Routes
│   ├── Sysctls
│   ├── Firewall rules
│   └── Processes (optional)
├── Links (point-to-point connections between nodes)
│   ├── Impairment (netem)
│   └── Rate limiting
├── Networks (shared L2 segments via bridges)
│   ├── VLAN configuration
│   └── Connected nodes
└── Profiles (reusable templates)
```

### 4.3 Topology File Format

```toml
# Lab metadata
[lab]
name = "datacenter-sim"
description = "Simulated datacenter with spine-leaf topology"
# prefix for namespace names (default: lab name)
prefix = "dc"

# ─────────────────────────────────────────────
# PROFILES: Reusable templates
# ─────────────────────────────────────────────

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1", "net.ipv6.conf.all.forwarding" = "1" }

[profiles.router.firewall]
policy = "accept"  # default chain policy

[profiles.host]
sysctls = { "net.ipv4.ip_forward" = "0" }

# ─────────────────────────────────────────────
# NODES: Network namespaces
# ─────────────────────────────────────────────

[nodes.spine1]
profile = "router"

[nodes.spine1.interfaces.lo]
addresses = ["10.255.0.1/32"]

[nodes.spine2]
profile = "router"

[nodes.spine2.interfaces.lo]
addresses = ["10.255.0.2/32"]

[nodes.leaf1]
profile = "router"

[nodes.leaf1.interfaces.lo]
addresses = ["10.255.1.1/32"]

[nodes.leaf1.routes]
default = { via = "10.0.11.1" }
"10.0.0.0/8" = { via = "10.0.11.1", metric = 100 }

[nodes.leaf2]
profile = "router"

[nodes.leaf2.interfaces.lo]
addresses = ["10.255.1.2/32"]

[nodes.server1]
profile = "host"

[nodes.server1.routes]
default = { via = "10.1.1.1" }

# Run a process inside this node
[[nodes.server1.exec]]
cmd = ["iperf3", "-s"]
background = true

[nodes.server2]
profile = "host"

[nodes.server2.routes]
default = { via = "10.1.2.1" }

[[nodes.server2.exec]]
cmd = ["iperf3", "-s"]
background = true

# ─────────────────────────────────────────────
# LINKS: Point-to-point connections
# ─────────────────────────────────────────────

# Spine-Leaf fabric (L3 point-to-point)
[[links]]
endpoints = ["spine1:eth1", "leaf1:eth1"]
addresses = ["10.0.11.1/30", "10.0.11.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine1:eth2", "leaf2:eth1"]
addresses = ["10.0.12.1/30", "10.0.12.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine2:eth1", "leaf1:eth2"]
addresses = ["10.0.21.1/30", "10.0.21.2/30"]
mtu = 9000

[[links]]
endpoints = ["spine2:eth2", "leaf2:eth2"]
addresses = ["10.0.22.1/30", "10.0.22.2/30"]
mtu = 9000

# Leaf to server links with impairment
[[links]]
endpoints = ["leaf1:eth3", "server1:eth0"]
addresses = ["10.1.1.1/24", "10.1.1.10/24"]

[[links]]
endpoints = ["leaf2:eth3", "server2:eth0"]
addresses = ["10.1.2.1/24", "10.1.2.10/24"]

# ─────────────────────────────────────────────
# NETWORKS: Shared L2 segments (bridges)
# ─────────────────────────────────────────────

# Uncomment for L2 segment example:
# [networks.mgmt]
# kind = "bridge"
# mtu = 1500
# members = ["spine1:mgmt", "spine2:mgmt", "leaf1:mgmt", "leaf2:mgmt"]
# subnet = "192.168.100.0/24"
# # Auto-assigns .1, .2, .3, .4 to members in order

# ─────────────────────────────────────────────
# IMPAIRMENTS: Link-level network emulation
# ─────────────────────────────────────────────

# Apply netem to specific links (by endpoint names)
[impairments."spine1:eth1"]
delay = "10ms"
jitter = "2ms"

[impairments."spine1:eth2"]
delay = "10ms"
jitter = "2ms"
loss = "0.1%"

# WAN-like link between two nodes
[impairments."leaf2:eth3"]
delay = "50ms"
jitter = "5ms"
loss = "0.5%"
rate = "100mbit"
corrupt = "0.01%"
reorder = "0.5%"

# ─────────────────────────────────────────────
# FIREWALL: Per-node nftables rules
# ─────────────────────────────────────────────

[nodes.server1.firewall]
policy = "drop"

[[nodes.server1.firewall.rules]]
match = "ct state established,related"
action = "accept"

[[nodes.server1.firewall.rules]]
match = "tcp dport 5201"  # iperf3
action = "accept"

[[nodes.server1.firewall.rules]]
match = "icmp"
action = "accept"

# ─────────────────────────────────────────────
# RATE LIMITING: Per-interface traffic shaping
# ─────────────────────────────────────────────

[rate_limits."server1:eth0"]
egress = "1gbit"
ingress = "1gbit"
burst = "10mbit"

[rate_limits."server2:eth0"]
egress = "100mbit"
ingress = "100mbit"
```

### 4.4 More Complex Examples

#### VLAN Trunk + Access Ports

```toml
[lab]
name = "vlan-lab"

[nodes.switch]
profile = "router"

[nodes.pc1]
[nodes.pc2]
[nodes.pc3]

[networks.office]
kind = "bridge"
vlan_filtering = true

[networks.office.vlans.10]
name = "engineering"
[networks.office.vlans.20]
name = "sales"

# Trunk port (carries all VLANs)
[networks.office.ports.switch]
interface = "eth0"
vlans = [10, 20]
tagged = true

# Access ports (single VLAN, untagged)
[networks.office.ports.pc1]
interface = "eth0"
pvid = 10
untagged = true
addresses = ["10.10.0.2/24"]

[networks.office.ports.pc2]
interface = "eth0"
pvid = 10
untagged = true
addresses = ["10.10.0.3/24"]

[networks.office.ports.pc3]
interface = "eth0"
pvid = 20
untagged = true
addresses = ["10.20.0.2/24"]
```

#### WireGuard VPN Tunnel

```toml
[lab]
name = "vpn-lab"

[nodes.office]
profile = "router"

[nodes.office.wireguard.wg0]
private_key = "auto"  # auto-generate
listen_port = 51820
addresses = ["10.100.0.1/24"]
peers = ["remote"]

[nodes.remote]
profile = "router"

[nodes.remote.wireguard.wg0]
private_key = "auto"
addresses = ["10.100.0.2/24"]
peers = ["office"]

[[links]]
endpoints = ["office:eth0", "remote:eth0"]
addresses = ["203.0.113.1/24", "203.0.113.2/24"]

# Simulate WAN conditions on the link
[impairments."office:eth0"]
delay = "30ms"
jitter = "5ms"
loss = "0.1%"
rate = "50mbit"
```

#### Multi-Tenant with VRF

```toml
[lab]
name = "vrf-lab"

[nodes.pe_router]
profile = "router"

[nodes.pe_router.vrfs.red]
table = 100
interfaces = ["eth1"]
routes = { "0.0.0.0/0" = { via = "10.0.1.2" } }

[nodes.pe_router.vrfs.blue]
table = 200
interfaces = ["eth2"]
routes = { "0.0.0.0/0" = { via = "10.0.2.2" } }

[nodes.tenant_a]
[nodes.tenant_b]

[[links]]
endpoints = ["pe_router:eth1", "tenant_a:eth0"]
addresses = ["10.0.1.1/24", "10.0.1.2/24"]

[[links]]
endpoints = ["pe_router:eth2", "tenant_b:eth0"]
addresses = ["10.0.2.1/24", "10.0.2.2/24"]
```

#### VXLAN Overlay

```toml
[lab]
name = "overlay-lab"

[nodes.vtep1]
profile = "router"

[nodes.vtep1.interfaces.vxlan100]
kind = "vxlan"
vni = 100
local = "10.0.0.1"
remote = "10.0.0.2"
port = 4789
addresses = ["192.168.100.1/24"]

[nodes.vtep2]
profile = "router"

[nodes.vtep2.interfaces.vxlan100]
kind = "vxlan"
vni = 100
local = "10.0.0.2"
remote = "10.0.0.1"
port = 4789
addresses = ["192.168.100.2/24"]

# Underlay link
[[links]]
endpoints = ["vtep1:eth0", "vtep2:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]
```

### 4.5 Equivalent Rust Builder DSL

The same topology can be built programmatically:

```rust
use nlink_lab::{Lab, Profile};

let lab = Lab::new("datacenter-sim")
    .profile("router", Profile::new()
        .sysctl("net.ipv4.ip_forward", "1")
        .sysctl("net.ipv6.conf.all.forwarding", "1"))
    .profile("host", Profile::new()
        .sysctl("net.ipv4.ip_forward", "0"))
    // Nodes
    .node("spine1", |n| n.profile("router")
        .lo("10.255.0.1/32"))
    .node("leaf1", |n| n.profile("router")
        .lo("10.255.1.1/32")
        .route("default", "10.0.11.1"))
    .node("server1", |n| n.profile("host")
        .route("default", "10.1.1.1")
        .exec("iperf3", &["-s"]).background())
    // Links
    .link("spine1:eth1", "leaf1:eth1", |l| l
        .addresses("10.0.11.1/30", "10.0.11.2/30")
        .mtu(9000))
    .link("leaf1:eth3", "server1:eth0", |l| l
        .addresses("10.1.1.1/24", "10.1.1.10/24"))
    // Impairments
    .impair("spine1:eth1", |i| i
        .delay("10ms").jitter("2ms"))
    // Rate limits
    .rate_limit("server1:eth0", |r| r
        .egress("1gbit").ingress("1gbit"));

// Deploy
let running = lab.deploy().await?;

// Interact
let output = running.exec("server1", "ping", &["-c", "3", "10.1.1.1"]).await?;
println!("{}", output);

// Diagnostics
let report = running.diagnose().await?;
for issue in &report.issues {
    println!("[{:?}] {}", issue.severity, issue.message);
}

// Teardown
running.destroy().await?;
```

### 4.6 Topology Validation Rules

Before deployment, the lab engine validates the topology:

| Rule | Description |
|------|-------------|
| **Unique IPs** | No duplicate addresses within same L2/L3 segment |
| **Valid CIDRs** | All addresses must be valid CIDR notation |
| **Endpoint pairing** | Every link has exactly 2 endpoints |
| **No dangling refs** | All node/interface references in links exist |
| **VLAN range** | VIDs must be 1-4094 |
| **MTU consistency** | Warn if mismatched MTUs on connected interfaces |
| **Route reachability** | Warn if gateway is not in any connected subnet |
| **Profile exists** | All referenced profiles must be defined |
| **No name conflicts** | Node names must be unique |
| **Interface uniqueness** | No duplicate interface names within a node |

## 5. Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     nlink-lab CLI                        │
│  deploy / destroy / status / exec / diagnose / capture  │
└──────────┬──────────────────────────────────────────────┘
           │
┌──────────▼──────────────────────────────────────────────┐
│                    Lab Engine                            │
│  ┌─────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Parser  │ │Validator │ │ Deployer │ │  Manager   │  │
│  │ (TOML)  │ │ (rules)  │ │ (apply)  │ │(lifecycle) │  │
│  └────┬────┘ └────┬─────┘ └────┬─────┘ └─────┬──────┘  │
│       └───────────┴────────────┴──────────────┘         │
└──────────┬──────────────────────────────────────────────┘
           │
┌──────────▼──────────────────────────────────────────────┐
│                      nlink                              │
│  ┌──────────┐ ┌────┐ ┌─────────┐ ┌───────┐ ┌───────┐  │
│  │Namespace │ │Link│ │  TC/    │ │nftables│ │ Diag  │  │
│  │  mgmt    │ │mgmt│ │ Netem   │ │       │ │nostics│  │
│  └──────────┘ └────┘ └─────────┘ └───────┘ └───────┘  │
└──────────┬──────────────────────────────────────────────┘
           │
┌──────────▼──────────────────────────────────────────────┐
│                   Linux Kernel                          │
│  Netlink  │  Namespaces  │  TC  │  nftables  │  veth   │
└─────────────────────────────────────────────────────────┘
```

### 5.1 Deployment Sequence

```
1. Parse topology file → TopologyGraph
2. Validate (IP conflicts, dangling refs, VLAN ranges, ...)
3. Create namespaces (batch)
4. Create bridge networks (if any)
5. Create veth pairs spanning namespaces
6. Create additional interfaces (vxlan, bond, vlan, wireguard, ...)
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
18. Write state file for lifecycle management
```

### 5.2 State Management

Running labs are tracked in `~/.nlink-lab/` (or `$XDG_STATE_HOME/nlink-lab/`):

```
~/.nlink-lab/
  labs/
    datacenter-sim/
      state.json        # Namespace names, PIDs, creation time
      topology.toml     # Copy of the topology file used
```

This enables:
- `nlink-lab status` — list running labs
- `nlink-lab destroy <name>` — clean teardown
- `nlink-lab exec <lab> <node> <cmd>` — run commands in nodes
- Crash recovery (cleanup orphaned namespaces on next run)

## 6. CLI Design

```
nlink-lab deploy <topology.toml>     Deploy a lab from topology file
nlink-lab destroy <name>             Tear down a running lab
nlink-lab status [name]              Show running labs or specific lab details
nlink-lab exec <lab> <node> <cmd>    Run a command in a lab node
nlink-lab diagnose <lab> [node]      Run network diagnostics
nlink-lab capture <lab> <link>       Start packet capture on a link
nlink-lab impair <lab> <link> ...    Modify link impairment at runtime
nlink-lab graph <topology.toml>      Print topology as DOT/ASCII graph
nlink-lab validate <topology.toml>   Validate topology without deploying
```

### Example Session

```bash
# Deploy the lab
$ nlink-lab deploy datacenter.toml
Lab "datacenter-sim" deployed in 47ms
  Nodes: spine1, spine2, leaf1, leaf2, server1, server2
  Links: 6 point-to-point
  Impairments: 3 links with netem

# Check status
$ nlink-lab status datacenter-sim
Lab: datacenter-sim (running since 2026-03-17 14:30:00)
  Nodes: 6
  Links: 6
  Processes: 2 (iperf3 on server1, server2)

# Run diagnostics
$ nlink-lab diagnose datacenter-sim
[OK] spine1:eth1 → leaf1:eth1 (10ms delay, 0 drops)
[OK] spine1:eth2 → leaf2:eth1 (10ms delay, 0.08% loss)
[OK] leaf1:eth3 → server1:eth0 (0 drops)
[WARN] leaf2:eth3 → server2:eth0 (50ms delay, 0.4% loss, rate limited 100mbit)
No bottlenecks detected.

# Execute commands in nodes
$ nlink-lab exec datacenter-sim server2 ping -c 3 10.1.1.10
PING 10.1.1.10 (10.1.1.10) 56(84) bytes of data.
64 bytes from 10.1.1.10: icmp_seq=1 ttl=62 time=70.2 ms
64 bytes from 10.1.1.10: icmp_seq=2 ttl=62 time=68.8 ms
64 bytes from 10.1.1.10: icmp_seq=3 ttl=62 time=72.1 ms

# Modify impairment at runtime
$ nlink-lab impair datacenter-sim leaf2:eth3 --delay 100ms --loss 5%
Updated impairment on leaf2:eth3

# Capture traffic
$ nlink-lab capture datacenter-sim spine1:eth1 -w /tmp/spine1.pcap
Capturing on spine1:eth1... (press Ctrl+C to stop)

# Tear down
$ nlink-lab destroy datacenter-sim
Lab "datacenter-sim" destroyed (6 namespaces removed)
```

## 7. Test Integration

nlink-lab can be used as a Rust library for integration tests:

```rust
#[tokio::test]
async fn test_app_handles_packet_loss() {
    let lab = Lab::new("loss-test")
        .node("server", |n| n.route("default", "10.0.0.1"))
        .node("client", |n| n.route("default", "10.0.0.2"))
        .link("server:eth0", "client:eth0", |l| l
            .addresses("10.0.0.1/24", "10.0.0.2/24"))
        .impair("server:eth0", |i| i.loss("5%"))
        .deploy().await.unwrap();

    // Run your application in the server node
    let handle = lab.spawn("server", Command::new("my-app").arg("--listen=10.0.0.1:8080"))
        .await.unwrap();

    // Test from client
    let output = lab.exec("client", "curl", &["-s", "http://10.0.0.1:8080/health"])
        .await.unwrap();
    assert!(output.contains("ok"));

    // Increase loss and test degraded behavior
    lab.set_impairment("server:eth0", |i| i.loss("50%")).await.unwrap();

    // Verify app handles high loss gracefully
    let result = lab.exec("client", "curl", &["-s", "--max-time", "5", "http://10.0.0.1:8080/health"])
        .await;
    // App should timeout or return error, not crash
    assert!(result.is_err() || result.unwrap().contains("timeout"));

    lab.destroy().await.unwrap();
}
```

## 8. Implementation Roadmap

### Phase 1: Foundation (nlink library additions) — ✅ COMPLETE

1. ~~**Sysctl support**~~ — `sysctl::get/set/set_many` + `namespace::get_sysctl/set_sysctl/set_sysctls`
2. ~~**Interface rename**~~ — `set_link_name()` / `set_link_name_by_index()` (was already done)
3. ~~**Public namespace exec**~~ — `namespace::spawn()` / `spawn_output()` with `pre_exec` + `setns()`
4. ~~**VRF enslavement**~~ — `VrfLink::new()` + `set_link_master()` (was already done)

### Phase 2: Core Lab Engine

1. **TOML parser** — serde-based topology deserialization
2. **Topology validator** — all validation rules from section 4.6
3. **Deployer** — the 18-step deployment sequence
4. **State manager** — lab state persistence and cleanup
5. **CLI** — deploy, destroy, status, exec

### Phase 3: Advanced Features

1. **Runtime impairment modification** — change netem/rate at runtime
2. **Diagnostics integration** — per-lab network health checks
3. **Packet capture** — spawn tcpdump in namespace, save to file
4. **ASCII/DOT graph** — topology visualization
5. **Process manager** — spawn, monitor, restart processes in nodes

### Phase 4: Ecosystem

1. **Example topologies** — common patterns (spine-leaf, WAN, MPLS, VPN)
2. **Test harness** — `#[nlink_lab::test]` proc macro for auto-deploy/destroy
3. **CI integration** — run network-dependent tests in CI (requires `CAP_NET_ADMIN`)
4. **Documentation** — user guide, API docs, topology cookbook

## 9. Why Not Just Use containerlab?

| Reason | Detail |
|--------|--------|
| **No Docker required** | Runs on pure Linux namespaces — works in CI, VMs, bare metal |
| **Millisecond startup** | No image pulls, no container runtime overhead |
| **Deep TC control** | 19 qdisc types, HTB hierarchies, flower filters — not just basic netem |
| **Built-in firewall** | nftables per-node without shelling out |
| **Rust type safety** | Topology errors caught at parse time, not at runtime |
| **Library-first** | Use from `#[tokio::test]` — no CLI subprocess needed |
| **Diagnostics** | Built-in bottleneck detection, connectivity checks |
| **Event monitoring** | Real-time netlink events across all lab namespaces |
| **No vendor lock-in** | No NOS images needed — pure Linux networking |
| **Reproducible** | Same kernel = same behavior, no container image drift |

containerlab excels at testing **vendor NOS software** (SR Linux, cEOS, etc.). nlink-lab
excels at testing **your applications** in realistic network conditions.

## 10. Summary

nlink provides **all** networking primitives needed for the lab engine. The originally
identified library gaps have been fully resolved:

- ~~Sysctl management~~ — `sysctl::get/set` + `namespace::set_sysctl/set_sysctls`
- ~~Namespace process execution~~ — `namespace::spawn()` / `spawn_output()` via `pre_exec` + `setns`
- ~~Interface rename~~ — `set_link_name()` / `set_link_name_by_index()`
- ~~VRF enslavement~~ — `VrfLink::new()` + `set_link_master()`

The library is ready. The next step is Phase 2: building the lab engine itself.

The proposed TOML-based topology format provides a clean, type-safe alternative to
containerlab's YAML, with significantly more networking depth. The Rust builder DSL
enables programmatic topology construction for integration tests.

**Estimated remaining effort:**
- ~~nlink library gaps: ~1 week~~ ✅ Done
- Core lab engine (parse + validate + deploy + destroy): ~2 weeks
- CLI + state management: ~1 week
- Advanced features (capture, diagnostics, runtime modification): ~2 weeks
