# nlink-lab Deep Analysis Report

**Date:** 2026-03-28
**Scope:** Full codebase review — code quality, feature gaps, nlink dependency, improvement roadmap

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Code Quality Issues](#2-code-quality-issues)
3. [NLL Parser & DSL Gaps](#3-nll-parser--dsl-gaps)
4. [Validator & Deploy Improvements](#4-validator--deploy-improvements)
5. [CLI & UX Gaps](#5-cli--ux-gaps)
6. [Test Coverage Gaps](#6-test-coverage-gaps)
7. [nlink Dependency Assessment](#7-nlink-dependency-assessment)
8. [Feature Ideas](#8-feature-ideas)
9. [Missing Examples & Documentation](#9-missing-examples--documentation)
10. [Prioritized Roadmap](#10-prioritized-roadmap)

---

## 1. Executive Summary

nlink-lab is a well-architected network lab engine with solid Phase 2-3 implementation.
The TOML/NLL parser, 18-step deployer, 14-rule validator, and CLI are all functional
and well-tested. The nlink dependency is production-ready with 100% API coverage.

**Strengths:** Clean architecture, good separation of concerns, comprehensive type system,
solid integration test framework, dual format support (TOML + NLL).

**Primary concerns:** Edge-case handling in deploy, incomplete NLL features (image/cmd,
ICMP firewall), no feature flags, testing gaps in advanced topologies, and a few
panic-risk sites.

---

## 2. Code Quality Issues

### 2.1 Panic Risks (High Priority)

| Location | Issue | Fix |
|----------|-------|-----|
| `deploy.rs:1320` | `unwrap()` on `/dev/urandom` open | Use `getrandom` crate or return `Result` |
| `deploy.rs:250` | Direct `.as_raw_fd()` without validation | Validate FD before use |

### 2.2 State Persistence (Medium Priority)

- **Non-atomic writes** (`state.rs:85`): `save()` writes directly to the state file.
  A crash mid-write corrupts the file. Use temp-file + rename pattern.
- **No file locking** for concurrent lab instances — two deploys with the same name
  could race on the state file.
- **Fallback to `/tmp`** when XDG vars not set (`state.rs:60-72`) — state lost on reboot.
  Should prefer `$HOME/.local/state/nlink-lab/` instead.

### 2.3 Error Handling Gaps

- `Error::Nlink(#[from] nlink::Error)` (`error.rs:25`) is a catch-all that hides
  specifics. Consider stratifying into `LinkError`, `RouteError`, `NamespaceError`.
- Missing error variants: `AlreadyRunning`, `TimeoutError`, `PartialDeployFailure`.
- Container exec path in `running.rs:117-132` uses `DeployFailed` for exec errors —
  should have a dedicated `ExecFailed` variant.

### 2.4 Type Safety

- Interface `kind` is `Option<String>` in `InterfaceConfig` — should be an enum:
  `{Dummy, Vxlan, Vlan, Bond, Wireguard, Loopback}`.
- `EndpointRef::parse()` returns `Option` — should return `Result<EndpointRef, ParseError>`
  with context.
- Builder accepts any string for interface kinds — no compile-time validation.

### 2.5 Dead Code & Warnings

Pre-existing compiler warnings (visible in build output):

- `deploy.rs:16` — unused import `self` in container module
- `running.rs:8` — unused import `Severity`
- `deploy.rs:28` — field `name` never read in `Container` variant
- `deploy.rs:77` — method `ns_name()` never used
- `error.rs:73-88` — several fields assigned but never read in `NllDiagnostic`

### 2.6 Cleanup & Destroy Robustness

- `running.rs:264-304`: `destroy()` uses best-effort cleanup with silent failures.
  Should log warnings via `tracing::warn!` when cleanup steps fail.
- `running.rs:254-261`: `kill_process()` doesn't verify the PID belongs to this lab —
  could kill an unrelated process if PID was recycled.
- No check that a lab isn't running before `state::remove()`.

---

## 3. NLL Parser & DSL Gaps

### 3.1 Bugs

| Location | Bug | Impact |
|----------|-----|--------|
| `parser.rs:154` | `parse_name()` accepts bare `Int` tokens | `node 123 { }` parses as valid |
| `lower.rs:642-653` | Rate limiting only applied to left endpoint | Right endpoint rate silently ignored |
| `nll/mod.rs:31` | No-op `replace(\|_\| false, "")` | Dead code in error message cleanup |
| `parser.rs:878-883` | Extra address pairs in link block silently dropped | Data loss without warning |
| `lower.rs:201-204` | Division by zero in interpolation returns raw `${N}` | Fails later with confusing error |

### 3.2 Missing NLL Features (Spec vs Implementation)

| Feature | Spec | Implementation | Status |
|---------|------|----------------|--------|
| `image`/`cmd` on nodes | Defined | Parsed but **not lowered** to Topology | Incomplete |
| `cmd` as string list | `cmd ["sh", "-c", "..."]` | Only accepts single string | Incomplete |
| ICMP firewall rules | Implied by spec | Only `ct`, `tcp`, `udp` match types | Missing |
| IP header matching | `ip saddr/daddr` | Not supported | Missing |
| Variable loop bounds | `for i in 1..${N}` | Only integer literals | Missing |
| String escaping | `\"` in strings | Not supported | Missing |
| Multi-variable expressions | `${i * 2 + j}` | Only single binary ops | Missing |
| Interpolation without spaces | `${i+1}` | Requires spaces: `${i + 1}` | Inconsistent |

### 3.3 Parser Quality

- **No error recovery**: Parser stops at first error. Cannot report multiple errors
  in one pass. Typical for recursive descent but limits UX.
- **Error messages lack context**: `"expected statement"` should list valid keywords.
- **UTF-8 column counting**: `lexer.rs:272` uses byte offsets — multi-byte characters
  produce wrong column numbers in diagnostics.
- **Duplicate names**: Profile/node names silently overwrite. Should warn or error.

---

## 4. Validator & Deploy Improvements

### 4.1 Validator Gaps

**Missing validation rules:**

- **Subnet overlap detection**: `10.0.0.0/24` and `10.0.0.128/25` overlap but aren't flagged.
- **WireGuard peer validation**: Peer node references aren't checked in validator.
- **VRF table ID uniqueness**: Duplicate table IDs across VRFs aren't detected.
- **Interface name length**: Linux limits interface names to 15 characters.
- **VLAN ID bounds in types**: Only validated in validator, not at parse time.
- **Container image format**: Empty image strings not caught early.
- **Duplicate link endpoints**: Same `node:iface` pair used twice.

**Logic issues:**

- `validator.rs:740`: Route reachability check skips nodes with no subnets (empty subnet
  list) — should still warn about unreachable gateways.
- `validator.rs:757`: Unreferenced node check doesn't warn about nodes with only
  explicit interfaces (which might be orphaned).

### 4.2 Deployer Improvements

**Correctness:**

- `deploy.rs:237-241`: Peer name truncation to 15 chars is silent. Names could collide
  after truncation. Should warn or error.
- `deploy.rs:752-756`: WireGuard peer resolution assumes first matching WG interface
  on peer node. Ambiguous if multiple WG interfaces exist.
- `deploy.rs:763`: `find_peer_endpoint()` returns any address — should prefer the
  correct routing domain address.
- `deploy.rs:1104-1142`: `apply_match_expr()` only handles 3 expression types. Unknown
  expressions log a warning but silently drop the rule — could unintentionally allow traffic.

**Robustness:**

- No health checks between deployment steps (e.g., verify namespace can run commands
  before proceeding to link creation).
- No partial rollback — currently all-or-nothing. A failure at step 14 (TC qdiscs)
  rolls back everything including successfully created namespaces.
- No timeout handling for network operations (WireGuard setup, nftables application).

**Performance:**

- Creates separate nlink connections per node per step instead of reusing.
- No parallelization of namespace creation or veth pair setup — could be concurrent
  since namespaces are independent.

### 4.3 Hand-Rolled ISO 8601 Implementation

`deploy.rs:1407-1438` contains a manual date/time formatting algorithm. Should use the
`time` or `chrono` crate to avoid potential edge cases (leap years, timezone handling).

---

## 5. CLI & UX Gaps

### 5.1 Missing Commands

| Command | Description | Value |
|---------|-------------|-------|
| `nlink-lab export` | Dump running lab state as TOML/NLL for reproduction | High |
| `nlink-lab diff` | Compare running lab vs topology file (drift detection) | High |
| `nlink-lab wait` | Block until lab is ready (useful in scripts) | Medium |
| `nlink-lab logs` | View real-time netlink events for a lab | Medium |
| `nlink-lab clone` | Duplicate a running lab with different name | Low |

### 5.2 UX Improvements

- **No shell completions**: clap supports generating bash/zsh/fish completions but
  it's not wired up. Easy win.
- **`capture` command**: No BPF filter support, no rotation, no format selection.
- **`impair` command**: No preview/dry-run mode, no way to see current impairments
  before modifying.
- **No `--dry-run` flag on `deploy`**: Would show what would be created without
  actually deploying.
- **No `--json` output**: Status, diagnose, and ps commands only output human-readable
  tables. Machine-readable JSON output would enable scripting.

---

## 6. Test Coverage Gaps

### 6.1 Integration Tests Missing

| Test | Topology | Verifies |
|------|----------|----------|
| `vrf_isolation` | vrf-multitenant | Cross-VRF traffic **fails** (not just same-VRF works) |
| `firewall_enforcement` | firewall | Blocked traffic actually drops |
| `rate_limit_applied` | iperf-benchmark | iperf3 throughput within expected range |
| `vxlan_tunnel` | vxlan-overlay | Overlay connectivity works |
| `container_deploy` | container | Container node starts and is reachable |
| `deploy_destroy_cleanup` | simple | After destroy, no namespaces/veths remain |
| `concurrent_labs` | simple × 2 | Two labs coexist without conflicts |
| `impairment_modification` | simple | Runtime `set_impairment()` changes effective delay |
| `ipv6_connectivity` | (new) | IPv6 ping between nodes |
| `large_topology` | (new: 50+ nodes) | Stress test for deployment performance |

### 6.2 Unit Test Gaps

- No tests for `apply_match_expr()` in deploy.rs (firewall rule parsing).
- No tests for state file corruption recovery.
- No tests for concurrent state file access.
- No tests for WireGuard peer resolution edge cases.
- Builder DSL has no test for invalid inputs (expected errors).

---

## 7. nlink Dependency Assessment

### 7.1 Current Status

- **Source:** `git = "https://github.com/p13marc/nlink"` with `features = ["full"]`
- **Blocker:** Git dependency prevents publishing nlink-lab to crates.io.
- **API coverage:** 100% — all needed primitives are available.
- **Readiness report:** All 5 identified gaps resolved as of 2026-03-22.

### 7.2 nlink Improvements That Would Benefit nlink-lab

**High value:**

| Improvement | Benefit for nlink-lab |
|-------------|----------------------|
| **Qdisc query API** (`get_qdisc()`) | `running.rs:182-189` currently tries `change_qdisc()` then falls back to `add_qdisc()`. A query would make this clean. |
| **Rule batching** for nftables | `apply_firewall()` could apply all rules atomically instead of one-at-a-time. |
| **Link query by name** (`get_link_by_name()`) | Avoid filtering `get_links()` in-memory. |
| **Route query API** (`get_routes()`) | Enable deployment verification and diagnostics. |

**Medium value:**

| Improvement | Benefit |
|-------------|---------|
| Higher-level firewall DSL | Eliminate custom `apply_match_expr()` parser |
| TC statistics streaming | Real-time monitoring of lab network health |
| Namespace event monitoring | Track lab state changes without polling |
| Transaction/rollback helper | Simplify deploy.rs cleanup logic |

### 7.3 Path to crates.io

To publish nlink-lab, nlink must first be published:
1. Publish `nlink` to crates.io with stable API
2. Switch nlink-lab from `git` to `version` dependency
3. Add feature flags (see §8.3)
4. Publish `nlink-lab` and `nlink-lab-macros`

---

## 8. Feature Ideas

### 8.1 High-Value Features

**Topology snapshots & diff:**
Deploy a topology, make runtime changes (impairments, routes), then `nlink-lab export`
to capture the current state. `nlink-lab diff` compares running state vs. original file.
Enables "configuration drift detection" for long-running labs.

**Hot-reload / apply:**
`nlink-lab apply topology.toml` — reconcile a running lab with an updated topology file.
Only create/destroy what changed. Massively speeds up iteration during development.

**Topology composition / imports:**
```nll
import "base-dc.nll" as dc
import "wan-overlay.nll" as wan

link dc.spine1:wan0 -- wan.pe1:eth0 { ... }
```
Build complex topologies from reusable modules. Essential for large-scale labs.

**IPv6 support in examples & tests:**
Currently all examples are IPv4-only. Add dual-stack and IPv6-only examples.

### 8.2 Medium-Value Features

**Dry-run mode:**
Show what would be created without root access. Useful for CI validation, documentation,
and debugging topology files.

**Machine-readable output (JSON):**
`nlink-lab status --json`, `nlink-lab diagnose --json`. Enables integration with
monitoring tools, dashboards, and CI pipelines.

**Lab checkpoints:**
Save/restore lab state at a point in time. Useful for testing: deploy, run test A,
restore checkpoint, run test B — without full redeploy.

**ECMP / multipath routes:**
Currently routes support single `via` gateway. Add support for equal-cost multipath:
```toml
[nodes.router.routes]
"10.0.0.0/8" = { ecmp = ["10.1.0.1", "10.2.0.1"] }
```

### 8.3 Cargo Feature Flags

Currently all features compile unconditionally. Suggested flags:

```toml
[features]
default = ["containers", "nftables", "tc", "wireguard"]
containers = []     # Docker/Podman node support
nftables = []       # Firewall rules
tc = []             # Traffic control (netem, HTB)
wireguard = []      # WireGuard VPN interfaces
vxlan = []          # VXLAN overlays
nll = ["dep:logos"]  # NLL DSL parser (TOML always available)
diagnostics = []    # Health check engine
full = ["containers", "nftables", "tc", "wireguard", "vxlan", "nll", "diagnostics"]
```

This allows minimal builds for embedded/constrained environments and speeds up
compilation when not all features are needed.

---

## 9. Missing Examples & Documentation

### 9.1 Topology Patterns Not Demonstrated

| Pattern | Priority | Notes |
|---------|----------|-------|
| Bond interface with failover | High | Types exist, no example |
| IPv6-only topology | High | Needed for modern networking |
| Dual-stack (IPv4 + IPv6) | High | Common real-world pattern |
| Asymmetric impairments (`->` / `<-`) | Medium | NLL supports it, no example |
| ECMP / multipath routing | Medium | Not yet supported |
| Multi-bridge topology | Medium | Multiple L2 segments interconnected |
| GRE / IP-in-IP tunnels | Low | Design doc mentions, not implemented |
| Bridge STP | Low | Spanning tree protocol |

### 9.2 Documentation Gaps

| Document | Priority | Description |
|----------|----------|-------------|
| Troubleshooting guide | High | Common errors, kernel requirements, permission issues |
| Topology cookbook | High | Advanced patterns with explanations |
| CI integration guide | Medium | GitHub Actions / GitLab CI examples for `#[lab_test]` |
| Container node docs | Medium | Docker/Podman support is undocumented |
| Performance guide | Low | Node limits, deployment benchmarks, kernel tuning |
| Contributing guide | Low | How to add new interface types, extend the parser |

---

## 10. Prioritized Roadmap

### Tier 1: Bug Fixes & Safety (1-2 days)

- [ ] Replace `/dev/urandom` unwrap with `getrandom` crate
- [ ] Fix rate limiting to apply to both endpoints in NLL lowering
- [ ] Fix `parse_name()` to reject bare integer tokens
- [ ] Atomic state file writes (temp + rename)
- [ ] Clean up compiler warnings (dead code, unused imports)
- [ ] Fix no-op `replace()` in NLL diagnostic module

### Tier 2: Code Quality (2-3 days)

- [ ] Add interface kind enum (replace `Option<String>`)
- [ ] Improve error types (stratify nlink errors, add `ExecFailed`)
- [ ] Add `tracing::warn!` to cleanup/destroy silent failures
- [ ] PID ownership validation in `kill_process()`
- [ ] Use `time` crate for ISO 8601 formatting
- [ ] Builder validation via `finalize()` method

### Tier 3: Completeness (3-5 days)

- [ ] Implement `image`/`cmd` NLL lowering
- [ ] Add ICMP and IP header matching to firewall rules
- [ ] Add shell completions (bash/zsh/fish)
- [ ] Add `--json` output to status, diagnose, ps commands
- [ ] Add `--dry-run` to deploy command
- [ ] Add missing integration tests (VRF isolation, firewall enforcement, VXLAN)
- [ ] Add bond and IPv6 example topologies

### Tier 4: New Features (1-2 weeks)

- [ ] `nlink-lab export` — dump running lab as TOML
- [ ] `nlink-lab diff` — drift detection
- [ ] Topology composition / imports in NLL
- [ ] Hot-reload / apply (reconcile running lab with updated topology)
- [ ] Cargo feature flags
- [ ] Parallel namespace creation in deployer

### Tier 5: nlink Upstream (Separate effort)

- [ ] Publish nlink to crates.io
- [ ] Add `get_qdisc()` query API
- [ ] Add nftables rule batching
- [ ] Add `get_routes()` / `get_link_by_name()` query APIs
- [ ] Add TC statistics streaming

---

*This report was generated from a full read of every source file in the repository,
cross-referenced against the NLL DSL specification and nlink readiness report.*
