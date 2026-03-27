# Plan 072: Lab Templates

**Priority:** Medium
**Effort:** 2-3 days
**Target:** `crates/nlink-lab/src/templates.rs` (new), `bins/lab/src/main.rs`

## Summary

Add `nlink-lab init <template>` to scaffold a topology file from a built-in
template. Templates cover common network patterns and serve as starting points
for custom topologies. Each template generates both `.toml` and `.nll` files
with inline comments explaining the topology.

## Motivation

New users need a fast way to get started. Instead of reading docs and writing
topology from scratch, `nlink-lab init spine-leaf` creates a ready-to-deploy
topology in the current directory. Templates also serve as living documentation
of supported features.

## Design

### CLI

```
nlink-lab init <template> [OPTIONS]

Arguments:
  <template>     Template name (see list below)

Options:
  -o, --output <DIR>    Output directory (default: current directory)
  -f, --format <FMT>    Output format: toml (default), nll, both
  -n, --name <NAME>     Override the lab name
  --list                List available templates
  --force               Overwrite existing files
```

**Example:**

```bash
$ nlink-lab init spine-leaf --name my-dc
Created my-dc.toml (6 nodes, 8 links)

$ nlink-lab init spine-leaf --format both
Created spine-leaf.toml (6 nodes, 8 links)
Created spine-leaf.nll (6 nodes, 8 links)
```

### Built-in Templates

| Template | Nodes | Description | Key Features |
|----------|-------|-------------|--------------|
| `simple` | 2 | Two nodes, one link | Minimal starting point |
| `router` | 3 | Router between two hosts | IP forwarding, default routes |
| `spine-leaf` | 6 | 2-spine 2-leaf + 2 servers | Profiles, loopback, multi-hop |
| `wan` | 4 | Two sites over impaired WAN link | Delay, loss, rate limiting |
| `firewall` | 3 | Server behind stateful firewall | nftables rules, conntrack |
| `vlan-trunk` | 4 | Switch with VLANs | Bridge, VLAN filtering, pvid |
| `vrf` | 3 | PE router with VRF isolation | VRF, per-tenant routing |
| `wireguard` | 2 | Site-to-site WireGuard VPN | WG interfaces, encrypted tunnel |
| `vxlan` | 2 | VXLAN overlay between VTEPs | VXLAN, underlay + overlay |
| `container` | 2 | FRR router + bare host | Container image, mixed topology |
| `mesh` | 5 | Full mesh of 5 nodes | For-loop in NLL, N*(N-1)/2 links |
| `iperf` | 2 | Throughput test topology | iperf3 exec, rate limiting |

### Template Registry

```rust
/// A built-in topology template.
pub struct Template {
    /// Short name used on CLI.
    pub name: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of links.
    pub link_count: usize,
    /// Key features demonstrated.
    pub features: &'static [&'static str],
    /// TOML content (with comments).
    pub toml: &'static str,
    /// NLL content (with comments).
    pub nll: &'static str,
}

/// Get all available templates.
pub fn list() -> &'static [Template] { ... }

/// Find a template by name.
pub fn get(name: &str) -> Option<&'static Template> { ... }

/// Generate a topology file from a template, with optional name override.
pub fn render(template: &Template, name: Option<&str>) -> (String, String) {
    // Replace lab name in both TOML and NLL content
    // Returns (toml_content, nll_content)
}
```

### Template Content Storage

Templates are embedded in the binary via `include_str!()`:

```
crates/nlink-lab/src/templates/
  mod.rs                # Template struct, list(), get(), render()
  simple.toml           # Template files
  simple.nll
  router.toml
  router.nll
  spine-leaf.toml
  spine-leaf.nll
  ...
```

Each template file includes comments explaining the topology:

```toml
# Router topology: one router connecting two hosts
#
# Topology:
#   host1 (10.0.1.2) ── router ── host2 (10.0.2.2)
#                     eth0  eth1
#
# The router has IP forwarding enabled. Each host has a
# default route pointing to its gateway on the router.

[lab]
name = "router"

[profiles.router]
sysctls = { "net.ipv4.ip_forward" = "1" }

[nodes.router]
profile = "router"

[nodes.host1]
[nodes.host1.routes]
default = { via = "10.0.1.1" }

[nodes.host2]
[nodes.host2.routes]
default = { via = "10.0.2.1" }

[[links]]
endpoints = ["router:eth0", "host1:eth0"]
addresses = ["10.0.1.1/24", "10.0.1.2/24"]

[[links]]
endpoints = ["router:eth1", "host2:eth0"]
addresses = ["10.0.2.1/24", "10.0.2.2/24"]
```

### Reusing Existing Examples

The `examples/` directory already has 9 TOML + 9 NLL files covering most
templates. We can reuse these directly:

| Template | Source |
|----------|--------|
| `spine-leaf` | `examples/spine-leaf.toml` + `.nll` |
| `wan` | `examples/wan-impairment.toml` + `.nll` |
| `firewall` | `examples/firewall.toml` + `.nll` |
| `vlan-trunk` | `examples/vlan-trunk.toml` + `.nll` |
| `vrf` | `examples/vrf-multitenant.toml` + `.nll` |
| `wireguard` | `examples/wireguard-vpn.toml` + `.nll` |
| `vxlan` | `examples/vxlan-overlay.toml` + `.nll` |
| `iperf` | `examples/iperf-benchmark.toml` + `.nll` |

New templates to create: `simple`, `router`, `container`, `mesh`.

### Validation

Generated templates are validated at test time to ensure they always produce
valid topologies:

```rust
#[test]
fn all_templates_valid() {
    for template in templates::list() {
        let topo = parser::parse(template.toml).unwrap();
        let result = topo.validate();
        assert!(!result.has_errors(), "template '{}' has errors", template.name);
    }
}
```

## Implementation Order

### Phase 1: Core (day 1)

1. Create `crates/nlink-lab/src/templates/mod.rs` with `Template` struct
2. Embed existing example files as templates via `include_str!()`
3. Create new template files: `simple`, `router`, `container`, `mesh`
4. Implement `list()`, `get()`, `render()` with name substitution

### Phase 2: CLI (day 2)

5. Add `Init` command to `bins/lab/src/main.rs`
6. Add `--list` flag for listing templates
7. Handle `--format`, `--name`, `--output`, `--force` options
8. Write generated files to disk

### Phase 3: Polish (day 2-3)

9. Add inline comments to all template files explaining the topology
10. Add ASCII topology diagram in template comments
11. Register `templates` module in `lib.rs`
12. Test: all templates parse and validate
13. Test: name substitution works correctly

## Progress

### Phase 1: Core

- [x] Create `templates/mod.rs` with `Template` struct
- [x] Embed existing examples as templates
- [x] Create `simple`, `router`, `container`, `mesh` templates
- [x] Implement `list()`, `get()`, `render()`

### Phase 2: CLI

- [x] Add `Init` command to CLI
- [x] `--list` flag
- [x] `--format`, `--name`, `--output`, `--force` options
- [x] File writing with overwrite protection

### Phase 3: Polish

- [x] Inline comments with topology diagrams in all templates
- [x] Register module and re-export
- [x] Test: all templates parse and validate
- [x] Test: name substitution
