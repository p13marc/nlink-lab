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

A purpose-built DSL with typed literals, visual link syntax, loops, and variables:

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

### Loops and Variables

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

### Imports

Compose topologies from reusable modules:

```nll
import "base-dc.nll" as dc

lab "extended"

node monitor
link dc.spine1:eth3 -- monitor:eth0 { 10.99.0.1/30 -- 10.99.0.2/30 }
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
| `spine-leaf` | Datacenter fabric (6 nodes) |
| `wan-impairment` | WAN with delay, loss, rate limiting |
| `firewall` | Stateful nftables firewall |
| `vxlan-overlay` | VXLAN tunnel between VTEPs |
| `vrf-multitenant` | VRF tenant isolation |
| `wireguard-vpn` | Site-to-site WireGuard VPN |
| `iperf-benchmark` | Throughput testing with rate limiting |
| `vlan-trunk` | Bridge with VLAN filtering |
| `container` | Docker/Podman container nodes |
| `asymmetric` | Directional impairments (`->` / `<-`) |
| `imports/composed` | Topology composition via imports |

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
