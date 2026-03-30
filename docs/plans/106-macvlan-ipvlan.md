# Plan 106: macvlan and ipvlan Interface Support

**Date:** 2026-03-30
**Status:** Ready
**Effort:** Medium (1-2 days)
**Depends on:** nlink 0.12.0 (`MacvlanLink`, `IpvlanLink` already implemented)

---

## Problem Statement

nlink-lab cannot attach lab nodes to physical host interfaces. All topologies are
fully isolated virtual networks. Users who need to:

- Test against real external services from inside a lab
- Give lab nodes real LAN IPs for management access
- Simulate a device joining an existing physical network
- Bridge lab traffic to hardware NICs for capture/analysis

...must currently do this manually after deployment.

nlink already has `MacvlanLink` (all modes) and `IpvlanLink` (L2/L3/L3S), but
nlink-lab doesn't expose them in the NLL DSL.

## NLL Syntax

### macvlan

```nll
node gateway {
  macvlan eth0 parent "enp3s0" mode bridge
  route default via 192.168.1.1
}

# Shorthand (mode defaults to bridge)
node monitor {
  macvlan mon0 parent "enp3s0"
}
```

Modes: `bridge` (default), `private`, `vepa`, `passthru`, `source`.

### ipvlan

```nll
node router {
  ipvlan eth0 parent "enp3s0" mode l3
}

# With flags
node router {
  ipvlan eth0 parent "enp3s0" mode l2 flags private
}
```

Modes: `l2`, `l3` (default), `l3s`.
Flags: `bridge` (default), `private`, `vepa`.

## Implementation

### 1. Types (`types.rs`)

```rust
pub struct MacvlanConfig {
    pub name: String,
    pub parent: String,
    pub mode: MacvlanMode,
    pub mtu: Option<u32>,
}

pub enum MacvlanMode {
    Bridge,
    Private,
    Vepa,
    Passthru,
    Source,
}

pub struct IpvlanConfig {
    pub name: String,
    pub parent: String,
    pub mode: IpvlanMode,
    pub flags: Option<IpvlanFlags>,
    pub mtu: Option<u32>,
}

pub enum IpvlanMode {
    L2,
    L3,
    L3S,
}

pub enum IpvlanFlags {
    Bridge,
    Private,
    Vepa,
}
```

Add `macvlans: Vec<MacvlanConfig>` and `ipvlans: Vec<IpvlanConfig>` to `Node`.

### 2. Lexer (`lexer.rs`)

Add tokens: `Macvlan`, `Ipvlan`, `Parent`, `Mode`, `Flags`.

Note: `Mode` may conflict if used elsewhere. Use `Parent` as a new keyword.

### 3. AST (`ast.rs`)

Add `MacvlanDef` and `IpvlanDef` structs, add variants to `NodeProp`.

### 4. Parser (`parser.rs`)

Parse macvlan/ipvlan as node properties:
```
"macvlan" IDENT "parent" STRING ("mode" IDENT)? ("mtu" INT)?
"ipvlan" IDENT "parent" STRING ("mode" IDENT)? ("flags" IDENT)? ("mtu" INT)?
```

### 5. Lower (`lower.rs`)

Convert AST macvlan/ipvlan defs to typed configs on `Node`.

### 6. Deploy (`deploy.rs`)

Add to Step 6 (create additional interfaces):

```rust
// macvlan interfaces
for mv in &node.macvlans {
    let macvlan = nlink::netlink::link::MacvlanLink::new(&mv.name, &mv.parent)
        .mode(convert_macvlan_mode(mv.mode));
    conn.add_link(macvlan).await?;
}

// ipvlan interfaces
for iv in &node.ipvlans {
    let ipvlan = nlink::netlink::link::IpvlanLink::new(&iv.name, &iv.parent)
        .mode(convert_ipvlan_mode(iv.mode));
    conn.add_link(ipvlan).await?;
}
```

**Key consideration:** The parent interface (e.g., `enp3s0`) is on the **host**,
not inside the namespace. We need to:
1. Get the parent's ifindex from the host namespace
2. Create the macvlan/ipvlan in the host namespace
3. Move it into the target namespace via `set_link_netns()`

Or use `MacvlanLink::with_parent_index()` which nlink provides for this purpose.

### 7. Render (`render.rs`)

Render macvlan/ipvlan blocks inside node definitions.

### 8. Validator (`validator.rs`)

- Warn if parent interface doesn't exist on the host
- Error if macvlan mode is invalid
- Error if ipvlan mode/flags combination is invalid
- Warn that parent interfaces make the topology non-portable

### 9. Builder (`builder.rs`)

Add `macvlan()` and `ipvlan()` methods to `NodeBuilder`.

### 10. Tests

| Test | Description |
|------|-------------|
| `test_parse_macvlan` | Parser: macvlan with parent and mode |
| `test_parse_ipvlan` | Parser: ipvlan with parent, mode, flags |
| `test_lower_macvlan` | Lower: AST to typed MacvlanConfig |
| `test_lower_ipvlan` | Lower: AST to typed IpvlanConfig |
| `test_render_macvlan` | Render roundtrip |
| `test_validate_macvlan_mode` | Validator: invalid mode rejected |
| Integration: `deploy_macvlan` | Deploy with macvlan, verify interface exists |

### 11. Example

`examples/macvlan.nll`:
```nll
# macvlan: attach lab nodes to physical network.
#
# Requires: a physical interface on the host (edit "enp3s0" to match yours).
# The gateway node will appear as a separate device on your LAN.

lab "macvlan-demo"

node gateway {
  macvlan eth0 parent "enp3s0" mode bridge
  route default via 192.168.1.1
}

node internal {
  route default via 10.0.0.1
}

link gateway:veth0 -- internal:eth0 { subnet 10.0.0.0/24 }
```

### File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `MacvlanConfig`, `IpvlanConfig`, modes/flags enums, fields on `Node` |
| `lexer.rs` | Add `Macvlan`, `Ipvlan`, `Parent` tokens |
| `ast.rs` | Add `MacvlanDef`, `IpvlanDef`, `NodeProp` variants |
| `parser.rs` | Parse macvlan/ipvlan in node blocks |
| `lower.rs` | Lower to typed configs |
| `deploy.rs` | Create macvlan/ipvlan interfaces in Step 6 |
| `render.rs` | Render macvlan/ipvlan blocks |
| `validator.rs` | Validate modes, warn about non-portable parent refs |
| `builder.rs` | Add `macvlan()`/`ipvlan()` to `NodeBuilder` |
| `examples/macvlan.nll` | New example |
