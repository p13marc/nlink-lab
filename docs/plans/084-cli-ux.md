# Plan 084: CLI UX & New Commands

**Priority:** Medium
**Effort:** 3-4 days
**Target:** `bins/lab/src/main.rs`, `crates/nlink-lab/src/running.rs`

## Summary

Improve the CLI user experience with shell completions, machine-readable output,
dry-run mode, and new commands for topology export and drift detection.

## Phase 1: Quick Wins (1 day)

### Shell Completions

**Where:** `bins/lab/src/main.rs`

clap supports generating completions natively. Add a hidden `completions` subcommand:

```rust
#[derive(Subcommand)]
enum Commands {
    // ... existing commands ...

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

// In main():
Commands::Completions { shell } => {
    clap_complete::generate(shell, &mut Cli::command(), "nlink-lab", &mut std::io::stdout());
}
```

**Dependency:** Add `clap_complete = "4"`.

**Usage:**
```bash
# Bash
nlink-lab completions bash > /etc/bash_completion.d/nlink-lab

# Zsh
nlink-lab completions zsh > ~/.zfunc/_nlink-lab

# Fish
nlink-lab completions fish > ~/.config/fish/completions/nlink-lab.fish
```

### `--json` Output Flag

**Where:** `bins/lab/src/main.rs` — status, diagnose, ps commands.

Add a global `--json` flag:

```rust
#[derive(Parser)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}
```

For `status`:
```rust
if cli.json {
    let labs = state::list()?;
    println!("{}", serde_json::to_string_pretty(&labs)?);
} else {
    // existing table output
}
```

For `diagnose`:
```rust
if cli.json {
    let diags = lab.diagnose()?;
    println!("{}", serde_json::to_string_pretty(&diags)?);
}
```

For `ps`:
```rust
if cli.json {
    let procs = lab.processes();
    println!("{}", serde_json::to_string_pretty(&procs)?);
}
```

**Dependency:** Add `serde_json = "1"` (likely already in dep tree).

### `--dry-run` Flag on Deploy

**Where:** `bins/lab/src/main.rs` — deploy command.

Validate the topology and print what would be created without actually deploying:

```rust
Commands::Deploy { file, dry_run, .. } => {
    let topo = parser::parse_file(&file)?;
    let diags = topo.validate();
    diags.print();
    diags.bail()?;

    if dry_run {
        println!("Dry run — topology is valid\n");
        println!("Would create:");
        println!("  {} namespaces", topo.nodes.len());
        println!("  {} veth pairs", topo.links.len());
        println!("  {} bridge networks", topo.networks.len());
        for (name, node) in &topo.nodes {
            if node.container.is_some() {
                println!("  container: {name}");
            } else {
                println!("  namespace: {name}");
            }
        }
        // Print summary of interfaces, routes, impairments
        return Ok(());
    }
    // ... proceed with actual deploy
}
```

Does not require root privileges, making it useful for CI validation.

## Phase 2: New Commands (2-3 days)

### `nlink-lab export`

Dump a running lab's topology as TOML. Useful for reproducing a lab state or
creating a topology file from a deployed lab.

```rust
/// Export a running lab's topology as TOML
#[derive(Args)]
struct ExportArgs {
    /// Lab name
    name: String,
    /// Output format
    #[arg(long, default_value = "toml", value_parser = ["toml", "nll"])]
    format: String,
    /// Output file (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
}
```

**Implementation:**

```rust
Commands::Export(args) => {
    let lab = RunningLab::load(&args.name)?;
    let content = match args.format.as_str() {
        "toml" => toml::to_string_pretty(lab.topology())?,
        "nll" => nll::emit(lab.topology())?,  // New: NLL emitter
        _ => unreachable!(),
    };
    match args.output {
        Some(path) => std::fs::write(&path, &content)?,
        None => print!("{content}"),
    }
}
```

**Note:** NLL emitter is a new feature — converts `Topology` back to NLL syntax.
This can be deferred; TOML export is straightforward since `Topology` derives
`Serialize`.

### `nlink-lab diff`

Compare a running lab against its original topology file. Detects drift from
runtime modifications (impairment changes, killed processes, etc.).

```rust
/// Compare running lab state against topology file
#[derive(Args)]
struct DiffArgs {
    /// Lab name
    name: String,
    /// Original topology file to compare against
    #[arg(short, long)]
    file: Option<PathBuf>,
}
```

**Implementation approach:**

1. Load `RunningLab` from state
2. If `--file` provided, parse that topology; otherwise use the stored topology
3. For each node, run `ip addr`, `ip route`, `tc qdisc show`, `nft list ruleset`
4. Compare actual state against expected topology
5. Report differences:
   - Missing/extra interfaces
   - Wrong addresses
   - Changed impairments
   - Missing routes
   - Modified firewall rules
   - Dead background processes

```rust
pub struct LabDiff {
    pub node: String,
    pub category: DiffCategory,
    pub expected: String,
    pub actual: String,
}

pub enum DiffCategory {
    Interface,
    Address,
    Route,
    Impairment,
    Firewall,
    Process,
}
```

This is valuable for long-running labs where state may have drifted.

### `nlink-lab wait`

Block until a lab is fully ready. Useful in scripts and CI:

```rust
/// Wait for a lab to be ready
#[derive(Args)]
struct WaitArgs {
    /// Lab name
    name: String,
    /// Timeout in seconds
    #[arg(short, long, default_value = "30")]
    timeout: u64,
}
```

**Implementation:**
Poll `state::exists()` and optionally run connectivity checks until all pass or
timeout expires.

## Phase 3: Capture Improvements

### BPF Filter Support

**Where:** `bins/lab/src/main.rs` — capture command.

```rust
Commands::Capture { name, node, interface, filter, .. } => {
    let mut args = vec!["-i", &interface, "-w", &output_path];
    if let Some(f) = &filter {
        args.extend(["-f", f]);
    }
    lab.exec(node, "tcpdump", &args)?;
}
```

### Current Impairment Inspection

Add `nlink-lab impair --show` to display current impairments before modifying:

```rust
Commands::Impair { name, show, .. } => {
    if show {
        let lab = RunningLab::load(&name)?;
        for (node_name, _) in lab.topology().nodes.iter() {
            let output = lab.exec(node_name, "tc", &["qdisc", "show"])?;
            println!("--- {node_name} ---");
            println!("{}", output.stdout);
        }
        return Ok(());
    }
    // ... existing impairment logic
}
```

## Progress

### Phase 1: Quick Wins
- [x] Add shell completions (bash/zsh/fish) via `clap_complete`
- [x] Add `--json` global flag to status and ps commands
- [x] `--dry-run` flag on deploy (already existed)
- [x] Add `serde_json` + `clap_complete` dependencies to CLI

### Phase 2: New Commands
- [x] `nlink-lab export` — TOML/JSON export of running lab topology
- [ ] `nlink-lab diff` — drift detection against topology file
- [ ] `nlink-lab wait` — block until lab is ready

### Phase 3: Capture & Impairment
- [ ] BPF filter support in capture command
- [x] `nlink-lab impair --show` to inspect current TC state
