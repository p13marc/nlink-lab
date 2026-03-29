# Plan 096: Container Lifecycle — Health Checks, Config Injection, Dependencies

**Priority:** Medium
**Effort:** 3-4 days
**Depends on:** Plan 095 (needs container property plumbing)
**Target:** `crates/nlink-lab/src/`

## Summary

Advanced container lifecycle features: health checks with readiness polling,
config file injection (individual files and directory overlays), env-file
support, and dependency ordering between nodes.

---

## Phase 1: Health Checks (day 1-2)

### Problem

No way to know when a container is ready. Tests fail intermittently because
they run before services finish starting. The existing `nlink-lab wait` command
just checks if the lab state file exists — it doesn't check service health.

### Syntax

```nll
# Command-based health check
node router image "frr:latest" {
    healthcheck "pgrep zebra" {
        interval 2s
        timeout 30s
        retries 5
    }
}

# Simple startup delay (no command, just wait)
node slow-boot image "junos" {
    startup-delay 15s
}

# TCP port check shorthand
node web image "nginx" {
    healthcheck-tcp 80 {
        interval 1s
        timeout 10s
    }
}
```

### Deployment behavior

During deployment, after creating a container and its links:

1. If `startup-delay` is set, sleep for that duration.
2. If `healthcheck` is set, poll the command until it succeeds or timeout:
   ```
   loop {
       let result = runtime.exec(container_id, &health_cmd);
       if result.status.success() { break; }
       if elapsed > timeout { return Err("health check timed out"); }
       sleep(interval);
   }
   ```
3. If `healthcheck-tcp` is set, poll with `nc -z localhost <port>`.
4. Only after health check passes, proceed to next deployment step.

### Implementation

**Lexer**: Add tokens: `Healthcheck`, `HealthcheckTcp`, `StartupDelay`,
`Interval`, `Timeout`, `Retries`.

**AST** (`NodeDef`): Add fields:

```rust
pub healthcheck: Option<HealthcheckDef>,
pub startup_delay: Option<String>,  // duration literal
```

New AST struct:

```rust
pub struct HealthcheckDef {
    pub cmd: Option<String>,        // exec command
    pub tcp_port: Option<u16>,      // TCP port to check
    pub interval: Option<String>,   // polling interval (default: 2s)
    pub timeout: Option<String>,    // total timeout (default: 30s)
    pub retries: Option<u32>,       // max retries (default: 10)
}
```

**Types** (`Node`): Add matching fields with serde support.

**Deploy** (`deploy.rs`): After container creation and link setup, add
health check polling loop:

```rust
// After all containers are created and linked (before Step 16)
for (node_name, node) in &topology.nodes {
    if node.image.is_none() { continue; }

    if let Some(delay) = &node.startup_delay {
        let dur = parse_duration(delay)?;
        tracing::info!("waiting {delay} for '{node_name}' startup");
        tokio::time::sleep(dur).await;
    }

    if let Some(hc) = &node.healthcheck {
        let interval = parse_duration(hc.interval.as_deref().unwrap_or("2s"))?;
        let timeout = parse_duration(hc.timeout.as_deref().unwrap_or("30s"))?;
        let start = std::time::Instant::now();

        tracing::info!("waiting for '{node_name}' health check");
        loop {
            let cmd = if let Some(ref cmd) = hc.cmd {
                cmd.clone()
            } else if let Some(port) = hc.tcp_port {
                format!("nc -z localhost {port}")
            } else {
                break;
            };

            let container = &node_handles[node_name];
            let result = container.spawn_output(
                std::process::Command::new("sh").arg("-c").arg(&cmd)
            );
            if result.is_ok_and(|o| o.status.success()) {
                tracing::info!("'{node_name}' is healthy");
                break;
            }
            if start.elapsed() > timeout {
                return Err(Error::deploy_failed(format!(
                    "health check timed out for '{node_name}' after {timeout:?}"
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }
}
```

**Parser**: Add match arms in `parse_node()` block:

```rust
Some(Token::Healthcheck) => {
    *pos += 1;
    healthcheck = Some(parse_healthcheck(tokens, pos)?);
}
Some(Token::HealthcheckTcp) => {
    *pos += 1;
    let port = expect_int(tokens, pos)? as u16;
    let props = if check(tokens, *pos, &Token::LBrace) {
        parse_healthcheck_props(tokens, pos)?
    } else {
        HealthcheckProps::default()
    };
    healthcheck = Some(HealthcheckDef { cmd: None, tcp_port: Some(port), ..props });
}
Some(Token::StartupDelay) => {
    *pos += 1;
    startup_delay = Some(parse_value(tokens, pos)?);
}
```

### Tasks

- [ ] Add tokens to lexer
- [ ] Add `HealthcheckDef` AST struct
- [ ] Add healthcheck/startup_delay to NodeDef
- [ ] Implement `parse_healthcheck()` and `parse_healthcheck_props()`
- [ ] Add fields to Node types
- [ ] Implement polling loop in deploy.rs
- [ ] Update `nlink-lab wait` to check health status
- [ ] Add validation: healthcheck requires image
- [ ] Tests: healthcheck parse, startup-delay parse, timeout behavior

## Phase 2: Config File Injection (day 2-3)

### Problem

Configuring services inside containers requires listing individual volume
mounts or rebuilding images. Two patterns from competitors solve this:
containerlab's `startup-config` and Kathara's overlay directories.

### Syntax

```nll
# Individual file mounts (read-only)
node router image "frr:latest" {
    config "configs/frr.conf" "/etc/frr/frr.conf"
    config "configs/daemons" "/etc/frr/daemons"
}

# Directory overlay (Kathara-style)
node router image "frr:latest" {
    overlay "configs/router/"
}
```

With `overlay`, a file at `configs/router/etc/frr/frr.conf` appears at
`/etc/frr/frr.conf` inside the container. The directory structure is
mirrored into the container root.

### Implementation

**Lexer**: Add `Config`, `Overlay` tokens.

**AST** (`NodeDef`): Add:

```rust
pub configs: Vec<(String, String)>,  // (host_path, container_path)
pub overlay: Option<String>,         // overlay directory path
```

**Parser**: Add match arms:

```rust
Some(Token::Config) => {
    *pos += 1;
    let host = expect_string(tokens, pos)?;
    let container = expect_string(tokens, pos)?;
    configs.push((host, container));
}
Some(Token::Overlay) => {
    *pos += 1;
    overlay = Some(expect_string(tokens, pos)?);
}
```

**Types** (`Node`): Add matching fields.

**Container** (`CreateOpts`): Expand volumes with config/overlay entries:

```rust
// In deploy.rs, when building CreateOpts:
let mut all_volumes = node.volumes.clone().unwrap_or_default();

// Add individual configs as read-only bind mounts
for (host, container) in &node.configs {
    let abs_host = resolve_path(base_dir, host);
    all_volumes.push(format!("{abs_host}:{container}:ro"));
}

// Add overlay: walk directory, create bind for each file
if let Some(overlay_dir) = &node.overlay {
    let abs_dir = resolve_path(base_dir, overlay_dir);
    for entry in walkdir::WalkDir::new(&abs_dir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let rel = entry.path().strip_prefix(&abs_dir)?;
            let container_path = format!("/{}", rel.display());
            all_volumes.push(format!("{}:{}:ro", entry.path().display(), container_path));
        }
    }
}
```

**Note**: Overlay requires walking a directory tree. Consider adding
`walkdir` as a dependency, or implement a simple recursive walk with
`std::fs::read_dir`.

**Path resolution**: Config and overlay paths should resolve relative to
the topology file's directory (same as imports).

### Tasks

- [ ] Add `Config`, `Overlay` tokens to lexer
- [ ] Add fields to NodeDef AST
- [ ] Add parser match arms
- [ ] Add fields to Node types
- [ ] Implement config → volume conversion in deploy
- [ ] Implement overlay directory walking in deploy
- [ ] Resolve paths relative to topology file
- [ ] Add validation: config/overlay require image, paths must exist
- [ ] Tests: config parse, overlay parse, path resolution

## Phase 3: Env from File (day 3)

### Problem

Large env var sets are unwieldy as inline lists. Common pattern is to
read from a `.env` file.

### Syntax

```nll
node app image "myapp" {
    env-file "configs/app.env"
    env ["OVERRIDE=true"]    # inline env overrides file values
}
```

File format (standard `.env`):
```
DATABASE_URL=postgres://db:5432/app
REDIS_URL=redis://cache:6379
LOG_LEVEL=info
# comments are stripped
```

### Implementation

**Lexer**: Add `EnvFile` token.

**AST** (`NodeDef`): Add `env_file: Option<String>`.

**Parser**: Add match arm.

**Deploy**: Read env file, parse into HashMap, merge with inline env
(inline wins on conflict):

```rust
let mut env = HashMap::new();
if let Some(env_file) = &node.env_file {
    let content = std::fs::read_to_string(resolve_path(base_dir, env_file))?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some((k, v)) = line.split_once('=') {
            env.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
}
// Inline env overrides
if let Some(inline) = &node.env {
    env.extend(inline.iter().map(|(k, v)| (k.clone(), v.clone())));
}
```

### Tasks

- [ ] Add `EnvFile` token to lexer
- [ ] Add `env_file` to NodeDef AST
- [ ] Add parser match arm
- [ ] Add field to Node types
- [ ] Implement env file reading + merging in deploy
- [ ] Resolve path relative to topology file
- [ ] Add validation: env-file requires image
- [ ] Tests: parse, file reading, merge with inline env

## Phase 4: Dependency Ordering (day 3-4)

### Problem

Containers start in arbitrary order. A service that depends on a database
may start before the database is ready. Containerlab doesn't solve this
either — it's a gap in all network lab tools.

### Syntax

```nll
node db image "postgres:16" {
    healthcheck "pg_isready" { interval 2s; timeout 30s }
}
node app image "myapp" {
    depends-on [db]
    env ["DATABASE_URL=postgres://db:5432/app"]
}
node worker image "myworker" {
    depends-on [db, app]
}
```

### Deployment behavior

1. Build a dependency DAG from `depends-on` declarations
2. Topologically sort nodes
3. Deploy in dependency order: root nodes first, then their dependents
4. Wait for health check (if any) before deploying dependent nodes
5. Nodes without dependencies deploy in parallel (or in declaration order)

### Implementation

**Lexer**: Add `DependsOn` token.

**AST** (`NodeDef`): Add `depends_on: Vec<String>`.

**Types** (`Node`): Add `depends_on: Vec<String>`.

**Validator**: Add rules:
- `depends-on-exists`: referenced nodes must exist
- `depends-on-no-cycle`: no circular dependencies
- `depends-on-requires-image`: warn if depending on non-container node

**Deploy** (`deploy.rs`): Change the deployment loop from sequential
to dependency-ordered:

```rust
fn topo_sort(nodes: &HashMap<String, Node>) -> Result<Vec<String>> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for (name, node) in nodes {
        in_degree.entry(name).or_insert(0);
        for dep in &node.depends_on {
            adj.entry(dep.as_str()).or_default().push(name);
            *in_degree.entry(name).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree.iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut order = Vec::new();

    while let Some(node) = queue.pop() {
        order.push(node.to_string());
        if let Some(deps) = adj.get(node) {
            for dep in deps {
                let d = in_degree.get_mut(dep).unwrap();
                *d -= 1;
                if *d == 0 { queue.push(dep); }
            }
        }
    }

    if order.len() != nodes.len() {
        return Err(Error::Validation("circular dependency detected".into()));
    }
    Ok(order)
}
```

Then in deploy Step 3, create containers in topological order, running
health checks between groups.

### Tasks

- [ ] Add `DependsOn` token to lexer
- [ ] Add `depends_on` to NodeDef AST
- [ ] Add parser match arm
- [ ] Add field to Node types
- [ ] Implement `topo_sort()` for dependency ordering
- [ ] Change deploy Step 3 to use dependency order
- [ ] Insert health check waits between dependency groups
- [ ] Add validation: deps exist, no cycles
- [ ] Tests: simple deps, chain deps, cycle detection, mixed container/namespace

## Progress

### Phase 1: Health Checks
- [ ] Tokens + AST
- [ ] Parser (healthcheck, healthcheck-tcp, startup-delay)
- [ ] Types
- [ ] Deploy polling loop
- [ ] Validation + Tests

### Phase 2: Config Injection
- [ ] Tokens + AST
- [ ] Parser (config, overlay)
- [ ] Types
- [ ] Volume conversion + overlay walking
- [ ] Path resolution + Validation + Tests

### Phase 3: Env from File
- [ ] Token + AST + Parser
- [ ] Types
- [ ] File reading + merge logic
- [ ] Validation + Tests

### Phase 4: Dependency Ordering
- [ ] Token + AST + Parser
- [ ] Types
- [ ] topo_sort()
- [ ] Deploy reorder
- [ ] Cycle detection + Validation + Tests
