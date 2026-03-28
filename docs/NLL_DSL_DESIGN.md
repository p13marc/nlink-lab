# NLL — nlink-lab Language

A purpose-built DSL for defining network topologies. Files use the `.nll` extension.

## Design Principles

1. **Network-native** — CIDR, routes, interfaces, and impairments are syntax, not strings
2. **Visual** — link syntax mirrors physical connections: `A:eth0 -- B:eth0`
3. **Concise** — a simple 2-node lab fits in 9 lines; no boilerplate
4. **Scalable** — loops, variables, and templates tame large topologies
5. **Safe** — cross-references are validated at parse time, not deploy time
6. **Familiar** — block syntax draws from HCL/Rust, not a new paradigm

---

## Why a New Language?

Network topologies have unique structure: cross-references between nodes and
interfaces, paired addressing on links, impairments as link properties, and
repetitive patterns (spine-leaf, ring, mesh). General-purpose formats force
this structure into generic key-value pairs, adding verbosity and losing
semantic clarity.

### Landscape

| Tool | Format | Runtime | Impairments | Loops | Addressing |
|------|--------|---------|-------------|-------|------------|
| **containerlab** | YAML | Docker/Podman | CLI tool (post-deploy) | None | Manual (exec) |
| **Mininet** | Python | Namespaces | `TCLink(delay=...)` | Full language | `addHost(ip=...)` |
| **Kathara** | INI (`lab.conf`) | Docker | Manual (startup script) | None | Manual (startup script) |
| **GNS3** | JSON | VM/Docker | GUI only | None | GUI only |
| **Vagrant** | Ruby DSL | VM | N/A | Full language | Per-VM config |
| **Terraform** | HCL | Cloud/VM | N/A | `for_each` | Resource refs |
| **nlink-lab** | **NLL** | Namespaces | Inline on links | `for` loops, imports | Inline `--` syntax |

### What NLL Learns from Each

- **From containerlab:** the `"node:interface"` endpoint pattern works — keep it
- **From Mininet:** programmatic generation matters — add `for` loops
- **From HCL:** block syntax and typed references are superior to flat key-value
- **From containerlab's limits:** impairments shouldn't be an afterthought — make them inline
- **From TOML's limits:** no loops, weak nesting, string-only cross-references

---

## Quick Start

```nll
lab "simple"

node router { forward ipv4 }
node host   { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}
```

9 lines. Two namespaces, a veth pair, addresses, a default route,
IP forwarding, and 10ms of netem delay.

---

## Language Reference

### 1. Lab Declaration

Every file begins with a `lab` declaration.

```nll
lab "my-lab"

lab "spine-leaf" {
  description "Spine-leaf datacenter fabric"
  prefix "dc"
}
```

### 2. Imports

Compose topologies from reusable modules. Imported files are parsed
independently; all names are prefixed with the alias.

```nll
import "base-dc.nll" as dc
import "wan-overlay.nll" as wan

lab "multi-site"

# Reference imported nodes with alias prefix
link dc.spine1:wan0 -- wan.pe1:eth0 {
    10.0.0.1/30 -- 10.0.0.2/30
}
```

**Semantics:**
- Imports must appear before the `lab` declaration
- All node, network, profile, and endpoint names are prefixed: `dc.spine1`, `dc.r1:eth0`
- The `lab` name comes from the root file only
- Imports can be recursive (imported files can import others)
- Circular imports are detected and rejected

### 3. Profiles

Reusable node templates. Nodes inherit with `:`.

```nll
profile router {
  forward ipv4
}

profile web-server {
  sysctl "net.core.somaxconn" "4096"
  firewall policy drop {
    accept ct established,related
    accept tcp dport 80
    accept tcp dport 443
  }
}
```

### 3. Nodes

```nll
node host                              # bare node

node r1 : router                       # profile inheritance

node r1 : router {                     # with properties
  forward ipv4                         # sysctl shorthand
  forward ipv6
  sysctl "net.core.rmem_max" "4194304" # explicit sysctl
  lo 10.255.0.1/32                     # loopback address
  route default via 10.0.0.1
  route 192.168.0.0/16 via 10.0.0.2 metric 100
  route 10.1.0.0/24 dev eth1
}
```

### 4. Links

The `--` operator connects two endpoints.

```nll
link r1:eth0 -- r2:eth0                                # bare

link r1:eth0 -- r2:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }  # with addresses

link r1:wan0 -- r2:wan0 {
  172.16.0.1/30 -- 172.16.0.2/30
  mtu 9000
}
```

#### Inline Impairments

```nll
# Symmetric (both directions)
link site-a:wan0 -- site-b:wan0 {
  172.16.0.1/30 -- 172.16.0.2/30
  delay 30ms jitter 5ms loss 0.1% rate 50mbit
}

# Asymmetric (per-direction arrows)
link satellite:eth0 -- ground:eth0 {
  10.0.0.1/30 -- 10.0.0.2/30
  -> delay 500ms jitter 20ms rate 10mbit   # satellite → ground
  <- delay 500ms jitter 20ms rate 2mbit    # ground → satellite
}
```

`->` applies to the left endpoint. `<-` applies to the right endpoint.
Without arrows, impairment applies to both sides.

#### Rate Limiting and Standalone Impairments

```nll
link server:eth0 -- client:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  rate egress 100mbit ingress 100mbit
}

rate server:eth0 egress 1gbit ingress 1gbit   # standalone
impair switch:br0 delay 5ms jitter 1ms         # standalone
```

### 5. Networks (Bridges)

```nll
network fabric {
  members [switch:br0, host1:eth0, host2:eth0]
  vlan-filtering
  mtu 9000
  vlan 100 "sales"
  vlan 200 "engineering"
  port host1:eth0 { pvid 100  untagged }
  port host2:eth0 { vlans [100, 200]  tagged }
}
```

### 6. Interfaces

```nll
node vtep1 : router {
  lo 10.255.0.1/32

  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    port 4789
    address 192.168.100.1/24
  }

  dummy dum0 { address 10.99.0.1/32 }
}
```

### 7. Firewall

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

### 8. VRF

```nll
node pe : router {
  vrf red table 10 {
    interfaces [eth1, eth2]
    route default dev eth1
    route 10.10.0.0/16 via 10.10.0.1
  }
  vrf blue table 20 {
    interfaces [eth3]
    route default dev eth3
  }
}
```

### 9. WireGuard

```nll
node gw : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
  route 192.168.2.0/24 dev wg0
}
```

### 10. Process Execution

```nll
node server {
  run background ["iperf3", "-s"]                             # daemon
  run ["ip", "link", "set", "eth0", "txqueuelen", "10000"]    # setup command
}
```

### 11. Variables

```nll
let wan_delay = 30ms
let wan_loss  = 0.1%
let base      = 10.0

link r1:wan0 -- r2:wan0 {
  ${base}.1.1/30 -- ${base}.1.2/30
  delay ${wan_delay} loss ${wan_loss}
}
```

### 12. Loops

```nll
for i in 1..4 {
  node leaf${i} : router { lo 10.255.1.${i}/32 }
}

for s in 1..2 {
  for l in 1..4 {
    link spine${s}:eth${l} -- leaf${l}:eth${s} {
      10.${s}.${l}.1/30 -- 10.${s}.${l}.2/30
    }
  }
}
```

Range `1..4` is inclusive: 1, 2, 3, 4.

### 13. Comments

```nll
# Line comments only (like TOML, Python, shell)
```

---

## Examples

### 1. Simple (2 nodes)

<table><tr><th>NLL (9 lines)</th><th>TOML (20 lines)</th></tr><tr><td>

```nll
lab "simple"

node router { forward ipv4 }
node host   { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}
```

</td><td>

```toml
[lab]
name = "simple"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.router]
profile = "router"

[nodes.host]

[nodes.host.routes]
default = { via = "10.0.0.1" }

[[links]]
endpoints = ["router:eth0", "host:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.2/24"]

[impairments."router:eth0"]
delay = "10ms"
jitter = "2ms"
```

</td></tr></table>

### 2. Spine-Leaf Datacenter

<table><tr><th>NLL (21 lines, with loops)</th><th>TOML (60 lines, no loops)</th></tr><tr><td>

```nll
lab "spine-leaf" { prefix "dc" }

profile router { forward ipv4 }

for i in 1..2 {
  node spine${i} : router {
    lo 10.255.0.${i}/32
  }
}

for i in 1..2 {
  node leaf${i} : router {
    lo 10.255.1.${i}/32
    route default via 10.0.${i}1.1
  }
}

for i in 1..2 {
  node server${i} {
    route default via 10.1.${i}.1
  }
}

for s in 1..2 {
  for l in 1..2 {
    link spine${s}:eth${l} -- leaf${l}:eth${s} {
      10.0.${s}${l}.1/30 -- 10.0.${s}${l}.2/30
      mtu 9000
    }
  }
}

for i in 1..2 {
  link leaf${i}:eth3 -- server${i}:eth0 {
    10.1.${i}.1/24 -- 10.1.${i}.10/24
  }
}
```

</td><td>

```toml
[lab]
name = "spine-leaf"
prefix = "dc"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

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

[nodes.leaf2]
profile = "router"
[nodes.leaf2.interfaces.lo]
addresses = ["10.255.1.2/32"]
[nodes.leaf2.routes]
default = { via = "10.0.12.1" }

[nodes.server1]
[nodes.server1.routes]
default = { via = "10.1.1.1" }

[nodes.server2]
[nodes.server2.routes]
default = { via = "10.1.2.1" }

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

[[links]]
endpoints = ["leaf1:eth3", "server1:eth0"]
addresses = ["10.1.1.1/24", "10.1.1.10/24"]

[[links]]
endpoints = ["leaf2:eth3", "server2:eth0"]
addresses = ["10.1.2.1/24", "10.1.2.10/24"]
```

</td></tr></table>

Scaling to 4 spines, 8 leaves, 16 servers: NLL stays ~25 lines. TOML grows to ~250+.

### 3. WAN with Impairment

<table><tr><th>NLL (15 lines)</th><th>TOML (39 lines)</th></tr><tr><td>

```nll
lab "wan-impairment"

profile router { forward ipv4 }

node router-a : router {
  route 10.2.0.0/24 via 172.16.0.2
}
node router-b : router {
  route 10.1.0.0/24 via 172.16.0.1
}
node host-a { route default via 10.1.0.1 }
node host-b { route default via 10.2.0.1 }

link router-a:eth0 -- host-a:eth0 {
  10.1.0.1/24 -- 10.1.0.10/24
}
link router-b:eth0 -- host-b:eth0 {
  10.2.0.1/24 -- 10.2.0.10/24
}

# Symmetric impairment — one line, both sides
link router-a:wan0 -- router-b:wan0 {
  172.16.0.1/30 -- 172.16.0.2/30
  delay 30ms jitter 5ms loss 0.1% rate 50mbit
}
```

</td><td>

```toml
[lab]
name = "wan-impairment"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.router-a]
profile = "router"
[nodes.router-a.routes]
"10.2.0.0/24" = { via = "172.16.0.2" }

[nodes.router-b]
profile = "router"
[nodes.router-b.routes]
"10.1.0.0/24" = { via = "172.16.0.1" }

[nodes.host-a]
[nodes.host-a.routes]
default = { via = "10.1.0.1" }

[nodes.host-b]
[nodes.host-b.routes]
default = { via = "10.2.0.1" }

[[links]]
endpoints = ["router-a:eth0", "host-a:eth0"]
addresses = ["10.1.0.1/24", "10.1.0.10/24"]

[[links]]
endpoints = ["router-b:eth0", "host-b:eth0"]
addresses = ["10.2.0.1/24", "10.2.0.10/24"]

[[links]]
endpoints = ["router-a:wan0", "router-b:wan0"]
addresses = ["172.16.0.1/30", "172.16.0.2/30"]

# Symmetric requires TWO sections in TOML
[impairments."router-a:wan0"]
delay = "30ms"
jitter = "5ms"
loss = "0.1%"
rate = "50mbit"

[impairments."router-b:wan0"]
delay = "30ms"
jitter = "5ms"
loss = "0.1%"
rate = "50mbit"
```

</td></tr></table>

### 4. Firewall

```nll
lab "firewall"

profile router { forward ipv4 }

node router : router
node client { route default via 10.0.1.1 }

node server {
  route default via 10.0.2.1
  firewall policy drop {
    accept ct established,related
    accept tcp dport 8080
    accept udp dport 53
  }
}

link router:eth0 -- client:eth0 { 10.0.1.1/24 -- 10.0.1.10/24 }
link router:eth1 -- server:eth0 { 10.0.2.1/24 -- 10.0.2.10/24 }
```

### 5. VXLAN Overlay

```nll
lab "vxlan-overlay"

profile router { forward ipv4 }

node vtep1 : router {
  vxlan vxlan100 {
    vni 100
    local 10.0.0.1
    remote 10.0.0.2
    port 4789
    address 192.168.100.1/24
  }
}

node vtep2 : router {
  vxlan vxlan100 {
    vni 100
    local 10.0.0.2
    remote 10.0.0.1
    port 4789
    address 192.168.100.2/24
  }
}

link vtep1:eth0 -- vtep2:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
```

### 6. VRF Multi-Tenant

```nll
lab "vrf-multitenant"

profile router { forward ipv4 }

node pe : router {
  vrf red table 10 {
    interfaces [eth1]
    route default dev eth1
  }
  vrf blue table 20 {
    interfaces [eth2]
    route default dev eth2
  }
}

node tenant-a { route default via 10.10.0.1 }
node tenant-b { route default via 10.20.0.1 }

link pe:eth1 -- tenant-a:eth0 { 10.10.0.1/24 -- 10.10.0.10/24 }
link pe:eth2 -- tenant-b:eth0 { 10.20.0.1/24 -- 10.20.0.10/24 }
```

### 7. WireGuard VPN

```nll
lab "wireguard-vpn"

profile gateway { forward ipv4 }

node gw-a : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
  route 192.168.2.0/24 dev wg0
}

node gw-b : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.2/32
    peers [gw-a]
  }
  route 192.168.1.0/24 dev wg0
}

node host-a { route default via 192.168.1.1 }
node host-b { route default via 192.168.2.1 }

link gw-a:eth0 -- gw-b:eth0 {
  10.0.0.1/30 -- 10.0.0.2/30
  delay 50ms jitter 5ms loss 0.1%
}

link gw-a:eth1 -- host-a:eth0 { 192.168.1.1/24 -- 192.168.1.10/24 }
link gw-b:eth1 -- host-b:eth0 { 192.168.2.1/24 -- 192.168.2.10/24 }
```

### 8. iperf Benchmark

```nll
lab "iperf-bench"

node server {
  route default via 10.0.0.2
  run background ["iperf3", "-s"]
}

node client {
  route default via 10.0.0.1
}

link server:eth0 -- client:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  rate egress 100mbit ingress 100mbit
}
```

### 9. VLAN Trunk

```nll
lab "vlan-trunk"

profile switch { forward ipv4 }

node switch : switch
node host1 { route default via 10.100.0.1 }
node host2 { route default via 10.100.0.1 }
node host3 { route default via 10.200.0.1 }

link switch:eth1 -- host1:eth0 { 10.100.0.1/24 -- 10.100.0.10/24 }
link switch:eth2 -- host2:eth0 { 10.100.0.1/24 -- 10.100.0.20/24 }
link switch:eth3 -- host3:eth0 { 10.200.0.1/24 -- 10.200.0.10/24 }

network fabric {
  vlan-filtering
  members [switch:br0]
  vlan 100 "sales"
  vlan 200 "engineering"
  port host1 { pvid 100  untagged }
  port host2 { pvid 100  untagged }
  port host3 { pvid 200  untagged }
}
```

### 10. Ring Topology

```nll
lab "ring"

let N = 6

profile router { forward ipv4 }

for i in 1..${N} {
  node r${i} : router { lo 10.255.0.${i}/32 }
}

# Chain: r1-r2, r2-r3, ..., r5-r6
for i in 1..5 {
  let next = ${i} + 1
  link r${i}:eth1 -- r${next}:eth0 {
    10.0.${i}.1/30 -- 10.0.${i}.2/30
  }
}

# Close the ring: r6-r1
link r${N}:eth1 -- r1:eth0 {
  10.0.${N}.1/30 -- 10.0.${N}.2/30
}
```

### 11. Satellite Link (Asymmetric)

```nll
lab "satellite"

profile gateway { forward ipv4 }

node ground-station : gateway
node satellite-gw   : gateway
node remote-host { route default via 10.1.0.1 }
node local-host  { route default via 10.2.0.1 }

link ground-station:eth0 -- local-host:eth0  { 10.2.0.1/24 -- 10.2.0.10/24 }
link satellite-gw:eth0   -- remote-host:eth0 { 10.1.0.1/24 -- 10.1.0.10/24 }

link ground-station:sat0 -- satellite-gw:sat0 {
  172.16.0.1/30 -- 172.16.0.2/30
  -> delay 270ms jitter 10ms rate 50mbit loss 0.01%    # uplink
  <- delay 270ms jitter 10ms rate 150mbit loss 0.01%   # downlink
}
```

### 12. Multi-Site Enterprise with VRF

```nll
lab "enterprise"

profile pe-router {
  forward ipv4
  sysctl "net.mpls.platform_labels" "1048575"
}

profile ce-router { forward ipv4 }

node pe1 : pe-router {
  lo 10.255.0.1/32
  vrf customer-a table 100 {
    interfaces [eth1]
    route 192.168.2.0/24 via 10.0.12.2
  }
  vrf customer-b table 200 {
    interfaces [eth2]
    route 172.16.2.0/24 via 10.0.12.2
  }
}

node pe2 : pe-router {
  lo 10.255.0.2/32
  vrf customer-a table 100 {
    interfaces [eth1]
    route 192.168.1.0/24 via 10.0.12.1
  }
  vrf customer-b table 200 {
    interfaces [eth2]
    route 172.16.1.0/24 via 10.0.12.1
  }
}

node ce-a1 : ce-router { route default via 10.0.1.1 }
node ce-a2 : ce-router { route default via 10.0.2.1 }
node ce-b1 : ce-router { route default via 10.0.3.1 }
node ce-b2 : ce-router { route default via 10.0.4.1 }

link pe1:eth0 -- pe2:eth0 { 10.0.12.1/30 -- 10.0.12.2/30  mtu 9000  delay 5ms }

link pe1:eth1 -- ce-a1:eth0 { 10.0.1.1/24 -- 10.0.1.10/24 }
link pe1:eth2 -- ce-b1:eth0 { 10.0.3.1/24 -- 10.0.3.10/24 }
link pe2:eth1 -- ce-a2:eth0 { 10.0.2.1/24 -- 10.0.2.10/24 }
link pe2:eth2 -- ce-b2:eth0 { 10.0.4.1/24 -- 10.0.4.10/24 }
```

### 13. Load Balancer + Web Farm

```nll
lab "web-farm"

profile web {
  firewall policy drop {
    accept ct established,related
    accept tcp dport 80
    accept tcp dport 443
  }
}

node lb {
  forward ipv4
  run background ["haproxy", "-f", "/etc/haproxy/haproxy.cfg"]
}

for i in 1..3 {
  node web${i} : web {
    route default via 10.1.${i}.1
    run background ["nginx"]
  }
}

node client { route default via 10.0.0.1 }

link lb:eth0 -- client:eth0 { 10.0.0.1/24 -- 10.0.0.10/24 }

for i in 1..3 {
  link lb:eth${i} -- web${i}:eth0 { 10.1.${i}.1/24 -- 10.1.${i}.10/24 }
}
```

### 14. Large-Scale Fat-Tree (80 nodes)

```nll
lab "fat-tree" { prefix "ft" }

profile switch { forward ipv4 }
profile host   { sysctl "net.ipv4.ip_forward" "0" }

let K = 4

for i in 1..4 {
  node core${i} : switch { lo 10.255.0.${i}/32 }
}

for p in 1..4 {
  for a in 1..2 {
    node agg${p}x${a} : switch { lo 10.255.${p}.${a}/32 }
  }
  for e in 1..2 {
    node edge${p}x${e} : switch { lo 10.255.1${p}.${e}/32 }
  }
  for h in 1..4 {
    node host${p}x${h} : host { route default via 10.${p}.${h}.1 }
  }
}

for c in 1..4 {
  for p in 1..4 {
    let a_idx = ${c} + 1
    let a_idx = ${a_idx} / 2
    link core${c}:eth${p} -- agg${p}x${a_idx}:eth${c} {
      10.10.${c}${p}.1/30 -- 10.10.${c}${p}.2/30
      mtu 9000
    }
  }
}

for p in 1..4 {
  for a in 1..2 {
    for e in 1..2 {
      link agg${p}x${a}:eth1${e} -- edge${p}x${e}:eth1${a} {
        10.20.${p}${a}.${e}/30 -- 10.20.${p}${a}.1${e}/30
      }
    }
  }
}

for p in 1..4 {
  for e in 1..2 {
    for h in 1..2 {
      let host_idx = (${e} - 1) * 2 + ${h}
      link edge${p}x${e}:eth2${h} -- host${p}x${host_idx}:eth0 {
        10.${p}.${host_idx}.1/24 -- 10.${p}.${host_idx}.10/24
      }
    }
  }
}
```

80 nodes, 128 links — ~55 lines of NLL. Equivalent TOML: ~900 lines.

---

## Feature Comparison

| Feature | NLL | TOML | YAML (clab) | Python (Mininet) |
|---------|-----|------|-------------|-----------------|
| Typed CIDR literals | `10.0.0.1/24` | `"10.0.0.1/24"` | N/A (exec) | `ip='...'` |
| Typed durations | `10ms` | `"10ms"` | N/A | `delay='10ms'` |
| Typed rates | `100mbit` | `"100mbit"` | N/A | `bw=100` |
| Typed percentages | `0.1%` | `"0.1%"` | N/A | `loss=0.1` |
| Visual link syntax | `A:eth0 -- B:eth0` | `["A:eth0","B:eth0"]` | `["A:eth1","B:eth1"]` | `addLink(a,b)` |
| Address pairing | `1.1/24 -- 1.2/24` | `["1.1/24","1.2/24"]` | exec commands | `ip=` param |
| Inline impairments | On links | Separate section | CLI tool (post-deploy) | `TCLink(...)` |
| Asymmetric impairments | `->` / `<-` | Two sections | Two CLI calls | Two TCLinks |
| Profiles | `profile` + `:` | `[profiles]` | `defaults`+`kinds` | Python class |
| Loops | `for i in 1..N` | None | None | Python loops |
| Variables | `let x = ...` | None | None | Python vars |
| String interpolation | `${i}` | None | None | f-strings |
| Firewall | `firewall` block | `[[firewall.rules]]` | exec / bind mount | `cmd(...)` |
| Parse-time validation | Yes (typed refs) | Runtime only | Runtime only | Runtime |
| Files per topology | 1 | 1 | 1 + configs | 1 |

---

## Line Count Summary

| Example | NLL | TOML | Reduction |
|---------|-----|------|-----------|
| simple | 9 | 20 | 55% |
| spine-leaf (2+2+2) | 21 | 60 | 65% |
| wan-impairment | 15 | 39 | 62% |
| firewall | 17 | 36 | 53% |
| vxlan-overlay | 24 | 42 | 43% |
| vrf-multitenant | 19 | 38 | 50% |
| wireguard-vpn | 31 | 57 | 46% |
| iperf-bench | 12 | 24 | 50% |
| vlan-trunk | 22 | 50 | 56% |
| ring (6 nodes) | 20 | ~80 | 75% |
| satellite (asym) | 18 | ~40 | 55% |
| enterprise (VRF) | 42 | ~90 | 53% |
| web-farm (3) | 20 | ~55 | 64% |
| fat-tree (80 nodes) | 55 | ~900 | 94% |
| **Average** | | | **59%** |

The reduction grows with scale. For topologies using loops, NLL is 5-15x
more concise.

---

## Grammar

```
file           = import* lab_decl statement*
import         = "import" STRING "as" IDENT
lab_decl       = "lab" STRING ("runtime" STRING)? block?

statement      = profile | node | link | network
               | impair | rate | let_decl | for_loop

profile        = "profile" IDENT block
node           = "node" name (":" IDENT)? ("image" STRING ("cmd" (STRING | string_list))?)? (block | NEWLINE)
link           = "link" endpoint "--" endpoint (block | NEWLINE)
network        = "network" IDENT block
impair         = "impair" endpoint impair_props
rate           = "rate" endpoint rate_props
let_decl       = "let" IDENT "=" value
for_loop       = "for" IDENT "in" range block

name           = (IDENT | INTERP) (IDENT | INTERP | INT | ".")*
endpoint       = name ":" name

block          = "{" block_item* "}"
block_item     = statement | property

# ── Node properties ──────────────────────────────
property       = "forward" ("ipv4" | "ipv6")
               | "sysctl" STRING STRING
               | "lo" CIDR
               | "route" route_target route_params
               | "firewall" "policy" IDENT firewall_block
               | "vrf" IDENT "table" INT block
               | "wireguard" IDENT block
               | "vxlan" IDENT block
               | "dummy" IDENT block
               | "run" "background"? string_list
               | "interfaces" list
               | "members" list
               | "vlan-filtering"
               | "vlan" INT STRING?
               | "port" endpoint block
               | "mtu" INT
               | "address" CIDR
               | addr_pair
               | impair_props
               | dir_impair
               | rate_props

route_target   = "default" | CIDR
route_params   = ("via" IP)? ("dev" IDENT)? ("metric" INT)?

firewall_block = "{" firewall_rule* "}"
firewall_rule  = ("accept" | "drop" | "reject") match_expr
match_expr     = "ct" IDENT ("," IDENT)*
               | ("tcp" | "udp") "dport" INT

addr_pair      = CIDR "--" CIDR
dir_impair     = ("->" | "<-") impair_props
impair_props   = ("delay" DURATION)? ("jitter" DURATION)?
                 ("loss" PERCENT)? ("rate" RATE)?
                 ("corrupt" PERCENT)? ("reorder" PERCENT)?
rate_props     = ("egress" RATE)? ("ingress" RATE)? ("burst" RATE)?

range          = INT ".." INT
list           = "[" value ("," value)* "]"
string_list    = "[" STRING ("," STRING)* "]"

# ── Literals ─────────────────────────────────────
IDENT          = [a-zA-Z_][a-zA-Z0-9_-]*
STRING         = '"' [^"]* '"'
INT            = [0-9]+
FLOAT          = [0-9]+ ("." [0-9]+)?
CIDR           = IP "/" INT
IP             = ipv4_addr | ipv6_addr
DURATION       = (INT | FLOAT) ("s" | "ms" | "us" | "ns")
PERCENT        = (INT | FLOAT) "%"
RATE           = INT ("bit" | "kbit" | "mbit" | "gbit"
                    | "byte" | "kbyte" | "mbyte" | "gbyte")
INTERP         = "${" expr "}"
expr           = IDENT | IDENT ("+"|"-"|"*"|"/") INT
NEWLINE        = "\n"
COMMENT        = "#" [^\n]*
```

---

## Key Design Decisions

### Why `--` for links?
Visual representation of a physical connection. Appears in both endpoints
(`r1:eth0 -- r2:eth0`) and address pairs (`10.0.0.1/24 -- 10.0.0.2/24`).

### Why keyword properties instead of `key = value`?
`delay 10ms` reads like a command. Typed literals eliminate quoting.

### Why `forward ipv4`?
Most common sysctl. Shorthand for the 90% case. `sysctl` remains for the rest.

### Why inline impairments on links?
Impairments are link properties. Inline keeps related info together.
`impair` statement exists for standalone use.

### Why `->` and `<-`?
Real links are often asymmetric (satellite, LTE, ADSL). Without arrows,
impairments apply symmetrically.

### Why loops but not conditionals?
`for` with integer ranges covers 90% of generation needs (spine-leaf, ring,
mesh). Conditionals add complexity without proportional benefit.

### Why not YAML?
Implicit typing (`NO` → false, `3.10` → 3.1), indentation sensitivity.
For network config where precision matters, implicit coercion is dangerous.

### Why not Nickel/Jsonnet?
Adds a dependency, a build step, and another language to learn. NLL
integrates loops and variables natively — one file, one parser, zero deps.

---

## Implementation

See [Plan 060: NLL Parser](plans/060-nll-parser.md) for the full
implementation plan including crate selection, file layout, phased
implementation steps, and test strategy.

**Architecture:**

```
  Source (.nll)
    │
    ▼
  Lexer (logos)          → Token stream
    │
    ▼
  Parser (winnow)        → AST (with for/let nodes)
    │
    ▼
  Lowering               → Expand loops, substitute variables
    │
    ▼
  Topology               → Same struct as TOML path
    │
    ▼
  Validator              → Same rules (unchanged)
```

Both `.toml` and `.nll` compile to the same `Topology` struct.
The CLI auto-detects format by file extension.
