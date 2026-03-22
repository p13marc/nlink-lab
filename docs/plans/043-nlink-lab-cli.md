# Plan 043: nlink-lab CLI Binary

**Priority:** Critical (Phase 2, step 4)
**Effort:** 2-3 days
**Target:** `bins/lab/`
**Depends on:** Plan 040 (types), Plan 041 (validator), Plan 042 (deployer)

## Summary

The `nlink-lab` CLI binary. Thin wrapper around the `nlink-lab` library crate.
Uses `clap` for argument parsing, `tracing` for logging.

The CLI is intentionally thin — all logic lives in the library so it can be reused
from Rust integration tests via the builder DSL.

## Status

**~30% complete.** Clap structure and all 5 subcommands are defined. Command bodies
are stubs that print "not yet implemented".

## What's Done

- `bins/lab/Cargo.toml` with dependencies
- `bins/lab/src/main.rs` with clap CLI and all 5 subcommands
- Basic error handling with `ExitCode`
- `tracing_subscriber::fmt::init()` for logging

## CLI Commands

```
nlink-lab deploy <topology.toml> [--dry-run]    Deploy a lab
nlink-lab destroy <name> [--force]              Tear down a running lab
nlink-lab status [name]                         Show running labs or details
nlink-lab exec <lab> <node> -- <cmd...>         Run a command in a lab node
nlink-lab validate <topology.toml>              Validate topology without deploying
```

## Detailed Command Specifications

### `deploy`

**Flow:**
1. Parse topology file → `Topology`
2. Validate → print warnings, bail on errors
3. Check if lab name already exists in state → fail with `Error::AlreadyExists` (or `--force`)
4. Deploy → `RunningLab`
5. Print summary

**Arguments:**
- `topology` (positional, required): path to TOML file
- `--dry-run`: parse + validate only, don't deploy
- `--force`: destroy existing lab with same name before deploying

**Output (success):**
```
Lab "datacenter-sim" deployed in 47ms
  Nodes:       spine1, spine2, leaf1, leaf2, server1, server2
  Links:       6 point-to-point
  Impairments: 3
  Processes:   2 background
```

**Output (dry-run):**
```
Topology "datacenter-sim" is valid
  Nodes:       6
  Links:       6
  Profiles:    2
  Networks:    0
  Impairments: 3
  Rate limits: 2
```

**Output (validation errors):**
```
Validation failed for "datacenter-sim":
  ERROR [dangling-node-ref] node 'spine3' referenced in links[4].endpoints[0] does not exist
  ERROR [valid-cidr] invalid CIDR '10.0.0.1' in links[2].addresses[0]: missing '/' separator
  WARN  [route-reachability] route 'default' on node 'server1': gateway '10.1.1.1' not reachable from any connected subnet
```

**Implementation:**
```rust
Commands::Deploy { topology, dry_run, force } => {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    let result = topo.validate();

    // Print warnings regardless
    for w in result.warnings() {
        eprintln!("  WARN  {w}");
    }

    if result.has_errors() {
        eprintln!("Validation failed for {:?}:", topo.lab.name);
        for e in result.errors() {
            eprintln!("  ERROR {e}");
        }
        return Err(Error::Validation("see errors above".into()));
    }

    if dry_run {
        println!("Topology {:?} is valid", topo.lab.name);
        print_topology_summary(&topo);
        return Ok(());
    }

    if force && state::exists(&topo.lab.name) {
        let lab = RunningLab::load(&topo.lab.name).await?;
        lab.destroy().await?;
    }

    let start = Instant::now();
    let lab = topo.deploy().await?;
    let elapsed = start.elapsed();

    println!("Lab {:?} deployed in {:?}", topo.lab.name, elapsed);
    print_deploy_summary(&topo);
    Ok(())
}
```

### `destroy`

**Flow:**
1. Load state for lab name
2. Destroy (kill PIDs, delete namespaces, remove bridges, remove state)
3. Print confirmation

**Arguments:**
- `name` (positional, required): lab name
- `--force`: don't fail if some resources are already gone

**Output:**
```
Lab "datacenter-sim" destroyed (6 namespaces removed)
```

**Implementation:**
```rust
Commands::Destroy { name, force } => {
    let lab = match RunningLab::load(&name).await {
        Ok(lab) => lab,
        Err(e) if force => {
            // Force cleanup: try to delete namespaces by prefix
            force_cleanup(&name).await?;
            println!("Lab {name:?} force-cleaned");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let node_count = lab.namespace_count();
    lab.destroy().await?;
    println!("Lab {name:?} destroyed ({node_count} namespaces removed)");
    Ok(())
}
```

### `status`

**Flow (no args):** List all running labs from state directory.
**Flow (with name):** Show detailed status for one lab.

**Arguments:**
- `name` (optional): specific lab name

**Output (list):**
```
NAME              NODES  CREATED
datacenter-sim    6      2026-03-22 14:30:00
simple            2      2026-03-22 15:00:00
```

**Output (detail):**
```
Lab: datacenter-sim
Created: 2026-03-22 14:30:00

Nodes:
  spine1    namespace: dc-spine1     up
  spine2    namespace: dc-spine2     up
  leaf1     namespace: dc-leaf1      up
  server1   namespace: dc-server1   up

Links:
  spine1:eth1 ↔ leaf1:eth1   10.0.11.1/30 ↔ 10.0.11.2/30
  leaf1:eth3  ↔ server1:eth0 10.1.1.1/24  ↔ 10.1.1.10/24

Impairments:
  spine1:eth1  delay=10ms jitter=2ms

Processes:
  server1  iperf3 -s  (pid 12345, running)
```

**Implementation:**
```rust
Commands::Status { name } => {
    match name {
        None => {
            let labs = RunningLab::list()?;
            if labs.is_empty() {
                println!("No running labs.");
            } else {
                println!("{:<18} {:<6} {}", "NAME", "NODES", "CREATED");
                for info in labs {
                    println!("{:<18} {:<6} {}", info.name, info.node_count, info.created_at);
                }
            }
        }
        Some(name) => {
            let lab = RunningLab::load(&name).await?;
            print_lab_detail(&lab);
        }
    }
    Ok(())
}
```

### `exec`

**Flow:**
1. Load state for lab name
2. Run command in node namespace
3. Print stdout/stderr, exit with command's exit code

**Arguments:**
- `lab` (positional, required): lab name
- `node` (positional, required): node name
- `--` separator
- `cmd` (trailing var arg, required): command and arguments

**Output:** Raw command output (no decoration), exit with command's exit code.

**Implementation:**
```rust
Commands::Exec { lab, node, cmd } => {
    let running = RunningLab::load(&lab).await?;
    let output = running.exec(&node, &cmd[0], &cmd[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>()).await?;

    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}
```

### `validate`

**Flow:**
1. Parse topology file
2. Run validator
3. Print issues with severity
4. Exit code 1 if errors, 0 if only warnings or clean

**Arguments:**
- `topology` (positional, required): path to TOML file

**Output (valid):**
```
Topology "datacenter-sim" is valid
  Nodes:       6
  Links:       6
  Profiles:    2
  Networks:    0
  Impairments: 3
  Rate limits: 2
```

**Output (errors):**
```
Validation failed for "datacenter-sim":
  ERROR [dangling-node-ref] node 'spine3' referenced in links[4].endpoints[0] does not exist
```

**Implementation:**
```rust
Commands::Validate { topology } => {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    let result = topo.validate();

    for w in result.warnings() {
        eprintln!("  WARN  {w}");
    }

    if result.has_errors() {
        eprintln!("Validation failed for {:?}:", topo.lab.name);
        for e in result.errors() {
            eprintln!("  ERROR {e}");
        }
        return Err(Error::Validation("see errors above".into()));
    }

    println!("Topology {:?} is valid", topo.lab.name);
    print_topology_summary(&topo);
    Ok(())
}
```

## Async Runtime

The CLI needs `tokio` for the async deployer. The `main` function stays sync and
delegates to a tokio runtime:

```rust
fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new().unwrap();
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> nlink_lab::Result<()> {
    // ... command dispatch
}
```

## Root/Capability Check

Deploy and destroy require `CAP_NET_ADMIN`. Add a check:

```rust
fn check_root() {
    if !nix::unistd::Uid::effective().is_root() {
        eprintln!("warning: nlink-lab typically requires root or CAP_NET_ADMIN");
    }
}
```

Call this before `deploy`, `destroy`, and `exec` commands. It's a warning, not a
hard error — `CAP_NET_ADMIN` without root UID is valid.

**Alternative without nix dependency:** Read `/proc/self/status` for `CapEff` or
simply check `libc::geteuid() == 0`.

## Progress

### Clap Structure Updates

- [ ] Add `--force` flag to `deploy` command
- [ ] Add `--force` flag to `destroy` command
- [ ] Verify `--` separator works for `exec` trailing args

### Async Runtime

- [ ] Switch `run()` to `async fn`
- [ ] Add tokio runtime in `main()`

### Command Implementations

- [ ] `validate` — parse, validate, print issues, exit code
- [ ] `deploy --dry-run` — parse, validate, print summary (no deploy)
- [ ] `deploy` — full deploy with timing and summary output
- [ ] `deploy --force` — destroy existing lab first
- [ ] `destroy` — load state, destroy, print confirmation
- [ ] `destroy --force` — best-effort cleanup even if state is missing/stale
- [ ] `status` (no args) — list running labs in table format
- [ ] `status <name>` — detailed lab status
- [ ] `exec` — run command in node, forward stdout/stderr, exit with command's code

### Root/Capability Check

- [ ] Detect non-root without `CAP_NET_ADMIN`
- [ ] Print warning before deploy/destroy/exec

### Output Formatting Helpers

- [ ] `print_topology_summary(topo)` — node/link/profile counts
- [ ] `print_deploy_summary(topo)` — node names, link count, impairment count, process count
- [ ] `print_lab_detail(lab)` — detailed status with nodes, links, impairments, processes

### Error Handling

- [ ] User-friendly error messages (no raw debug output)
- [ ] Exit code 1 for errors
- [ ] Exit code 0 for success (including warnings)
- [ ] Forward command exit code from `exec`

### Tests

- [ ] `validate` on valid topology → exit 0, prints summary
- [ ] `validate` on invalid topology → exit 1, prints errors
- [ ] `deploy --dry-run` on valid topology → exit 0, no namespaces created
- [ ] Integration: `deploy` + `exec` + `destroy` round-trip
- [ ] `status` shows deployed lab
- [ ] `exec` forwards exit code from command
