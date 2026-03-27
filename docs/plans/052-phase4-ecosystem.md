# Plan 052: Phase 4 — Ecosystem

**Priority:** High
**Effort:** 5-7 days
**Target:** `examples/`, `crates/nlink-lab-macros/` (new), `tests/`

## Summary

Build the ecosystem around nlink-lab: example topologies, a `#[nlink_lab::test]`
proc macro for integration testing, real integration tests, and documentation.

## 1. Example Topologies

**Effort:** 1 day

Create TOML topology files for common network patterns. Each example should be
self-contained, validated by CI, and documented with comments.

**Target directory:** `examples/`

### Examples to Create

| File | Description | Nodes | Key Features |
|------|-------------|-------|--------------|
| `simple.toml` | Two nodes, one link (exists) | 2 | Basic veth |
| `spine-leaf.toml` | Datacenter spine-leaf fabric | 6 | Profiles, loopback, multi-hop routes, netem |
| `wan-impairment.toml` | Two sites connected by WAN link | 4 | High delay, loss, rate limiting |
| `vlan-trunk.toml` | Switch with VLAN trunk + access ports | 4 | Bridge, VLAN filtering, pvid, tagged/untagged |
| `vrf-multitenant.toml` | PE router with VRF isolation | 3 | VRF, per-tenant routing tables |
| `wireguard-vpn.toml` | Two sites connected by WireGuard | 2 | WG interfaces, encrypted tunnel over WAN |
| `vxlan-overlay.toml` | VXLAN overlay between VTEPs | 2 | VXLAN, underlay + overlay addresses |
| `firewall.toml` | Server with nftables firewall | 3 | Firewall policy + rules, conntrack |
| `iperf-benchmark.toml` | Performance test topology | 2 | iperf3 server, rate limiting, netem |

### Progress

- [x] `spine-leaf.toml` + `.nll`
- [x] `wan-impairment.toml` + `.nll`
- [x] `vlan-trunk.toml` + `.nll`
- [x] `vrf-multitenant.toml` + `.nll`
- [x] `wireguard-vpn.toml` + `.nll`
- [x] `vxlan-overlay.toml` + `.nll`
- [x] `firewall.toml` + `.nll`
- [x] `iperf-benchmark.toml` + `.nll`
- [x] CI: validate all examples parse (test in `parser/toml.rs` + `parser/nll/lower.rs`)

## 2. Test Harness Proc Macro

**Effort:** 2-3 days

A `#[nlink_lab::test]` proc macro that auto-deploys a topology before the test and
destroys it after. This is the key differentiator for library-first usage.

**Target:** New crate `crates/nlink-lab-macros/`

### API Design

```rust
use nlink_lab_macros::lab_test;

// From TOML file
#[lab_test("examples/simple.toml")]
async fn test_ping(lab: RunningLab) {
    let output = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
    assert_eq!(output.exit_code, 0);
}

// From builder DSL (inline)
#[lab_test]
async fn test_loss(lab: RunningLab) {
    // lab is auto-deployed from the topology returned by the setup function
}

// With builder topology
#[lab_test(topology = "my_topology")]
async fn test_custom(lab: RunningLab) {
    let output = lab.exec("server", "curl", &["-s", "http://10.0.0.1"]).unwrap();
    assert!(output.stdout.contains("ok"));
}

fn my_topology() -> Topology {
    Lab::new("custom")
        .node("server", |n| n)
        .node("client", |n| n)
        .link("server:eth0", "client:eth0", |l| l
            .addresses("10.0.0.1/24", "10.0.0.2/24"))
        .build()
}
```

### Macro Expansion

`#[lab_test("examples/simple.toml")]` expands to:

```rust
#[tokio::test]
async fn test_ping() {
    let topology = nlink_lab::parser::parse_file("examples/simple.toml").unwrap();
    topology.validate().bail().unwrap();
    let lab = topology.deploy().await.unwrap();
    let _guard = LabGuard::new(&lab);  // Destroys on drop (even on panic)

    // Original test body with `lab` in scope
    let output = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
    assert_eq!(output.exit_code, 0);

    lab.destroy().await.unwrap();
}
```

### Crate Setup

```
crates/nlink-lab-macros/
  Cargo.toml       # proc-macro crate, depends on syn + quote
  src/lib.rs       # #[lab_test] implementation
```

```toml
[package]
name = "nlink-lab-macros"
edition = "2024"

[lib]
proc-macro = true

[dependencies]
syn = { version = "2", features = ["full"] }
quote = "1"
proc-macro2 = "1"
```

### Implementation Details

The macro:
1. Parses the attribute argument (file path or `topology = "fn_name"`)
2. Wraps the function body with deploy/destroy logic
3. Adds `#[tokio::test]` attribute
4. Generates a `LabGuard` struct that calls `destroy()` on drop for panic safety

**Root requirement:** Tests using this macro need `CAP_NET_ADMIN`. The macro should
check and skip with a clear message if not available:

```rust
if unsafe { libc::geteuid() } != 0 {
    eprintln!("skipping {}: requires root or CAP_NET_ADMIN", stringify!(test_name));
    return;
}
```

### Progress

- [x] Create `crates/nlink-lab-macros/` crate
- [x] Add to workspace `Cargo.toml`
- [x] Implement `#[lab_test("file.toml")]` — file-based topology
- [x] Implement `#[lab_test(topology = fn)]` — function-based topology
- [x] `LabGuard` for panic-safe cleanup
- [x] Root/capability check with skip
- [x] Re-export from `nlink-lab` crate: `pub use nlink_lab_macros::lab_test;`
- [x] Test: basic macro expansion works (12 integration tests compile and run)
- [ ] Test: lab deploys and destroys around test body (requires root CI)

## 3. Integration Tests

**Effort:** 1-2 days

Real end-to-end tests that deploy topologies, verify connectivity, and destroy.
These require root and run in CI with `CAP_NET_ADMIN`.

**Target:** `tests/integration/` in the `nlink-lab` crate, or a separate `tests/` directory.

### Test Cases

| Test | Description | Verifies |
|------|-------------|----------|
| `deploy_minimal` | Deploy 2-node topology, check namespaces exist | Namespace creation, veth pairs |
| `deploy_with_addresses` | Deploy, run `ip addr` in node | Address assignment |
| `deploy_with_routes` | Deploy, run `ip route` in node | Route configuration |
| `deploy_with_sysctls` | Deploy, check sysctl values | Sysctl application |
| `deploy_with_netem` | Deploy, check `tc qdisc show` | Netem impairment |
| `exec_ping` | Deploy, ping between nodes | End-to-end connectivity |
| `exec_exit_code` | Run failing command, check exit code | Exit code forwarding |
| `spawn_background` | Deploy with exec, check PID exists | Background process spawning |
| `destroy_cleanup` | Deploy then destroy, check no namespaces left | Clean teardown |
| `deploy_rollback` | Force a failure mid-deploy, check cleanup | Rollback on error |
| `state_persistence` | Deploy, load from state, verify | State save/load |
| `validate_cli` | Run CLI validate on examples | CLI integration |
| `deploy_bridge` | Deploy bridge topology, verify L2 connectivity | Bridge networks |
| `deploy_firewall` | Deploy with nftables, verify rules | Firewall deployment |

### Test Infrastructure

```rust
/// Skip test if not running as root.
fn require_root() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: requires root");
        return;
    }
}

/// Generate a unique lab name to avoid conflicts in parallel test runs.
fn unique_lab_name(base: &str) -> String {
    format!("{}-{}", base, std::process::id())
}
```

### Progress

- [x] Test infrastructure (root skip + unique lab names via `#[lab_test]` macro)
- [x] `deploy_simple_toml` / `deploy_simple_nll` — deploy from both formats
- [x] `exec_ping` — ping between two nodes
- [x] `exec_ip_addr` — ip addr shows correct addresses
- [x] `exec_ip_route` — ip route shows correct routes
- [x] `sysctl_forwarding` — sysctl values correct
- [x] `netem_applied` — tc qdisc shows netem
- [x] `exit_code_forwarded` — failing command returns non-zero
- [x] `state_persistence` — load from state matches deploy
- [x] `deploy_from_builder` — builder DSL topology
- [x] `deploy_firewall` — nftables rules exist in namespace
- [x] `deploy_spine_leaf` — 6-node datacenter fabric
- [ ] `deploy_bridge` — L2 connectivity through bridge (needs plan 050)

## 4. Documentation

**Effort:** 1 day

### Rust API Docs

The crate already has doc comments on all public types and functions. Ensure
`cargo doc` builds cleanly and the module-level examples compile.

- [x] `cargo doc --no-deps -p nlink-lab` builds without warnings
- [x] Module-level examples in `lib.rs` are up to date with current API
- [x] Builder DSL examples in `builder.rs` are complete

### User Guide (README.md at repo root)

- [x] What nlink-lab is (one paragraph)
- [x] Quick start: install, deploy simple lab, exec, destroy
- [x] Topology file format overview (TOML + NLL with examples)
- [x] Builder DSL example
- [x] Testing with `#[lab_test]`
- [x] Comparison with containerlab (brief table)
- [x] Requirements (Linux, root/CAP_NET_ADMIN, kernel version)
