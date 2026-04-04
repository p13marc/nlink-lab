# Plan 147: Show Resolved IPs in Validate/Render

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (1–2 hours)
**Priority:** P2 — helps debug subnet auto-allocation before deploy

---

## Problem Statement

`nlink-lab validate` validates the topology but doesn't show what IPs will be assigned.
When using `network` blocks with `subnet` auto-allocation, users can't verify IP
assignments without deploying.

## Proposed CLI

```bash
$ nlink-lab validate --show-ips topology.nll
Topology "my-lab" is valid
  Nodes:       3
  Links:       2

  Addresses:
    infra:eth0      10.1.0.1/24  (network "lan")
    publisher:eth0  10.1.0.2/24  (network "lan")
    subscriber:eth0 10.1.0.3/24  (network "lan")
    router:eth0     10.0.0.1/24  (link)
    router:eth1     10.0.0.2/24  (link)
```

Also available in `render --json` output (already includes the full topology which
has the resolved addresses).

## Implementation

### Step 1: CLI flag (`bins/lab/src/main.rs`)

Add `--show-ips` to the `Validate` command:

```rust
/// Show resolved IP addresses for all interfaces.
#[arg(long)]
show_ips: bool,
```

### Step 2: Collect and print addresses

After validation, iterate the topology to collect all addresses:

```rust
if show_ips {
    println!("\n  Addresses:");
    // From links
    for link in &topo.links {
        // print endpoint -> address pairs
    }
    // From network ports
    for (net_name, network) in &topo.networks {
        for member in &network.members {
            // print member -> port address pairs
        }
    }
    // From node interfaces (loopback, etc.)
    for (name, node) in &topo.nodes {
        for (iface, cfg) in &node.interfaces {
            // print node:iface -> address pairs
        }
    }
}
```

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +35 | CLI flag + address collection/printing |
| **Total** | ~35 | |
