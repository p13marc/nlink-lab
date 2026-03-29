# Plan 102: CLI Quality — Must-Fix and Should-Fix Improvements

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `bins/lab/src/main.rs`, `crates/nlink-lab/src/running.rs`

## Summary

Fix the 4 must-fix issues and 7 should-fix issues from the CLI report.
These are all changes to `main.rs` — no parser or DSL changes.

---

## Phase 1: Must-Fix Items (day 1)

### 1.1 `diagnose` missing `--json`

**Current** (line 615-644): Raw text output, ignores `json` flag.

**Fix**: Wrap the existing output with a json check. The `NodeDiagnostic`
type already derives `Debug` — add `Serialize` to it and to `InterfaceDiag`
and `Issue` (from nlink).

If nlink types don't derive Serialize, create wrapper structs:

```rust
if json {
    // Serialize diagnostics as JSON
    let results: Vec<_> = results.iter().map(|d| {
        serde_json::json!({
            "node": d.node,
            "interfaces": d.interfaces.len(),
            "issues": d.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
        })
    }).collect();
    println!("{}", serde_json::to_string_pretty(&results)?);
} else {
    // existing text output
}
```

### 1.2 `exec` validate node exists

**Current** (line 487-491): Calls `running.exec()` directly. If node
doesn't exist, error is from nlink internals.

**Fix**: Check node exists before exec, show available nodes on error:

```rust
let running = nlink_lab::RunningLab::load(&lab)?;
let node_names: Vec<&str> = running.node_names().collect();
if !node_names.contains(&node.as_str()) {
    eprintln!("Error: node '{node}' not found in lab '{lab}'");
    eprintln!("Available nodes: {}", node_names.join(", "));
    return Err(nlink_lab::Error::NodeNotFound { name: node });
}
```

### 1.3 `destroy` detailed feedback

**Current** (line 437-439): Shows `({node_count} namespaces removed)`.

**Fix**: Show breakdown before destroying:

```rust
let node_count = lab.namespace_count();
let container_count = lab.topology().nodes.values()
    .filter(|n| n.image.is_some()).count();
let link_count = lab.topology().links.len();
let process_count = lab.process_status().iter()
    .filter(|p| p.alive).count();
lab.destroy().await?;
println!("Lab {name:?} destroyed:");
println!("  Nodes:       {node_count}");
if container_count > 0 {
    println!("  Containers:  {container_count} stopped and removed");
}
println!("  Links:       {link_count}");
if process_count > 0 {
    println!("  Processes:   {process_count} killed");
}
```

### 1.4 `status` detailed node table

**Current** (line 470-484): Comma-separated node names.

**Fix**: Show a table with node type and image:

```rust
println!("Lab: {}", lab.name());
println!("Created: {}", state.created_at);
println!("Nodes: {}  Links: {}  Impairments: {}",
    lab.namespace_count(), topo.links.len(), topo.impairments.len());
println!();
println!("  {:<16} {:<12} {}", "NODE", "TYPE", "IMAGE");
for (name, node) in &topo.nodes {
    let kind = if node.image.is_some() { "container" } else { "namespace" };
    let image = node.image.as_deref().unwrap_or("--");
    println!("  {:<16} {:<12} {}", name, kind, image);
}
```

### Tasks

- [ ] Add JSON output to diagnose command
- [ ] Add node validation to exec command with available nodes list
- [ ] Add detailed breakdown to destroy output
- [ ] Add node table to status (specific lab) output
- [ ] Tests: exec with invalid node name

## Phase 2: Should-Fix Items (day 1-2)

### 2.1 `wait` progress feedback

**Current** (line 894-908): Silent polling.

**Fix**: Print waiting message and elapsed time:

```rust
eprint!("Waiting for lab '{name}'...");
let start = Instant::now();
loop {
    if nlink_lab::state::exists(&name) {
        eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
        break;
    }
    if start.elapsed() > Duration::from_secs(timeout) {
        eprintln!(" timeout after {timeout}s");
        return Err(...);
    }
    std::thread::sleep(Duration::from_millis(500));
}
```

### 2.2 `destroy --all`

**Fix**: Add `--all` flag to Destroy command:

```rust
Destroy {
    name: Option<String>,  // was: String (required)
    #[arg(long)]
    force: bool,
    #[arg(long)]
    all: bool,
}
```

When `--all`, list all labs and destroy each:

```rust
if all {
    let labs = nlink_lab::RunningLab::list()?;
    for info in &labs {
        let lab = nlink_lab::RunningLab::load(&info.name)?;
        lab.destroy().await?;
        println!("Destroyed '{}'", info.name);
    }
    println!("{} labs destroyed", labs.len());
}
```

### 2.3 `capture` forward exit code

**Current** (line 675-680): Prints output but ignores exit code.

**Fix**: Forward exit code like `exec` does:

```rust
if output.exit_code != 0 {
    std::process::exit(output.exit_code);
}
```

### 2.4 `apply` show change summary

**Current**: Just says "Applied in X.Xs".

**Fix**: Print the diff summary after apply:

```rust
println!("Applied changes to '{}' in {:.1}s:", lab_name, elapsed.as_secs_f64());
if !diff.nodes_added.is_empty() {
    println!("  Added:   {} node(s): {}", diff.nodes_added.len(), diff.nodes_added.join(", "));
}
if !diff.nodes_removed.is_empty() {
    println!("  Removed: {} node(s): {}", diff.nodes_removed.len(), diff.nodes_removed.join(", "));
}
if !diff.links_added.is_empty() {
    println!("  Added:   {} link(s)", diff.links_added.len());
}
```

### 2.5 `--verbose` / `--quiet` global flags

**Fix**: Add to Cli struct:

```rust
#[arg(short, long, global = true)]
verbose: bool,

#[arg(short, long, global = true)]
quiet: bool,
```

Verbose enables tracing at `info` level (shows deploy steps).
Quiet suppresses all output except errors.

### Tasks

- [ ] Add progress feedback to wait command
- [ ] Add `--all` flag to destroy command
- [ ] Forward exit code from capture command
- [ ] Show diff summary after apply
- [ ] Add `--verbose` / `--quiet` global flags
- [ ] Wire verbose to tracing level

## Phase 3: Deploy Improvements (day 2)

### 3.1 Deploy suggests next steps

After deploy, print helpful next commands:

```rust
if !quiet {
    println!();
    println!("Next steps:");
    println!("  nlink-lab status {}          # inspect", topo.lab.name);
    println!("  nlink-lab exec {} <node> -- <cmd>", topo.lab.name);
    println!("  nlink-lab destroy {}", topo.lab.name);
}
```

### 3.2 `shell` command

Shorthand for interactive exec:

```rust
Shell {
    lab: String,
    node: String,
    #[arg(long, default_value = "/bin/sh")]
    shell: String,
}
```

Implementation: For containers, use `docker exec -it`. For namespaces,
use `nsenter` with stdin/stdout attached:

```rust
Commands::Shell { lab, node, shell } => {
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Validate node exists
    if let Some(container) = running.container_for(&node) {
        let rt = running.runtime_binary().unwrap();
        let status = std::process::Command::new(rt)
            .args(["exec", "-it", &container.id, &shell])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    } else {
        let ns = running.namespace_for(&node)?;
        let status = std::process::Command::new("nsenter")
            .args(["--net=/var/run/netns/".to_string() + ns, &shell])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}
```

### Tasks

- [ ] Add next-steps hint after deploy
- [ ] Add `shell` command with interactive TTY support
- [ ] Container shell via `docker exec -it`
- [ ] Namespace shell via `nsenter`
- [ ] Add `container_for()` and public `namespace_for()` to RunningLab

## Progress

### Phase 1: Must-Fix
- [ ] diagnose --json
- [ ] exec node validation
- [ ] destroy detailed feedback
- [ ] status node table

### Phase 2: Should-Fix
- [ ] wait progress
- [ ] destroy --all
- [ ] capture exit code
- [ ] apply change summary
- [ ] --verbose / --quiet flags

### Phase 3: Deploy Improvements
- [ ] Next-steps hint
- [ ] shell command
- [ ] container_for() API
