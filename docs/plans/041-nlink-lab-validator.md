# Plan 041: nlink-lab Topology Validator

**Priority:** Critical (Phase 2, step 2)
**Effort:** 2-3 days
**Target:** `crates/nlink-lab`
**Depends on:** Plan 040 (types)

## Summary

Validate a parsed `Topology` before deployment. Catches configuration errors early
with clear, context-rich error messages. Implements all validation rules from
`NLINK_LAB.md` section 4.6.

The validator is **not** the TOML parser — the parser ensures the file is valid TOML
that deserializes into `Topology`. The validator checks semantic correctness: do
referenced nodes exist? Are IP addresses valid CIDR? Are VLAN IDs in range?

## Status

**0% complete.** No file exists yet.

## API Design

**File:** `crates/nlink-lab/src/validator.rs` (new)

```rust
use nlink_lab::{Topology, ValidationResult, ValidationIssue, Severity};

let topology = parser::parse_file("datacenter.toml")?;
let result = topology.validate();

if result.has_errors() {
    for issue in result.errors() {
        eprintln!("ERROR: {issue}");
    }
    std::process::exit(1);
}

for warning in result.warnings() {
    eprintln!("WARN: {warning}");
}

// Or: bail immediately on errors
result.bail()?;  // Returns Err(Error::Validation(...)) if errors exist
```

## Types

```rust
/// Result of topology validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    issues: Vec<ValidationIssue>,
}

impl ValidationResult {
    pub fn has_errors(&self) -> bool;
    pub fn has_warnings(&self) -> bool;
    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue>;
    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue>;
    pub fn issues(&self) -> &[ValidationIssue];
    pub fn bail(&self) -> crate::Result<()>;  // Err if has_errors
}

/// A single validation issue.
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub rule: &'static str,       // e.g., "valid-cidr", "dangling-node-ref"
    pub message: String,
    pub location: Option<String>, // e.g., "links[2].endpoints[0]", "nodes.server1.routes.default"
}

impl std::fmt::Display for ValidationIssue {
    // Format: "[ERROR] valid-cidr: invalid CIDR '10.0.0.1' in links[2].endpoints[0]"
}

/// Issue severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}
```

## Validation Rules — Detailed Specification

### Error-Level Rules (block deployment)

#### 1. `valid-cidr` — All addresses must be valid CIDR notation

Parse every address string in the topology using `std::net::IpAddr` + prefix length.

**Locations to check:**
- `links[i].addresses[0]` and `links[i].addresses[1]` (format: `"addr/prefix"`)
- `nodes.<name>.interfaces.<iface>.addresses[i]`
- `nodes.<name>.wireguard.<wg>.addresses[i]`
- `networks.<name>.ports.<port>.addresses[i]`
- `networks.<name>.subnet`

**Implementation:** Split on `'/'`, parse IP with `IpAddr::from_str`, parse prefix as
`u8`, check prefix ≤ 32 (v4) or ≤ 128 (v6). Reject missing prefix, non-numeric prefix,
or prefix out of range.

**Error message:** `"invalid CIDR '{value}' at {location}: {reason}"`

#### 2. `endpoint-format` — All endpoints match `"node:interface"` pattern

Every string that represents an endpoint must parse via `EndpointRef::parse()`.

**Locations to check:**
- `links[i].endpoints[0]` and `links[i].endpoints[1]`
- `impairments` keys (e.g., `"spine1:eth1"`)
- `rate_limits` keys
- `networks.<name>.members[i]`

**Error message:** `"invalid endpoint '{value}' at {location}: expected 'node:interface' format"`

#### 3. `dangling-node-ref` — Link/impairment/rate_limit endpoint nodes must exist in `nodes`

After parsing all endpoints, verify that the node name portion exists in `topology.nodes`.

**Locations to check:**
- `links[i].endpoints[j]` → node part must be in `nodes`
- `impairments` keys → node part must be in `nodes`
- `rate_limits` keys → node part must be in `nodes`
- `networks.<name>.members[i]` → node part must be in `nodes`
- `networks.<name>.ports` keys → must be in `nodes`

**Error message:** `"node '{node}' referenced in {location} does not exist"`

#### 4. `dangling-profile-ref` — Node profile names must exist in `profiles`

If `nodes.<name>.profile = "router"`, then `profiles.router` must exist.

**Error message:** `"profile '{profile}' referenced by node '{node}' does not exist"`

#### 5. `interface-uniqueness` — No duplicate interface names within a node

A node gets interfaces from multiple sources:
1. Explicitly declared in `nodes.<name>.interfaces`
2. Created by links referencing this node (e.g., `"spine1:eth1"` creates `eth1` on `spine1`)
3. Created by network memberships

Collect all interface names per node and check for duplicates.

**Error message:** `"duplicate interface '{iface}' on node '{node}' (from {source1} and {source2})"`

#### 6. `vlan-range` — VLAN IDs must be 1-4094

**Locations to check:**
- `networks.<name>.vlans` keys (the VLAN ID is the key)
- `networks.<name>.ports.<port>.vlans[i]`
- `networks.<name>.ports.<port>.pvid`

**Error message:** `"VLAN ID {id} out of range (1-4094) at {location}"`

#### 7. `impairment-ref-valid` — Impairment keys must reference interfaces that exist

An impairment key like `"spine1:eth1"` is only valid if `spine1` is a node and
`eth1` is an interface that will exist on it (via links or explicit interfaces).

**Error message:** `"impairment references unknown interface '{endpoint}': node '{node}' has no interface '{iface}'"`

#### 8. `rate-limit-ref-valid` — Rate limit keys must reference interfaces that exist

Same logic as impairment ref validation.

**Error message:** `"rate limit references unknown interface '{endpoint}': node '{node}' has no interface '{iface}'"`

#### 9. `route-gateway-type` — Route config must have at least `via` or `dev`

A route with neither `via` nor `dev` is meaningless.

**Error message:** `"route '{dest}' on node '{node}' has neither 'via' nor 'dev'"`

### Warning-Level Rules (non-blocking, printed to stderr)

#### 10. `unique-ips` — No duplicate addresses within same L2/L3 segment

For each link, check that the two endpoint addresses don't collide. Across all links
and interfaces, check for globally duplicate IPs.

**Warning message:** `"duplicate address '{addr}' found on {location1} and {location2}"`

#### 11. `mtu-consistency` — Warn if mismatched MTUs on connected interfaces

If `links[i].mtu` is set and differs from another link on the same node, or if an
explicit interface MTU differs from the link MTU, warn.

**Warning message:** `"MTU mismatch: {endpoint1} has MTU {mtu1} but {endpoint2} has MTU {mtu2}"`

#### 12. `route-reachability` — Warn if gateway not in any connected subnet

For each route with `via`, check whether the gateway IP falls within any subnet
assigned to an interface on that node (via links or explicit addresses).

**Implementation:** Parse all interface addresses on the node into (network, prefix_len).
Check if the gateway IP is within any of those networks.

**Warning message:** `"route '{dest}' on node '{node}': gateway '{gw}' not reachable from any connected subnet"`

#### 13. `unreferenced-node` — Warn if a node has no connections

A node with no links and no network memberships is isolated — likely a mistake.

**Warning message:** `"node '{node}' has no links or network connections"`

#### 14. `empty-exec-cmd` — Warn if exec has empty cmd

**Warning message:** `"node '{node}' exec[{i}] has empty cmd"`

## Implementation Approach

### Helper: Collect All Interfaces Per Node

Many rules need to know what interfaces exist on each node. Build this once:

```rust
fn collect_interfaces(topology: &Topology) -> HashMap<String, HashMap<String, InterfaceSource>> {
    // For each node, map interface_name -> source (explicit, link, network)
}

enum InterfaceSource {
    Explicit,                  // nodes.<name>.interfaces.<iface>
    Link { link_index: usize }, // links[i] created this iface
    Network { network: String }, // networks.<name>.members
    Wireguard,                 // nodes.<name>.wireguard.<wg>
}
```

### Helper: Parse CIDR

```rust
fn parse_cidr(s: &str) -> Result<(IpAddr, u8), String> {
    let (addr_str, prefix_str) = s.rsplit_once('/').ok_or("missing '/' separator")?;
    let addr: IpAddr = addr_str.parse().map_err(|e| format!("invalid IP: {e}"))?;
    let prefix: u8 = prefix_str.parse().map_err(|e| format!("invalid prefix: {e}"))?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(format!("prefix {prefix} exceeds maximum {max}"));
    }
    Ok((addr, prefix))
}
```

### Helper: IP in Subnet Check

```rust
fn ip_in_subnet(ip: IpAddr, network: IpAddr, prefix_len: u8) -> bool {
    // Mask both to prefix_len bits and compare
}
```

### Validation Entry Point

```rust
impl Topology {
    pub fn validate(&self) -> ValidationResult {
        let mut issues = Vec::new();
        let interfaces = collect_interfaces(self);

        // Run all rules
        validate_cidrs(self, &mut issues);
        validate_endpoint_format(self, &mut issues);
        validate_dangling_node_refs(self, &mut issues);
        validate_dangling_profile_refs(self, &mut issues);
        validate_interface_uniqueness(self, &interfaces, &mut issues);
        validate_vlan_range(self, &mut issues);
        validate_impairment_refs(self, &interfaces, &mut issues);
        validate_rate_limit_refs(self, &interfaces, &mut issues);
        validate_route_config(self, &mut issues);
        // Warnings
        validate_unique_ips(self, &mut issues);
        validate_mtu_consistency(self, &mut issues);
        validate_route_reachability(self, &interfaces, &mut issues);
        validate_unreferenced_nodes(self, &interfaces, &mut issues);
        validate_exec_cmds(self, &mut issues);

        ValidationResult { issues }
    }
}
```

## Progress

### Types

- [ ] `ValidationResult` struct with `has_errors()`, `has_warnings()`, `errors()`, `warnings()`, `bail()`
- [ ] `ValidationIssue` struct with `Display` impl
- [ ] `Severity` enum
- [ ] `InterfaceSource` enum (helper)
- [ ] `collect_interfaces()` helper function
- [ ] `parse_cidr()` helper function
- [ ] `ip_in_subnet()` helper function

### Error-Level Rules

- [ ] Rule 1: `valid-cidr` — all address strings are valid CIDR
- [ ] Rule 2: `endpoint-format` — all endpoints parse as `node:interface`
- [ ] Rule 3: `dangling-node-ref` — endpoint node names exist
- [ ] Rule 4: `dangling-profile-ref` — profile names exist
- [ ] Rule 5: `interface-uniqueness` — no duplicate interfaces per node
- [ ] Rule 6: `vlan-range` — VLAN IDs are 1-4094
- [ ] Rule 7: `impairment-ref-valid` — impairment keys reference valid interfaces
- [ ] Rule 8: `rate-limit-ref-valid` — rate limit keys reference valid interfaces
- [ ] Rule 9: `route-gateway-type` — routes have `via` or `dev`

### Warning-Level Rules

- [ ] Rule 10: `unique-ips` — no duplicate addresses per segment
- [ ] Rule 11: `mtu-consistency` — connected interfaces have matching MTUs
- [ ] Rule 12: `route-reachability` — gateways are in connected subnets
- [ ] Rule 13: `unreferenced-node` — nodes have at least one connection
- [ ] Rule 14: `empty-exec-cmd` — exec commands are non-empty

### Integration with `Topology`

- [ ] `impl Topology { pub fn validate(&self) -> ValidationResult }`
- [ ] `impl ValidationResult { pub fn bail(&self) -> crate::Result<()> }`

### Public API

- [ ] Add `pub mod validator;` to `lib.rs`
- [ ] Re-export `ValidationResult`, `ValidationIssue`, `Severity`

### Tests

Each rule should have at least one positive test (valid topology passes) and one
negative test (invalid topology produces the expected issue).

- [ ] Valid topology passes with no issues
- [ ] `valid-cidr`: malformed CIDR detected (missing prefix, bad IP, prefix > 32)
- [ ] `endpoint-format`: bad endpoint format detected
- [ ] `dangling-node-ref`: link references nonexistent node
- [ ] `dangling-profile-ref`: node references nonexistent profile
- [ ] `interface-uniqueness`: duplicate interface on same node detected
- [ ] `vlan-range`: VLAN ID 0, 4095 detected
- [ ] `impairment-ref-valid`: impairment references unknown interface
- [ ] `rate-limit-ref-valid`: rate limit references unknown interface
- [ ] `route-gateway-type`: route with neither `via` nor `dev`
- [ ] `unique-ips`: duplicate IP warning
- [ ] `mtu-consistency`: MTU mismatch warning
- [ ] `route-reachability`: unreachable gateway warning
- [ ] `unreferenced-node`: isolated node warning
- [ ] `empty-exec-cmd`: empty cmd warning
