# Plan 127: Glob Patterns in Network Member Lists

**Date:** 2026-04-01
**Status:** Ready
**Effort:** Medium (1 day)
**Priority:** P2 — network membership auto-adapts to fleet size

---

## Problem Statement

Network member lists must enumerate every endpoint. Adding a new drone
to the fleet requires updating every modem network's member list:

```nll
network fo {
  members [c2-fw:fo, a18-black:fo, a19-black:fo, a9-cc:fo, a10-cc:fo]
}
```

## Proposed Syntax

### Glob patterns

```nll
network fo {
  members [c2-fw:fo, *-black:fo, *-cc:fo]
  subnet 172.100.1.0/24
}
```

`*-black:fo` matches any node whose name ends with `-black`, interface `fo`.
The `*` matches any prefix.

### Supported patterns

| Pattern | Matches |
|---------|---------|
| `*-black:fo` | `a18-black:fo`, `a19-black:fo`, `xyz-black:fo` |
| `*-cc:wifi` | `a9-cc:wifi`, `a10-cc:wifi` |
| `c2-*:eth0` | `c2-fw:eth0`, `c2-dcs:eth0` |
| `*:lo` | All nodes' loopback interfaces |

## Implementation

### Lowerer

During network lowering, after all nodes are defined, resolve glob patterns:

```rust
fn resolve_glob_members(
    members: &[String],
    all_nodes: &HashMap<String, Node>,
) -> Vec<String> {
    let mut resolved = Vec::new();
    for member in members {
        if let Some((node_pattern, iface)) = member.split_once(':') {
            if node_pattern.contains('*') {
                // Glob: match against all node names
                let regex = glob_to_regex(node_pattern);
                for node_name in all_nodes.keys() {
                    if regex.is_match(node_name) {
                        resolved.push(format!("{node_name}:{iface}"));
                    }
                }
            } else {
                resolved.push(member.clone());
            }
        } else {
            resolved.push(member.clone());
        }
    }
    resolved
}

fn glob_to_regex(pattern: &str) -> Regex {
    let escaped = regex::escape(pattern).replace(r"\*", ".*");
    Regex::new(&format!("^{escaped}$")).unwrap()
}
```

**No regex crate needed** — simple `*` matching can be done with
`str::split('*')` and prefix/suffix checks:

```rust
fn glob_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') { return pattern == name; }
    let parts: Vec<&str> = pattern.split('*').collect();
    match parts.len() {
        2 => name.starts_with(parts[0]) && name.ends_with(parts[1]),
        _ => false,  // only single * supported
    }
}
```

### Ordering

Glob resolution must happen AFTER all imports are processed (so imported
nodes exist in the node list). The current lowering order is:
1. Imports → adds prefixed nodes
2. Local nodes
3. Networks (with member resolution)

This order already works — networks are lowered after nodes.

## Tests

| Test | Description |
|------|-------------|
| `test_glob_member_suffix` | `*-black:fo` matches `a18-black`, `a19-black` |
| `test_glob_member_prefix` | `c2-*:eth0` matches `c2-fw`, `c2-dcs` |
| `test_glob_no_match` | `*-nonexistent:fo` produces empty list (warning) |
| `test_glob_mixed` | Mix of literal and glob members |
| `test_glob_with_imports` | Glob resolves imported node names |

## Documentation Updates

| File | Change |
|------|--------|
| **README.md** | Add glob pattern example in network section |
| **CLAUDE.md** | Mention glob patterns in NLL features |
| **NLL_DSL_DESIGN.md** | Add glob syntax to network_item grammar |
| **examples/infra-c2-a18-a9.nll** | Convert member lists to use globs |

## File Changes

| File | Change |
|------|--------|
| `lower.rs` | Add `resolve_glob_members()` during network lowering |
| `render.rs` | Render glob patterns as-is (they're strings in the AST) |
| `validator.rs` | Warn on glob patterns that match zero nodes |
