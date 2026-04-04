# Plan 138: Node IP Discovery CLI

**Date:** 2026-04-04
**Status:** Done
**Effort:** Small (2–3 hours)
**Priority:** P2 — enables dynamic address construction in test scripts

---

## Problem Statement

Node IPs can be extracted from `nlink-lab status --json` by parsing the full topology
and matching link endpoints, but there's no direct query. Test scripts need IP addresses
to construct service URLs, and the current approach requires `jq` gymnastics.

## Proposed CLI

```bash
# Get all IPs for a node
nlink-lab ip my-lab server
# eth0: 10.0.0.1/24
# lo: 10.255.0.1/32

# JSON output
nlink-lab ip --json my-lab server
# {"eth0": ["10.0.0.1/24"], "lo": ["10.255.0.1/32"]}

# Specific interface, bare IP (no prefix length)
nlink-lab ip my-lab server --iface eth0
# 10.0.0.1

# Specific interface with prefix
nlink-lab ip my-lab server --iface eth0 --cidr
# 10.0.0.1/24
```

The bare-IP form (without `--cidr`) is designed for direct interpolation in scripts:

```bash
ADDR=$(nlink-lab ip my-lab server --iface eth0)
nlink-lab exec my-lab client -- curl http://${ADDR}:8080/health
```

## Design Decisions

### Data source

Read from the persisted topology (not live `ip addr` output). The topology already has
all assigned addresses from link definitions, network memberships, and loopback pools.
This is faster and works without entering the namespace.

### Address collection

Collect addresses from:
1. **Links:** For each link, both endpoints have addresses. If the node matches the
   left endpoint, collect left addresses. Same for right.
2. **Networks:** Bridge members may have addresses assigned.
3. **Loopback:** `lo_addrs` field on the node.
4. **Mgmt:** If management network is configured, include `mgmt0` address.

### Multiple addresses per interface

An interface can have multiple IPs (IPv4 + IPv6, or multiple v4). Return all of them.
`--iface` with bare output returns the **first** address only. `--json` returns the
full list.

## Implementation

### Step 1: Library method (`running.rs`)

```rust
/// Collect all IP addresses for a node, grouped by interface name.
pub fn node_addresses(&self, node: &str) -> Result<HashMap<String, Vec<String>>> {
    // Verify node exists
    self.namespace_for(node)?;

    let mut addrs: HashMap<String, Vec<String>> = HashMap::new();

    // From links
    for link in &self.topology.links {
        if link.left.node == node {
            for addr in &link.left.addresses {
                addrs.entry(link.left.iface.clone()).or_default().push(addr.clone());
            }
        }
        if link.right.node == node {
            for addr in &link.right.addresses {
                addrs.entry(link.right.iface.clone()).or_default().push(addr.clone());
            }
        }
    }

    // From loopback
    let node_def = self.topology.nodes.iter().find(|n| n.name == node);
    if let Some(n) = node_def {
        for addr in &n.lo_addrs {
            addrs.entry("lo".to_string()).or_default().push(addr.clone());
        }
    }

    Ok(addrs)
}
```

### Step 2: CLI definition (`bins/lab/src/main.rs`)

```rust
/// Show IP addresses assigned to a node.
Ip {
    /// Lab name.
    lab: String,

    /// Node name.
    node: String,

    /// Filter by interface name.
    #[arg(long)]
    iface: Option<String>,

    /// Show CIDR notation (include prefix length).
    #[arg(long)]
    cidr: bool,
},
```

### Step 3: CLI handler

```rust
Commands::Ip { lab, node, iface, cidr } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let addrs = running.node_addresses(&node)?;

    if let Some(ref iface_name) = iface {
        let iface_addrs = addrs.get(iface_name)
            .ok_or_else(|| format!("interface '{iface_name}' not found on node '{node}'"))?;

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&iface_addrs)?);
        } else if let Some(first) = iface_addrs.first() {
            if cidr {
                println!("{first}");
            } else {
                // Strip /prefix
                println!("{}", first.split('/').next().unwrap_or(first));
            }
        }
    } else {
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&addrs)?);
        } else {
            let mut sorted: Vec<_> = addrs.iter().collect();
            sorted.sort_by_key(|(k, _)| k.clone());
            for (iface_name, iface_addrs) in sorted {
                for addr in iface_addrs {
                    if cidr {
                        println!("{iface_name}: {addr}");
                    } else {
                        println!("{iface_name}: {}", addr.split('/').next().unwrap_or(addr));
                    }
                }
            }
        }
    }
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_ip_all_interfaces` | integration.rs | Lists all interfaces with addresses |
| `test_ip_specific_interface` | integration.rs | `--iface eth0` returns single IP |
| `test_ip_bare_no_prefix` | integration.rs | Default strips `/24` |
| `test_ip_cidr_flag` | integration.rs | `--cidr` keeps prefix length |
| `test_ip_json` | integration.rs | JSON output structure |
| `test_ip_unknown_node_error` | running.rs | Unknown node → error |
| `test_ip_unknown_iface_error` | running.rs | Unknown interface → error |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `running.rs` | +30 | `node_addresses()` method |
| `main.rs` | +45 | CLI variant + handler |
| Tests | +50 | 7 test functions |
| **Total** | ~125 | |
