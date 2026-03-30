# Plan 111: Benchmark Block

**Date:** 2026-03-30
**Status:** Implemented (2026-03-30) — types, parser, lower; execution engine deferred
**Effort:** Medium (3-5 days)
**Depends on:** Nothing (but benefits from Plan 109 CI integration)

---

## Problem Statement

Performance testing in network labs requires manual orchestration:

1. Start iperf3 server in one namespace
2. Run iperf3 client in another
3. Parse JSON output
4. Compare against expected baseline
5. Clean up server process

This is tedious and error-prone. A first-class `benchmark` block would make
performance testing declarative and integrate with CI/CD.

## NLL Syntax

```nll
lab "perf-test"

node server { route default via ${router.eth1} }
node client { route default via ${router.eth0} }
node router : router

link router:eth0 -- client:eth0 { subnet 10.0.1.0/24 }
link router:eth1 -- server:eth0 { subnet 10.0.2.0/24 rate 1gbit }

benchmark "throughput" {
  iperf3 client server {
    duration 10s
    streams 4
    assert bandwidth > 900mbit
    assert jitter < 1ms         # UDP mode
  }
}

benchmark "latency" {
  ping client server {
    count 100
    assert avg < 5ms
    assert p99 < 10ms
    assert loss < 1%
  }
}
```

## Implementation

### 1. Types (`types.rs`)

```rust
pub struct Benchmark {
    pub name: String,
    pub tests: Vec<BenchmarkTest>,
}

pub enum BenchmarkTest {
    Iperf3 {
        from: String,
        to: String,
        duration: Option<String>,
        streams: Option<u32>,
        udp: bool,
        assertions: Vec<BenchmarkAssertion>,
    },
    Ping {
        from: String,
        to: String,
        count: Option<u32>,
        assertions: Vec<BenchmarkAssertion>,
    },
}

pub struct BenchmarkAssertion {
    pub metric: String,      // "bandwidth", "jitter", "avg", "p99", "loss"
    pub op: CompareOp,       // >, <, >=, <=
    pub value: String,       // "900mbit", "5ms", "1%"
}

pub enum CompareOp { Gt, Lt, Gte, Lte }
```

### 2. Execution

The benchmark runner:

1. Spawns iperf3 server in target namespace (background)
2. Waits 500ms for server to start
3. Runs iperf3 client with `--json` in source namespace
4. Parses JSON output for metrics
5. Evaluates assertions against metrics
6. Kills server process
7. Returns structured results

For ping:
1. Runs `ping -c<count> -q` in source namespace
2. Parses summary line for min/avg/max/mdev and loss %
3. Evaluates assertions

### 3. Output

```json
{
  "benchmark": "throughput",
  "tests": [
    {
      "type": "iperf3",
      "from": "client",
      "to": "server",
      "results": {
        "bandwidth_bps": 950000000,
        "jitter_ms": 0.3,
        "loss_percent": 0.0
      },
      "assertions": [
        { "metric": "bandwidth", "op": ">", "value": "900mbit", "actual": "950mbit", "passed": true }
      ]
    }
  ]
}
```

### 4. CLI

```bash
# Run benchmarks after deploy
sudo nlink-lab benchmark run mylab

# Run specific benchmark
sudo nlink-lab benchmark run mylab throughput

# Save results as baseline
sudo nlink-lab benchmark run mylab --save-baseline

# Compare against baseline (for CI regression detection)
sudo nlink-lab benchmark run mylab --compare-baseline
```

### File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `Benchmark`, `BenchmarkTest`, `BenchmarkAssertion` |
| `lexer.rs` | Add `Benchmark`, `Assert`, `Bandwidth`, `Streams` tokens |
| `ast.rs` | Add benchmark AST types |
| `parser.rs` | Parse benchmark blocks |
| `lower.rs` | Lower benchmarks |
| `benchmark.rs` | **New:** benchmark execution engine |
| `lib.rs` | Add `mod benchmark` |
| `bins/lab/src/main.rs` | Add `benchmark` subcommand |
| `examples/benchmark.nll` | New example |
