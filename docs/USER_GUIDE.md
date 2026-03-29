# nlink-lab User Guide

## Installation

Requirements:

- Linux kernel 4.19 or later
- Root privileges or `CAP_NET_ADMIN` capability
- Rust 1.85+ toolchain

Build from source:

```bash
git clone https://github.com/p13marc/nlink-lab.git
cd nlink-lab
cargo build --release -p nlink-lab-cli
sudo install target/release/nlink-lab /usr/local/bin/
```

Generate shell completions:

```bash
nlink-lab completions bash > /etc/bash_completion.d/nlink-lab
nlink-lab completions zsh > /usr/share/zsh/site-functions/_nlink-lab
```

---

## Your First Lab

### 1. Write the topology

Save this as `first.nll`:

```nll
lab "my-first-lab"

profile router { forward ipv4 }

node router : router
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
}
```

This creates two network namespaces connected by a veth pair. The router has IPv4 forwarding enabled, and the host has a default route pointing at the router.

### 2. Validate

```bash
nlink-lab validate first.nll
```

Expected output:

```
Topology 'my-first-lab' is valid (2 nodes, 1 link)
```

### 3. Deploy

```bash
sudo nlink-lab deploy first.nll
```

Expected output:

```
Deploying lab 'my-first-lab'...
  [1/18] Parsing topology
  [2/18] Validating
  ...
  [18/18] Writing state
Lab 'my-first-lab' deployed (2 nodes, 1 link) in 0.3s
```

### 4. Check status

```bash
nlink-lab status
```

```
LAB              NODES  LINKS  CREATED
my-first-lab     2      1      2026-03-29T10:00:00Z
```

### 5. Inspect

```bash
sudo nlink-lab exec my-first-lab router -- ip addr show eth0
```

```
2: eth0@if3: <BROADCAST,MULTICAST,UP> mtu 1500 ...
    inet 10.0.0.1/24 scope global eth0
```

```bash
sudo nlink-lab exec my-first-lab host -- ping -c 3 10.0.0.1
```

```
PING 10.0.0.1 (10.0.0.1) 56(84) bytes of data.
64 bytes from 10.0.0.1: icmp_seq=1 ttl=64 time=0.05 ms
...
3 packets transmitted, 3 received, 0% packet loss
```

### 6. Destroy

```bash
sudo nlink-lab destroy my-first-lab
```

```
Destroying lab 'my-first-lab'...
Lab 'my-first-lab' destroyed
```

---

## NLL by Example

### 1. Profiles and IP Forwarding

Profiles are reusable node templates. Nodes inherit with `:`.

```nll
profile router { forward ipv4 }
profile dual-stack { forward ipv4  forward ipv6 }

node r1 : router
node r2 : dual-stack
```

`forward ipv4` is shorthand for `sysctl "net.ipv4.ip_forward" "1"`. Use `sysctl` for arbitrary kernel parameters.

### 2. Routes

```nll
node host {
  route default via 10.0.0.1
  route 192.168.0.0/16 via 10.0.0.2 metric 100
  route 10.1.0.0/24 dev eth1
}
```

Routes support `via`, `dev`, and `metric` modifiers.

### 3. Link Impairments (Symmetric)

Impairments placed directly in a link block apply to both directions.

```nll
link r1:wan0 -- r2:wan0 {
  172.16.0.1/30 -- 172.16.0.2/30
  delay 30ms jitter 5ms loss 0.1% rate 50mbit
}
```

Available properties: `delay`, `jitter`, `loss`, `rate`, `corrupt`, `reorder`.

Standalone form:

```nll
impair switch:br0 delay 5ms jitter 1ms
```

See `examples/wan-impairment.nll`.

### 4. Asymmetric Impairments

Use `->` and `<-` for per-direction impairments. `->` applies to the left endpoint, `<-` to the right.

```nll
link ground:sat0 -- satellite:sat0 {
  172.16.0.1/30 -- 172.16.0.2/30
  -> delay 270ms jitter 10ms rate 50mbit    # ground to satellite
  <- delay 270ms jitter 10ms rate 150mbit   # satellite to ground
}
```

See `examples/asymmetric.nll`.

### 5. For Loops and Variables

```nll
let N = 4
let base = 10.0

for i in 1..${N} {
  node r${i} : router { lo 10.255.0.${i}/32 }
}

for i in 1..3 {
  let next = ${i} + 1
  link r${i}:eth1 -- r${next}:eth0 {
    ${base}.${i}.1/30 -- ${base}.${i}.2/30
  }
}
```

Ranges are inclusive: `1..4` produces 1, 2, 3, 4. Interpolation with `${...}` works in names, addresses, and values. Expressions support `+`, `-`, `*`, `/`.

See `examples/spine-leaf.nll` for a full datacenter fabric using nested loops.

### 6. Firewall (nftables)

```nll
node server {
  firewall policy drop {
    accept ct established,related
    accept tcp dport 8080
    accept udp dport 53
    drop tcp dport 22
  }
}
```

Rules are applied via nftables. The `policy` sets the default chain action.

See `examples/firewall.nll`.

### 7. Bridge Networks with VLANs

```nll
network fabric {
  vlan-filtering
  members [switch:br0, host1:eth0, host2:eth0]
  vlan 100 "sales"
  vlan 200 "engineering"
  port host1:eth0 { pvid 100  untagged }
  port host2:eth0 { vlans [100, 200]  tagged }
}
```

See `examples/vlan-trunk.nll`.

### 8. WireGuard Tunnels

```nll
node gw-a : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
  route 192.168.2.0/24 dev wg0
}
```

`key auto` generates a keypair at deploy time. Peers reference other nodes by name; public keys are exchanged automatically.

See `examples/wireguard-vpn.nll`.

### 9. VRF Multi-Tenancy

```nll
node pe : router {
  vrf red table 10 {
    interfaces [eth1, eth2]
    route default dev eth1
  }
  vrf blue table 20 {
    interfaces [eth3]
    route default dev eth3
  }
}
```

Each VRF gets its own routing table. Interfaces are bound to a VRF at deploy time.

See `examples/vrf-multitenant.nll`.

### 10. Containers

```nll
node router image "alpine:latest" cmd "sleep infinity"
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
}
```

Nodes with `image` run as containers (Docker or Podman) instead of bare namespaces. Container nodes and namespace nodes can be mixed freely.

See `examples/container.nll`.

### 11. Imports for Composition

File `base-network.nll`:

```nll
lab "base"
profile router { forward ipv4 }
node r1 : router
node r2 : router
link r1:eth0 -- r2:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
```

File `composed.nll`:

```nll
import "base-network.nll" as dc

lab "composed"

node host { route default via 10.1.0.1 }

link dc.r1:eth1 -- host:eth0 {
  10.1.0.1/24 -- 10.1.0.10/24
}
```

Imports must appear before the `lab` declaration. All imported names are prefixed with the alias (`dc.r1`, `dc.r2`). Imports can be recursive; circular imports are rejected.

See `examples/imports/`.

### 12. Subnet Pools

Named pools eliminate manual address planning:

```nll
pool fabric 10.0.0.0/16 /30
pool access 10.1.0.0/16 /24

link s1:e1 -- l1:e1 { pool fabric }   # 10.0.0.1/30 -- 10.0.0.2/30
link s1:e2 -- l2:e1 { pool fabric }   # 10.0.0.5/30 -- 10.0.0.6/30
link l1:e3 -- h1:e0 { pool access }   # 10.1.0.1/24 -- 10.1.0.2/24
```

Subnets are allocated sequentially. Pool exhaustion is an error at parse time.

### 13. Topology Patterns

Generate common topologies in a single statement:

```nll
mesh cluster { node [a, b, c, d]; pool links }     # full mesh
ring backbone { count 6; pool backbone }             # ring
star campus { hub router; spokes [s1, s2, s3] }      # hub-and-spoke
```

Patterns expand to regular nodes and links during lowering. Use `nlink-lab render` to see the expanded topology.

### 14. Reachability Assertions

Declare post-deploy connectivity checks in the topology:

```nll
validate {
    reach host1 host2        # host1 can ping host2
    no-reach host1 host3     # firewall should block this
}
```

---

## CLI Reference

### Commands

| Command | Description |
|---------|-------------|
| `deploy` | Deploy a lab from a topology file |
| `destroy` | Tear down a running lab |
| `apply` | Apply topology changes to a running lab |
| `status` | Show running labs or details of a specific lab |
| `exec` | Run a command inside a lab node |
| `validate` | Validate a topology file without deploying |
| `impair` | Modify link impairment at runtime |
| `capture` | Capture packets on an interface (tcpdump) |
| `diagnose` | Run diagnostics on a lab |
| `daemon` | Start the Zenoh backend daemon for a running lab |
| `metrics` | Stream live metrics from a lab via Zenoh |
| `init` | Create a topology file from a built-in template |
| `graph` | Print topology as DOT graph |
| `render` | Expand loops/variables/imports and print flat NLL |
| `ps` | List processes running in a lab |
| `kill` | Kill a tracked background process |
| `wait` | Wait for a lab to be ready |
| `diff` | Compare two topology files |
| `export` | Export a running lab's topology |
| `completions` | Generate shell completions |

### deploy

```bash
sudo nlink-lab deploy topology.nll [--dry-run] [--force] [--daemon]
```

- `--dry-run` -- validate and show the deployment plan without executing it.
- `--force` -- destroy any existing lab with the same name before deploying.
- `--daemon` -- start the Zenoh metrics daemon after deployment.

### destroy

```bash
sudo nlink-lab destroy <lab-name> [--force]
```

`--force` continues cleanup even if some resources are already gone.

### exec

```bash
sudo nlink-lab exec <lab> <node> -- <command> [args...]
```

Everything after `--` is passed to the command. Examples:

```bash
sudo nlink-lab exec mylab router -- ip route show
sudo nlink-lab exec mylab host -- iperf3 -c 10.0.0.1 -t 10
sudo nlink-lab exec mylab host -- bash
```

### status

```bash
nlink-lab status [<lab-name>] [--json]
```

Without a lab name, lists all running labs. With a name, shows detailed node and link information. `--json` for machine-readable output.

### apply

```bash
sudo nlink-lab apply topology.nll [--dry-run]
```

Hot-reload: compares the new topology against the running lab and applies only the differences. Use `--dry-run` to preview changes.

### impair

```bash
sudo nlink-lab impair <lab> <endpoint> --delay 50ms --jitter 5ms --loss 1%
sudo nlink-lab impair <lab> --show
sudo nlink-lab impair <lab> <endpoint> --clear
```

Modify netem parameters at runtime without redeploying. `--show` displays current impairments on all interfaces. `--clear` removes all impairments from an endpoint.

### capture

```bash
sudo nlink-lab capture <lab> <endpoint> [-w capture.pcap] [-c 100] [-f "tcp port 80"]
```

Runs tcpdump inside the node's namespace. `-w` writes to pcap file. `-c` limits packet count. `-f` sets BPF filter.

### daemon

```bash
sudo nlink-lab daemon <lab> [--interval 2] [--zenoh-mode peer] [--zenoh-listen tcp/0.0.0.0:7447]
```

Starts the Zenoh backend. Publishes per-interface metrics at the specified interval. See "Daemon Mode and TopoViewer" below.

### metrics

```bash
nlink-lab metrics <lab> [--node router] [--format table] [--count 10] [--zenoh-connect tcp/127.0.0.1:7447]
```

Subscribes to live metrics from a running daemon. Does not require root. `--count` exits after N samples. `--format json` for machine-readable output.

### init

```bash
nlink-lab init <template> [-o ./output/] [-n my-lab] [--force]
nlink-lab init --list
```

Creates a topology file from a built-in template. Use `--list` to see available templates.

---

## Runtime Operations

### Modify Impairments at Runtime

No redeployment needed. Change delay, jitter, loss, or rate on any interface:

```bash
sudo nlink-lab impair mylab router:wan0 --delay 100ms --loss 5%
sudo nlink-lab impair mylab router:wan0 --clear
sudo nlink-lab impair mylab --show
```

### Packet Capture

Capture traffic on any interface:

```bash
sudo nlink-lab capture mylab router:eth0 -w /tmp/router-eth0.pcap -f "icmp"
```

Open the pcap with Wireshark or tcpdump for analysis.

### Diagnostics

```bash
sudo nlink-lab diagnose mylab
sudo nlink-lab diagnose mylab router
```

Checks interface state, address assignment, route tables, and connectivity for all nodes or a specific node.

### Hot-Reload with Apply

Edit the topology file and apply changes to a running lab:

```bash
# Add a new node or change impairments in the .nll file, then:
sudo nlink-lab apply topology.nll --dry-run   # preview
sudo nlink-lab apply topology.nll             # apply
```

### Process Management

```bash
sudo nlink-lab ps mylab                       # list background processes
sudo nlink-lab kill mylab <pid>               # kill a specific process
```

Background processes are those started with `run background [...]` in the topology.

---

## Templates

List all templates:

```bash
nlink-lab init --list
```

| Template | Description | Nodes | Key Features |
|----------|-------------|-------|--------------|
| `simple` | Two nodes with one link and netem impairment | 2 | veth, addresses, routes, netem |
| `router` | Router between two subnets with IP forwarding | 3 | profiles, ip-forwarding, default-routes |
| `spine-leaf` | Datacenter fabric: 2 spines, 2 leaves, 2 servers | 6 | profiles, loopback, multi-hop |
| `wan` | Two sites over impaired WAN link | 4 | delay, loss, rate-limiting, jitter |
| `firewall` | Server behind a stateful nftables firewall | 3 | nftables, conntrack, policy |
| `vlan-trunk` | Bridge with VLAN filtering, trunk and access ports | 4 | bridge, vlan-filtering, pvid, tagged |
| `vrf` | PE router with VRF tenant isolation | 3 | vrf, routing-tables, tenant-isolation |
| `wireguard` | Site-to-site WireGuard VPN tunnel | 2 | wireguard, encryption, tunnel |
| `vxlan` | VXLAN overlay between two VTEPs | 2 | vxlan, overlay, underlay |
| `container` | Alpine container connected to a namespace host | 2 | container, mixed-topology, docker |
| `mesh` | Full mesh of 4 nodes (6 links) | 4 | full-mesh, point-to-point |
| `iperf` | Throughput test with iperf3 and rate limiting | 2 | iperf3, rate-limiting, exec |

Create a lab from a template:

```bash
nlink-lab init spine-leaf -n my-dc -o ./labs/
sudo nlink-lab deploy ./labs/my-dc.nll
```

---

## Daemon Mode and TopoViewer

### Zenoh Backend Daemon

The daemon collects per-interface metrics (rx/tx bytes, packets, errors, drops, bitrates) and publishes them over Zenoh.

Start with deployment:

```bash
sudo nlink-lab deploy topology.nll --daemon
```

Or attach to a running lab:

```bash
sudo nlink-lab daemon mylab --interval 2
```

The daemon publishes on these Zenoh key expressions:

- `nlink-lab/<lab>/metrics/snapshot` -- full metrics snapshot (all nodes, all interfaces)
- `nlink-lab/<lab>/metrics/<node>/<iface>` -- per-interface metrics

### Streaming Metrics

Subscribe from any machine (no root required):

```bash
nlink-lab metrics mylab
nlink-lab metrics mylab --node router --format json --count 5
```

Table output shows per-interface rx/tx rates, packet counts, errors, and drops.

### TopoViewer GUI

The topoviewer is an Iced-based GUI that visualizes the live topology. It connects to the daemon via Zenoh and displays:

- Force-directed graph layout of nodes and links
- Live per-interface throughput metrics on link edges
- Pan, zoom, click-select, and drag interaction
- PNG export of the current view

Launch it while a daemon is running to get a real-time view of your lab's network state.
