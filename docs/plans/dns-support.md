# Plan: DNS Support for nlink-lab

**Date:** 2026-03-30
**Status:** Draft

---

## Problem Statement

Lab nodes cannot resolve each other by name. Users must use raw IP addresses
(`ping 10.0.2.2`) instead of hostnames (`ping server`). Additionally, processes
spawned inside namespaces inherit the host's `/etc/resolv.conf`, which often
points to `127.0.0.53` (systemd-resolved) — unreachable from inside a network
namespace.

## Root Cause Analysis

nlink creates **network namespaces only** (`CLONE_NEWNET`). It does not create
mount namespaces (`CLONE_NEWNS`). This means:

- All namespaces share the host filesystem.
- `/etc/hosts` and `/etc/resolv.conf` are the same for all namespaces and the host.
- Writing per-namespace DNS config requires either:
  - **(a)** Modifying the shared host files (simple but pollutes the host), or
  - **(b)** Adding mount namespace support so each namespace can have its own
    `/etc/hosts` and `/etc/resolv.conf` (proper isolation).

nlink-lab enters namespaces via `setns(CLONE_NEWNET)` in a `pre_exec()` hook
(not `ip netns exec`). The `ip netns exec` convention — which auto-bind-mounts
files from `/etc/netns/<name>/` — does **not** apply.

### Why Mount Namespaces Aren't Needed Today

Everything nlink-lab currently configures is **inherently namespace-aware at the
kernel level** — no mount isolation required:

| What | Mechanism | Why it's per-netns |
|------|-----------|-------------------|
| Interfaces, addresses, routes | Netlink socket bound to namespace | Kernel routes netlink messages to the target namespace |
| Sysctls (`net.ipv4.ip_forward`) | Write to `/proc/sys/net/...` | `/proc/sys/net/` is **virtualized** per network namespace by the kernel |
| nftables rules | Netfilter netlink socket | Per-namespace netfilter tables |
| TC qdiscs | Netlink socket | Per-namespace traffic control |

The sysctl case is worth highlighting: when nlink's thread-based `execute_in()`
calls `setns(CLONE_NEWNET)` and then writes to `/proc/sys/net/ipv4/ip_forward`,
it writes to the **target namespace's** sysctl, not the host's. This works
because procfs is namespace-aware — the kernel returns different contents for
`/proc/sys/net/` depending on which network namespace the reading process is in.

`/etc/hosts` and `/etc/resolv.conf` are **regular files on the real filesystem**.
The kernel does not virtualize them. This is why DNS is the first nlink-lab
feature that requires mount namespace isolation.

### Where Unshare Happens: Child Process, Not Thread

A common concern is that `unshare(CLONE_NEWNS)` from a thread would affect the
entire process. This is true — but nlink's spawn path doesn't use threads for
process creation. The flow is:

```
parent: fork()
  └─ child process: pre_exec() → exec()
```

`pre_exec()` runs in the **forked child process**, not in a parent thread. At
that point the child is fully independent. Calling `unshare(CLONE_NEWNS)` there
creates a private mount namespace for the child only — the parent process and
its threads are completely unaffected.

This is exactly how `ip netns exec` works in iproute2 (`ip/ipnetns.c`):
`fork()` → child calls `setns(CLONE_NEWNET)` + `unshare(CLONE_NEWNS)` +
bind-mounts → `exec()`.

The **thread-based** code path (`execute_in()` / `enter()`) used for sysctl
writes remains unchanged — it only does `setns(CLONE_NEWNET)` and doesn't need
mount namespace tricks because `/proc/sys/net/` is already per-netns.

## Do We Need to Improve nlink First?

**No — this is done.** nlink 0.12.0 (released 2026-03-30) added mount namespace
support in spawn functions. Both Phase 1 and Phase 2 can now be implemented.

nlink 0.12.0 provides `spawn_with_etc()` / `spawn_output_with_etc()` which, in
the forked child's `pre_exec()`:

1. Call `setns(fd, CLONE_NEWNET)` to enter the network namespace
2. Call `unshare(CLONE_NEWNS)` to create a private mount namespace (safe — runs
   in the child process, not a thread)
3. Call `mount("/", MS_SLAVE | MS_REC)` to prevent mount propagation
4. Bind-mount files from `/etc/netns/<ns-name>/` over `/etc/`
5. Remount `/sys`

This mirrors `ip netns exec` behavior (see `ip-netns(8)`) without subprocess
overhead. All bind mount paths are pre-computed before `fork()` to ensure
async-signal-safety.

See `docs/NLINK_FEATURE_REQUEST_MOUNT_NS.md` for the original feature request.

---

## Design: Two Phases

### Phase 1 — Host /etc/hosts Injection (no nlink changes)

**Approach:** Append a managed section to the host's `/etc/hosts` with all
node name→IP mappings. Remove it on lab destroy. This is the same approach
containerlab uses.

**Why this works:** Since all namespaces share the host filesystem, every
namespace sees the host's `/etc/hosts`. `gethostbyname()` and `getaddrinfo()`
use it via nsswitch.

**Format:**
```
###### NLINK-LAB-mylab-START ######
10.0.1.1    router
10.0.1.2    client
10.0.2.2    server
###### NLINK-LAB-mylab-END ######
```

**Multi-homed nodes:** If a node has multiple interfaces with different IPs,
list all of them. The first entry wins for forward lookups, but all are valid.
The node name maps to its "primary" IP (first interface by definition order).
Optionally generate `<node>-<iface>` aliases:
```
10.0.1.1    router router-eth0
10.0.2.1    router-eth1
```

### Phase 2 — Per-Namespace /etc/hosts + /etc/resolv.conf (requires nlink changes)

**Approach:** Create `/etc/netns/<ns-name>/hosts` and
`/etc/netns/<ns-name>/resolv.conf` per namespace. Enhance nlink's spawn
functions to create a mount namespace and bind-mount these files, mirroring
`ip netns exec` behavior.

**Why this is better:**
- No host filesystem pollution
- Per-namespace DNS configuration (different nodes can have different resolvers)
- Supports lab-specific DNS servers (e.g., a dnsmasq node)
- Multiple labs running simultaneously don't conflict

---

## Phase 1 — Detailed Implementation Plan

### 1.1 NLL Syntax

Add an optional `dns` field to the `lab` block:

```nll
lab "example" {
  dns hosts           # default: auto-generate /etc/hosts entries
}
```

For Phase 1, `dns hosts` is the only option. Future phases could add
`dns dnsmasq` or `dns off`. If omitted, behavior is unchanged (no hosts
injection) to avoid surprising existing users.

**Alternative:** Default to `dns hosts` always (opt-out via `dns off`).
Decision: start with explicit opt-in, switch to default-on in a future release
after validation.

#### Types

```rust
// In types.rs
pub enum DnsMode {
    Off,
    Hosts,
}
```

Add `dns: DnsMode` to `LabConfig` (default: `Off`).

#### Parser

In the NLL parser, inside the lab block parsing, recognize `dns` keyword
followed by `hosts` or `off`.

### 1.2 Hosts File Generation

New function in `deploy.rs`:

```rust
fn generate_hosts_entries(topology: &Topology) -> Vec<(String, String)>
```

Returns `Vec<(ip, hostname)>` by iterating over all nodes and their link
endpoints, collecting assigned IP addresses (stripping the prefix length).

**Logic:**
1. For each node, collect all IPs from link endpoints where that node is
   endpoint A or B.
2. First IP encountered becomes the "primary" (maps to bare node name).
3. Additional IPs get `<node>-<iface>` aliases.
4. Skip link-local and unassigned interfaces.

### 1.3 Host /etc/hosts Management

New module: `crates/nlink-lab/src/dns.rs`

```rust
/// Marker lines for managed sections.
const HOSTS_START: &str = "###### NLINK-LAB-{name}-START ######";
const HOSTS_END: &str   = "###### NLINK-LAB-{name}-END ######";

/// Inject lab host entries into /etc/hosts.
pub fn inject_hosts(lab_name: &str, entries: &[(String, String)]) -> Result<()>

/// Remove lab host entries from /etc/hosts.
pub fn remove_hosts(lab_name: &str) -> Result<()>
```

**Safety considerations:**
- Read `/etc/hosts` → check for existing section → replace or append.
- Use atomic write: write to `/etc/hosts.nlink-tmp` → `rename()` over
  `/etc/hosts`. This prevents partial writes on crash.
- File locking: `flock()` on `/etc/hosts.nlink-lock` to prevent concurrent
  deploy/destroy races across labs.
- Permissions: deployer already runs as root (required for namespace creation).

### 1.4 Deploy Integration

Add as **Step 12.5** (after address assignment + routes, before nftables):

```rust
// In deploy()
if topology.lab.dns == DnsMode::Hosts {
    let entries = generate_hosts_entries(&topology);
    dns::inject_hosts(&topology.lab.name, &entries)?;
    cleanup.add_hosts_cleanup(topology.lab.name.clone());
}
```

The cleanup guard ensures hosts entries are removed if deployment fails
partway through.

### 1.5 Destroy Integration

In the destroy path, call `dns::remove_hosts(lab_name)` before removing
namespaces. Also handle `destroy --all` by scanning for all `NLINK-LAB-*`
sections.

### 1.6 State Tracking

Add a `dns_injected: bool` field to `LabState` so that `destroy` knows
whether to clean up `/etc/hosts` even if the topology file is unavailable.

### 1.7 Tests

| Test | Description |
|------|-------------|
| `test_generate_hosts_entries` | Unit test: topology with 3 nodes → correct IP/name pairs |
| `test_generate_hosts_multi_homed` | Unit test: node with 2 interfaces → primary + alias entries |
| `test_inject_hosts` | Unit test: writes section to temp file, verify content |
| `test_inject_hosts_idempotent` | Unit test: inject twice → single section (replaced, not duplicated) |
| `test_remove_hosts` | Unit test: section removed, rest of file preserved |
| `test_remove_hosts_missing` | Unit test: no-op when section doesn't exist |
| `test_dns_hosts_nll_parse` | Parser test: `lab "x" { dns hosts }` parsed correctly |
| `test_dns_off_nll_parse` | Parser test: `lab "x" { dns off }` parsed correctly |
| Integration: `deploy_dns_hosts` | Deploy a lab with `dns hosts`, exec `getent hosts <node>` in a namespace, verify resolution |

### 1.8 File Changes Summary

| File | Change |
|------|--------|
| `crates/nlink-lab/src/types.rs` | Add `DnsMode` enum, `dns` field to `LabConfig` |
| `crates/nlink-lab/src/dns.rs` | **New:** hosts file generation, injection, removal |
| `crates/nlink-lab/src/lib.rs` | Add `mod dns; pub use dns::*;` |
| `crates/nlink-lab/src/deploy.rs` | Add Step 12.5: hosts injection |
| `crates/nlink-lab/src/running.rs` | Call `remove_hosts` in destroy path |
| `crates/nlink-lab/src/state.rs` | Add `dns_injected` to `LabState` |
| `crates/nlink-lab/src/parser/nll/lexer.rs` | Add `Dns` token |
| `crates/nlink-lab/src/parser/nll/ast.rs` | Add `dns` field to lab config AST |
| `crates/nlink-lab/src/parser/nll/parser.rs` | Parse `dns hosts`/`dns off` in lab block |
| `crates/nlink-lab/src/parser/nll/lower.rs` | Lower AST dns field to `DnsMode` |
| `crates/nlink-lab/src/render.rs` | Render `dns hosts` in NLL output |
| `crates/nlink-lab/src/validator.rs` | No changes needed |
| `examples/firewall.nll` | Add `dns hosts` to demonstrate feature |
| `tests/integration.rs` | Add `deploy_dns_hosts` test |

---

## Phase 2 — Per-Namespace Isolation (nlink 0.12.0 — available now)

> **Unblocked:** nlink 0.12.0 shipped the mount namespace spawn functions on
> 2026-03-30. Phase 2 can be implemented immediately.

### 2.1 nlink 0.12.0 API (delivered)

nlink 0.12.0 provides the following functions:

| Function | Description |
|----------|-------------|
| `namespace::spawn_with_etc(name, cmd)` | Spawn with `/etc/netns/<name>/` overlay |
| `namespace::spawn_output_with_etc(name, cmd)` | Same, capture output |
| `namespace::spawn_path_with_etc(path, name, cmd)` | Path-based variant |
| `namespace::spawn_output_path_with_etc(path, name, cmd)` | Path-based, capture output |
| `NamespaceSpec::spawn_with_etc(cmd)` | Integrated method on NamespaceSpec |
| `NamespaceSpec::spawn_output_with_etc(cmd)` | Integrated method on NamespaceSpec |

Key implementation details:
- Pre-computes all bind mount paths **before** `fork()` (async-signal-safe)
- Uses `MS_SLAVE | MS_REC` for mount propagation (matches iproute2)
- Remounts `/sys` inside the namespace
- Skips overlay silently if `/etc/netns/<name>/` doesn't exist (no-op)

### 2.2 nlink-lab Changes

1. **During deploy:** Create `/etc/netns/<prefix>-<node>/hosts` and
   `/etc/netns/<prefix>-<node>/resolv.conf` for each namespace.

2. **Switch spawn calls in NodeHandle:** Use `spawn_with_etc()` /
   `spawn_output_with_etc()` instead of `spawn()` / `spawn_output()` for
   background processes and exec commands. This is a small change in the
   `NodeHandle::spawn()` and `NodeHandle::spawn_output()` methods.

3. **Switch exec calls in RunningLab:** `RunningLab::exec()` uses
   `namespace::spawn_output()` — switch to `spawn_output_with_etc()`.

4. **During destroy:** Remove `/etc/netns/<prefix>-<node>/` directories.

5. **resolv.conf generation:** Configurable upstream DNS:
   ```nll
   lab "example" {
     dns hosts
     dns upstream 8.8.8.8   # or "host" to auto-detect host's upstream
   }
   ```
   Default: detect the host's upstream DNS (parse `/etc/resolv.conf` or
   `/run/systemd/resolve/resolv.conf` to find real nameserver, not 127.0.0.53).

### 2.3 Backward Compatibility

Phase 1 (host /etc/hosts) remains available as a fallback for environments
where mount namespace creation is restricted. The `dns` option could support:
```nll
dns hosts         # Phase 1: host /etc/hosts injection
dns isolated      # Phase 2: per-namespace via mount namespace
dns off           # No DNS configuration
```

---

## Execution Order

| Step | Where | Effort | Blocked? |
|------|-------|--------|----------|
| **1. Phase 1: NLL parser + types** | nlink-lab | Small | No |
| **2. Phase 1: dns.rs module** | nlink-lab | Small | No |
| **3. Phase 1: Deploy/destroy integration** | nlink-lab | Small | No |
| **4. Phase 1: Tests** | nlink-lab | Small | No |
| **5. Phase 2: Per-namespace file generation** | nlink-lab | Small | No (nlink 0.12.0 delivered) |
| **6. Phase 2: Switch spawn/exec to `_with_etc` variants** | nlink-lab | Small | No |
| **7. Phase 2: Destroy cleanup (/etc/netns/ dirs)** | nlink-lab | Small | No |
| **8. Phase 2: Tests** | nlink-lab | Small | No |

Both phases can now be implemented with nlink 0.12.0. They can be shipped
together or incrementally (Phase 1 first for a quick win, Phase 2 as follow-up).

---

## Open Questions

1. **Default behavior:** Should `dns hosts` be the default (opt-out via `dns off`),
   or should it be opt-in? Recommendation: opt-in initially, default-on later.

2. **Multi-homed naming:** Should `router-eth0` aliases be generated automatically,
   or only the bare node name? Recommendation: both — primary IP gets bare name,
   all IPs get `<node>-<iface>` aliases.

3. **IPv6:** Should AAAA-style entries be generated for IPv6 addresses?
   Recommendation: yes, `/etc/hosts` supports IPv6 natively.

4. **Containers:** For container nodes, should we also inject into the container's
   `/etc/hosts`? Docker/podman have `--add-host` flags.
   Recommendation: yes, via `--add-host` at container creation time.

5. **Management network:** If `mgmt` is enabled, should mgmt IPs also appear in
   hosts? Recommendation: no, only data-plane IPs to avoid confusion.
