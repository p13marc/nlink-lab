# Plan 085: Test Coverage Expansion

**Priority:** Medium
**Effort:** 2-3 days
**Target:** `crates/nlink-lab/tests/integration.rs`, `crates/nlink-lab/src/`

## Summary

Close the test coverage gaps identified in the deep analysis. Focus on verifying
that advanced features actually work (not just that they deploy), and add stress
tests and negative tests.

## Phase 1: Advanced Feature Verification (1-2 days)

### VRF Isolation Test

The existing `deploy_vrf` test checks that VRF interfaces exist and tenants can
reach the PE. It does NOT verify cross-VRF isolation.

```rust
#[lab_test("examples/vrf-multitenant.toml")]
async fn vrf_isolation(lab: RunningLab) {
    // Tenant A should NOT be able to reach Tenant B's subnet
    let output = lab
        .exec("tenant-a", "ping", &["-c1", "-W1", "10.20.0.10"])
        .unwrap();
    assert_ne!(
        output.exit_code, 0,
        "tenant-a should NOT reach tenant-b (VRF isolation broken): {}",
        output.stdout
    );
}
```

### Firewall Enforcement Test

The existing `deploy_firewall` test checks rules exist. Add a test that verifies
actual packet filtering:

```rust
#[lab_test("examples/firewall.toml")]
async fn firewall_enforcement(lab: RunningLab) {
    // Server has nftables rules. Verify allowed traffic works:
    let output = lab
        .exec("client", "ping", &["-c1", "-W1", "<server-ip>"])
        .unwrap();
    assert_eq!(output.exit_code, 0, "ICMP should be allowed");

    // If the firewall drops something specific, test that too.
    // The exact test depends on what the firewall.toml rules allow/deny.
}
```

### Rate Limiting Test

Verify that rate limiting actually constrains throughput:

```rust
#[lab_test("examples/iperf-benchmark.toml")]
async fn rate_limit_applied(lab: RunningLab) {
    // Check HTB qdisc exists
    let output = lab
        .exec("server", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("htb") || output.stdout.contains("tbf"),
        "expected rate limiting qdisc: {}",
        output.stdout
    );
}
```

### VXLAN Tunnel Test

```rust
#[lab_test("examples/vxlan-overlay.toml")]
async fn vxlan_tunnel(lab: RunningLab) {
    // Verify VXLAN interface exists
    let output = lab
        .exec("vtep1", "ip", &["link", "show", "vxlan100"])
        .unwrap();
    assert_eq!(output.exit_code, 0, "vxlan100 not found: {}", output.stderr);

    // Verify overlay connectivity
    let output = lab
        .exec("vtep1", "ping", &["-c1", "-W1", "192.168.100.2"])
        .unwrap();
    assert_eq!(
        output.exit_code, 0,
        "VXLAN overlay connectivity failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
}
```

### Runtime Impairment Modification

```rust
#[lab_test("examples/simple.toml")]
async fn impairment_modification(lab: RunningLab) {
    // Modify impairment at runtime
    lab.set_impairment("router", "eth0", nlink_lab::Impairment {
        delay: Some("50ms".into()),
        ..Default::default()
    }).await.unwrap();

    // Verify the new impairment is applied
    let output = lab
        .exec("router", "tc", &["qdisc", "show", "dev", "eth0"])
        .unwrap();
    assert!(
        output.stdout.contains("50ms") || output.stdout.contains("50000us"),
        "expected 50ms delay in tc output: {}",
        output.stdout
    );
}
```

## Phase 2: Lifecycle Tests (1 day)

### Destroy Cleanup Verification

```rust
#[lab_test("examples/simple.toml")]
async fn destroy_cleanup(lab: RunningLab) {
    let name = lab.name().to_string();
    let ns_count = lab.namespace_count();
    assert!(ns_count > 0);

    // Destroy the lab
    lab.destroy().await.unwrap();

    // Verify: no state file
    assert!(!nlink_lab::state::exists(&name));

    // Verify: namespaces are gone
    let ns_list = nlink::namespace::list().unwrap_or_default();
    for ns in &ns_list {
        assert!(
            !ns.contains(&name),
            "namespace '{ns}' still exists after destroy"
        );
    }
}
```

Note: This test calls `destroy()` manually, so the `#[lab_test]` cleanup guard
needs to handle the case where the lab is already destroyed.

### Concurrent Lab Coexistence

```rust
#[lab_test(topology = lab_a_topology)]
async fn concurrent_labs_a(lab: RunningLab) {
    // Deploy a second lab while this one is running
    let topo_b = nlink_lab::Lab::new("concurrent-b")
        .node("x", |n| n)
        .node("y", |n| n)
        .link("x:eth0", "y:eth0", |l| l.addresses("10.1.0.1/24", "10.1.0.2/24"))
        .build();

    let lab_b = topo_b.deploy().await.unwrap();

    // Both labs should be functional
    let output_a = lab.exec("a", "ping", &["-c1", "-W1", "10.0.0.2"]).unwrap();
    assert_eq!(output_a.exit_code, 0, "lab A broken");

    let output_b = lab_b.exec("y", "ping", &["-c1", "-W1", "10.1.0.1"]).unwrap();
    assert_eq!(output_b.exit_code, 0, "lab B broken");

    lab_b.destroy().await.unwrap();
}

fn lab_a_topology() -> nlink_lab::Topology {
    nlink_lab::Lab::new("concurrent-a")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| l.addresses("10.0.0.1/24", "10.0.0.2/24"))
        .build()
}
```

## Phase 3: Unit Test Gaps (0.5 day)

### Firewall Match Expression Parsing

**Where:** `deploy.rs` — add unit tests for `apply_match_expr()`.

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn parse_ct_state() {
        // "ct state established,related" → CtState match
    }

    #[test]
    fn parse_tcp_dport() {
        // "tcp dport 80" → TcpDport(80)
    }

    #[test]
    fn reject_unknown_match() {
        // "foo bar baz" → error
    }
}
```

### Builder Invalid Input Tests

**Where:** `builder.rs` — test that invalid inputs produce errors.

```rust
#[test]
fn builder_empty_lab_name() {
    let topo = Lab::new("").node("a", |n| n).build();
    let diags = topo.validate();
    assert!(diags.has_errors());
}

#[test]
fn builder_duplicate_link_endpoints() {
    let topo = Lab::new("t")
        .node("a", |n| n).node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| l)
        .link("a:eth0", "b:eth1", |l| l)  // a:eth0 used twice
        .build();
    let diags = topo.validate();
    assert!(diags.has_errors());
}
```

### State File Corruption Recovery

**Where:** `state.rs` — test loading from a corrupted state file.

```rust
#[test]
fn load_corrupted_state() {
    // Write invalid TOML to state file
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("state.toml"), "not valid toml {{{{").unwrap();
    let result = state::load_from(dir.path());
    assert!(result.is_err());
    // Should return a clear error, not panic
}
```

## Phase 4: New Example Topologies

Add example files that demonstrate untested patterns:

### Bond Interface Example

```toml
# examples/bond-failover.toml
[lab]
name = "bond-failover"

[nodes.router]
[nodes.router.interfaces.bond0]
kind = "bond"
members = ["eth1", "eth2"]

[nodes.host]

[[links]]
endpoints = ["router:eth1", "host:eth0"]
addresses = ["10.0.0.1/24", "10.0.0.10/24"]

[[links]]
endpoints = ["router:eth2", "host:eth1"]
addresses = ["10.0.1.1/24", "10.0.1.10/24"]
```

### IPv6 Example

```toml
# examples/ipv6-simple.toml
[lab]
name = "ipv6-simple"

[nodes.router]
[profiles.router]
sysctls = { "net.ipv6.conf.all.forwarding" = "1" }

[nodes.host]
[nodes.host.routes]
"default" = { via = "fd00::1" }

[[links]]
endpoints = ["router:eth0", "host:eth0"]
addresses = ["fd00::1/64", "fd00::2/64"]
```

### Asymmetric Impairment Example (NLL)

```nll
# examples/asymmetric.nll
lab "asymmetric"

node server
node client { route default via 10.0.0.1 }

link server:eth0 -- client:eth0 {
    10.0.0.1/24 -- 10.0.0.2/24
    -> delay 5ms                  # server→client: low latency
    <- delay 100ms jitter 20ms    # client→server: high latency
}
```

## Progress

### Phase 1: Feature Verification
- [ ] VRF isolation test (cross-VRF traffic should fail)
- [ ] Firewall enforcement test (blocked traffic drops)
- [ ] Rate limiting verification (qdisc exists)
- [ ] VXLAN tunnel connectivity test
- [ ] Runtime impairment modification test

### Phase 2: Lifecycle Tests
- [ ] Destroy cleanup verification (no leftover namespaces)
- [ ] Concurrent lab coexistence

### Phase 3: Unit Tests
- [ ] Firewall match expression parsing tests
- [ ] Builder invalid input tests
- [ ] State file corruption recovery test

### Phase 4: New Examples
- [ ] Bond failover example (TOML + NLL)
- [ ] IPv6 simple example (TOML + NLL)
- [ ] Asymmetric impairment example (NLL)
