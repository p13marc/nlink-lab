# Network integration tests in `cargo test`

Spin up a real network topology, run assertions, tear it down — all
inside an ordinary `cargo test` run. No Docker daemon, no
`docker-compose.yaml`, no Makefile, no Pytest fixture, no shell
script.

## When to use this

- You're writing a Rust crate that does network IO (a P2P protocol,
  a load balancer, a custom transport) and want integration tests
  that exercise it under realistic packet conditions.
- You want network failure tests — partition, loss, jitter — to be
  part of `cargo test` so they run on every commit, not as
  a separate suite.
- You're tired of "integration test" meaning "shell script
  that calls `iptables` and prays."

## Why nlink-lab

containerlab + Pytest + Docker works, but every step is its own
tool. nlink-lab inverts the model: the test is Rust, the topology
is a value, the lab's lifetime is the test's lifetime. When the
test ends, the lab is gone. When the test panics, the lab is gone
(via Drop). Your CI just runs `cargo test`.

## The macro

```rust
use nlink_lab::lab_test;
use nlink_lab::RunningLab;

#[lab_test("examples/simple.nll")]
async fn ping_works(lab: RunningLab) {
    let out = lab
        .exec("host", "ping", &["-c1", "10.0.0.1"])
        .unwrap();
    assert_eq!(out.exit_code, 0);
}
```

That's the whole test. `cargo test` deploys the topology, calls
your function with a `RunningLab` handle, then tears down.

The macro skips silently if you're not running as root — `cargo
test` reports green even though the test didn't actually run.
That's surprising; it's there because most CI runners aren't
privileged by default. To ensure the test runs, use a privileged
runner or `sudo cargo test`.

## Forms

```rust
// Path to an NLL file
#[lab_test("examples/simple.nll")]
async fn t1(lab: RunningLab) { ... }

// A function that builds the Topology programmatically
#[lab_test(topology = my_topology)]
async fn t2(lab: RunningLab) { ... }

// With NLL `param` overrides (mirrors CLI `--set`)
#[lab_test("wan.nll", set { delay = "20ms", loss = "0.5%" })]
async fn t3(lab: RunningLab) { ... }

// With a per-test timeout (test panics if it exceeds N seconds)
#[lab_test("simple.nll", timeout = 30)]
async fn t4(lab: RunningLab) { ... }

// Combining
#[lab_test("wan.nll", set { delay = "200ms" }, timeout = 60)]
async fn t5(lab: RunningLab) { ... }

fn my_topology() -> nlink_lab::Topology {
    nlink_lab::Lab::new("custom")
        .node("a", |n| n)
        .node("b", |n| n)
        .link("a:eth0", "b:eth0", |l| {
            l.addresses("10.0.0.1/24", "10.0.0.2/24")
        })
        .build()
}
```

The `topology = ...` form is useful when the topology depends on
test parameters — e.g. property-based tests where a `proptest`
strategy generates the topology shape.

The `set { … }` form maps to the same NLL `param` overrides the
CLI `--set k=v` flag uses. Common use case: the same NLL file
serves multiple tests with different parameter values, instead of
maintaining one NLL per scenario.

The `timeout = N` form wraps the test body in `tokio::time::timeout`
and panics with a clear message after N seconds. Default: no
timeout (the test runs as long as `cargo test`'s own timeout
allows).

## What gets cleaned up

The macro wraps the test body in a guard so a panic doesn't leak
the lab:

```text
deploy → run test body → destroy
              │
              └─ on panic: Drop runs, deletes namespaces, removes state
```

The lab name is suffixed with the test function name and the
process PID — multiple test processes don't collide.

## A real example: P2P partition recovery

A test that verifies a consensus protocol handles a 5-second link
partition mid-run. The topology defines the impair; the test does
the partition.

```rust
use nlink_lab::lab_test;
use nlink_lab::RunningLab;
use std::time::Duration;
use tokio::time::sleep;

#[lab_test("tests/topologies/raft-3node.nll")]
async fn leader_recovers_after_partition(lab: RunningLab) {
    // Start the protocol on each node
    for node in ["n1", "n2", "n3"] {
        lab.exec(node, "/usr/local/bin/my-raft-node",
                 &["--config", "/etc/raft.toml"])
           .unwrap();
    }

    // Let the cluster elect a leader.
    sleep(Duration::from_secs(2)).await;
    let leader = find_leader(&lab).await;

    // Partition the leader from the rest.
    lab.exec("n2", "ip", &["link", "set", "eth0", "down"]).unwrap();
    sleep(Duration::from_secs(5)).await;

    // Heal the partition.
    lab.exec("n2", "ip", &["link", "set", "eth0", "up"]).unwrap();
    sleep(Duration::from_secs(3)).await;

    // Assert: a new leader was elected during the partition.
    let new_leader = find_leader(&lab).await;
    assert_ne!(leader, new_leader, "expected re-election after partition");
}
```

Run:

```bash
sudo cargo test --test integration leader_recovers_after_partition
```

Or in CI, on a privileged runner:

```yaml
# .github/workflows/integration.yml
- run: sudo cargo test --test integration -- --include-ignored
```

## Combining with proptest

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn handles_any_realistic_latency(latency_ms in 1u32..2000) {
        let topo = make_topology(latency_ms);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let lab = topo.deploy().await.unwrap();
            run_my_protocol(&lab).await;
            assert!(things_work(&lab).await);
            lab.destroy().await.unwrap();
        });
    }
}
```

(Property-based testing here is best for parameter sweeps — it
shrinks failures to minimal repros automatically. The
`#[lab_test]` macro doesn't compose with proptest; use the
manual API as shown.)

## Performance

Each test is one deploy + one destroy. A 3-node lab boots in well
under a second; a 12-node ring takes 1–2s. CI runners with the
right caps can run hundreds of these per minute. The Docker
alternative (containerlab + clab destroy + clab deploy per test)
is multi-second per cycle.

## Troubleshooting

- **The test silently passed without running.** You're not root.
  Run with `sudo` or add capabilities.
- **"address already in use" on second run.** A previous test left
  state behind. Run `sudo nlink-lab destroy --orphans` once.
- **Tests interfere with each other.** Each test gets a unique lab
  name suffix, but namespaces share the host kernel. If you're
  running tests in parallel (the default), make sure topologies
  use distinct address ranges. Or use `cargo test --
  --test-threads=1` for serial execution.
- **CI's lab is slower than local.** GitHub Actions runners are
  often single-core and shared. Don't be surprised if a test that
  takes 200ms locally takes 800ms on a hosted runner.

## When this is the wrong tool

- For unit tests that don't actually need a network, `#[tokio::test]`
  + a mock socket is faster.
- For multi-host distributed-system tests, you need real machines.
  nlink-lab is single-host.
- For tests that need vendor-NOS behavior (Cisco IOS, Juniper Junos,
  Nokia SR Linux), use containerlab; nlink-lab can't run those
  images.

## See also

- [`#[lab_test]` macro source](../../crates/nlink-lab-macros/src/lib.rs)
- [`RunningLab` API](https://docs.rs/nlink-lab/latest/nlink_lab/struct.RunningLab.html)
- [scenario block](../NLL_DSL_DESIGN.md) — for time-driven faults
  declared in NLL instead of test code
- [TESTING_GUIDE.md](../TESTING_GUIDE.md) — broader testing patterns
