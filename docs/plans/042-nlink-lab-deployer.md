# Plan 042: nlink-lab Deployer & Lifecycle

**Priority:** Critical (Phase 2, step 3)
**Effort:** 5-7 days
**Target:** `crates/nlink-lab`

## Summary

The deployer takes a validated `Topology` and creates the actual network lab using
nlink APIs. It follows the 18-step deployment sequence from NLINK_LAB.md section 5.1.
Also implements destroy (teardown) and state management for tracking running labs.

## API Design

```rust
use nlink_lab::{Topology, RunningLab, DeployOptions};

// Deploy
let topology = parse_file("datacenter.toml")?;
topology.validate().bail()?;  // fail on errors
let lab = topology.deploy().await?;

// Or with options
let lab = topology.deploy_with(DeployOptions {
    dry_run: false,
    verbose: true,
}).await?;

// Interact
let output = lab.exec("server1", "ping", &["-c1", "10.1.1.1"]).await?;
lab.spawn("server1", Command::new("iperf3").arg("-s"))?;

// Runtime modification
lab.set_impairment("spine1:eth1", |i| i.delay("50ms")).await?;

// Teardown
lab.destroy().await?;

// Lifecycle
let labs = RunningLab::list()?;
let lab = RunningLab::load("datacenter-sim")?;
```

## Deployment Sequence (18 steps)

The deployer executes these in order, rolling back on failure:

```
 1. Resolve topology (merge profiles, resolve names)
 2. Validate (bail on errors)
 3. Create namespaces via namespace::create()
 4. Create bridge networks (if any) in host namespace
 5. Create veth pairs with peer_netns_fd() for cross-namespace links
 6. Create additional interfaces (vxlan, bond, vlan, wireguard, dummy)
 7. Assign interfaces to bridges/bonds via set_link_master()
 8. Configure VLANs on bridge ports
 9. Set interface addresses via add_address_by_index()
10. Bring interfaces up via set_link_up()
11. Apply sysctls via namespace::set_sysctls()
12. Add routes per namespace
13. Apply nftables rules per namespace
14. Apply TC qdiscs/impairments per interface
15. Apply rate limits
16. Spawn background processes via namespace::spawn()
17. Run validation (optional connectivity checks)
18. Write state file
```

## State Management

```
~/.nlink-lab/labs/<name>/
  state.json      # {namespaces, pids, created_at, topology_hash}
  topology.toml   # Copy of topology used
```

`state.json` tracks:
- Namespace names created (for cleanup)
- Background process PIDs (for cleanup)
- Creation timestamp
- Topology file hash (for drift detection)

## Progress

### Deployer Core (`deploy.rs`)

- [ ] `DeployOptions` struct (dry_run, verbose)
- [ ] `impl Topology { pub async fn deploy(&self) -> Result<RunningLab> }`
- [ ] `impl Topology { pub async fn deploy_with(&self, opts) -> Result<RunningLab> }`
- [ ] Resolve step: merge profiles into nodes, compute namespace names with prefix
- [ ] Step 3: Create namespaces (batch, with rollback on failure)
- [ ] Step 5: Create veth pairs across namespaces
- [ ] Step 9: Set addresses per namespace
- [ ] Step 10: Bring interfaces up
- [ ] Step 11: Apply sysctls
- [ ] Step 12: Add routes
- [ ] Step 14: Apply netem impairments
- [ ] Step 15: Apply rate limits
- [ ] Step 16: Spawn background processes
- [ ] Step 18: Write state file
- [ ] Rollback: destroy created namespaces on deploy failure

### Bridge Networks (`deploy.rs` — optional for MVP)

- [ ] Step 4: Create bridge interfaces in a "management" namespace
- [ ] Step 7: Assign interfaces to bridges
- [ ] Step 8: Configure VLANs on bridge ports

### nftables (`deploy.rs` — optional for MVP)

- [ ] Step 13: Apply firewall rules per namespace

### Additional Interface Types (`deploy.rs` — optional for MVP)

- [ ] Step 6: VXLAN interfaces
- [ ] Step 6: Bond interfaces
- [ ] Step 6: VLAN sub-interfaces
- [ ] Step 6: WireGuard interfaces
- [ ] Step 6: VRF interfaces

### RunningLab (`running.rs`)

- [ ] `RunningLab` struct — holds state, namespace names, PIDs
- [ ] `pub async fn exec(&self, node, cmd, args) -> Result<String>`
- [ ] `pub fn spawn(&self, node, cmd) -> Result<Child>`
- [ ] `pub async fn set_impairment(&self, endpoint, f) -> Result<()>`
- [ ] `pub async fn destroy(&self) -> Result<()>` — kill PIDs, delete namespaces, remove state
- [ ] `pub async fn diagnose(&self) -> Result<DiagnosticReport>` — per-node diagnostics

### State Management (`state.rs`)

- [ ] `LabState` struct (serde Serialize/Deserialize)
- [ ] `state_dir()` — `$XDG_STATE_HOME/nlink-lab/labs/` or `~/.nlink-lab/labs/`
- [ ] `pub fn save(name, state, topology_toml) -> Result<()>` — write state + topology
- [ ] `pub fn load(name) -> Result<(LabState, Topology)>` — read state + topology
- [ ] `pub fn list() -> Result<Vec<String>>` — list running lab names
- [ ] `pub fn remove(name) -> Result<()>` — remove state directory
- [ ] `RunningLab::load(name) -> Result<RunningLab>` — reconstruct from state

### Destroy (`deploy.rs`)

- [ ] Kill tracked PIDs (SIGTERM, then SIGKILL after timeout)
- [ ] Delete namespaces via `namespace::delete()`
- [ ] Remove state directory
- [ ] Handle partial cleanup (some namespaces already gone)
- [ ] Orphan detection: find stale state files where namespaces no longer exist

### Integration Tests

- [ ] Deploy a minimal 2-node topology, verify connectivity with ping
- [ ] Deploy with impairment, verify delay with ping RTT
- [ ] Deploy with routes, verify multi-hop reachability
- [ ] Destroy cleans up all namespaces
- [ ] Destroy kills background processes
- [ ] State file is created and loadable
- [ ] Deploy failure rolls back namespaces
- [ ] Re-deploy after destroy works cleanly

### Documentation

- [ ] Doc comments on all public types and methods
- [ ] Module-level deployment sequence example
