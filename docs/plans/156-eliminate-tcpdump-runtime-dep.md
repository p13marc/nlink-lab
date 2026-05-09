# Plan 156: Eliminate the `tcpdump` runtime dependency

**Date:** 2026-04-30
**Status:** Proposed
**Effort:** Small (1 day for the recommended option)
**Priority:** P2 — only matters when users pass `capture --filter`;
default path already C-dep-free.

---

## TL;DR

nlink-lab has **zero** compile-time C library dependencies. The
only non-Rust runtime dependency is a `std::process::Command`
shell-out to **`tcpdump -dd`** in `capture.rs:217`, used to
translate `--filter "tcp port 80"` syntax into BPF bytecode.
Eliminating it removes the last external-tool dependency from
the capture path.

`flowscope` is the wrong tool for this specific job (it does
flow analysis post-capture, not BPF compilation), but it's a
strong fit for a *separate* future feature (`nlink-lab analyze
trace.pcap` flow summaries).

The recommended fix: **a typed BPF builder in NLL + Rust API for
common cases, with an opt-in tcpdump fallback for legacy filter
strings.** ~1 day of work.

---

## Audit

### Compile-time C library dependencies: none

`cargo tree -p nlink-lab` shows zero `*-sys` crates that wrap a
non-system library:

- `netring` — pure-Rust AF_PACKET TPACKET_V3 ring buffer.
- `nlink` — pure-Rust netlink (no `libnl-sys`, no `libnetlink-sys`).
- `flate2` — uses the default `miniz_oxide` backend (pure Rust),
  not the C `zlib` backend.
- `tar`, `sha2`, `tempfile`, `serde`, `tokio`, `logos`, `winnow`,
  `miette`, `time`, `tracing` — pure Rust.
- `libc`, `linux-raw-sys` — FFI **declaration** crates (header
  bindings). They emit `extern "C"` declarations for syscalls
  and call into the system's `libc.so`/`vmlinux`. They do not
  link any third-party C library.
- `inotify-sys` — same shape as `libc`: FFI declarations for the
  inotify syscalls, no external C library.

The `nlink-lab` binary links against `libc` (universal on every
Linux distro, the same way every Rust binary does). It does not
require any other C library to be present at build or run time.

### Runtime: capture is already 100% pure Rust

The capture path in `crates/nlink-lab/src/capture.rs`:

| Step | Implementation |
|------|---------------|
| Enter target namespace | `setns(2)` via `nlink::netlink::namespace::enter` (libc syscall, no library) |
| Read packets | `netring` AF_PACKET TPACKET_V3 ring buffer (pure Rust) |
| Pcap file output | Hand-rolled writer (`capture.rs:25-205`) — nanosecond-magic `0xa1b23c4d`, `LINKTYPE_ETHERNET`. No libpcap, no `pcap-sys`. |
| Live mode summaries | Pure Rust ethernet/IP/TCP/UDP/ICMP decoders inline |

### The one wart: `compile_bpf_filter` shells out to tcpdump

`capture.rs:217`:

```rust
pub fn compile_bpf_filter(expression: &str) -> Result<Vec<BpfInsn>> {
    let output = std::process::Command::new("tcpdump")
        .args(["-dd", expression])    // dump as C-style BPF bytecode
        .output()?;
    // Parse `{ 0x28, 0, 0, 0x0000000c },` lines into BpfInsn
}
```

This is invoked when the user passes `nlink-lab capture LAB
NODE:IFACE -f "tcp port 80"`. The CLI flag is documented; the
function is reachable from the public library API (e.g. embedded
filter expressions from a `#[lab_test]` driver).

**Failure modes:**

- `tcpdump` not installed → `Error::Capture("failed to run
  tcpdump for BPF compilation: ...")`.
- `tcpdump` installed but the syntax is rejected → tcpdump's
  stderr surfaces verbatim; the CLI surface is correct.
- Indirect dependency on libpcap (tcpdump's own library
  dependency) — true on every distro tcpdump ships on.

So users who never pass `--filter` already see no C-dep surface
at runtime. Users who do pass `--filter` need tcpdump installed.

---

## Why `flowscope` doesn't fit this problem

[`p13marc/flowscope`](https://github.com/p13marc/flowscope)
provides:

- `FlowTracker` — bidirectional accounting + TCP state machine.
- `Reassembler` — per-flow byte stream reconstruction.
- `SessionParser`/`DatagramParser` — typed L7 parsers (HTTP,
  TLS/JA3, DNS).
- `FlowExtractor` trait — 5-tuple / IP-pair / MAC-pair.

It sits **downstream** of packet capture: given a stream of
packets (from netring, a pcap file, or eBPF), it produces flow
tables and parsed application messages. It is not a BPF
compiler. It cannot replace `tcpdump -dd`.

That said, flowscope is a strong fit for a separate, future
feature surface that **isn't part of this plan**:

- A `nlink-lab analyze <pcap>` subcommand that prints flow
  tables, top-talkers, retransmit counts, JA3 fingerprints.
- A `--summary` flag on `nlink-lab capture` that emits flow-level
  output instead of raw packets.
- A test-helper `lab.flow_summary("node", "iface")` that captures
  briefly and returns parsed flows for assertions.

These are tracked as Plan 157+ ideas; they don't gate this plan.

---

## Three options for replacing tcpdump

### Option A — pure-Rust cBPF compiler

Implement a parser + compiler for tcpdump's filter language
inside nlink-lab (or upstream into `netring`). Existing cBPF
crates on crates.io (e.g. `pcap-parser`) handle the on-wire
*format* but don't compile expressions; the closest reference is
tcpdump's `libpcap/gencode.c` which is ~3k LOC of careful C.

**Pros:** preserves the user-visible API. Existing scripts that
pipe a tcpdump expression to `nlink-lab capture --filter` keep
working.

**Cons:** non-trivial implementation. Edge cases (vlan tagged
matching, IPv6 extension headers, complex `and/or/not` precedence)
are where libpcap accumulated 25+ years of bug fixes.

**Effort:** Medium-large (3–5 days for a usable subset; full
parity with libpcap is a multi-month project).

### Option B — typed BPF builder DSL (recommended)

Replace the string filter expression with a typed builder for
the common cases. NLL gets a `filter { ... }` block; the Rust
API gets a `BpfFilter::new()` builder.

**NLL form:**

```nll-ignore
# Today (deferred to tcpdump):
nlink-lab capture lab router:eth0 -f "tcp port 80"

# Proposed builder (typed, no shell-out):
nlink-lab capture lab router:eth0 \
    --filter-tcp-port 80
# or via NLL:
capture router:eth0 {
  filter { tcp dport 80 }
}
```

**Rust API:**

```rust
use nlink_lab::capture::BpfFilter;

let filter = BpfFilter::builder()
    .tcp().dst_port(80)
    .build();
let cfg = CaptureConfig {
    interface: "eth0".into(),
    bpf_filter: Some(filter.into_insns()),
    ..
};
```

Common cases to cover (covers ≥ 95% of real-world `--filter`
usage):

- `tcp port N`, `udp port N` (src/dst variants).
- `host ADDR`, `src host ADDR`, `dst host ADDR` (v4 and v6).
- `net CIDR`, `src net CIDR`, `dst net CIDR`.
- `arp`, `vlan`, `mpls`.
- `and` / `or` / `not` composition (limited — let the user
  combine programs in code if they need deep nesting).

The legacy `--filter "<tcpdump syntax>"` flag stays available,
gated behind a `legacy-tcpdump-filter` feature and clearly
labeled as "requires tcpdump on $PATH." Default builds drop the
runtime dep entirely.

**Pros:** zero new dependencies. Typed surface composes cleanly
with the rest of NLL. Existing users who don't pass `--filter`
see no change. Existing users who do can either migrate or opt
into the legacy path.

**Cons:** semi-breaking for users who relied on advanced filter
expressions (rare in our use case — `nlink-lab` is for testing
your own application, not packet-trace forensics). Any
expression more complex than the supported set requires either
the legacy path or a hand-written `Vec<BpfInsn>`.

**Effort:** Small (~1 day). Most of the work is the typed
builder and a translation table to BPF bytecode. The bytecode
patterns for tcp/udp port, host, net are well-documented and
small (10–20 instructions each).

### Option C — eBPF programs via `aya`

Replace cBPF entirely with eBPF programs attached via
`setsockopt(SOL_SOCKET, SO_ATTACH_BPF, ...)`. Use [`aya`](https://aya-rs.dev)
to write programs in Rust and load them.

**Pros:** modern, programmable, future-proof. Opens the door to
much richer filtering (per-flow state, conntrack-aware matching,
packet rewriting if we ever wanted it).

**Cons:** much larger surface than the original problem calls
for. eBPF requires kernel ≥ 4.x and a verifier-friendly program
shape. The toolchain (LLVM + bpf target) is heavier to maintain
than cBPF.

**Effort:** Medium-large (5+ days). Significant new surface for a
project that doesn't otherwise use eBPF.

---

## Recommendation: Option B

**Why:** the actual user need is "filter packets to TCP/UDP/host
during a capture run." 95% of real filter expressions are simple
enough that a typed builder covers them, and the typed surface
is more discoverable than a tcpdump string anyway. The 5% who
need advanced expressions can opt into the `legacy-tcpdump-filter`
feature.

This is the path that minimally disrupts existing users while
removing the runtime tooling dependency from the default path.

## Implementation outline

### 1. New module: `crates/nlink-lab/src/capture/filter.rs`

```rust
pub struct BpfFilter {
    program: Vec<BpfInsn>,
}

impl BpfFilter {
    pub fn builder() -> BpfFilterBuilder { ... }
    pub fn into_insns(self) -> Vec<BpfInsn> { self.program }
}

pub struct BpfFilterBuilder {
    fragments: Vec<MatchFrag>,
    combinator: Combinator,
}

enum MatchFrag {
    EthType(u16),         // ARP, IPv4, IPv6, VLAN, MPLS
    IpProto(u8),          // TCP, UDP, ICMP, GRE
    SrcIp(IpAddr, u8),    // address + prefix
    DstIp(IpAddr, u8),
    AnyIp(IpAddr, u8),    // src OR dst
    SrcPort(u16),
    DstPort(u16),
    AnyPort(u16),
    VlanId(u16),
}
```

The compiler emits one well-known program per fragment, then
combines them via `and`/`or`/`not` BPF idioms. cBPF programs for
each fragment are < 30 instructions; the assembler is mechanical.

### 2. NLL syntax extension

```nll-ignore
capture router:eth0 {
  filter {
    tcp                       # eth_type ipv4 + ip_proto tcp
    dst_port 80
    src_net 10.0.1.0/24
  }
}
```

Parses to `Vec<MatchFrag>` with implicit `and` between siblings.
Explicit `any { ... }` for `or`, `not { ... }` for negation.

### 3. CLI flag form

```text
nlink-lab capture LAB EP \
    --filter-tcp                \
    --filter-dst-port 80        \
    --filter-src-net 10.0.1.0/24
```

Repeatable flags compose with implicit AND. For the OR /
parenthesized cases, fall back to NLL or library use.

### 4. Backward compat

```text
# Still works, but requires tcpdump on PATH:
nlink-lab capture LAB EP --filter "tcp port 80"
```

Behind a `legacy-tcpdump-filter` Cargo feature. When the feature
is **off** (default), passing `--filter` errors at parse time
with a clear migration suggestion.

### 5. Documentation

| File | Change |
|------|--------|
| `docs/cli/capture.md` | Replace `-f` examples with `--filter-*` flag forms; mention legacy path |
| `docs/cookbook/<existing-recipes>.md` | Update filter examples |
| `docs/NLL_DSL_DESIGN.md` | New section on `filter { }` blocks |
| `CHANGELOG.md` | "Removed: implicit tcpdump runtime dep on capture path" |

### 6. Tests

| Test | Description |
|------|-------------|
| `bpf_filter_tcp_port` | `BpfFilter::builder().tcp().dst_port(80)` produces a program that accepts a synthetic TCP-80 packet and rejects others. |
| `bpf_filter_host_v4` | Source/dest/any host filtering on IPv4 |
| `bpf_filter_host_v6` | Same for IPv6 |
| `bpf_filter_vlan` | VLAN tag matching |
| `bpf_filter_compose` | `and` / `or` / `not` combinations |
| `nll_filter_block_parses` | NLL `filter { tcp dst_port 80 }` lowers to the expected `BpfFilter` |

Each test runs the synthesized program against a few hand-crafted
test packets without touching the kernel — pure unit tests, no
root needed.

## Out of scope (this plan)

- **Full tcpdump expression-language parity.** That's option A,
  3–5 days of careful parsing work. Skip unless someone files an
  issue showing the typed builder isn't enough.
- **eBPF (option C).** Defer indefinitely. Possibly revisit if
  we ever want per-flow state in the filter.
- **`flowscope` integration.** Separate Plan 157+: capture
  --summary, analyze pcap, lab.flow_summary helper.

## Acceptance

- `cargo tree -p nlink-lab --no-default-features` shows zero
  `*-sys` crates wrapping a third-party library (already true).
- `nlink-lab capture LAB EP --filter-tcp --filter-dst-port 80`
  works without `tcpdump` on PATH.
- `nlink-lab capture LAB EP --filter "tcp port 80"` errors with
  a clear "requires `legacy-tcpdump-filter` feature" message in
  default builds; works in feature builds.
- 6 new lib tests for the BPF builder.
- CLI page + NLL spec updated.
- CHANGELOG entry under `[Unreleased]`.

## Files

| File | Change |
|------|--------|
| `crates/nlink-lab/src/capture/filter.rs` | New module — `BpfFilter`, builder, fragment compiler |
| `crates/nlink-lab/src/capture.rs` | Re-export filter module; gate `compile_bpf_filter` (legacy) behind feature flag |
| `crates/nlink-lab/Cargo.toml` | New `legacy-tcpdump-filter` feature (off by default) |
| `crates/nlink-lab/src/parser/nll/parser.rs` | Parse `filter { ... }` block in capture stmts |
| `crates/nlink-lab/src/parser/nll/lower.rs` | Lower to `BpfFilter` |
| `bins/lab/src/main.rs` | New `--filter-*` flags; legacy `--filter` gated |
| `docs/cli/capture.md` | Update |
| `docs/NLL_DSL_DESIGN.md` | New filter-block section |
| `CHANGELOG.md` | New entry under `[Unreleased]` |
