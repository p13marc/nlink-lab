# Plan 104: Polish — Management Network, Colors, Inspect, Man Page

**Priority:** Low
**Effort:** 3-4 days
**Depends on:** Plan 102 (needs status table and shell command patterns)
**Target:** `crates/nlink-lab/src/`, `bins/lab/src/`

## Summary

Remaining polish items from the CLI report, containerlab comparison, and
earlier plans. Management network (containerlab's best feature we lack),
colored CLI output, combined inspect command, deploy timing breakdown,
and man page generation.

---

## Phase 1: Management Network (day 1-2)

### Problem

Containerlab auto-creates an out-of-band management bridge connecting
all nodes. nlink-lab has no equivalent — users must manually create a
`network` block for management access. This is the most useful feature
containerlab has that nlink-lab doesn't.

### Syntax

```nll
lab "mylab" {
    mgmt 172.20.0.0/24    # auto-creates management bridge + assigns IPs
}
```

Or explicit:

```nll
lab "mylab" {
    mgmt {
        subnet 172.20.0.0/24
        interface mgmt0           # default: mgmt0
        bridge-name nlink-mgmt    # default: {prefix}-mgmt
    }
}
```

### Behavior

When `mgmt` is set on the lab:
1. Create a bridge interface in a management namespace
2. For each node, create a veth pair: `mgmt0` in the node, peer on the bridge
3. Auto-assign IPs from the subnet (sequential: .1, .2, .3, ...)
4. Bring all interfaces up
5. Store management IPs in state for display in `status`

This happens between Step 4 (bridge networks) and Step 5 (veth pairs)
in the deploy sequence.

### Implementation

**AST** (`ast.rs`, `LabDecl`): Add `mgmt: Option<String>` (CIDR subnet).

**Types** (`types.rs`, `LabConfig`): Add `mgmt_subnet: Option<String>`.

**Parser**: Parse `mgmt` in lab block.

**Deploy**: After creating user-defined networks, create the mgmt bridge
and connect all nodes.

**Status**: Show management IP for each node.

### Tasks

- [ ] Add `mgmt` to LabDecl AST and LabConfig types
- [ ] Parse `mgmt` in lab block (subnet CIDR)
- [ ] Create management bridge during deploy (new step between 4 and 5)
- [ ] Auto-assign IPs to all nodes sequentially
- [ ] Store management IPs in state
- [ ] Show management IPs in `status` node table
- [ ] Add `Mgmt` token to lexer
- [ ] Tests: deploy with mgmt, verify bridge + addresses
- [ ] Example: `examples/management-network.nll`

## Phase 2: Colored Output (day 2)

### Problem

CLI output is plain text. No visual distinction between errors, warnings,
success, and info. Hard to scan output for problems.

### Implementation

Use the `colored` crate (or ANSI codes directly to avoid dependencies):

```rust
// Helper macros or functions
fn print_pass(msg: &str) { eprintln!("  \x1b[32mPASS\x1b[0m  {msg}"); }
fn print_fail(msg: &str) { eprintln!("  \x1b[31mFAIL\x1b[0m  {msg}"); }
fn print_warn(msg: &str) { eprintln!("  \x1b[33mWARN\x1b[0m  {msg}"); }
fn print_info(msg: &str) { eprintln!("  \x1b[36mINFO\x1b[0m  {msg}"); }
```

Apply to:
- Validation output (`WARN`, `ERROR` labels)
- Validate assertions (`PASS`, `FAIL`)
- Deploy summary (node count in bold)
- Diagnose output (issue severity)
- Status table (container/namespace type badges)

Respect `NO_COLOR` environment variable (de facto standard).

### Tasks

- [ ] Add color helper functions (ANSI codes, no external dep)
- [ ] Respect `NO_COLOR` env var and `--no-color` flag
- [ ] Color validation output (WARN yellow, ERROR red)
- [ ] Color assertion output (PASS green, FAIL red)
- [ ] Color diagnose issues by severity
- [ ] Color deploy summary
- [ ] Add `--no-color` global flag

## Phase 3: `inspect` Command (day 2-3)

### Problem

To get a full picture of a running lab, users must run `status`, `diagnose`,
`impair --show`, and `ps` separately. An `inspect` command combines them.

### Syntax

```bash
nlink-lab inspect mylab
```

### Output

```
Lab: mylab
Created: 2026-03-29 14:32:10 UTC
Nodes: 5  Links: 8  Impairments: 2

  NODE      TYPE        IMAGE           MGMT IP
  router    namespace   --              172.20.0.1
  web       container   nginx:alpine    172.20.0.2
  db        container   postgres:16     172.20.0.3

  LINK                              ADDRESSES                    MTU
  router:eth0 -- web:eth0           10.0.1.1/24 -- 10.0.1.2/24  9000
  router:eth1 -- db:eth0            10.0.2.1/24 -- 10.0.2.2/24  1500

  IMPAIRMENTS
  router:eth0   delay=10ms jitter=2ms

  PROCESSES
  (none)

  DIAGNOSTICS
  router:eth0   UP  no issues
  web:eth0      UP  no issues
```

### Implementation

Combine outputs from `status`, topology inspection, `impair --show`,
`ps`, and `diagnose` into a single formatted display.

### Tasks

- [ ] Add `Inspect` CLI command
- [ ] Print lab metadata section
- [ ] Print node table (reuse status table logic)
- [ ] Print link table with addresses and MTU
- [ ] Print impairment summary
- [ ] Print process status
- [ ] Print diagnostics summary
- [ ] Support `--json` for machine-readable output

## Phase 4: Deploy Timing Breakdown (day 3)

### Problem

`deploy` shows total time but no per-phase breakdown. Users can't tell
which step is slow.

### Implementation

The deploy function already has `tracing::info!` markers at each step.
To show timing in the CLI, capture step timestamps:

**Option A**: Parse tracing output for step markers and compute deltas.

**Option B**: Return timing info from `deploy()`:

```rust
pub struct DeployResult {
    pub lab: RunningLab,
    pub timing: DeployTiming,
}

pub struct DeployTiming {
    pub parse_ms: u64,
    pub validate_ms: u64,
    pub namespaces_ms: u64,
    pub links_ms: u64,
    pub addresses_ms: u64,
    pub routes_ms: u64,
    pub firewall_ms: u64,
    pub impairments_ms: u64,
    pub assertions_ms: u64,
    pub total_ms: u64,
}
```

Option A is simpler (no API change). Option B is more robust.

For now, use `--verbose` (from plan 102) to show tracing output
which includes step markers. The timing breakdown becomes a verbose
feature, not a default.

### Tasks

- [ ] Add `Instant::now()` timing around each deploy phase
- [ ] Return `DeployTiming` from `deploy()` (or print via tracing)
- [ ] Show breakdown when `--verbose` is set
- [ ] Format: `  Step 3: namespaces     0.8s`

## Phase 5: Man Page (day 3)

### Problem

No man page. Users on systems without internet can't access help beyond
`--help`.

### Implementation

Use `clap_mangen` as a build dependency:

```toml
[build-dependencies]
clap_mangen = "0.2"
```

Build script:

```rust
// bins/lab/build.rs
fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let cmd = <Cli as clap::CommandFactory>::command();
    clap_mangen::Man::new(cmd)
        .render(&mut std::fs::File::create(
            std::path::Path::new(&out_dir).join("nlink-lab.1")
        ).unwrap())
        .unwrap();
}
```

Add to justfile:

```just
man:
    cargo build -p nlink-lab-cli
    install -m 644 target/debug/build/nlink-lab-cli-*/out/nlink-lab.1 /usr/local/share/man/man1/
```

### Tasks

- [ ] Add `clap_mangen` to build-dependencies
- [ ] Create `bins/lab/build.rs`
- [ ] Generate `nlink-lab.1` at build time
- [ ] Add `just man` recipe to install man page
- [ ] Add install instructions to README

## Progress

### Phase 1: Management Network
- [ ] AST + types + parser
- [ ] Deploy integration
- [ ] Status display
- [ ] Tests + example

### Phase 2: Colors
- [ ] Color helpers
- [ ] NO_COLOR / --no-color
- [ ] Apply to all commands

### Phase 3: Inspect
- [ ] Command implementation
- [ ] Combined output
- [ ] --json support

### Phase 4: Deploy Timing
- [ ] Per-phase timing
- [ ] Verbose output

### Phase 5: Man Page
- [ ] clap_mangen setup
- [ ] Build script
- [ ] Install recipe
