# Plan 042: nlink-lab Deployer & Lifecycle

**Priority:** Critical (Phase 2, step 3)
**Effort:** 5-7 days
**Target:** `crates/nlink-lab`
**Depends on:** Plan 040 (types), Plan 041 (validator)

## Summary

The deployer takes a validated `Topology` and creates the actual network lab using
nlink APIs. It follows the 18-step deployment sequence from `NLINK_LAB.md` section 5.1.
Also implements destroy (teardown), running lab interaction, and state management.

This is the largest and most critical plan — it's where the tool does real work.

## Status

**0% complete.** No files exist yet.

## Architecture Overview

```
deploy.rs   → Deployer (Topology → RunningLab)
running.rs  → RunningLab (exec, spawn, impair, destroy)
state.rs    → State persistence (save/load/list/remove)
```

The deployer creates a `RunningLab` which holds:
- The original `Topology`
- Per-node namespace names
- Per-node `Connection<Route>` handles (for runtime operations)
- Background process PIDs

## API Design

```rust
use nlink_lab::{Topology, RunningLab};

// Deploy (simplest path)
let topology = parser::parse_file("datacenter.toml")?;
topology.validate().bail()?;
let lab = topology.deploy().await?;

// Interact with running lab
let output = lab.exec("server1", "ping", &["-c1", "10.0.0.1"]).await?;
println!("{}", output.stdout);

lab.spawn("server1", &["iperf3", "-s"]).await?;

// Runtime impairment modification
lab.set_impairment("spine1:eth1", &Impairment {
    delay: Some("50ms".into()),
    ..Default::default()
}).await?;

// Teardown
lab.destroy().await?;

// Lifecycle management
let labs = RunningLab::list()?;
let lab = RunningLab::load("datacenter-sim").await?;
```

---

## Part 1: Deployer (`deploy.rs`)

### nlink API Mapping

The deployer translates topology types into nlink API calls. Here's the exact mapping
for each deployment step:

| Step | Topology Concept | nlink API |
|------|-----------------|-----------|
| 3. Create namespaces | `nodes` map | `namespace::create(ns_name)` |
| 4. Create bridges | `networks` map | `conn.add_link(BridgeLink::new(name))` in host ns |
| 5. Create veth pairs | `links` vec | `conn.add_link(VethLink::new(a, b).peer_netns_fd(fd))` |
| 6a. VXLAN | `nodes.*.interfaces` (kind=vxlan) | `conn.add_link(VxlanLink::new(...))` in node ns |
| 6b. Bond | `nodes.*.interfaces` (kind=bond) | `conn.add_link(BondLink::new(name))` in node ns |
| 6c. VLAN | `nodes.*.interfaces` (kind=vlan) | `conn.add_link(VlanLink::new(...))` in node ns |
| 6d. WireGuard | `nodes.*.wireguard` | `conn.add_link(WireguardLink::new(name))` in node ns |
| 6e. VRF | `nodes.*.vrfs` | `conn.add_link(VrfLink::new(name, table))` in node ns |
| 6f. Dummy | `nodes.*.interfaces` (kind=dummy) | `conn.add_link(DummyLink::new(name))` in node ns |
| 7. Bridge membership | `networks.*.members` | `conn.set_link_master(iface, bridge_idx)` |
| 8. Bridge VLANs | `networks.*.ports.*.vlans` | Bridge VLAN filtering API |
| 9. Addresses | `links.*.addresses`, `nodes.*.interfaces.*.addresses` | `conn.add_address_by_index(idx, ip, prefix)` |
| 10. Bring up | All created interfaces | `conn.set_link_up(iface)` / `set_link_up_by_index(idx)` |
| 11. Sysctls | `profiles.*.sysctls`, `nodes.*.sysctls` | `namespace::set_sysctls(ns, &[...])` |
| 12. Routes | `nodes.*.routes` | `conn.add_route(Ipv4Route/Ipv6Route::new(...))` |
| 13. Firewall | `nodes.*.firewall` | `conn.add_table(...)`, `add_chain(...)`, `add_rule(...)` |
| 14. Netem | `impairments.*` | `conn.add_qdisc(iface, NetemConfig::new()...)` |
| 15. Rate limits | `rate_limits.*` | `RateLimiter::new(iface).egress(...).apply(&conn)` |
| 16. Processes | `nodes.*.exec` | `namespace::spawn(ns, cmd)` |

### Deployment Sequence — Detailed

#### Step 1-2: Parse + Validate (already done by caller)

The deployer assumes it receives a valid `Topology`. The `deploy()` method on
`Topology` calls `validate().bail()?` internally as a safety net.

#### Step 3: Create Namespaces

```rust
for (node_name, _node) in &topology.nodes {
    let ns_name = topology.namespace_name(node_name);
    namespace::create(&ns_name)?;
    created_namespaces.push(ns_name);
}
```

**Rollback:** If any namespace creation fails, delete all previously created ones.

**Edge case:** Check `namespace::exists()` first. If it exists, either fail with
`Error::AlreadyExists` or offer a `--force` option to destroy and recreate.

#### Step 4: Create Bridge Networks

Bridges live in a management namespace (or the host namespace). For each
`topology.networks` entry:

```rust
let host_conn = Connection::<Route>::new().await?;
for (net_name, network) in &topology.networks {
    let bridge_name = format!("{}-{}", topology.lab.prefix(), net_name);
    host_conn.add_link(BridgeLink::new(&bridge_name)).await?;
    if let Some(true) = network.vlan_filtering {
        // Enable VLAN filtering on the bridge
        // (requires sysfs write or bridge netlink attribute)
    }
    host_conn.set_link_up(&bridge_name).await?;
}
```

#### Step 5: Create Veth Pairs

For each link, create a veth pair where each end is in a different namespace:

```rust
for (i, link) in topology.links.iter().enumerate() {
    let ep_a = EndpointRef::parse(&link.endpoints[0]).unwrap();
    let ep_b = EndpointRef::parse(&link.endpoints[1]).unwrap();

    let ns_a = topology.namespace_name(&ep_a.node);
    let ns_b = topology.namespace_name(&ep_b.node);

    // Open namespace fd for the peer end
    let ns_b_fd = namespace::open(&ns_b)?;

    // Get connection for namespace A
    let conn_a = namespace::connection_for::<Route>(&ns_a).await?;

    // Create veth pair: ep_a.iface in ns_a, ep_b.iface in ns_b
    let mut veth = VethLink::new(&ep_a.iface, &ep_b.iface)
        .peer_netns_fd(ns_b_fd.as_raw_fd());

    if let Some(mtu) = link.mtu {
        veth = veth.mtu(mtu);
    }

    conn_a.add_link(veth).await?;
}
```

**Key detail:** `VethLink::peer_netns_fd()` creates the peer end directly in the
target namespace. No need to create-then-move.

#### Step 6: Create Additional Interfaces

For each node, create interfaces declared in `nodes.*.interfaces` based on `kind`:

```rust
let conn = namespace::connection_for::<Route>(&ns_name).await?;

for (iface_name, iface_config) in &node.interfaces {
    match iface_config.kind.as_deref() {
        Some("vxlan") => {
            let mut vxlan = VxlanLink::new(iface_name, iface_config.vni.unwrap());
            if let Some(local) = &iface_config.local {
                vxlan = vxlan.local(local.parse()?);
            }
            if let Some(remote) = &iface_config.remote {
                vxlan = vxlan.remote(remote.parse()?);
            }
            if let Some(port) = iface_config.port {
                vxlan = vxlan.port(port);
            }
            conn.add_link(vxlan).await?;
        }
        Some("bond") => {
            conn.add_link(BondLink::new(iface_name)).await?;
        }
        Some("dummy") => {
            conn.add_link(DummyLink::new(iface_name)).await?;
        }
        Some("vlan") => {
            // Needs parent interface and VLAN ID
            // VlanLink::new(iface_name, parent, vid)
        }
        None => {
            // Loopback or pre-existing interfaces (lo, etc.) — skip creation
        }
        Some(kind) => {
            return Err(Error::invalid_topology(format!(
                "unknown interface kind '{kind}' on node '{node_name}'"
            )));
        }
    }
}

// VRF interfaces
for (vrf_name, vrf_config) in &node.vrfs {
    conn.add_link(VrfLink::new(vrf_name, vrf_config.table)).await?;
}

// WireGuard interfaces
for (wg_name, _wg_config) in &node.wireguard {
    conn.add_link(WireguardLink::new(wg_name)).await?;
}
```

#### Step 7: Assign Interfaces to Bridges/Bonds/VRFs

```rust
// Bridge membership
for (net_name, network) in &topology.networks {
    let bridge_name = format!("{}-{}", topology.lab.prefix(), net_name);
    let host_conn = Connection::<Route>::new().await?;
    let bridge_idx = host_conn.resolve_interface(&bridge_name.into()).await?;

    for member in &network.members {
        let ep = EndpointRef::parse(member).unwrap();
        let ns = topology.namespace_name(&ep.node);
        let conn = namespace::connection_for::<Route>(&ns).await?;
        // Need to move the interface to host ns, set master, move back
        // OR: create the veth peer in the host ns and set master there
        // This needs careful design — see "Bridge Architecture" below
    }
}

// VRF enslavement
for (vrf_name, vrf_config) in &node.vrfs {
    let conn = namespace::connection_for::<Route>(&ns).await?;
    let vrf_idx = conn.resolve_interface(&vrf_name.into()).await?;
    for iface in &vrf_config.interfaces {
        conn.set_link_master(iface, vrf_idx).await?;
    }
}
```

**Bridge Architecture Note:** Bridges connecting multiple namespaces require careful
handling. The bridge itself lives in one namespace (host or management). Each node
gets a veth pair: one end in the node namespace, the other end in the bridge namespace
attached to the bridge. This means network members produce veth pairs just like
point-to-point links.

#### Step 9: Set Interface Addresses

```rust
// From links
for link in &topology.links {
    if let Some(addresses) = &link.addresses {
        for (j, ep_str) in link.endpoints.iter().enumerate() {
            let ep = EndpointRef::parse(ep_str).unwrap();
            let ns = topology.namespace_name(&ep.node);
            let conn = namespace::connection_for::<Route>(&ns).await?;
            let (ip, prefix) = parse_cidr(&addresses[j])?;
            let idx = conn.resolve_interface(&ep.iface.into()).await?;
            conn.add_address_by_index(idx, ip, prefix).await?;
        }
    }
}

// From explicit interfaces
for (node_name, node) in &topology.nodes {
    let ns = topology.namespace_name(node_name);
    let conn = namespace::connection_for::<Route>(&ns).await?;
    for (iface_name, iface_config) in &node.interfaces {
        for addr_str in &iface_config.addresses {
            let (ip, prefix) = parse_cidr(addr_str)?;
            // For "lo", use the loopback index (1)
            let idx = if iface_name == "lo" {
                1 // loopback is always index 1
            } else {
                conn.resolve_interface(&iface_name.into()).await?
            };
            conn.add_address_by_index(idx, ip, prefix).await?;
        }
    }
}
```

#### Step 10: Bring Interfaces Up

```rust
for (node_name, _node) in &topology.nodes {
    let ns = topology.namespace_name(node_name);
    let conn = namespace::connection_for::<Route>(&ns).await?;
    let links = conn.get_links().await?;
    for link in links {
        conn.set_link_up_by_index(link.index).await?;
    }
}
```

#### Step 11: Apply Sysctls

```rust
for (node_name, node) in &topology.nodes {
    let ns = topology.namespace_name(node_name);
    let sysctls = topology.effective_sysctls(node);
    if !sysctls.is_empty() {
        let pairs: Vec<(&str, &str)> = sysctls.iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        namespace::set_sysctls(&ns, &pairs)?;
    }
}
```

#### Step 12: Add Routes

```rust
for (node_name, node) in &topology.nodes {
    let ns = topology.namespace_name(node_name);
    let conn = namespace::connection_for::<Route>(&ns).await?;

    for (dest, route_config) in &node.routes {
        let route = if dest == "default" {
            // Default route: 0.0.0.0/0 or ::/0
            Ipv4Route::new("0.0.0.0", 0)
        } else {
            let (addr, prefix) = parse_cidr(dest)?;
            Ipv4Route::from_addr(addr, prefix)
        };

        if let Some(gw) = &route_config.via {
            route = route.gateway(gw.parse()?);
        }
        if let Some(dev) = &route_config.dev {
            route = route.dev(dev);
        }
        if let Some(metric) = route_config.metric {
            route = route.metric(metric);
        }

        conn.add_route(route).await?;
    }
}
```

#### Step 13: Apply Firewall Rules

```rust
for (node_name, node) in &topology.nodes {
    if let Some(fw) = topology.effective_firewall(node) {
        let ns = topology.namespace_name(node_name);
        let conn = namespace::connection_for::<Route>(&ns).await?;

        // Create table
        let table = Table::new("nlink-lab", Family::Inet);
        conn.add_table(table).await?;

        // Create input chain with policy
        let policy = match fw.policy.as_deref() {
            Some("drop") => Policy::Drop,
            _ => Policy::Accept,
        };
        let chain = Chain::new("nlink-lab", "input")
            .family(Family::Inet)
            .hook(Hook::Input)
            .priority(Priority::Filter)
            .chain_type(ChainType::Filter)
            .policy(policy);
        conn.add_chain(chain).await?;

        // Add rules
        for fw_rule in &fw.rules {
            let mut rule = Rule::new("nlink-lab", "input")
                .family(Family::Inet);
            // Parse match expression and action
            // This needs a mini-parser for firewall rule syntax
            // e.g., "tcp dport 80" → .match_tcp_dport(80)
            // e.g., "ct state established,related" → .match_ct_state(...)
            conn.add_rule(rule).await?;
        }
    }
}
```

**Note:** The firewall rule `match` expression parsing is complex. For MVP, support
a limited set of common patterns:
- `"tcp dport <port>"` → `.match_tcp_dport(port)`
- `"udp dport <port>"` → `.match_udp_dport(port)`
- `"icmp"` → ICMP match
- `"ct state established,related"` → conntrack match

#### Step 14: Apply Netem Impairments

```rust
for (endpoint_str, impairment) in &topology.impairments {
    let ep = EndpointRef::parse(endpoint_str).unwrap();
    let ns = topology.namespace_name(&ep.node);
    let conn = namespace::connection_for::<Route>(&ns).await?;

    let mut netem = NetemConfig::new();

    if let Some(delay) = &impairment.delay {
        netem = netem.delay(parse_duration(delay)?);
    }
    if let Some(jitter) = &impairment.jitter {
        netem = netem.jitter(parse_duration(jitter)?);
    }
    if let Some(loss) = &impairment.loss {
        netem = netem.loss(parse_percent(loss)?);
    }
    if let Some(rate) = &impairment.rate {
        netem = netem.rate_bps(parse_rate_bps(rate)?);
    }
    if let Some(corrupt) = &impairment.corrupt {
        netem = netem.corrupt(parse_percent(corrupt)?);
    }
    if let Some(reorder) = &impairment.reorder {
        netem = netem.reorder(parse_percent(reorder)?);
    }

    conn.add_qdisc(&ep.iface, netem).await?;
}
```

**Helpers needed:**
- `parse_duration("10ms") -> Duration` — parse time strings (ms, us, s)
- `parse_percent("0.1%") -> f64` — parse percentage strings
- `parse_rate_bps("100mbit") -> u64` — parse rate strings (bit, kbit, mbit, gbit)

#### Step 15: Apply Rate Limits

```rust
for (endpoint_str, rate_limit) in &topology.rate_limits {
    let ep = EndpointRef::parse(endpoint_str).unwrap();
    let ns = topology.namespace_name(&ep.node);
    let conn = namespace::connection_for::<Route>(&ns).await?;

    let mut limiter = RateLimiter::new(&ep.iface);
    if let Some(egress) = &rate_limit.egress {
        limiter = limiter.egress(egress)?;
    }
    if let Some(ingress) = &rate_limit.ingress {
        limiter = limiter.ingress(ingress)?;
    }
    limiter.apply(&conn).await?;
}
```

**Conflict with netem:** If an interface has both an impairment (netem qdisc) and a
rate limit, they need to coexist. Options:
1. Netem first (root), rate limit as child class
2. Use netem's built-in `rate` parameter instead of separate rate limiter
3. Fail if both are configured on the same interface

For MVP: if both impairment and rate_limit are on the same interface, use netem's
`.rate_bps()` for egress and skip the separate `RateLimiter` for that interface.
Log a warning if `rate_limits.*.ingress` is set alongside an impairment.

#### Step 16: Spawn Background Processes

```rust
let mut pids = Vec::new();
for (node_name, node) in &topology.nodes {
    let ns = topology.namespace_name(node_name);
    for exec_config in &node.exec {
        if exec_config.cmd.is_empty() {
            continue;
        }
        let mut cmd = Command::new(&exec_config.cmd[0]);
        cmd.args(&exec_config.cmd[1..]);

        if exec_config.background {
            let child = namespace::spawn(&ns, cmd)?;
            pids.push((node_name.clone(), child.id()));
        } else {
            let output = namespace::spawn_output(&ns, cmd)?;
            if !output.status.success() {
                return Err(Error::deploy_failed(format!(
                    "exec on node '{}' failed: {}",
                    node_name,
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }
    }
}
```

#### Step 18: Write State

Delegate to `state.rs` (see Part 3).

### Rollback on Failure

If any step fails, the deployer must clean up everything it created:

```rust
async fn deploy_inner(topology: &Topology) -> Result<RunningLab> {
    let mut cleanup = Cleanup::new();

    // Step 3
    for (node_name, _) in &topology.nodes {
        let ns = topology.namespace_name(node_name);
        namespace::create(&ns)?;
        cleanup.add_namespace(ns.clone());
    }

    // ... more steps ...

    // If we get here, deployment succeeded — disarm cleanup
    cleanup.disarm();
    Ok(running_lab)
}

struct Cleanup {
    namespaces: Vec<String>,
    pids: Vec<u32>,
    armed: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed { return; }
        for pid in &self.pids {
            let _ = kill(*pid, Signal::SIGKILL);
        }
        for ns in &self.namespaces {
            let _ = namespace::delete(ns);
        }
    }
}
```

### Value Parsing Helpers (`helpers.rs`)

**File:** `crates/nlink-lab/src/helpers.rs` (new)

These convert human-friendly strings from TOML into values nlink expects:

```rust
/// Parse "10ms", "100us", "1s" → Duration
pub fn parse_duration(s: &str) -> Result<Duration>;

/// Parse "0.1%", "5%" → f64 (0.1, 5.0)
pub fn parse_percent(s: &str) -> Result<f64>;

/// Parse "100mbit", "1gbit", "10kbit" → u64 (bits per second)
pub fn parse_rate_bps(s: &str) -> Result<u64>;

/// Parse "10.0.0.1/24" → (IpAddr, u8)
pub fn parse_cidr(s: &str) -> Result<(IpAddr, u8)>;
```

**Supported duration units:** `ns`, `us`, `ms`, `s`
**Supported rate units:** `bit`, `kbit`, `mbit`, `gbit`, `bps`, `kbps`, `mbps`, `gbps`

---

## Part 2: RunningLab (`running.rs`)

### Struct Design

```rust
pub struct RunningLab {
    topology: Topology,
    namespace_names: HashMap<String, String>,  // node_name -> ns_name
    pids: Vec<(String, u32)>,                   // (node_name, pid)
    state_saved: bool,
}
```

### Methods

#### `exec` — Run command in node namespace

```rust
impl RunningLab {
    pub async fn exec(
        &self,
        node: &str,
        cmd: &str,
        args: &[&str],
    ) -> Result<ExecOutput> {
        let ns = self.namespace_for(node)?;
        let mut command = Command::new(cmd);
        command.args(args);
        let output = namespace::spawn_output(&ns, command)?;
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
```

#### `spawn` — Start background process in node

```rust
pub async fn spawn(&mut self, node: &str, cmd: &[&str]) -> Result<u32> {
    let ns = self.namespace_for(node)?;
    let mut command = Command::new(cmd[0]);
    command.args(&cmd[1..]);
    let child = namespace::spawn(&ns, command)?;
    let pid = child.id();
    self.pids.push((node.to_string(), pid));
    Ok(pid)
}
```

#### `set_impairment` — Modify netem at runtime

```rust
pub async fn set_impairment(
    &self,
    endpoint: &str,
    impairment: &Impairment,
) -> Result<()> {
    let ep = EndpointRef::parse(endpoint)
        .ok_or_else(|| Error::InvalidEndpoint { endpoint: endpoint.to_string() })?;
    let ns = self.namespace_for(&ep.node)?;
    let conn = namespace::connection_for::<Route>(&ns).await?;

    // Build netem config from impairment
    let netem = build_netem(impairment)?;

    // Try change first (if qdisc exists), then add
    match conn.change_qdisc(&ep.iface, "root", netem.clone()).await {
        Ok(()) => Ok(()),
        Err(_) => conn.add_qdisc(&ep.iface, netem).await.map_err(Into::into),
    }
}
```

#### `destroy` — Tear down the lab

```rust
pub async fn destroy(self) -> Result<()> {
    // 1. Kill background processes
    for (_node, pid) in &self.pids {
        let _ = kill_process(*pid);
    }

    // 2. Delete namespaces (this also destroys all interfaces within them)
    for (_node_name, ns_name) in &self.namespace_names {
        if namespace::exists(ns_name) {
            namespace::delete(ns_name)?;
        }
    }

    // 3. Delete bridge interfaces in host namespace (if any)
    let host_conn = Connection::<Route>::new().await?;
    for (net_name, _) in &self.topology.networks {
        let bridge_name = format!("{}-{}", self.topology.lab.prefix(), net_name);
        let _ = host_conn.del_link(&bridge_name).await; // Ignore if already gone
    }

    // 4. Remove state file
    state::remove(&self.topology.lab.name)?;

    Ok(())
}
```

#### `load` — Reconstruct from saved state

```rust
pub async fn load(name: &str) -> Result<Self> {
    let (lab_state, topology) = state::load(name)?;
    Ok(Self {
        topology,
        namespace_names: lab_state.namespaces,
        pids: lab_state.pids,
        state_saved: true,
    })
}
```

#### `list` — List running labs

```rust
pub fn list() -> Result<Vec<LabInfo>> {
    state::list()
}

pub struct LabInfo {
    pub name: String,
    pub node_count: usize,
    pub created_at: String,  // ISO 8601
}
```

---

## Part 3: State Management (`state.rs`)

### State Directory

```
$XDG_STATE_HOME/nlink-lab/labs/<name>/     (or ~/.local/state/nlink-lab/labs/<name>/)
  state.json      # Machine-readable lab state
  topology.toml   # Copy of the topology used for deployment
```

### State Format

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct LabState {
    pub name: String,
    pub created_at: String,           // ISO 8601
    pub namespaces: HashMap<String, String>,  // node_name -> namespace_name
    pub pids: Vec<(String, u32)>,     // (node_name, pid)
    pub topology_hash: String,        // SHA-256 of topology TOML
}
```

### Functions

```rust
/// Get the state directory path for a lab.
pub fn state_dir(name: &str) -> PathBuf;

/// Save lab state and topology.
pub fn save(state: &LabState, topology_toml: &str) -> Result<()>;

/// Load lab state and topology.
pub fn load(name: &str) -> Result<(LabState, Topology)>;

/// List all saved labs.
pub fn list() -> Result<Vec<LabInfo>>;

/// Remove a lab's state directory.
pub fn remove(name: &str) -> Result<()>;

/// Check if a lab state exists.
pub fn exists(name: &str) -> bool;
```

---

## Part 4: Topology Extension Methods

**File:** `crates/nlink-lab/src/types.rs` (add methods)

```rust
impl Topology {
    /// Deploy this topology. Validates first, then creates the lab.
    pub async fn deploy(&self) -> Result<RunningLab> {
        self.validate().bail()?;
        deploy::deploy(self).await
    }
}
```

---

## Progress

### Value Parsing Helpers (`helpers.rs`)

- [ ] `parse_duration("10ms") -> Duration`
- [ ] `parse_percent("0.1%") -> f64`
- [ ] `parse_rate_bps("100mbit") -> u64`
- [ ] `parse_cidr("10.0.0.1/24") -> (IpAddr, u8)`
- [ ] Unit tests for each parser (edge cases: "0ms", "100%", "0bit", bad input)

### Deployer Core (`deploy.rs`)

- [ ] `pub async fn deploy(topology: &Topology) -> Result<RunningLab>`
- [ ] `Cleanup` struct with `Drop` rollback
- [ ] Step 3: Create namespaces with rollback
- [ ] Step 5: Create veth pairs across namespaces
- [ ] Step 9: Set addresses (from links and explicit interfaces)
- [ ] Step 10: Bring all interfaces up per namespace
- [ ] Step 11: Apply sysctls per namespace
- [ ] Step 12: Add routes per namespace (IPv4 + IPv6, default route handling)
- [ ] Step 14: Apply netem impairments
- [ ] Step 15: Apply rate limits (handle conflict with netem)
- [ ] Step 16: Spawn background processes
- [ ] Step 18: Write state file

### Bridge Networks (MVP stretch — can defer)

- [ ] Step 4: Create bridge interfaces in host namespace
- [ ] Step 7: Create veth pairs for bridge members, attach to bridge
- [ ] Step 8: Configure VLANs on bridge ports (PVID, tagged/untagged)

### Firewall (MVP stretch — can defer)

- [ ] Step 13: Create nftables table and chain per node
- [ ] Firewall rule match expression parser (subset: tcp/udp dport, icmp, ct state)
- [ ] Apply firewall rules

### Additional Interface Types (MVP stretch — can defer)

- [ ] Step 6a: VXLAN interfaces
- [ ] Step 6b: Bond interfaces
- [ ] Step 6c: VLAN sub-interfaces
- [ ] Step 6d: WireGuard interfaces (key generation for `private_key = "auto"`)
- [ ] Step 6e: VRF interfaces + enslavement
- [ ] Step 6f: Dummy interfaces

### RunningLab (`running.rs`)

- [ ] `RunningLab` struct
- [ ] `ExecOutput` struct
- [ ] `exec(node, cmd, args) -> Result<ExecOutput>`
- [ ] `spawn(node, cmd) -> Result<u32>`
- [ ] `set_impairment(endpoint, impairment) -> Result<()>`
- [ ] `destroy(self) -> Result<()>` — kill PIDs, delete namespaces, remove bridges, remove state
- [ ] `load(name) -> Result<RunningLab>` — reconstruct from saved state
- [ ] `list() -> Result<Vec<LabInfo>>`
- [ ] `namespace_for(node) -> Result<&str>` — helper to look up namespace name

### State Management (`state.rs`)

- [ ] `LabState` struct with `Serialize + Deserialize`
- [ ] `LabInfo` struct
- [ ] `state_dir(name) -> PathBuf`
- [ ] `save(state, topology_toml) -> Result<()>`
- [ ] `load(name) -> Result<(LabState, Topology)>`
- [ ] `list() -> Result<Vec<LabInfo>>`
- [ ] `remove(name) -> Result<()>`
- [ ] `exists(name) -> bool`

### Topology Extension

- [ ] `impl Topology { pub async fn deploy(&self) -> Result<RunningLab> }`

### Public API Updates (`lib.rs`)

- [ ] Add `pub mod deploy;`
- [ ] Add `pub mod running;`
- [ ] Add `pub mod state;`
- [ ] Add `pub mod helpers;`
- [ ] Re-export `RunningLab`, `ExecOutput`, `LabInfo`

### Tests — Unit

- [ ] `parse_duration` tests (ms, us, s, ns, bad input)
- [ ] `parse_percent` tests (integer, decimal, bad input)
- [ ] `parse_rate_bps` tests (bit, kbit, mbit, gbit, bad input)
- [ ] `parse_cidr` tests (v4, v6, bad input, missing prefix)
- [ ] State save/load round-trip
- [ ] State list with multiple labs

### Tests — Integration (require root/CAP_NET_ADMIN)

- [ ] Deploy minimal 2-node topology, verify namespaces exist
- [ ] Deploy with addresses, verify `ip addr` output in namespace
- [ ] Deploy with routes, verify `ip route` output in namespace
- [ ] Deploy with impairments, verify netem qdisc exists
- [ ] Deploy with sysctls, verify sysctl values in namespace
- [ ] Exec command in deployed node
- [ ] Spawn background process, verify PID
- [ ] Destroy cleans up all namespaces
- [ ] Destroy kills background processes
- [ ] Deploy failure triggers rollback (e.g., duplicate namespace)
- [ ] Re-deploy after destroy works cleanly
- [ ] State file created and loadable after deploy
