# Integration Testing with nlink-lab

This guide covers writing integration tests for Rust projects that use nlink-lab
to create isolated network topologies. Tests deploy real Linux network namespaces,
so they require root privileges -- but they automatically skip when run without them.

## The `#[lab_test]` Macro

The `#[lab_test]` attribute macro handles the full lifecycle of a test topology:

1. Checks for root privileges (skips the test if not root).
2. Parses and validates the topology.
3. Assigns a unique lab name (appends test function name + PID) to avoid collisions.
4. Deploys the topology (creates namespaces, veths, addresses, routes, etc.).
5. Passes a `RunningLab` handle to your test body.
6. Destroys the lab after the test, even if the test panics.

### File-based form

Point the macro at an NLL topology file:

```rust
use nlink_lab::lab_test;
use nlink_lab::RunningLab;

#[lab_test("examples/simple.nll")]
async fn test_connectivity(lab: RunningLab) {
    let out = lab.exec("host", "ping", &["-c1", "-W1", "10.0.0.1"]).unwrap();
    assert_eq!(out.exit_code, 0);
}
```

### Builder-based form

Reference a function that returns a `Topology`:

```rust
use nlink_lab::{lab_test, Lab, RunningLab};

fn two_nodes() -> nlink_lab::Topology {
    Lab::new("two-nodes")
        .node("a", |n| n)
        .node("b", |n| n.route("default", |r| r.via("10.0.0.1")))
        .link("a:eth0", "b:eth0", |l| l.addresses("10.0.0.1/24", "10.0.0.2/24"))
        .build()
}

#[lab_test(topology = two_nodes)]
async fn test_route(lab: RunningLab) {
    let out = lab.exec("b", "ip", &["route", "show"]).unwrap();
    assert!(out.stdout.contains("default via 10.0.0.1"));
}
```

## Writing Tests

### Executing commands in nodes

`lab.exec(node, cmd, args)` runs a command inside the node's network namespace
and returns an `ExecOutput` with `stdout`, `stderr`, and `exit_code`:

```rust
#[lab_test("examples/simple.nll")]
async fn verify_address(lab: RunningLab) {
    let out = lab.exec("router", "ip", &["addr", "show", "eth0"]).unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("10.0.0.1/24"));
}
```

### Connectivity checks

```rust
#[lab_test("examples/simple.nll")]
async fn ping_gateway(lab: RunningLab) {
    let out = lab.exec("host", "ping", &["-c1", "-W1", "10.0.0.1"]).unwrap();
    assert_eq!(
        out.exit_code, 0,
        "ping failed: stdout={} stderr={}",
        out.stdout, out.stderr,
    );
}
```

### Verifying sysctls

```rust
#[lab_test("examples/simple.nll")]
async fn ip_forwarding_enabled(lab: RunningLab) {
    let out = lab.exec("router", "cat", &["/proc/sys/net/ipv4/ip_forward"]).unwrap();
    assert_eq!(out.stdout.trim(), "1");
}
```

### Structural assertions

Use `lab.topology()` to inspect the parsed topology without executing anything:

```rust
#[lab_test("examples/simple.nll")]
async fn topology_shape(lab: RunningLab) {
    let topo = lab.topology();
    assert_eq!(topo.nodes.len(), 2);
    assert_eq!(topo.links.len(), 1);
}
```

### Checking stderr and non-zero exit codes

```rust
#[lab_test(topology = two_nodes)]
async fn unreachable_host(lab: RunningLab) {
    let out = lab.exec("a", "ping", &["-c1", "-W1", "192.168.99.1"]).unwrap();
    assert_ne!(out.exit_code, 0, "expected ping to fail");
}
```

## Builder DSL for Tests

The builder DSL lets you construct topologies in pure Rust, which is useful when
test parameters are dynamic or you want the topology definition next to the test.

```rust
use nlink_lab::{Lab, Topology};

fn triangle() -> Topology {
    Lab::new("triangle")
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("r1", |n| n.profile("router"))
        .node("r2", |n| n.profile("router"))
        .node("r3", |n| n.profile("router"))
        .link("r1:eth0", "r2:eth0", |l| {
            l.addresses("10.0.1.1/24", "10.0.1.2/24")
        })
        .link("r2:eth1", "r3:eth0", |l| {
            l.addresses("10.0.2.1/24", "10.0.2.2/24")
        })
        .link("r3:eth1", "r1:eth1", |l| {
            l.addresses("10.0.3.1/24", "10.0.3.2/24")
        })
        .build()
}
```

You can also add impairments to test degraded-network behavior:

```rust
fn lossy_link() -> Topology {
    Lab::new("lossy")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .impair("a:eth0", |i| i.delay("50ms").loss("5%"))
        .build()
}
```

## CI Setup

### GitHub Actions

Tests that require root are skipped automatically when run without privileges,
so `cargo test` in a normal CI job simply skips them. To actually run the
integration tests, use `sudo`:

```yaml
name: Integration Tests
on: [push, pull_request]

jobs:
  integration:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable

      - name: Build
        run: cargo build --release -p my-crate

      - name: Unit tests (no root needed)
        run: cargo test -p my-crate --lib

      - name: Integration tests (root)
        run: sudo cargo test -p my-crate --test integration
```

The key point: unit tests and integration tests live in the same CI pipeline.
Unit tests run without root and always pass. Integration tests run with `sudo`
in a separate step. If you cannot use `sudo` in your CI environment, the
integration tests will skip gracefully rather than fail.

## Best Practices

**Use unique lab names.** The `#[lab_test]` macro automatically appends the
test function name and `std::process::id()` to the lab name, so parallel test
execution is safe by default. If you deploy labs manually (without the macro),
include something unique like the PID in the name.

**Keep topologies minimal.** Each node is a Linux network namespace with its
own veth pairs, addresses, and routes. Two or three nodes per test is usually
enough. Larger topologies slow down deploy/destroy and make failures harder
to diagnose.

**Test one thing per function.** A test that checks addressing, routing, and
firewall rules in one function is hard to debug when it fails. Split them.

**Separate integration tests from unit tests.** Put integration tests in a
dedicated file (e.g., `tests/integration.rs`) so you can run them independently:

```bash
# Just unit tests (fast, no root)
cargo test -p my-crate --lib

# Just integration tests (needs root)
sudo cargo test -p my-crate --test integration
```

**Prefer `exec` assertions over topology introspection.** Checking that
`lab.exec("node", "ip", &["addr"])` returns the expected address is more
meaningful than checking `lab.topology().links[0].left.addr` -- it proves
the kernel state matches the intent, not just that the data structure is
correct.

**Use timeouts in network commands.** Always pass `-W1` (or equivalent) to
`ping`, `curl`, and similar tools. Without a timeout, a failing connectivity
test can hang for 30+ seconds.
