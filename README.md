# nlink-lab

A Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. Deep control over L2/L3/L4 networking
(TC, nftables, WireGuard, VRF, VXLAN) through the NLL topology DSL or a
programmatic Rust API.

## Quick Start

```bash
# Build
cargo build -p nlink-lab-cli

# Create a topology from a template
nlink-lab init router

# Validate
nlink-lab validate router.nll

# Deploy (requires root)
sudo nlink-lab deploy router.nll

# Run commands in lab nodes
sudo nlink-lab exec router host -- ping -c1 10.0.0.1

# Show running labs
nlink-lab status

# Tear down
sudo nlink-lab destroy router
```

## NLL — nlink-lab Language

A purpose-built DSL with typed literals, visual link syntax, loops, variables,
cross-references, block comments, and composable modules:

```nll
lab "datacenter" {
  version "2.0"
  tags [fabric, l3]
}

defaults link { mtu 9000 }

profile router { forward ipv4 }
node router : router
node host { route default via ${router.eth0} }  /* cross-reference */

link router:eth0 -- host:eth0 {
  10.0.0.0/30          /* subnet auto-assign: .1 and .2 */
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
link dc.spine1:mon0 -- monitor:eth0 { 172.16.0.0/30 }
```

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

### Reachability Assertions

```nll
validate {
    reach host1 host2        # host1 can ping host2
    no-reach host1 host3     # firewall blocks this path
}
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
| `imports/base-network` | Reusable base network module |

All examples use the `.nll` format. Use `nlink-lab init --list` to create from templates.

## Comparison with containerlab

| | **nlink-lab** | **containerlab** |
|---|---|---|
| **Abstraction** | Network namespaces | Docker/podman containers |
| **Focus** | L2/L3/L4 networking primitives | NOS container orchestration |
| **Topology format** | NLL DSL (loops, imports, typed) | YAML |
| **Programmatic API** | Rust builder DSL + `#[lab_test]` macro | Go library |
| **Traffic control** | Native TC/netem integration | External tools |
| **Firewall** | Native nftables | Container-dependent |
| **VRF / VXLAN / WireGuard** | First-class support | Depends on NOS image |
| **Dependencies** | Linux kernel only | Docker/podman runtime |
| **Startup time** | Milliseconds (namespace creation) | Seconds (container pull + boot) |
| **Best for** | Protocol testing, network simulation, CI | Multi-vendor NOS labs |

## Requirements

- Linux (kernel 4.19+)
- Root or `CAP_NET_ADMIN` capability
- Rust 1.85+ (edition 2024)

## License

MIT OR Apache-2.0
