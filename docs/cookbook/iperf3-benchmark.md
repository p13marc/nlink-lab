# iperf3 benchmark with hard assertions

A throughput and latency benchmark whose pass/fail is part of the
topology. The lab declares the metric thresholds inline;
`nlink-lab benchmark` runs the test on the deployed lab and exits 2 if
a metric misses its threshold.

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

The `benchmark` block declares one or more tests; `nlink-lab benchmark
LAB NAME` runs them and asserts metric thresholds.

Assertion operators: `below`, `above`. Metrics:

- `ping`: `min`, `avg`, `max`, `p99` (milliseconds; `p99` is the max of a
  small sample) and `loss` (%);
- `iperf3`: `bandwidth` (bit/s, as the receiver got it) and, with `udp`,
  `jitter` (ms) and `loss` (%).

A threshold is written in the metric's unit: any duration (`500us`,
`20ms`, `1s`), any rate (`900mbit`, `1g`), a percentage. `validate`
rejects a metric the test does not report (`benchmark-metric`).

## NLL: iperf3 throughput

[`examples/iperf-benchmark.nll`](../../examples/iperf-benchmark.nll) (extended with a `benchmark` block):

```nll-ignore
lab "iperf-bench"

node server { route default via 10.0.0.2 }
node client { route default via 10.0.0.1 }

link server:eth0 -- client:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }

rate server:eth0 egress 100mbit ingress 100mbit
rate client:eth0 egress 100mbit ingress 100mbit

benchmark "throughput" {
  iperf3 client server {
    duration 10s
    streams 4
    assert bandwidth above 90mbit
  }
  iperf3 client server {
    udp
    rate 50mbit          # iperf3's -b; a UDP test runs at 1 Mbit/s without it
    assert loss below 1%
    assert jitter below 2ms
  }
}
```

(The `iperf-benchmark.nll` shipped today omits the `benchmark`
block — add it to convert "you can run iperf3 by hand" into "the
lab fails if iperf3 reports under spec.")

## Run

iperf3 must be installed on both nodes (on the host for namespace
nodes, in the image for container nodes):

```bash
sudo apt-get install iperf3   # or: dnf install iperf3
sudo nlink-lab deploy examples/benchmark.nll
sudo nlink-lab benchmark -L perf-test latency
```

Output looks like:

```text
Benchmark "latency": PASS
  ok  ping client -> server
      avg            2.3
      loss           0
      ok  assert avg below 50ms (actual 2.3)
      ok  assert loss below 5% (actual 0)
```

`nlink-lab benchmark -L perf-test` without a name lists the lab's
benchmarks. A test that measured nothing (no address for the target,
iperf3 missing) says why under its line.

### CI integration

```bash
# CI script
sudo nlink-lab deploy examples/benchmark.nll
sudo nlink-lab benchmark -L perf-test latency
RC=$?
sudo nlink-lab destroy perf-test
exit $RC
```

The exit code is 2 when an assertion fails.

### Tear down

```bash
sudo nlink-lab destroy perf-test
```

## How it works

`ping` benchmarks run `ping -q` in the source node and parse its
summary (iputils' `min/avg/max/mdev` or busybox's `min/avg/max`) and
the loss percentage.

`iperf3` benchmarks start a one-shot server in the destination node,
run the client in the source node with `-J`, and read the JSON summary
(`end.sum_received.bits_per_second` for TCP; `end.sum` with
`jitter_ms` and `lost_percent` for UDP).

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
