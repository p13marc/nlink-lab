# Plan 105: DNS Phase 2 — Per-Namespace Isolation

**Date:** 2026-03-30
**Status:** Implemented (2026-03-30)
**Effort:** Small (1-2 hours)
**Depends on:** nlink 0.12.0 (delivered)

---

## Problem Statement

DNS Phase 1 (host `/etc/hosts` injection) works for bare namespaces but is a
shared-file approach. Phase 2 creates per-namespace `/etc/hosts` and
`/etc/resolv.conf` files using nlink 0.12.0's mount namespace spawn functions.

Currently, `RunningLab::exec()` and `RunningLab::spawn()` still use
`namespace::spawn_output()` / `namespace::spawn()` instead of the `_with_etc`
variants. This means even if per-namespace files are created in
`/etc/netns/<name>/`, they won't be bind-mounted for spawned processes.

## Changes Required

### 1. Create per-namespace files during deploy

In `deploy.rs`, after Step 15b (host `/etc/hosts` injection), add Step 15c:

```rust
// ── Step 15c: Create per-namespace /etc/netns/ files ──────────
if topology.lab.dns == DnsMode::Hosts {
    for (node_name, _) in &topology.nodes {
        if node.image.is_some() { continue; } // containers use --add-host
        let ns_name = &namespace_names[node_name];
        let dir = format!("/etc/netns/{ns_name}");
        std::fs::create_dir_all(&dir)?;
        // Write hosts file
        let mut content = String::new();
        content.push_str("127.0.0.1\tlocalhost\n::1\t\tlocalhost\n");
        for entry in &entries {
            content.push_str(&entry.ip);
            for name in &entry.names {
                content.push('\t');
                content.push_str(name);
            }
            content.push('\n');
        }
        std::fs::write(format!("{dir}/hosts"), &content)?;
        // Write resolv.conf with host's upstream DNS
        let upstream = detect_upstream_dns();
        std::fs::write(format!("{dir}/resolv.conf"),
            format!("nameserver {upstream}\n"))?;
    }
}
```

### 2. Switch spawn/exec to `_with_etc` variants

**File:** `crates/nlink-lab/src/running.rs`

In `exec()` (bare namespace branch):
```rust
// Before:
namespace::spawn_output(ns_name, command)
// After:
namespace::spawn_output_with_etc(ns_name, command)
```

In `spawn()` (bare namespace branch):
```rust
// Before:
namespace::spawn(ns_name, command)
// After:
namespace::spawn_with_etc(ns_name, command)
```

### 3. Clean up /etc/netns/ dirs on destroy

In `RunningLab::destroy()`, after removing namespaces:
```rust
// Remove /etc/netns/ directories
for ns_name in self.namespace_names.values() {
    let dir = format!("/etc/netns/{ns_name}");
    let _ = std::fs::remove_dir_all(&dir);
}
```

### 4. Detect upstream DNS

New helper function:
```rust
fn detect_upstream_dns() -> String {
    // Try systemd-resolved's upstream config first
    if let Ok(content) = std::fs::read_to_string("/run/systemd/resolve/resolv.conf") {
        if let Some(ns) = parse_nameserver(&content) {
            return ns;
        }
    }
    // Fall back to /etc/resolv.conf, skipping 127.0.0.53
    if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf") {
        if let Some(ns) = parse_nameserver(&content) {
            return ns;
        }
    }
    "8.8.8.8".to_string() // last resort
}
```

### 5. Tests

| Test | Description |
|------|-------------|
| `test_exec_with_etc_overlay` | Integration: deploy with `dns hosts`, exec `cat /etc/hosts` in namespace, verify lab entries |
| `test_exec_resolv_conf` | Integration: exec `cat /etc/resolv.conf`, verify non-127.0.0.53 nameserver |
| `test_detect_upstream_dns` | Unit: mock resolv.conf parsing |
| `test_netns_cleanup` | Integration: after destroy, `/etc/netns/<name>/` removed |

### File Changes

| File | Change |
|------|--------|
| `deploy.rs` | Add Step 15c: create `/etc/netns/` files, add `detect_upstream_dns()` |
| `running.rs` | Switch `spawn`/`exec` to `_with_etc` variants, clean up dirs on destroy |
| `dns.rs` | Add `detect_upstream_dns()` and `parse_nameserver()` helpers |
