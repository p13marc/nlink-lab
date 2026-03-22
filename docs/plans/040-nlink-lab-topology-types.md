# Plan 040: nlink-lab Topology Types & TOML Parser

**Priority:** Critical (Phase 2, step 1)
**Effort:** 3-4 days
**Target:** `crates/nlink-lab`

## Summary

Create the `nlink-lab` crate with the topology data model and TOML parser. This is
the foundation — all subsequent plans build on these types.

The crate lives at `crates/nlink-lab/` and is a library crate that also powers the
`bins/lab/` CLI binary (Plan 043). The types must support both TOML deserialization
and programmatic construction via a Rust builder DSL.

## Status

**~80% complete.** Crate structure, all 16 topology types, parser, and error types
are implemented. Remaining: builder DSL, complex TOML test cases, `Serialize` derives.

## What's Done

- Workspace and crate structure (`crates/nlink-lab/` + `bins/lab/`)
- All topology types in `types.rs` with `Deserialize + Debug + Clone`
- `EndpointRef` parsing with `Display` impl
- `Topology` helper methods: `namespace_name()`, `effective_sysctls()`, `effective_firewall()`
- TOML parser (`parse()` and `parse_file()`)
- Error enum with 10 variants
- 11 unit tests covering core parsing scenarios

## Remaining Work

### Add `Serialize` to All Types

Required for state persistence (Plan 042 writes `topology.toml` and `state.json`).

**File:** `crates/nlink-lab/src/types.rs`

- [ ] Add `serde::Serialize` derive to all types (`Topology`, `LabConfig`, `Profile`,
      `Node`, `InterfaceConfig`, `RouteConfig`, `Link`, `Network`, `VlanConfig`,
      `PortConfig`, `Impairment`, `RateLimit`, `FirewallConfig`, `FirewallRule`,
      `ExecConfig`, `VrfConfig`, `WireguardConfig`)
- [ ] Add `Serialize` to `EndpointRef` (manual impl or `serde(into/from)`)

### Rust Builder DSL (`builder.rs`)

Provides programmatic topology construction for integration tests and dynamic topologies.
The builder produces the same `Topology` struct as TOML parsing.

**File:** `crates/nlink-lab/src/builder.rs` (new)

**Design:** The builder follows the fluent API from `NLINK_LAB.md` section 4.5:

```rust
use nlink_lab::builder::Lab;

let topology = Lab::new("my-lab")
    .profile("router", |p| p
        .sysctl("net.ipv4.ip_forward", "1"))
    .node("r1", |n| n
        .profile("router")
        .interface("lo", |i| i.address("10.0.0.1/32"))
        .route("default", |r| r.via("10.0.1.1")))
    .node("h1", |n| n
        .route("default", |r| r.via("10.0.0.1")))
    .link("r1:eth0", "h1:eth0", |l| l
        .addresses("10.0.0.1/24", "10.0.0.2/24")
        .mtu(9000))
    .impair("r1:eth0", |i| i
        .delay("10ms").jitter("2ms"))
    .rate_limit("h1:eth0", |r| r
        .egress("1gbit"))
    .build();
```

**Implementation detail:** Each builder struct (e.g., `ProfileBuilder`, `NodeBuilder`,
`LinkBuilder`) collects values and produces the corresponding type from `types.rs`.
The `Lab` struct holds a `Topology` being built and returns it from `.build()`.

Tasks:

- [ ] `Lab::new(name: &str) -> Self` — creates `Topology` with `LabConfig`
- [ ] `Lab::description(self, desc: &str) -> Self`
- [ ] `Lab::prefix(self, prefix: &str) -> Self`
- [ ] `Lab::profile(self, name: &str, f: impl FnOnce(ProfileBuilder) -> ProfileBuilder) -> Self`
- [ ] `ProfileBuilder` — `.sysctl(key, value)`, `.firewall(|f| ...)`, returns `Profile`
- [ ] `Lab::node(self, name: &str, f: impl FnOnce(NodeBuilder) -> NodeBuilder) -> Self`
- [ ] `NodeBuilder` — `.profile(name)`, `.interface(name, |i| ...)`, `.route(dest, |r| ...)`,
      `.exec(cmd, args)`, `.vrf(name, |v| ...)`, `.wireguard(name, |w| ...)`
- [ ] `InterfaceBuilder` — `.kind(k)`, `.address(cidr)`, `.vni(n)`, `.mtu(n)`
- [ ] `RouteBuilder` — `.via(gw)`, `.dev(name)`, `.metric(n)`
- [ ] `Lab::link(self, ep1: &str, ep2: &str, f: impl FnOnce(LinkBuilder) -> LinkBuilder) -> Self`
- [ ] `LinkBuilder` — `.addresses(a, b)`, `.mtu(n)`
- [ ] `Lab::network(self, name: &str, f: impl FnOnce(NetworkBuilder) -> NetworkBuilder) -> Self`
- [ ] `NetworkBuilder` — `.kind(k)`, `.vlan_filtering(b)`, `.member(ep)`, `.vlan(id, |v| ...)`
- [ ] `Lab::impair(self, endpoint: &str, f: impl FnOnce(ImpairmentBuilder) -> ImpairmentBuilder) -> Self`
- [ ] `ImpairmentBuilder` — `.delay(d)`, `.jitter(j)`, `.loss(l)`, `.rate(r)`, `.corrupt(c)`, `.reorder(r)`
- [ ] `Lab::rate_limit(self, endpoint: &str, f: impl FnOnce(RateLimitBuilder) -> RateLimitBuilder) -> Self`
- [ ] `RateLimitBuilder` — `.egress(r)`, `.ingress(r)`, `.burst(b)`
- [ ] `Lab::build(self) -> Topology`

### Additional Parser Tests

**File:** `crates/nlink-lab/src/parser.rs`

- [ ] Test: parse the full datacenter-sim example from `NLINK_LAB.md` section 4.3
- [ ] Test: parse VLAN trunk example (section 4.4)
- [ ] Test: parse WireGuard VPN example (section 4.4)
- [ ] Test: parse VRF multi-tenant example (section 4.4)
- [ ] Test: parse VXLAN overlay example (section 4.4)
- [ ] Test: malformed TOML returns `Error::TomlParse`
- [ ] Test: empty file returns error (missing `[lab]`)

### Builder Tests

**File:** `crates/nlink-lab/src/builder.rs`

- [ ] Test: builder produces same `Topology` as equivalent TOML
- [ ] Test: builder with profiles, nodes, links, impairments, rate limits
- [ ] Test: builder with networks and VLANs
- [ ] Test: builder with VRF configuration
- [ ] Test: builder with WireGuard configuration

### Public API Updates

**File:** `crates/nlink-lab/src/lib.rs`

- [ ] Add `pub mod builder;`
- [ ] Re-export `builder::Lab` (or `builder::LabBuilder`)
