# nlink Library Gaps Found During nlink-lab Implementation

**Date:** 2026-03-22
**Author:** nlink-lab development team
**nlink version:** `0.9.0` (git: `01d6a5aa`)
**Context:** Building the nlink-lab network lab engine on top of nlink

---

## Executive Summary

During the implementation of nlink-lab (a network lab engine for Linux namespaces),
we found that two features documented in the readiness report as "implemented" do
not exist in the nlink codebase. Additionally, we identified several small API
improvements that would benefit both nlink-lab and other consumers.

**Severity:**
- 2 missing features (sysctl module, namespace process spawning) — **worked around** in nlink-lab
- 3 minor API gaps — **easy fixes**

nlink-lab is fully functional today using workarounds, but upstreaming these would
eliminate duplicated unsafe code and provide a cleaner API for all nlink users.

---

## 1. Missing: Sysctl Module — `sysctl::get/set/set_many`

### What the readiness report says

> **Gap 1: Sysctl Management (commit `e18c602`) — ✅ IMPLEMENTED**
>
> New module: `crates/nlink/src/netlink/sysctl.rs`
>
> ```rust
> sysctl::get("net.ipv4.ip_forward")?;
> sysctl::set("net.ipv4.ip_forward", "1")?;
> namespace::set_sysctls("myns", &[("net.ipv4.ip_forward", "1")])?;
> ```

### What actually exists

**No `sysctl.rs` module exists.** There is no file at `crates/nlink/src/netlink/sysctl.rs`.
The namespace module has no `set_sysctl`, `get_sysctl`, or `set_sysctls` functions.

Verified by:
```
$ find crates/nlink/src -name "sysctl*"
(no results)

$ grep -r "pub fn set_sysctl\|pub fn get_sysctl\|pub fn set_sysctls" crates/nlink/src/
(no results)
```

### How nlink-lab works around it

nlink-lab uses `namespace::execute_in()` + raw `/proc/sys/` filesystem writes:

```rust
// deploy.rs:359-376
namespace::execute_in(ns_name, || {
    for (key, value) in &sysctls {
        let path = format!("/proc/sys/{}", key.replace('.', "/"));
        if let Err(e) = std::fs::write(&path, value) {
            return Err(nlink::Error::InvalidMessage(format!(
                "failed to set sysctl '{key}' = '{value}': {e}"
            )));
        }
    }
    Ok::<(), nlink::Error>(())
})
.map_err(|e| Error::deploy_failed(...))?
.map_err(|e| Error::deploy_failed(...))?;  // double unwrap: outer=setns, inner=closure
```

### Problems with the workaround

1. **No path traversal validation** — `key.replace('.', "/")` could be exploited with
   malicious sysctl keys containing `..`. A proper implementation should validate keys.
2. **Double error unwrapping** — `execute_in` returns `Result<T>` where T is the closure's
   return type, so `Result<Result<(), Error>>` requires two `?` calls.
3. **No read support** — nlink-lab doesn't read sysctls, but other consumers might need it.

### Requested API

```rust
// Module: crates/nlink/src/netlink/sysctl.rs

/// Read a sysctl value in the current namespace.
pub fn get(key: &str) -> Result<String>;

/// Write a sysctl value in the current namespace.
pub fn set(key: &str, value: &str) -> Result<()>;

/// Write multiple sysctl values in the current namespace.
pub fn set_many(pairs: &[(&str, &str)]) -> Result<()>;

// Added to: crates/nlink/src/netlink/namespace.rs

/// Write a sysctl value in a named namespace.
pub fn set_sysctl(name: &str, key: &str, value: &str) -> Result<()>;

/// Read a sysctl value from a named namespace.
pub fn get_sysctl(name: &str, key: &str) -> Result<String>;

/// Write multiple sysctl values in a named namespace.
pub fn set_sysctls(name: &str, pairs: &[(&str, &str)]) -> Result<()>;
```

**Key requirements:**
- Path traversal validation: reject keys containing `..`, `/`, or null bytes
- Synchronous API (filesystem I/O, no benefit from async)
- Trim trailing newlines from `get` return values

---

## 2. Missing: Namespace Process Spawning — `namespace::spawn()`

### What the readiness report says

> **Gap 2: Namespace Process Spawning (commit `e18c602`) — ✅ IMPLEMENTED**
>
> ```rust
> let child = namespace::spawn("myns", cmd)?;
> let output = namespace::spawn_output("myns", cmd)?;
> ```

### What actually exists

**No `spawn` or `spawn_output` functions exist** in the namespace module. The complete
public API of `namespace.rs` is:

```
connection_for, connection_for_path, connection_for_pid
open, open_path, open_pid
enter, enter_path
exists, create, delete
execute_in, execute_in_path
list
```

Verified by:
```
$ grep "pub fn spawn\|pub fn spawn_output" crates/nlink/src/netlink/namespace.rs
(no results)
```

### How nlink-lab works around it

nlink-lab implements namespace process spawning directly using `unsafe` `pre_exec` +
`setns`. This code is **duplicated in 4 places** across `deploy.rs` and `running.rs`:

```rust
// deploy.rs:753-773, deploy.rs:779-796, running.rs:215-237, running.rs:239-258
fn spawn_in_namespace(ns_path: &str, mut cmd: Command) -> Result<Child> {
    use std::os::unix::process::CommandExt;

    let ns_path = ns_path.to_string();
    // SAFETY: pre_exec runs between fork and exec in the child process.
    unsafe {
        cmd.pre_exec(move || {
            let file = std::fs::File::open(&ns_path)?;
            let ret = libc::setns(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::CLONE_NEWNET,
            );
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(...)
}
```

### Problems with the workaround

1. **4 copies of the same unsafe code** — violation of DRY, higher maintenance burden
2. **Manual path construction** — nlink-lab hardcodes `/var/run/netns/{name}` instead of
   using nlink's `NETNS_RUN_DIR` constant
3. **No `NamespaceSpec` integration** — nlink's `NamespaceSpec` enum (Named, Path, Pid)
   would be the natural entry point but has no `spawn` method

### Requested API

```rust
// Added to: crates/nlink/src/netlink/namespace.rs

/// Spawn a process in a named namespace.
///
/// Uses `pre_exec` + `setns` to switch the child process's network namespace
/// between fork and exec. The parent process is never affected.
pub fn spawn(name: &str, cmd: Command) -> Result<Child>;

/// Spawn a process in a named namespace and collect output.
pub fn spawn_output(name: &str, cmd: Command) -> Result<Output>;

/// Spawn a process in a namespace specified by path.
pub fn spawn_path<P: AsRef<Path>>(path: P, cmd: Command) -> Result<Child>;

/// Spawn a process and collect output in a namespace specified by path.
pub fn spawn_output_path<P: AsRef<Path>>(path: P, cmd: Command) -> Result<Output>;

// Added to: NamespaceSpec
impl NamespaceSpec<'_> {
    /// Spawn a process in this namespace.
    pub fn spawn(&self, cmd: Command) -> Result<Child>;

    /// Spawn a process in this namespace and collect output.
    pub fn spawn_output(&self, cmd: Command) -> Result<Output>;
}
```

**Key requirements:**
- Uses `CommandExt::pre_exec()` for the setns call
- `setns()` is async-signal-safe (it's a syscall) — safe in `pre_exec`
- Parent process is never affected (setns happens in child between fork/exec)
- The `Command` parameter should be taken by value (pre_exec consumes it)

---

## 3. Minor: `CtState` Missing `empty()` / `Default`

### Current state

```rust
pub struct CtState(pub u32);

impl CtState {
    pub const INVALID: Self = Self(1);
    pub const ESTABLISHED: Self = Self(2);
    pub const RELATED: Self = Self(4);
    pub const NEW: Self = Self(8);
    pub const UNTRACKED: Self = Self(64);
}
```

### Problem

Building a `CtState` from multiple flags requires starting from zero:

```rust
// What nlink-lab does:
let mut ct = CtState(0);  // Reach into the raw constructor
for state in states.split(',') {
    match state {
        "established" => ct = ct | CtState::ESTABLISHED,
        "related" => ct = ct | CtState::RELATED,
        ...
    }
}
```

### Requested fix

Add either `CtState::empty()` or `impl Default for CtState`:

```rust
impl CtState {
    pub const fn empty() -> Self { Self(0) }
}

// or

impl Default for CtState {
    fn default() -> Self { Self(0) }
}
```

---

## 4. Minor: `Nftables` Protocol Not Re-exported from `nlink` Root

### Current state

`Nftables` is available at `nlink::netlink::Nftables` but not re-exported from
the `nlink` crate root alongside `Route` and `Generic`:

```rust
// nlink/src/lib.rs:
pub use netlink::{Generic, Route};  // Nftables not here

// nlink/src/netlink/mod.rs:
pub use protocol::{..., Nftables, ...};  // Available here
```

### Problem

Users of nlink must write `nlink::netlink::Nftables` instead of `nlink::Nftables`.
`Route` and `Generic` are conveniently at the crate root.

### Requested fix

```rust
// nlink/src/lib.rs:
pub use netlink::{Generic, Nftables, Route};
```

Similarly, `Wireguard` protocol type would benefit from root re-export for the
same reason (needed for `Connection<Wireguard>` when configuring WG devices).

---

## 5. Suggestion: WireGuard Key Generation Helper

### Context

nlink provides full WireGuard device configuration via `Connection<Wireguard>`:

```rust
wg_conn.set_device("wg0", |d| d.private_key(key).listen_port(51820)).await?;
```

But generating keys requires external code. nlink-lab needs this for
`private_key = "auto"` in topology files.

### Suggestion

Add optional key generation to the WireGuard module:

```rust
// crates/nlink/src/netlink/genl/wireguard/keys.rs

/// Generate a WireGuard private key (32 random bytes, clamped per Curve25519).
pub fn generate_private_key() -> [u8; WG_KEY_LEN];

/// Derive a WireGuard public key from a private key (Curve25519 base point multiplication).
pub fn public_key_from_private(private: &[u8; WG_KEY_LEN]) -> [u8; WG_KEY_LEN];
```

This is **not a netlink operation** — it's pure cryptography. It could be:
- Feature-gated (e.g., `wireguard-keygen`)
- In a separate helper module
- Left to consumers (nlink-lab can implement it with the `curve25519-dalek` crate)

No strong opinion here — just flagging that it's a natural companion to the WG
configuration API.

---

## 6. What Works Well

The following nlink APIs were used extensively in nlink-lab and work correctly:

| API | Usage in nlink-lab | Notes |
|-----|-------------------|-------|
| `namespace::create/delete/exists/list` | Lab lifecycle | Solid |
| `namespace::connection_for<P>()` | Per-namespace netlink connections | Works for Route, Nftables, Wireguard |
| `namespace::open()` + `NamespaceFd` | `VethLink::peer_netns_fd()` | Clean API |
| `namespace::execute_in()` | Sysctl workaround | Works, but see gap #1 |
| `VethLink::new().peer_netns_fd()` | Cross-namespace veth pairs | Excellent — no create-then-move needed |
| `BridgeLink::new().vlan_filtering()` | Bridge networks | Clean builder |
| `set_link_master()` | Bridge/VRF enslavement | Works with name or index |
| `add_address_by_index()` | IP address assignment | Namespace-safe |
| `Ipv4Route/Ipv6Route` builders | Route configuration | Rich builder API |
| `NetemConfig` builder | Network impairment | All netem params supported |
| `RateLimiter` | Rate limiting | High-level, easy to use |
| `add_qdisc/change_qdisc` | Runtime impairment modification | Works for netem updates |
| nftables `Chain/Rule` builders | Firewall rules | `match_tcp_dport`, `match_ct_state`, etc. |
| `BridgeVlanBuilder` | Bridge VLAN ports | pvid/untagged/tagged flags |
| `Diagnostics::scan()` | Network health checks | Per-namespace diagnostics |

The overall nlink API design is excellent — typed builders, namespace-aware operations,
comprehensive netlink coverage. The two missing features are the only real gaps.

---

## Summary of Requested Changes

| # | Type | Description | Effort | Impact |
|---|------|-------------|--------|--------|
| 1 | **Missing feature** | `sysctl` module + namespace wrappers | 1 day | Eliminates unsafe workaround, adds input validation |
| 2 | **Missing feature** | `namespace::spawn()` + `spawn_output()` | 1 day | Eliminates 4 duplicated unsafe blocks |
| 3 | Minor | `CtState::empty()` or `Default` impl | 5 min | Ergonomic improvement |
| 4 | Minor | Re-export `Nftables` (and `Wireguard`) from crate root | 5 min | Consistency with `Route`/`Generic` |
| 5 | Suggestion | WireGuard key generation helper | 0.5 day | Nice-to-have, could live in nlink-lab |

**Priority:** #2 (spawn) > #1 (sysctl) > #3-4 (trivial) > #5 (optional)

The spawn functions are the highest priority because they eliminate the most duplicated
unsafe code with the cleanest API improvement.
