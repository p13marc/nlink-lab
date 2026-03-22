# Plan 040: nlink-lab Topology Types & TOML Parser

**Priority:** Critical (Phase 2, step 1)
**Effort:** 3-4 days
**Target:** New crate `crates/nlink-lab`

## Summary

Create the `nlink-lab` crate with the topology data model and TOML parser. This is
the foundation — all subsequent plans build on these types.

The crate lives at `crates/nlink-lab/` and is a library crate that also powers the
`bins/lab/` CLI binary (Plan 043). The types must support both TOML deserialization
and programmatic construction via a Rust builder DSL.

## Crate Setup

```
crates/nlink-lab/
  Cargo.toml          # depends on nlink, serde, toml
  src/
    lib.rs            # Public API: Lab, Profile, Node, Link, etc.
    types.rs          # Core topology types (serde + builder)
    parser.rs         # TOML parsing and normalization
    error.rs          # LabError type
```

`Cargo.toml` dependencies:
- `nlink` (workspace) — for networking types (IpAddr, etc.)
- `serde` + `serde_derive` — deserialization
- `toml` — TOML parsing
- `thiserror` — error types
- `tokio` (workspace) — async runtime (for deploy/destroy)

## Type Design

The topology types mirror the TOML structure from NLINK_LAB.md section 4.3:

```rust
// Top-level topology
pub struct Topology {
    pub lab: LabConfig,
    pub profiles: HashMap<String, Profile>,
    pub nodes: HashMap<String, Node>,
    pub links: Vec<Link>,
    pub networks: HashMap<String, Network>,       // L2 bridge segments
    pub impairments: HashMap<String, Impairment>,  // keyed by "node:iface"
    pub rate_limits: HashMap<String, RateLimit>,   // keyed by "node:iface"
}

pub struct LabConfig {
    pub name: String,
    pub description: Option<String>,
    pub prefix: Option<String>,
}

pub struct Profile {
    pub sysctls: HashMap<String, String>,
    pub firewall: Option<FirewallConfig>,
}

pub struct Node {
    pub profile: Option<String>,
    pub sysctls: HashMap<String, String>,          // merged with profile
    pub interfaces: HashMap<String, InterfaceConfig>,
    pub routes: HashMap<String, RouteConfig>,
    pub firewall: Option<FirewallConfig>,
    pub exec: Vec<ExecConfig>,
    pub vrfs: HashMap<String, VrfConfig>,
    pub wireguard: HashMap<String, WireguardConfig>,
}

pub struct Link {
    pub endpoints: [String; 2],                    // "node:iface" format
    pub addresses: Option<[String; 2]>,            // CIDR for each end
    pub mtu: Option<u32>,
}

pub struct Impairment {
    pub delay: Option<String>,
    pub jitter: Option<String>,
    pub loss: Option<String>,
    pub rate: Option<String>,
    pub corrupt: Option<String>,
    pub reorder: Option<String>,
}

pub struct RateLimit {
    pub egress: Option<String>,
    pub ingress: Option<String>,
    pub burst: Option<String>,
}
```

## Progress

### Crate Setup

- [ ] Create `crates/nlink-lab/Cargo.toml` with dependencies
- [ ] Add `crates/nlink-lab` to workspace `Cargo.toml`
- [ ] Create `src/lib.rs` with module structure
- [ ] Create `src/error.rs` with `LabError` type

### Core Types (`types.rs`)

- [ ] `Topology` — top-level container
- [ ] `LabConfig` — lab metadata (name, description, prefix)
- [ ] `Profile` — reusable node template (sysctls, firewall)
- [ ] `Node` — namespace definition (profile, interfaces, routes, exec, vrfs, wireguard)
- [ ] `InterfaceConfig` — explicit interface (kind, addresses, vni, etc.)
- [ ] `RouteConfig` — route entry (via, metric)
- [ ] `Link` — point-to-point veth connection (endpoints, addresses, mtu)
- [ ] `Network` — L2 bridge segment (kind, vlan_filtering, vlans, ports)
- [ ] `Impairment` — netem config (delay, jitter, loss, rate, corrupt, reorder)
- [ ] `RateLimit` — per-interface shaping (egress, ingress, burst)
- [ ] `FirewallConfig` — nftables rules (policy, rules list)
- [ ] `FirewallRule` — single rule (match expression, action)
- [ ] `ExecConfig` — process to spawn (cmd, background)
- [ ] `VrfConfig` — VRF definition (table, interfaces, routes)
- [ ] `WireguardConfig` — WG interface (private_key, listen_port, addresses, peers)
- [ ] All types derive `serde::Deserialize` + `Debug` + `Clone`

### TOML Parser (`parser.rs`)

- [ ] `pub fn parse(toml_str: &str) -> Result<Topology>` — parse TOML string
- [ ] `pub fn parse_file(path: &Path) -> Result<Topology>` — parse TOML file
- [ ] Profile merging: node inherits sysctls/firewall from referenced profile
- [ ] Endpoint parsing: split `"node:iface"` into (node_name, iface_name)
- [ ] Default prefix: use lab name if prefix not specified

### Rust Builder DSL (`builder.rs`)

- [ ] `Lab::new(name)` — create topology programmatically
- [ ] `.profile(name, Profile::new().sysctl(...))` — add profile
- [ ] `.node(name, |n| n.profile(...).lo(...).route(...))` — add node
- [ ] `.link(ep1, ep2, |l| l.addresses(...).mtu(...))` — add link
- [ ] `.impair(endpoint, |i| i.delay(...).loss(...))` — add impairment
- [ ] `.rate_limit(endpoint, |r| r.egress(...))` — add rate limit
- [ ] `.build() -> Topology` — finalize

### Tests

- [ ] Parse the datacenter-sim example from NLINK_LAB.md section 4.3
- [ ] Parse VLAN trunk example
- [ ] Parse WireGuard example
- [ ] Parse VRF example
- [ ] Parse VXLAN overlay example
- [ ] Builder DSL produces same types as TOML parsing
- [ ] Profile merging works correctly
- [ ] Error on malformed TOML

### Documentation

- [ ] Doc comments on all public types
- [ ] Module-level examples
