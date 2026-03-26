# nlink-lab

A Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. Deep control over L2/L3/L4 networking
(TC, nftables, WireGuard, VRF, VXLAN) through declarative topology files or a
programmatic Rust API.

## Quick Start

```bash
# Build
cargo build -p nlink-lab-cli

# Validate a topology
nlink-lab validate examples/simple.toml

# Deploy (requires root)
sudo nlink-lab deploy examples/simple.toml

# Run commands in lab nodes
sudo nlink-lab exec simple host -- ping -c1 10.0.0.1
sudo nlink-lab exec simple router -- ip route

# Show running labs
nlink-lab status

# Tear down
sudo nlink-lab destroy simple
```

## Topology Formats

nlink-lab supports two topology formats that produce identical results:

### TOML

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

### NLL (nlink-lab Language)

A purpose-built DSL with typed literals, visual link syntax, loops, and variables:

```
lab "simple"

profile router { forward ipv4 }
node router : router
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}
```

NLL supports `for` loops and `${variables}` for generating large topologies:

```
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

## Rust API

```rust
use nlink_lab::{Lab, parser};

// From file
let topo = parser::parse_file("datacenter.toml")?;

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

#[lab_test("examples/simple.toml")]
async fn test_connectivity(lab: RunningLab) {
    let out = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
    assert_eq!(out.exit_code, 0);
}

#[lab_test(topology = my_topology)]
async fn test_custom(lab: RunningLab) {
    // ...
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

Each example is available in both `.toml` and `.nll` formats.

## Requirements

- Linux (kernel 4.19+)
- Root or `CAP_NET_ADMIN` capability
- Rust 1.85+ (edition 2024)

## License

MIT OR Apache-2.0
