# nlink-lab — Project Analysis Report

**Date:** 2026-04-02
**Scope:** Feature completeness, test quality, documentation, known gaps, recommendations

---

## 1. Project Status Overview

| Metric | Value |
|--------|-------|
| Rust source files | 43 |
| Total lines of Rust | ~25,800 |
| Unit tests | 305 |
| Integration tests | 29 |
| Stress tests | 10 |
| Fuzz targets | 2 |
| NLL example files | 33 |
| CLI commands | 30 |
| Validator rules | 20 |
| Crates in workspace | 6 |

**Overall assessment:** The project is **feature-complete for core functionality**.
All implementation plans (050–127) are done. Two remaining plans are deferred/blocked
and non-critical. The codebase compiles cleanly with zero clippy warnings.

---

## 2. Feature Completeness

### Fully Implemented

| Category | Features |
|----------|----------|
| **Topology primitives** | Nodes, links, networks (bridges), profiles, VRF, VXLAN, VLAN |
| **Addressing** | IPv4/IPv6, CIDR, `subnet()`/`host()` functions, named pools, auto-addressing |
| **Routing** | Static routes, route groups, `routing auto` (BFS/Dijkstra), default route generation |
| **Security** | nftables firewall (src/dst matching), NAT (masquerade/snat/dnat), WireGuard |
| **Impairment** | TC netem (delay, jitter, loss, corrupt, reorder), rate limiting, asymmetric, inline syntax |
| **Containers** | Docker/Podman, cpu/memory limits, caps, healthchecks, depends-on, config injection |
| **DNS** | Auto `/etc/hosts` generation, per-namespace injection, container `--add-host` |
| **Wi-Fi** | mac80211_hwsim, hostapd/wpa_supplicant config, AP/Station/Mesh modes |
| **DSL features** | For-loops, let variables, interpolation, conditionals, imports, `for_each` fleet, glob patterns, sites |
| **CLI** | 30 commands: deploy, destroy, apply, exec, shell, validate, test, render, inspect, capture, diff, etc. |
| **Testing** | CI runner (JUnit/TAP), scenarios (fault injection), benchmarks (ping/iperf3 assertions) |
| **Observability** | Zenoh metrics streaming, diagnostics, live impairment modification |
| **State** | Persistent state (`~/.nlink-lab/`), flock-based locking, apply/hot-reload |
| **Visualization** | DOT graph, ASCII graph, topoviewer GUI (iced) |

### Partially Implemented

| Feature | Status | Gap |
|---------|--------|-----|
| **IPv6 firewall** | Addresses parse, forwarding works | `src`/`dst` matching in nftables rules not wired for IPv6 |
| **Error recovery** | Single error per file reported | Parser aborts at first error — no multi-error recovery |
| **Render round-trip** | Basic topologies round-trip | Advanced features (profiles, impairments, networks) not fully tested |

### Not Implemented (Blocked/Deferred)

| Feature | Plan | Blocker |
|---------|------|---------|
| **Per-pair impairment on bridges** | 128 | Needs nlink TC filter API (`add_filter` with u32 match) |
| **NAT `translate` shorthand** | 129 | Deferred — for-loop approach is sufficient |
| **Editor/IDE support** | — | No tree-sitter grammar, no LSP server, no syntax highlighting |
| **DHCP** | — | Out of scope — static addressing + DNS covers use cases |

---

## 3. Test Quality

### Strengths

- **Integration tests (29)** cover real deployment: namespace creation, veth pairs, routing,
  firewall blocking, VRF isolation, DNS resolution, state persistence, apply/diff
- **Stress tests** verify parsing 500-node topologies in <5 seconds
- **Validator tests (24)** cover all 20 validation rules with error cases
- **Fuzz testing** ensures parser/lexer never panic on arbitrary input
- **Lower.rs (97 tests)** is the most thoroughly tested module — loops, imports, interpolation,
  IP functions, conditions, auto-routing, glob patterns all covered

### Gaps

| Area | Tests | Issue |
|------|-------|-------|
| **Parser error cases** | 0 | All 31 parser tests are happy-path — no invalid syntax tested |
| **Parser error messages** | 0 | miette diagnostics exist but error text never verified |
| **CLI binary** | 0 | `bins/lab/main.rs` (1,900 lines) has zero tests |
| **Render advanced features** | 0 | Profiles, impairments, networks, routes, firewall not round-trip tested |
| **Scenario execution** | 3 | Only utility functions tested, not step execution |
| **Benchmark execution** | 3 | Metric parsing tested, not full benchmark runs |
| **Container lifecycle** | 0 | Image pull, healthcheck, depends-on not unit tested |
| **Wi-Fi deployment** | 0 | Config generation tested, hwsim management not tested |

### Recommended Test Additions

**High priority (low effort, high value):**

1. **Parser error tests (10-15 tests):** Feed invalid NLL and assert specific error messages.
   Examples: duplicate node names, malformed CIDR, unclosed braces, bad interpolation.

2. **Render round-trip tests (5-10 tests):** Parse → render → re-parse for topologies
   using profiles, impairments, networks, routes, NAT, containers.

3. **CLI smoke tests (5-8 tests):** Use `assert_cmd` crate or similar to test
   `validate`, `render --json`, `render --dot` on example files.

**Medium priority:**

4. **Scenario step execution tests:** Mock namespace ops, verify step ordering and timing.
5. **Container integration tests:** Test Docker/Podman detection and config generation.

---

## 4. Architecture Assessment

### What Works Well

- **Library-first design** — `crates/nlink-lab` is a clean library; `bins/lab` is thin CLI wrapper
- **logos + hand-written RD parser** — right choice for context-sensitive keywords and
  interpolation adjacency (see `docs/PARSER_ANALYSIS.md`)
- **AST → Topology lowering** — clean separation; loops/imports/variables resolved in lower.rs,
  deploy.rs only sees flat `Topology`
- **nlink abstraction** — all netlink operations go through nlink crate, no raw netlink in nlink-lab
- **State management** — flock-based locking prevents concurrent deploy/destroy races

### Technical Debt

| Item | Severity | Location |
|------|----------|----------|
| **deploy.rs is 3,000 lines** | Medium | Single file handles 18+ deployment steps |
| **lower.rs is 4,100 lines** | Medium | Import resolution, loop expansion, IP functions all in one file |
| **parser.rs is 3,500 lines** | Low | Recursive descent — inherently long but well-structured |
| **No structured logging** | Low | Uses `eprintln!` in places instead of `tracing` spans |

### Refactoring Opportunities (Not Urgent)

- **Split deploy.rs** into `deploy/mod.rs` + `deploy/namespace.rs`, `deploy/interfaces.rs`,
  `deploy/routing.rs`, `deploy/firewall.rs`, `deploy/containers.rs`
- **Split lower.rs** into `lower/mod.rs` + `lower/imports.rs`, `lower/loops.rs`,
  `lower/interpolation.rs`, `lower/ipfunc.rs`
- Neither is blocking — the code is readable as-is. Only worth doing if these files
  continue to grow.

---

## 5. Documentation

### Current State

| Document | Lines | Status |
|----------|-------|--------|
| `README.md` | ~400 | Comprehensive: features, install, examples, CLI reference |
| `CLAUDE.md` | ~350 | Complete: architecture, types, DSL features, deploy sequence |
| `docs/NLL_DSL_DESIGN.md` | ~830 | Full language spec with grammar and examples |
| `docs/NLINK_LAB.md` | ~500 | Design document: architecture, roadmap, comparisons |
| `docs/PARSER_ANALYSIS.md` | ~100 | Parser crate evaluation |
| `docs/plans/` | 2 files | Active plans (128, 129) + completed plan index |
| Examples | 33 files | Cover all DSL features |

### Gaps

- **No CHANGELOG** — version history not tracked
- **No CONTRIBUTING guide** — process for external contributors undefined
- **No `--help` long descriptions** — CLI subcommands have brief help text only
- **No API documentation** — `cargo doc` works but no `///` doc comments on public API

---

## 6. Missing Critical Features

**None.** The project covers all critical network lab functionality:
namespaces, links, bridges, addressing, routing, firewall, NAT, impairment,
containers, DNS, Wi-Fi, state management, and CI integration.

The two deferred plans (128, 129) are enhancements, not critical gaps.

---

## 7. Improvement Recommendations

### Priority 1 — Parser Error Quality (1-2 days)

The parser currently returns a single error and stops. Users writing NLL files
need better feedback:

1. **Multi-error reporting:** In the main parse loop, catch errors and skip to
   the next top-level keyword (`node`, `link`, `network`, `profile`). Report
   all errors at once instead of one-at-a-time.

2. **Error message tests:** Add 10-15 tests that feed bad NLL and verify the
   error message includes the right span, label, and help text.

### Priority 2 — Editor Support (2-3 days)

NLL files are written by hand. Without syntax highlighting, writing them is
painful. A tree-sitter grammar would enable:

- Syntax highlighting in VS Code, Neovim, Helix, Zed
- Basic error detection in editors
- Folding, indentation, bracket matching

The grammar is simple enough (~30 rules) to write in a day. A VS Code extension
wrapping it takes another day.

### Priority 3 — Test Coverage (1-2 days)

Close the gaps identified in Section 3:
- Parser error case tests
- Render round-trip tests for advanced features
- CLI smoke tests

### Priority 4 — `let` Inside Import Templates (half day)

Currently, `let` bindings inside `site` blocks and imported templates don't
resolve. This forces users to compute addresses in the main file and pass
them as parameters. Supporting `let` in imported scopes would make parametric
templates more self-contained.

### Priority 5 — Structured Logging (1 day)

Replace `eprintln!` calls with `tracing` spans. Add `--verbose` levels
(info/debug/trace) that show deployment progress step by step. This helps
users debug why a deployment failed.

### Priority 6 — Performance (nice to have)

- Parser is already fast (500 nodes < 5s)
- Deploy is IO-bound (netlink calls), not CPU-bound
- No performance issues observed

---

## 8. Comparison with Alternatives

| Feature | nlink-lab | containerlab | netlab | GNS3 |
|---------|-----------|-------------|--------|------|
| **Runtime** | Native namespaces | Docker | Vagrant/libvirt | QEMU/Docker |
| **DSL** | NLL (purpose-built) | YAML | YAML + Jinja2 | GUI |
| **Loops/Variables** | Native | None | Jinja2 | None |
| **IP computation** | `subnet()`/`host()` | Manual | Plugin-based | Manual |
| **Auto-routing** | Built-in BFS | None | Plugin-based | None |
| **TC/netem** | Native | Manual post-deploy | Plugin-based | Limited |
| **nftables** | Native | Manual post-deploy | None | None |
| **NAT** | Native DSL | Manual | Manual | GUI |
| **Fleet deploy** | `for_each` import | None | None | None |
| **CI integration** | JUnit/TAP | Basic | None | None |
| **Wi-Fi emulation** | mac80211_hwsim | None | None | None |
| **Hot reload** | `apply` command | Partial | None | None |

nlink-lab's unique strengths: purpose-built DSL with computed addressing,
native impairment/firewall, fleet templates, and CI-first testing.

---

## 9. Conclusion

nlink-lab is a **mature, feature-rich network lab engine** with comprehensive
DSL support, deep networking control, and solid test coverage. The core is
production-ready.

**Top 3 improvements that would have the most impact:**

1. **Parser error recovery** — report multiple errors per file
2. **Tree-sitter grammar** — syntax highlighting for NLL files
3. **Test coverage for error paths** — parser error cases, render round-trips

No critical features are missing. The project is in a refinement phase where
quality-of-life improvements (better errors, editor support) matter more than
new features.
