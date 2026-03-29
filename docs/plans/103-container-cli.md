# Plan 103: Container CLI — Visibility, Logs, Stats, Pull

**Priority:** Medium
**Effort:** 2-3 days
**Depends on:** Plan 102 (needs `container_for()` API and node validation pattern)
**Target:** `bins/lab/src/main.rs`, `crates/nlink-lab/src/running.rs`, `crates/nlink-lab/src/container.rs`

## Summary

The CLI has zero container-specific commands. Container nodes are deployed
and exec'd transparently but there's no way to inspect, manage, or monitor
the container layer. This plan adds 5 commands: `containers`, `logs`,
`pull`, `stats`, and `restart`.

---

## Phase 1: Container Visibility (day 1)

### 1.1 `containers` command

List containers in a running lab with live status from the runtime:

```bash
$ nlink-lab containers mylab

  NODE    IMAGE            CONTAINER ID    STATUS     PID
  web     nginx:alpine     a1b2c3d4e5f6    running    4521
  db      postgres:16      f6e5d4c3b2a1    running    4522
  cache   redis:7          1a2b3c4d5e6f    running    4523
```

**Implementation**:

```rust
Containers {
    lab: String,
}
```

Handler:

```rust
Commands::Containers { lab } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if containers.is_empty() {
        println!("No container nodes in lab '{lab}'.");
        return Ok(());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&containers)?);
    } else {
        println!("  {:<16} {:<20} {:<14} {:<10} {}", "NODE", "IMAGE", "CONTAINER ID", "STATUS", "PID");
        for (name, state) in containers {
            let short_id = &state.id[..12.min(state.id.len())];
            let status = if rt.exists(&state.id) { "running" } else { "stopped" };
            println!("  {:<16} {:<20} {:<14} {:<10} {}", name, state.image, short_id, status, state.pid);
        }
    }
    Ok(())
}
```

**Requires**: Make `containers()` public on RunningLab (currently `pub(crate)`).

### 1.2 `status` container info

Enhance the status node table (from plan 102 phase 1.4) to show
container-specific info when the node has an image.

Already handled by plan 102's status table — this phase just ensures
container state is accessible.

### Tasks

- [ ] Add `Containers` CLI command
- [ ] Make `RunningLab::containers()` public
- [ ] Make `RunningLab::runtime_binary()` public
- [ ] Create Runtime from binary name for status checks
- [ ] Add `--json` support
- [ ] Test with mixed container/namespace labs

## Phase 2: Container Logs (day 1-2)

### `logs` command

Stream container stdout/stderr:

```bash
nlink-lab logs mylab web                # show all logs
nlink-lab logs mylab web --follow       # stream (tail -f style)
nlink-lab logs mylab web --tail 50      # last 50 lines
```

**Implementation**:

```rust
Logs {
    lab: String,
    node: String,
    #[arg(long)]
    follow: bool,
    #[arg(long)]
    tail: Option<u32>,
}
```

Handler:

```rust
Commands::Logs { lab, node, follow, tail } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container node. Logs are only available for container nodes."
        ))
    })?;
    let rt_binary = running.runtime_binary().unwrap();

    let mut args = vec!["logs".to_string()];
    if follow {
        args.push("--follow".to_string());
    }
    if let Some(n) = tail {
        args.push("--tail".to_string());
        args.push(n.to_string());
    }
    args.push(container.id.clone());

    let status = std::process::Command::new(rt_binary)
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}
```

**Requires**: Public `container_for()` method on RunningLab:

```rust
pub fn container_for(&self, node: &str) -> Option<&ContainerState> {
    self.containers.get(node)
}
```

### Tasks

- [ ] Add `Logs` CLI command
- [ ] Add `container_for()` public method to RunningLab
- [ ] Support `--follow` (pass to docker/podman logs)
- [ ] Support `--tail N` (pass to docker/podman logs)
- [ ] Error message for non-container nodes
- [ ] Test with container and namespace nodes

## Phase 3: Pull and Stats (day 2)

### 3.1 `pull` command

Pre-pull all images referenced in a topology:

```bash
$ nlink-lab pull topology.nll
Pulling nginx:alpine... done
Pulling postgres:16... done
Pulling redis:7... done
3 images pulled
```

**Implementation**:

```rust
Pull {
    topology: PathBuf,
}
```

Handler:

```rust
Commands::Pull { topology } => {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    let images: Vec<&str> = topo.nodes.values()
        .filter_map(|n| n.image.as_deref())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    if images.is_empty() {
        println!("No container images in topology.");
        return Ok(());
    }
    let rt = nlink_lab::container::Runtime::detect()?;
    for image in &images {
        eprint!("Pulling {image}...");
        rt.pull_image(image)?;
        eprintln!(" done");
    }
    println!("{} image(s) pulled", images.len());
    Ok(())
}
```

### 3.2 `stats` command

Show live container resource usage:

```bash
$ nlink-lab stats mylab

  NODE    CPU%    MEMORY       MEM%
  web     2.3%    45.2 MiB     17.6%
  db      8.1%    198 MiB      38.7%
  cache   0.5%    12.1 MiB     4.7%
```

**Implementation**:

```rust
Stats {
    lab: String,
}
```

Handler uses `docker stats --no-stream --format` / `podman stats --no-stream --format`:

```rust
Commands::Stats { lab } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if containers.is_empty() {
        println!("No container nodes in lab '{lab}'.");
        return Ok(());
    }
    let rt_binary = running.runtime_binary().unwrap();
    let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();

    let output = std::process::Command::new(rt_binary)
        .args(["stats", "--no-stream", "--format",
            "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}"])
        .args(&ids)
        .output()?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}
```

### Tasks

- [ ] Add `Pull` CLI command
- [ ] Make `Runtime::detect()` and `Runtime::pull_image()` accessible
- [ ] Deduplicate images before pulling
- [ ] Add `Stats` CLI command
- [ ] Pass container IDs to `docker stats`
- [ ] Support `--json` for stats

## Phase 4: Container Restart (day 2-3)

### `restart` command

Restart a single container node without destroying the whole lab:

```bash
nlink-lab restart mylab web
```

**Implementation**:

This is complex because restarting a container means:
1. Stop the container (`docker stop`)
2. Remove the container (`docker rm`)
3. Create a new container with the same options
4. Re-attach networking (veth pairs, addresses, routes)
5. Update state file with new container ID/PID

**Simpler approach**: Just restart via the runtime, which preserves
the network namespace:

```rust
Commands::Restart { lab, node } => {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!("'{node}' is not a container node"))
    })?;
    let rt_binary = running.runtime_binary().unwrap();

    eprint!("Restarting '{node}'...");
    let status = std::process::Command::new(rt_binary)
        .args(["restart", &container.id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        eprintln!(" done");
    } else {
        eprintln!(" failed");
        std::process::exit(1);
    }
    Ok(())
}
```

Note: `docker restart` preserves the container and its network namespace,
so veth pairs and addresses remain intact. This is the simplest approach.

### Tasks

- [ ] Add `Restart` CLI command
- [ ] Use `docker/podman restart` (preserves networking)
- [ ] Validate node is a container node
- [ ] Error message for namespace nodes
- [ ] Test restart preserves connectivity

## Progress

### Phase 1: Container Visibility
- [ ] Containers command
- [ ] Public containers()/runtime_binary()

### Phase 2: Logs
- [ ] Logs command
- [ ] container_for() API
- [ ] --follow / --tail

### Phase 3: Pull + Stats
- [ ] Pull command
- [ ] Stats command

### Phase 4: Restart
- [ ] Restart command
- [ ] Validate container node
