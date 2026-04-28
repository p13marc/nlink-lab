# iperf3 benchmark with hard assertions

A throughput and latency benchmark whose pass/fail is part of the
topology. The lab declares the metric thresholds inline; deploy
runs the test and fails the build if iperf3 reports below SLA.

## When to use this

- A CI gate that catches latency regressions in your network
  stack (kernel upgrade, iptables rule change, custom shaper).
- Validating that a configuration change didn't unintentionally
  reduce throughput.
- Measuring how an application's performance degrades under
  realistic impairment.

## Why nlink-lab

containerlab can run iperf3 in a `linux` container and a CI step
can grep its output for pass/fail, but the assertion logic ends
up in YAML+shell glue. nlink-lab puts the SLA *in the topology*:
the same NLL describes the lab and what "passing" means.

## NLL: latency benchmark with assertion

[`examples/benchmark.nll`](../../examples/benchmark.nll):

```nll
lab "perf-test"
profile router { forward ipv4 }

node router : router
node server { route default via ${router.eth1} }
node client { route default via ${router.eth0} }

link router:eth0 -- client:eth0 { subnet 10.0.1.0/24 }
link router:eth1 -- server:eth0 { subnet 10.0.2.0/24 }

benchmark "latency" {
  ping client server {
    count 10
    assert avg below 50ms
    assert loss below 5%
  }
}
```

The `benchmark` block declares one or more named tests; each test
runs at deploy time and asserts metric thresholds.

Assertion operators: `below`, `above`. Metrics: `avg`, `min`,
`max`, `mdev`, `loss`, `p50`, `p99` (where the underlying tool
provides them).

## NLL: iperf3 throughput

[`examples/iperf-benchmark.nll`](../../examples/iperf-benchmark.nll):

```nll
lab "iperf-bench"

node server { route default via 10.0.0.2 }
node client { route default via 10.0.0.1 }

link server:eth0 -- client:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

rate server:eth0 egress 100mbit ingress 100mbit
rate client:eth0 egress 100mbit ingress 100mbit

benchmark "throughput" {
  iperf3 client server {
    duration 10s
    parallel 4
    assert sender-bps above 90mbit
    assert retransmits below 100
  }
}
```

(The `iperf-benchmark.nll` shipped today omits the `benchmark`
block — add it to convert "you can run iperf3 by hand" into "the
lab fails if iperf3 reports under spec.")

## Run

iperf3 must be installed on the host (the binary is exec'd inside
the namespaces):

```bash
sudo apt-get install iperf3   # or: dnf install iperf3
sudo nlink-lab deploy examples/benchmark.nll
```

Deploy runs the benchmark block as part of step 17 (validation).
Output looks like:

```text
benchmark "latency"
  ping client → server  count=10
    avg=2.3ms     ✓ (assert: below 50ms)
    loss=0%       ✓ (assert: below 5%)
  PASS
```

If an assertion fails, deploy exits non-zero with the reason.

### Manual run after deploy

You can re-run a benchmark on demand without redeploying:

```bash
sudo nlink-lab benchmark run perf-test latency
```

### CI integration

```bash
# CI script
sudo nlink-lab deploy --json examples/benchmark.nll
RC=$?
sudo nlink-lab destroy perf-test
exit $RC
```

The exit code reflects assertion success.

### Tear down

```bash
sudo nlink-lab destroy perf-test
```

## How it works

`ping` benchmarks shell out to the system `ping` and parse the
summary line for `min/avg/max/mdev` and the loss percentage.

`iperf3` benchmarks spawn the server in the destination node, run
the client in the source node with `--json`, and assert against
fields in the JSON output (`end.sum_sent.bits_per_second`,
`end.sum_sent.retransmits`, etc.).

Both runners are implemented in
[`crates/nlink-lab/src/benchmark.rs`](../../crates/nlink-lab/src/benchmark.rs)
— roughly 370 LOC of parsers + assertion evaluator.

## Variations

- **Combine with impairment**: add `delay 200ms loss 1%` on the
  link, then assert iperf3 throughput stays above some lower
  threshold. Catches over-aggressive congestion-control tuning.
- **Multiple benchmarks**: declare several `benchmark` blocks
  (latency, throughput, packet-rate) and assert on all of them.
- **Per-PR regression gate**: store the previous run's metrics in
  CI artifacts, compare against the new run, fail on >10%
  regression.

## When this is the wrong tool

- You need to benchmark a real application's RPS, not raw network
  performance. Use a load-generator (wrk, bombardier, k6) inside
  the client node — `spawn` it and parse its output yourself.
- You need flame graphs of in-kernel network paths. Use `perf`
  inside the namespace; the benchmark block won't help with that.

## See also

- [NLL: benchmark block](../NLL_DSL_DESIGN.md)
- [scenario engine](../NLL_DSL_DESIGN.md) — for time-varying
  conditions during the benchmark
