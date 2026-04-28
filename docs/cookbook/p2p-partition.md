# Mid-test partition with the scenario engine

Most network-failure tests are a Bash script that calls `iptables -A
INPUT -j DROP` and `sleep`s. That isn't a test; it's a fragile
script with no validation, no timing guarantees, and no record of
what actually happened.

nlink-lab's `scenario` block puts the timeline in the topology:
declare what should happen at `at 3s`, `at 11s`, `at 16s`, and the
engine fires each step within ±100ms of its scheduled time, with
validation gates that fail the test if state isn't right.

This recipe partitions one node mid-test, validates the rest of
the fabric stays connected, heals the partition, and finally
clears the WAN impair to show how runtime impair lifecycle
composes.

## When to use this

- Testing how a distributed system responds to partition + heal
  cycles.
- Verifying a routing daemon converges after a link flap.
- CI gates that fail when "partition recovery within 5s" stops
  being true.
- Reproducible chaos: the same NLL fires the same scenario every
  time.

## Why nlink-lab

containerlab + a bash script can do something similar, but you
end up with the failure logic in YAML *and* in shell, the timing
is whatever `sleep` does, and there's no built-in assertion of
the validation result. nlink-lab's scenario engine is a real
typed primitive: actions (`down`/`up`/`clear`/`validate`/`exec`/
`log`) at fixed time offsets, with the validation report as the
test result.

## NLL

[`examples/cookbook/p2p-partition.nll`](../../examples/cookbook/p2p-partition.nll):

```nll
lab "p2p-partition" { dns hosts }

profile router { forward ipv4 }

node hub : router
node alice  { route default via ${hub.eth0} }
node bob    { route default via ${hub.eth1} }
node carol  { route default via ${hub.eth2} }

link hub:eth0 -- alice:eth0  { subnet 10.0.1.0/24 }
link hub:eth1 -- bob:eth0    { subnet 10.0.2.0/24 }
link hub:eth2 -- carol:eth0  { subnet 10.0.3.0/24 }

# WAN impairment on alice's path so the lab feels real.
impair hub:eth0 delay 30ms jitter 5ms loss 0.5%

scenario "partition-and-heal" {
  at 0s {
    log "baseline: full mesh reachable"
    exec alice "ping" "-c" "3" "-W" "1" "bob"
    validate {
      reach alice bob
      reach alice carol
      reach bob carol
    }
  }

  at 3s {
    log "partitioning alice from the rest of the fabric"
    down hub:eth0
  }

  at 6s {
    validate {
      no-reach bob alice
      no-reach carol alice
      reach bob carol         # rest of fabric still works
    }
  }

  at 11s {
    log "healing the partition"
    up hub:eth0
  }

  at 14s {
    validate {
      reach alice bob
      reach alice carol
      reach bob carol
    }
  }

  at 16s {
    log "clearing the WAN impair"
    clear hub:eth0
    exec alice "ping" "-c" "3" "-W" "1" "bob"
  }
}
```

Five action kinds appear here:

- **`down ENDPOINT`** — `ip link set DEV down` in the namespace.
- **`up ENDPOINT`** — `ip link set DEV up`.
- **`clear ENDPOINT`** — remove all impairments from the
  interface (deletes the root qdisc).
- **`validate { ... }`** — run a block of reach / no-reach /
  tcp-connect / latency-under / route-has / dns-resolves
  assertions. Failures abort the scenario and the lab reports
  non-zero exit.
- **`exec NODE "cmd" "arg" ...`** — run a one-shot command in
  a node. Useful for capturing observed behavior into the lab's
  test output.
- **`log "message"`** — labelled log line in the scenario output.

## Run

```bash
sudo nlink-lab deploy examples/cookbook/p2p-partition.nll
```

`deploy` runs the *deploy-time* `validate { }` block (the smoke
test) but does not run named scenarios. To execute the
`partition-and-heal` scenario:

```bash
sudo nlink-lab scenario run p2p-partition partition-and-heal
```

Output:

```text
[0.001s] log: baseline: full mesh reachable
[0.024s] exec alice ping bob → exit 0 (3 packets received)
[0.045s] validate: reach alice bob ✓
         validate: reach alice carol ✓
         validate: reach bob carol ✓
[3.001s] log: partitioning alice from the rest of the fabric
[3.005s] down hub:eth0
[6.001s] validate: no-reach bob alice ✓
         validate: no-reach carol alice ✓
         validate: reach bob carol ✓
[11.001s] log: healing the partition
[11.004s] up hub:eth0
[14.001s] validate: reach alice bob ✓
          validate: reach alice carol ✓
          validate: reach bob carol ✓
[16.001s] log: clearing the WAN impair
[16.003s] clear hub:eth0
[16.018s] exec alice ping bob → exit 0 (3 packets received, avg 0.4ms)

scenario "partition-and-heal" PASSED in 16.022s
```

The exec output shows alice's RTT before the impair clear (~30ms)
and after (~0.4ms). The numbers tell the story: WAN impair was
real, and the scenario engine cleanly removed it.

If any `validate` step fails, the scenario aborts immediately:

```text
[6.001s] validate: no-reach bob alice ✗ — alice IS reachable from bob

scenario "partition-and-heal" FAILED at step at=6s
```

The exit code reflects success/failure for CI consumption.

## Tear down

```bash
sudo nlink-lab destroy p2p-partition
```

## Inside `validate { }`

The validate block is the main pass/fail mechanism. Six
assertion kinds are supported:

| Assertion | What |
|-----------|------|
| `reach A B` | ICMP ping A → B succeeds |
| `no-reach A B` | ICMP ping A → B fails (must time out, not return RST) |
| `tcp-connect A B PORT [retries N interval Ts]` | TCP SYN A → B:PORT succeeds; optional retry semantics |
| `latency-under A B Tms samples N` | mean RTT A → B < Tms over N pings |
| `route-has NODE DEST via GW` | route exists in NODE's table |
| `dns-resolves NODE NAME ADDR` | NODE's resolver returns ADDR for NAME |

`tcp-connect` with `retries`/`interval` is the right choice for
asserting "service comes up within N seconds" — useful after a
heal step where the application takes a moment to recover.

## CI integration

Run as part of an integration suite:

```bash
sudo nlink-lab deploy examples/cookbook/p2p-partition.nll
sudo nlink-lab scenario run --json p2p-partition partition-and-heal > /tmp/result.json
RC=$?
sudo nlink-lab destroy p2p-partition
exit $RC
```

The JSON form has per-step timing and success/failure for
machine-readable CI reports.

## Variations

- **Probabilistic faults**: instead of `down`/`up`, use `clear` +
  re-deploy with different impair settings. Or pre-declare
  multiple `impair` endpoints with different impairment levels
  and rotate via the runtime
  [`impair`](../cli/impair.md) CLI.
- **Multiple scenarios per topology**: declare several `scenario
  "name" { … }` blocks and run them by name. The deploy step
  installs all of them; `scenario run` selects which fires.
- **Long scenarios with periodic checks**: a scenario can run
  for hours. Common pattern: `at 0s` to start, then `at 60s`,
  `at 120s`, `at 180s`, … each with a quick `validate` to catch
  drift over time.
- **Combine with the benchmark block**: `scenario` for chaos,
  `benchmark` for SLA. Both can coexist in the same lab.

## Composing with `#[lab_test]`

You can drive the scenario from a Rust integration test instead
of the CLI:

```rust
use nlink_lab::lab_test;
use nlink_lab::RunningLab;

#[lab_test("examples/cookbook/p2p-partition.nll", timeout = 30)]
async fn partition_recovery_works(lab: RunningLab) {
    let result = lab.run_scenario("partition-and-heal").await.unwrap();
    assert!(result.passed());
    assert!(result.duration().as_secs() < 20);
}
```

See [Cookbook: Rust integration tests](rust-integration-test.md).

## When this is the wrong tool

- For sub-millisecond timing precision, the scenario engine
  isn't the right primitive — its scheduler aims for ±100ms.
  Use a per-test custom Rust harness instead.
- For interactive chaos exploration ("let me drop a few
  packets and see what happens"), use the runtime
  [`impair`](../cli/impair.md) CLI, not a pre-declared
  scenario.
- For chaos testing of vendor NOS software, you need that
  vendor's image — use containerlab.

## See also

- [NLL: scenario syntax](../NLL_DSL_DESIGN.md)
- [`impair` CLI](../cli/impair.md) — runtime impair manipulation
- [Cookbook: Rust integration test](rust-integration-test.md) —
  drive the scenario from `cargo test`
