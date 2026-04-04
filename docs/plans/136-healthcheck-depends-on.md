# Plan 136: Health Checks and `depends_on` Enforcement

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Medium (1–2 days)
**Priority:** P1 — parsed fields are silently ignored, creating a correctness trap

---

## Problem Statement

NLL parses `healthcheck`, `healthcheck_interval`, `healthcheck_timeout`, `startup_delay`,
and `depends_on` fields on nodes, but they are **completely ignored during deployment**.
All node processes start in parallel with no ordering or readiness checking.

This is worse than "not implemented" — users write `depends_on [mediator]` expecting it
to work, and it silently does nothing. The deployment succeeds, but services may fail
because their dependencies aren't ready.

## Proposed Behaviour

```nll
node mediator {
    run "/usr/bin/mediator --listen 0.0.0.0:15987" background
    healthcheck "bash -c 'echo > /dev/tcp/127.0.0.1/15987'"
    healthcheck_interval 500ms
    healthcheck_timeout 10s
}

node bridge {
    depends_on [mediator]
    run "/usr/bin/bridge --config /etc/bridge.json5" background
    startup_delay 1s
}
```

Deployment should:
1. Create all namespaces, links, addresses, routes, sysctls, firewall (steps 1–14)
2. Topologically sort nodes by `depends_on` (detect cycles → error)
3. For each batch of nodes with satisfied dependencies:
   a. Apply `startup_delay` (sleep before starting)
   b. Spawn background processes
   c. Poll healthcheck until healthy (or timeout → error)
4. Only proceed to next batch after current batch is healthy
5. Return `RunningLab` only when all nodes are healthy (or have no healthcheck)

## Design Decisions

### Topological sort

`depends_on` forms a DAG. Use Kahn's algorithm (BFS-based topo sort) — simple, detects
cycles. Nodes without `depends_on` are in the first batch.

### Healthcheck execution

Run the healthcheck command inside the node's namespace using `exec()`. Poll at
`healthcheck_interval` (default: 1s). Fail after `healthcheck_timeout` (default: 30s).

### Nodes without background processes

If a node has `depends_on` but no `run ... background`, the dependency is satisfied
immediately after the node's network is configured. This supports "wait for networking
to be ready" use cases.

### Nodes without healthcheck but with `run`

If a node spawns processes but has no healthcheck, it's considered "ready" immediately
after processes are spawned. The `startup_delay` still applies.

### Error on cycle

If `depends_on` creates a cycle, fail at validation time (not deploy time). Add a
validator rule.

## Implementation

### Step 1: Validator (`validator.rs`)

Add a cycle detection rule:

```rust
fn validate_depends_on_cycle(topology: &Topology) -> Vec<ValidationError> {
    // Build adjacency list from depends_on
    // Run Kahn's algorithm
    // If not all nodes are visited → cycle exists
    // Return error naming the cycle participants
}
```

Also validate that `depends_on` references existing node names.

### Step 2: Topological sort utility (`deploy.rs`)

```rust
/// Returns nodes grouped by dependency level.
/// Level 0: no dependencies. Level 1: depends only on level 0. Etc.
fn topo_sort_nodes(nodes: &[Node]) -> Result<Vec<Vec<&Node>>> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for node in nodes {
        in_degree.entry(&node.name).or_insert(0);
        for dep in &node.depends_on {
            adj.entry(dep.as_str()).or_default().push(&node.name);
            *in_degree.entry(&node.name).or_insert(0) += 1;
        }
    }

    let mut levels = Vec::new();
    let mut queue: Vec<&str> = in_degree.iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();

    while !queue.is_empty() {
        levels.push(queue.clone());
        let mut next = Vec::new();
        for &n in &queue {
            for &dependent in adj.get(n).unwrap_or(&vec![]) {
                let d = in_degree.get_mut(dependent).unwrap();
                *d -= 1;
                if *d == 0 {
                    next.push(dependent);
                }
            }
        }
        queue = next;
    }

    // Convert node names back to &Node references
    // ...
    Ok(levels)
}
```

### Step 3: Deploy step 16 refactor (`deploy.rs`)

Currently step 16 ("spawn background processes") spawns all processes at once.
Refactor to:

```rust
// ── Step 16: Spawn background processes (dependency-ordered) ──
let levels = topo_sort_nodes(&topology.nodes)?;
for level in &levels {
    for node in level {
        // Apply startup_delay
        if let Some(delay) = &node.startup_delay {
            let dur = parse_duration(delay)?;
            tokio::time::sleep(dur).await;
        }

        // Spawn background processes for this node
        for exec_cfg in &node.exec {
            if exec_cfg.background {
                let pid = namespace::spawn_with_etc(ns_name, &cmd)?;
                pids.push((node.name.clone(), pid));
            }
        }

        // Poll healthcheck
        if let Some(hc) = &node.healthcheck {
            let interval = node.healthcheck_interval
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(Duration::from_secs(1));
            let timeout = node.healthcheck_timeout
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(Duration::from_secs(30));

            let deadline = Instant::now() + timeout;
            loop {
                let probe = namespace::spawn_output_with_etc(ns_name, &parse_cmd(hc))?;
                if probe.exit_code == 0 {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(Error::deploy_failed(format!(
                        "healthcheck timeout for node '{}': {hc}", node.name
                    )));
                }
                tokio::time::sleep(interval).await;
            }
        }
    }
}
```

### Step 4: Parallel within levels

Nodes within the same dependency level can be started in parallel using `tokio::join!`
or `futures::future::join_all`. This maximizes throughput while respecting ordering.

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_validate_depends_on_cycle` | validator.rs | Cycle detected → error |
| `test_validate_depends_on_unknown_node` | validator.rs | Reference to non-existent node → error |
| `test_topo_sort_linear` | deploy.rs | A→B→C produces 3 levels |
| `test_topo_sort_diamond` | deploy.rs | A→{B,C}→D produces 3 levels |
| `test_topo_sort_no_deps` | deploy.rs | All nodes in level 0 |
| `test_deploy_healthcheck_pass` | integration.rs | Healthcheck succeeds → deploy completes |
| `test_deploy_healthcheck_timeout` | integration.rs | Healthcheck fails → deploy errors |
| `test_deploy_depends_on_ordering` | integration.rs | Dependent node starts after dependency |
| `test_deploy_startup_delay` | integration.rs | Delay is respected before process spawn |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `validator.rs` | +40 | Cycle detection + unknown node check |
| `deploy.rs` | +80 | Topo sort + ordered spawn + healthcheck polling |
| Tests | +80 | 9 test functions |
| **Total** | ~200 | |
