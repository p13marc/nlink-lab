# nlink-lab

A Rust-based network lab engine for creating isolated, reproducible network
topologies using Linux network namespaces. Deep control over L2/L3/L4 networking
(TC, nftables, WireGuard, VRF, VXLAN) through the NLL topology DSL or a
programmatic Rust API.

## Quick Start

```bash
# Install system-wide (with just)
just install                # builds + installs to /usr/local/bin + sets CAP_NET_ADMIN

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

Post-deploy connectivity checks declared in the topology:

```nll
validate {
    reach host1 host2        # host1 can ping host2
    no-reach host1 host3     # firewall blocks this path
}
```

Use `--skip-validate` to disable assertion execution at deploy time.

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
| `management-network` | OOB management bridge with `mgmt` subnet |
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
- Root or `CAP_NET_ADMIN` capability
- Rust 1.85+ (edition 2024)

## License

MIT OR Apache-2.0
