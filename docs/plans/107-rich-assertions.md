# Plan 107: Rich Validation Assertions

**Date:** 2026-03-30
**Status:** Implemented (2026-03-30)
**Effort:** Medium (2-3 days)
**Depends on:** Nothing

---

## Problem Statement

The `validate` block currently supports only two assertions:

```nll
validate {
  reach host1 host2       # ping succeeds
  no-reach host1 host3    # ping fails
}
```

This is sufficient for basic connectivity checks but insufficient for real
network testing. Users need to validate:

- TCP/UDP port connectivity (not just ICMP)
- Latency bounds (SLA verification)
- Route existence and correctness
- DNS resolution (with `dns hosts`)
- Bandwidth minimums (after rate limiting)

## NLL Syntax

```nll
validate {
  # Existing
  reach client server
  no-reach attacker server

  # New: TCP/UDP connectivity
  tcp-connect client server 80
  tcp-connect client server 443 timeout 5s
  udp-open client server 53

  # New: Latency bounds
  latency-under client server 50ms
  latency-under client server 100ms samples 10

  # New: Route verification
  route-has router "10.0.0.0/24" via "10.0.1.1"
  route-has router "default" dev "eth0"

  # New: DNS resolution (requires dns hosts)
  dns-resolves client "server" "10.0.2.2"

  # New: Bandwidth (requires iperf3 installed in namespace)
  bandwidth-above client server 900mbit duration 5s
}
```

## Implementation

### 1. Types (`types.rs`)

Extend the `Assertion` enum:

```rust
pub enum Assertion {
    Reach { from: String, to: String },
    NoReach { from: String, to: String },
    TcpConnect {
        from: String,
        to: String,
        port: u16,
        timeout: Option<String>,
    },
    UdpOpen {
        from: String,
        to: String,
        port: u16,
    },
    LatencyUnder {
        from: String,
        to: String,
        max_ms: String,
        samples: Option<u32>,
    },
    RouteHas {
        node: String,
        destination: String,
        via: Option<String>,
        dev: Option<String>,
    },
    DnsResolves {
        from: String,
        name: String,
        expected_ip: String,
    },
    BandwidthAbove {
        from: String,
        to: String,
        min_rate: String,
        duration: Option<String>,
    },
}
```

### 2. Lexer (`lexer.rs`)

Add tokens for new assertion keywords. Most can reuse existing tokens
(`Tcp`, `Udp`, `Route`, `Dns`) or be parsed as identifiers.

New compound keywords:
- `tcp-connect`
- `udp-open`
- `latency-under`
- `route-has`
- `dns-resolves`
- `bandwidth-above`

### 3. Parser (`parser.rs`)

Extend `parse_assertion()` to handle new assertion types.

### 4. AST + Lower

Add corresponding AST types and lower to `Assertion` variants.

### 5. Execution (`running.rs` or new `assertions.rs`)

Each assertion type maps to a command executed inside a namespace:

| Assertion | Implementation |
|-----------|----------------|
| `reach` / `no-reach` | `ping -c1 -W2 <ip>` (existing) |
| `tcp-connect` | `bash -c "echo > /dev/tcp/<ip>/<port>"` or `nc -z -w<timeout> <ip> <port>` |
| `udp-open` | `nc -zu -w2 <ip> <port>` |
| `latency-under` | `ping -c<samples> <ip>`, parse avg from output |
| `route-has` | `ip route show <dest>`, check for via/dev in output |
| `dns-resolves` | `getent hosts <name>`, check IP in output |
| `bandwidth-above` | `iperf3 -c <ip> -t<duration> -J`, parse bits_per_second |

**Key design decision:** Assertions should use only tools available in minimal
namespaces. `ping`, `ip`, `bash` are always present. `nc` and `iperf3` are
optional — assertions that need them should fail gracefully with a clear message
("nc not found; install netcat to use tcp-connect assertions").

### 6. Output

Assertion results should be structured for CI consumption:

```rust
pub struct AssertionResult {
    pub assertion: String,      // human-readable description
    pub passed: bool,
    pub detail: Option<String>, // e.g., "latency: 12ms (max: 50ms)"
    pub duration_ms: u64,       // how long the check took
}
```

The `--json` flag should output results as JSON for CI pipelines.

### 7. Tests

| Test | Description |
|------|-------------|
| `test_parse_tcp_connect` | Parser: tcp-connect with timeout |
| `test_parse_latency_under` | Parser: latency-under with samples |
| `test_parse_route_has` | Parser: route-has with via/dev |
| `test_parse_dns_resolves` | Parser: dns-resolves assertion |
| `test_lower_all_assertions` | Lower: all assertion types |
| `test_render_assertions` | Render: roundtrip for new assertions |
| Integration: `validate_tcp_connect` | Deploy, run tcp-connect assertion |
| Integration: `validate_route_has` | Deploy, verify route assertion |
| Integration: `validate_dns_resolves` | Deploy with `dns hosts`, verify dns-resolves |

### File Changes

| File | Change |
|------|--------|
| `types.rs` | Extend `Assertion` enum with 6 new variants |
| `lexer.rs` | Add compound keyword tokens |
| `ast.rs` | Add assertion AST types |
| `parser.rs` | Extend assertion parsing |
| `lower.rs` | Lower new assertion types |
| `render.rs` | Render new assertions |
| `running.rs` or new `assertions.rs` | Execute assertion commands |
| `deploy.rs` | Pass structured results from Step 19 |
