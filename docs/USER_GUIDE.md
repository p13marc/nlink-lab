# nlink-lab User Guide

A 60-minute walkthrough from "I just installed this" to "I have a
realistic site-to-site WAN with WireGuard, firewall, mid-test
chaos injection, and a `cargo test` integration." Built around
one topology that grows, not 18 disconnected snippets.

If you want a feature reference instead of a tutorial, jump to
[NLL by Example](#nll-by-example) below or read
[`docs/NLL_DSL_DESIGN.md`](NLL_DSL_DESIGN.md).

---

## Install (5 minutes)

Requirements:

- Linux kernel 4.19+ (5.x recommended).
- One of: root, SUID install, or `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`
  capabilities. `CAP_DAC_OVERRIDE` for DNS injection,
  `CAP_SYS_MODULE` for Wi-Fi (mac80211_hwsim auto-load).
- Rust 1.85+ toolchain.

Build and install:

```bash
git clone https://github.com/p13marc/nlink-lab.git
cd nlink-lab

# Option 1: SUID root (recommended — full feature support)
just install

# Option 2: Capabilities only (no SUID)
just install-caps

# Option 3: Manual
cargo build --release -p nlink-lab-cli
sudo install -o root -g root -m 4755 \
    target/release/nlink-lab /usr/local/bin/
```

Shell completions:

```bash
nlink-lab completions bash > /etc/bash_completion.d/nlink-lab
nlink-lab completions zsh  > /usr/share/zsh/site-functions/_nlink-lab
```

Verify:

```bash
nlink-lab --help | head -5
```

---

## Step 1 — A 2-node lab to confirm everything works (5 minutes)

Save this as `wan.nll`. We'll grow this same file across the next
seven steps.

```nll-ignore
lab "wan"

profile router { forward ipv4 }

node site-a-router : router
node site-a-client { route default via 10.0.1.1 }

link site-a-router:lan -- site-a-client:eth0 {
  10.0.1.1/24 -- 10.0.1.10/24
}
```

What this declares:

- **`profile router { forward ipv4 }`** — a reusable template.
  `forward ipv4` is shorthand for `sysctl
  "net.ipv4.ip_forward" "1"`. Profiles avoid repetition when many
  nodes share traits.
- **`node site-a-router : router`** — a network namespace. The
  `: router` inherits the profile.
- **`node site-a-client { route default via 10.0.1.1 }`** — a
  client with a default gateway. nlink-lab adds the route in the
  namespace's routing table at deploy.
- **`link ... { 10.0.1.1/24 -- 10.0.1.10/24 }`** — a veth pair
  connecting the two namespaces, with addresses on each end. The
  syntax mirrors how you'd draw it: `A:iface -- B:iface`.

Now validate, deploy, smoke-test, and tear down:

```bash
nlink-lab validate wan.nll        # syntax + 20 validator rules
sudo nlink-lab deploy wan.nll     # ~200ms; writes state to ~/.nlink-lab/wan/
sudo nlink-lab exec wan site-a-client -- ping -c 3 10.0.1.1
sudo nlink-lab destroy wan
```

The ping succeeds because step 9 of the deploy sequence assigned
addresses, step 10 brought interfaces up, step 12 added routes.
The 18-step sequence is documented in
[ARCHITECTURE.md](ARCHITECTURE.md); the takeaway here is that
deploy is **declarative** — you describe state, nlink-lab makes
it real.

---

## Step 2 — Two sites with a WAN link (10 minutes)

Now grow the file. Add a second site and a WAN link between the
two routers:

```nll-ignore
lab "wan" { dns hosts }

profile router { forward ipv4 }

# Site A
node site-a-router : router
node site-a-client { route default via 10.0.1.1 }

# Site B
node site-b-router : router
node site-b-server { route default via 10.0.2.1 }

# LAN: each site
link site-a-router:lan -- site-a-client:eth0 {
  10.0.1.1/24 -- 10.0.1.10/24
}
link site-b-router:lan -- site-b-server:eth0 {
  10.0.2.1/24 -- 10.0.2.10/24
}

# WAN: between the two routers
link site-a-router:wan -- site-b-router:wan {
  10.255.0.1/30 -- 10.255.0.2/30
}
```

Two new things:

- **`dns hosts`** in the lab block — auto-generates `/etc/hosts`
  entries in every namespace so you can `ping site-b-server` by
  name instead of by IP. Convenience, not infrastructure.
- **The WAN link** — a third veth between the two routers.

But pinging across will still fail:

```bash
sudo nlink-lab deploy wan.nll
sudo nlink-lab exec wan site-a-client -- ping -c 2 10.0.2.10
# 100% packet loss
```

The router doesn't know how to reach `10.0.2.0/24` yet. Add the
return-path routes:

```nll-ignore
node site-a-router : router {
  route 10.0.2.0/24 via 10.255.0.2
}
node site-b-router : router {
  route 10.0.1.0/24 via 10.255.0.1
}
```

Reconcile **without redeploying**:

```bash
sudo nlink-lab apply wan.nll
```

Now the path is end-to-end:

```bash
sudo nlink-lab exec wan site-a-client -- ping -c 3 site-b-server
# 0% loss
```

Why `apply` matters here: deploy is destructive (it rebuilds the
whole lab), but `apply` is **reconcile** — it diffs the live
state against the new NLL and only issues the deltas. Two
new routes added; nothing else touched. CI loops use `apply` to
keep the lab in sync with the NLL on every change.

---

## Step 3 — WAN impairment (5 minutes)

A real internet path doesn't deliver line-rate packets at zero
delay. Add realistic impairment to the WAN side:

```nll-ignore
link site-a-router:wan -- site-b-router:wan {
  10.255.0.1/30 -- 10.255.0.2/30
  delay 30ms jitter 5ms loss 0.5% rate 50mbit
}
```

`apply` again:

```bash
sudo nlink-lab apply wan.nll
sudo nlink-lab exec wan site-a-client -- ping -c 5 site-b-server
```

The pings now show ~30ms RTT with occasional drops — netem in
action. Properties available: `delay`, `jitter`, `loss`,
`rate`, `corrupt`, `reorder`. Standalone form:

```nll-ignore
impair site-a-router:wan delay 30ms jitter 5ms
```

For asymmetric paths (satellite uplink: fast down, slow up), use
directional arrows:

```nll-ignore
link ground:sat -- satellite:sat {
  172.16.0.1/30 -- 172.16.0.2/30
  -> rate 50mbit    # ground → satellite (uplink)
  <- rate 150mbit   # satellite → ground (downlink)
}
```

For per-pair impair on a shared L2 (3+ peers all sharing a
bridge, each pair seeing different latency), see the
[satellite-mesh cookbook recipe](cookbook/satellite-mesh.md).

---

## Step 4 — Encrypt the inter-site link with WireGuard (10 minutes)

The WAN underlay carries traffic in cleartext today. Add a
WireGuard tunnel terminating on each router:

```nll-ignore
node site-a-router : router {
  route 10.0.2.0/24 via 10.255.0.2
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [site-b-router]
  }
}

node site-b-router : router {
  route 10.0.1.0/24 via 10.255.0.1
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.2/32
    peers [site-a-router]
  }
}
```

`key auto` generates an X25519 keypair at deploy time. The
`peers [site-b-router]` declaration is **symmetric** — when
nlink-lab lowers this, it pairs up the two `wireguard` blocks,
fills in each peer's public key, and writes the configuration
via the kernel WireGuard interface (genetlink) directly. No
manual key copying, no `wg setconf`, no `exec:` hooks.

Apply and verify:

```bash
sudo nlink-lab apply wan.nll
sudo nlink-lab exec wan site-a-router -- wg show
```

You'll see `site-b-router`'s public key listed as a peer with a
recent handshake.

For the customer traffic to actually use the tunnel, point the
inter-site routes at `wg0` instead of the underlay:

```nll-ignore
node site-a-router : router {
  route 10.0.2.0/24 dev wg0
  wireguard wg0 { ... }
}
node site-b-router : router {
  route 10.0.1.0/24 dev wg0
  wireguard wg0 { ... }
}
```

Apply, ping, observe encrypted traffic between the two routers.

---

## Step 5 — Stateful firewall on the server (10 minutes)

Site B's server should accept only HTTP and SSH from the trusted
LAN. Add a stateful nftables policy:

```nll-ignore
node site-b-server {
  route default via 10.0.2.1

  firewall policy drop {
    accept ct established,related      # return traffic
    accept tcp dport 80                # HTTP open to anyone
    accept tcp dport 22 src 10.0.1.0/24  # SSH from site-a LAN only
  }
}
```

`apply`. Verify:

```bash
# Spawn a tiny HTTP server inside the namespace
sudo nlink-lab spawn wan site-b-server -- \
    python3 -m http.server 80 --bind 0.0.0.0

# A wait-for: block until the port accepts connections
sudo nlink-lab wait-for wan site-b-server --tcp 0.0.0.0:80 --timeout 5

# From site-a-client: HTTP works (port 80 accept)
sudo nlink-lab exec wan site-a-client -- curl -fsS site-b-server/

# From site-a-client: blocked port times out (default-drop, no RST)
sudo nlink-lab exec wan site-a-client -- timeout 2 nc -v site-b-server 9999
echo "exit code: $?"   # nonzero — firewall dropped (no RST, just timeout)
```

`apply` of a firewall edit triggers an atomic flush+rebuild of the
node's nftables table. The kernel never sees a half-built ruleset;
conntrack state is preserved across the swap.

---

## Step 6 — Mid-test chaos with the scenario engine (10 minutes)

Now the question: how does this whole thing behave when the WAN
link goes down?

Add a `scenario` block. It declares a timeline of fault-injection
actions, fired by the scenario engine within ±100ms of each
declared offset:

```nll-ignore
scenario "wan-flap" {
  at 0s {
    log "baseline: WAN reachable"
    validate { reach site-a-client site-b-server }
  }

  at 3s {
    log "bringing the WAN underlay down on site-b"
    down site-b-router:wan
  }

  at 5s {
    validate { no-reach site-a-client site-b-server }
  }

  at 10s {
    log "healing"
    up site-b-router:wan
  }

  at 15s {
    log "asserting recovery"
    validate { reach site-a-client site-b-server }
  }
}
```

Run it:

```bash
sudo nlink-lab apply wan.nll
sudo nlink-lab scenario run wan wan-flap
```

The scenario runner emits a timeline:

```text
[0.001s] log: baseline: WAN reachable
[0.024s] validate: reach site-a-client site-b-server ✓
[3.001s] log: bringing the WAN underlay down on site-b
[3.005s] down site-b-router:wan
[5.001s] validate: no-reach site-a-client site-b-server ✓
[10.001s] log: healing
[10.004s] up site-b-router:wan
[15.001s] validate: reach site-a-client site-b-server ✓

scenario "wan-flap" PASSED in 15.022s
```

If any `validate` fails, the scenario aborts and exits non-zero.
This is the difference between a real test and a bash script
that calls `iptables` and prays — declarative, validated,
reproducible.

For deeper exploration see the
[partition cookbook recipe](cookbook/p2p-partition.md).

---

## Step 7 — Run as a CI gate (5 minutes)

For CI, the all-in-one verb is `nlink-lab test`. It deploys,
runs the validate block + scenarios, then destroys, in one
shot:

```bash
sudo nlink-lab test wan.nll
```

```text
PASS  wan.nll
  topology: 4 nodes, 3 links, 1 scenario
  deploy:   0.3s
  validate: 4 assertions ✓
  scenario "wan-flap": PASSED (15.0s)
  destroy:  0.2s
TOTAL: 1 passed, 0 failed
```

JUnit XML for CI dashboards:

```bash
sudo nlink-lab test --junit results.xml wan.nll
```

For library-first testing — the wedge nlink-lab has against
containerlab — add a `#[lab_test]`-driven Rust test:

```rust
use nlink_lab::lab_test;
use nlink_lab::RunningLab;

#[lab_test("wan.nll", capture = true, timeout = 30)]
async fn wan_recovers_from_partition(lab: RunningLab) {
    let result = lab.run_scenario("wan-flap").await.unwrap();
    assert!(result.passed());
}
```

`capture = true` quietly captures pcaps on every interface for
the test duration. On panic, the pcaps land at
`target/lab_test_captures/<test>-<pid>/`; on success, they're
discarded. CI flake → pcap auto-attached to the failure record.

See the [Rust integration test cookbook](cookbook/rust-integration-test.md).

---

## Step 8 — Tear down or share (5 minutes)

When you're done:

```bash
sudo nlink-lab destroy wan
```

To send the lab to a coworker (bug repro, "look at this"):

```bash
nlink-lab export --archive wan.nll -o wan-repro.nlz
```

The recipient runs:

```bash
nlink-lab inspect wan-repro.nlz       # summary, no extract
sudo nlink-lab import wan-repro.nlz   # extract → validate → deploy
```

The `.nlz` archive is a gzipped tarball with the NLL, params,
rendered Topology snapshot, and SHA-256 checksums. Format
versioning lets older nlink-lab read newer archives; checksums
catch bit rot and tampering. See
[Cookbook: lab portability](cookbook/lab-portability.md).

---

## What's next

You've now used roughly half the language and most of the
tooling. From here:

- **More cookbook recipes**: [VRF](cookbook/vrf-multitenant.md),
  [macvlan](cookbook/macvlan-host-bridge.md),
  [container nodes](cookbook/healthcheck-depends-on.md),
  [satellite mesh](cookbook/satellite-mesh.md).
- **Reference**: [NLL by Example](#nll-by-example) below covers
  all 18 NLL features in compact form. The full grammar is in
  [`docs/NLL_DSL_DESIGN.md`](NLL_DSL_DESIGN.md).
- **CLI reference**: every command has a page in
  [`docs/cli/`](cli/).
- **Architecture**: [`ARCHITECTURE.md`](ARCHITECTURE.md) is the
  contributor on-ramp.
- **vs containerlab**: [`COMPARISON.md`](COMPARISON.md) is the
  honest comparison.

If something didn't work, [TROUBLESHOOTING.md](TROUBLESHOOTING.md)
covers the common failure modes and how to file a useful bug
report.

---

## NLL by Example

### 1. Profiles and IP Forwarding

Profiles are reusable node templates. Nodes inherit with `:`.

```nll-ignore
profile router { forward ipv4 }
profile dual-stack { forward ipv4  forward ipv6 }

node r1 : router
node r2 : dual-stack
```

`forward ipv4` is shorthand for `sysctl "net.ipv4.ip_forward" "1"`. Use `sysctl` for arbitrary kernel parameters.

### 2. Routes

```nll-ignore
node host {
  route default via 10.0.0.1
  route 192.168.0.0/16 via 10.0.0.2 metric 100
  route 10.1.0.0/24 dev eth1
}
```

Routes support `via`, `dev`, and `metric` modifiers.

### 3. Link Impairments (Symmetric)

Impairments placed directly in a link block apply to both directions.

```nll-ignore
link r1:wan0 -- r2:wan0 {
  172.16.0.1/30 -- 172.16.0.2/30
  delay 30ms jitter 5ms loss 0.1% rate 50mbit
}
```

Available properties: `delay`, `jitter`, `loss`, `rate`, `corrupt`, `reorder`.

Standalone form:

```nll-ignore
impair switch:br0 delay 5ms jitter 1ms
```

See `examples/wan-impairment.nll`.

### 4. Asymmetric Impairments

Use `->` and `<-` for per-direction impairments. `->` applies to the left endpoint, `<-` to the right.

```nll-ignore
link ground:sat0 -- satellite:sat0 {
  172.16.0.1/30 -- 172.16.0.2/30
  -> delay 270ms jitter 10ms rate 50mbit    # ground to satellite
  <- delay 270ms jitter 10ms rate 150mbit   # satellite to ground
}
```

See `examples/asymmetric.nll`.

### 5. For Loops and Variables

```nll-ignore
let N = 4
let base = 10.0

for i in 1..${N} {
  node r${i} : router { lo 10.255.0.${i}/32 }
}

for i in 1..3 {
  let next = ${i} + 1
  link r${i}:eth1 -- r${next}:eth0 {
    ${base}.${i}.1/30 -- ${base}.${i}.2/30
  }
}
```

Ranges are inclusive: `1..4` produces 1, 2, 3, 4. Interpolation with `${...}` works in names, addresses, and values. Expressions support `+`, `-`, `*`, `/`.

See `examples/spine-leaf.nll` for a full datacenter fabric using nested loops.

### 6. Firewall (nftables)

```nll-ignore
node server {
  firewall policy drop {
    accept ct established,related
    accept tcp dport 8080
    accept udp dport 53
    drop tcp dport 22
  }
}
```

Rules are applied via nftables. The `policy` sets the default chain action.

See `examples/firewall.nll`.

### 7. Bridge Networks with VLANs

```nll-ignore
network fabric {
  vlan-filtering
  members [switch:br0, host1:eth0, host2:eth0]
  vlan 100 "sales"
  vlan 200 "engineering"
  port host1:eth0 { pvid 100  untagged }
  port host2:eth0 { vlans [100, 200]  tagged }
}
```

See `examples/vlan-trunk.nll`.

### 8. WireGuard Tunnels

```nll-ignore
node gw-a : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
  route 192.168.2.0/24 dev wg0
}
```

`key auto` generates a keypair at deploy time. Peers reference other nodes by name; public keys are exchanged automatically.

See `examples/wireguard-vpn.nll`.

### 9. VRF Multi-Tenancy

```nll-ignore
node pe : router {
  vrf red table 10 {
    interfaces [eth1, eth2]
    route default dev eth1
  }
  vrf blue table 20 {
    interfaces [eth3]
    route default dev eth3
  }
}
```

Each VRF gets its own routing table. Interfaces are bound to a VRF at deploy time.

See `examples/vrf-multitenant.nll`.

### 10. Containers

```nll-ignore
node router image "alpine:latest" cmd "sleep infinity"
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
}
```

Nodes with `image` run as containers (Docker or Podman) instead of bare namespaces. Container nodes and namespace nodes can be mixed freely.

See `examples/container.nll`.

### 11. Imports for Composition

File `base-network.nll`:

```nll-ignore
lab "base"
profile router { forward ipv4 }
node r1 : router
node r2 : router
link r1:eth0 -- r2:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
```

File `composed.nll`:

```nll-ignore
import "base-network.nll" as dc

lab "composed"

node host { route default via 10.1.0.1 }

link dc.r1:eth1 -- host:eth0 {
  10.1.0.1/24 -- 10.1.0.10/24
}
```

Imports must appear before the `lab` declaration. All imported names are prefixed with the alias (`dc.r1`, `dc.r2`). Imports can be recursive; circular imports are rejected.

See `examples/imports/`.

### 12. Subnet Pools

Named pools eliminate manual address planning:

```nll-ignore
pool fabric 10.0.0.0/16 /30
pool access 10.1.0.0/16 /24

link s1:e1 -- l1:e1 { pool fabric }   # 10.0.0.1/30 -- 10.0.0.2/30
link s1:e2 -- l2:e1 { pool fabric }   # 10.0.0.5/30 -- 10.0.0.6/30
link l1:e3 -- h1:e0 { pool access }   # 10.1.0.1/24 -- 10.1.0.2/24
```

Subnets are allocated sequentially. Pool exhaustion is an error at parse time.

### 13. Topology Patterns

Generate common topologies in a single statement:

```nll-ignore
mesh cluster { node [a, b, c, d]; pool links }     # full mesh
ring backbone { count 6; pool backbone }             # ring
star campus { hub router; spokes [s1, s2, s3] }      # hub-and-spoke
```

Patterns expand to regular nodes and links during lowering. Use `nlink-lab render` to see the expanded topology.

### 14. Reachability Assertions

Declare post-deploy connectivity checks in the topology:

```nll-ignore
validate {
    reach host1 host2        # host1 can ping host2
    no-reach host1 host3     # firewall should block this
}
```

Assertions run automatically after deploy. Use `--skip-validate` to disable.

### 15. Nested Interpolation

Inner `${}` expressions are resolved first, enabling dynamic references:

```nll-ignore
for i in 1..4 {
    node host${i} { route default via ${router${i}.eth0} }
}
```

### 16. Render Output Modes

```bash
nlink-lab render topology.nll          # expanded flat NLL (default)
nlink-lab render --json topology.nll   # JSON
nlink-lab render --dot topology.nll    # Graphviz DOT graph
nlink-lab render --ascii topology.nll  # text summary
```

### 17. Management Network

Auto-create an out-of-band management bridge connecting all nodes:

```nll-ignore
lab "mylab" {
    mgmt 172.20.0.0/24
}
```

All nodes get a `mgmt0` interface with a sequential IP from the subnet.

### 18. Container Management

```bash
nlink-lab containers mylab               # list container nodes
nlink-lab logs mylab web --follow        # stream logs
nlink-lab stats mylab                    # live CPU/memory
sudo nlink-lab restart mylab web         # restart one container
nlink-lab pull topology.nll              # pre-pull all images
```

---

## CLI Reference

### Commands

| Command | Description |
|---------|-------------|
| `deploy` | Deploy a lab from a topology file |
| `destroy` | Tear down a running lab |
| `apply` | Apply topology changes to a running lab |
| `status` | Show running labs or details of a specific lab |
| `exec` | Run a command inside a lab node |
| `validate` | Validate a topology file without deploying |
| `impair` | Modify link impairment at runtime |
| `capture` | Capture packets on an interface (tcpdump) |
| `diagnose` | Run diagnostics on a lab |
| `daemon` | Start the Zenoh backend daemon for a running lab |
| `metrics` | Stream live metrics from a lab via Zenoh |
| `init` | Create a topology file from a built-in template |
| `graph` | Print topology as DOT graph |
| `render` | Expand loops/variables/imports and print flat NLL (`--json`, `--dot`, `--ascii`) |
| `ps` | List processes running in a lab |
| `kill` | Kill a tracked background process |
| `wait` | Wait for a lab to be ready |
| `diff` | Compare two topology files |
| `export` | Export a running lab's topology |
| `shell` | Open an interactive shell in a lab node |
| `inspect` | Combined status + links + impairments view (`--json`) |
| `containers` | List container nodes with ID, image, PID |
| `logs` | Show container logs (`--follow`, `--tail`) |
| `pull` | Pre-pull all container images from a topology |
| `stats` | Show live container CPU/memory usage |
| `restart` | Restart a single container node |
| `completions` | Generate shell completions |

### deploy

```bash
sudo nlink-lab deploy topology.nll [--dry-run] [--force] [--daemon]
```

- `--dry-run` -- validate and show the deployment plan without executing it.
- `--force` -- destroy any existing lab with the same name before deploying.
- `--daemon` -- start the Zenoh metrics daemon after deployment.

### destroy

```bash
sudo nlink-lab destroy <lab-name> [--force]
```

`--force` continues cleanup even if some resources are already gone.

### exec

```bash
sudo nlink-lab exec <lab> <node> -- <command> [args...]
```

Everything after `--` is passed to the command. Examples:

```bash
sudo nlink-lab exec mylab router -- ip route show
sudo nlink-lab exec mylab host -- iperf3 -c 10.0.0.1 -t 10
sudo nlink-lab exec mylab host -- bash
```

### status

```bash
nlink-lab status [<lab-name>] [--json]
```

Without a lab name, lists all running labs. With a name, shows detailed node and link information. `--json` for machine-readable output.

### apply

```bash
sudo nlink-lab apply topology.nll [--dry-run]
```

Hot-reload: compares the new topology against the running lab and applies only the differences. Use `--dry-run` to preview changes.

### impair

```bash
sudo nlink-lab impair <lab> <endpoint> --delay 50ms --jitter 5ms --loss 1%
sudo nlink-lab impair <lab> --show
sudo nlink-lab impair <lab> <endpoint> --clear
```

Modify netem parameters at runtime without redeploying. `--show` displays current impairments on all interfaces. `--clear` removes all impairments from an endpoint.

### capture

```bash
sudo nlink-lab capture <lab> <endpoint> [-w capture.pcap] [-c 100] [-f "tcp port 80"]
```

Runs tcpdump inside the node's namespace. `-w` writes to pcap file. `-c` limits packet count. `-f` sets BPF filter.

### daemon

```bash
sudo nlink-lab daemon <lab> [--interval 2] [--zenoh-mode peer] [--zenoh-listen tcp/0.0.0.0:7447]
```

Starts the Zenoh backend. Publishes per-interface metrics at the specified interval. See "Daemon Mode and TopoViewer" below.

### metrics

```bash
nlink-lab metrics <lab> [--node router] [--format table] [--count 10] [--zenoh-connect tcp/127.0.0.1:7447]
```

Subscribes to live metrics from a running daemon. Does not require root. `--count` exits after N samples. `--format json` for machine-readable output.

### init

```bash
nlink-lab init <template> [-o ./output/] [-n my-lab] [--force]
nlink-lab init --list
```

Creates a topology file from a built-in template. Use `--list` to see available templates.

---

## Runtime Operations

### Modify Impairments at Runtime

No redeployment needed. Change delay, jitter, loss, or rate on any interface:

```bash
sudo nlink-lab impair mylab router:wan0 --delay 100ms --loss 5%
sudo nlink-lab impair mylab router:wan0 --clear
sudo nlink-lab impair mylab --show
```

### Packet Capture

Capture traffic on any interface:

```bash
sudo nlink-lab capture mylab router:eth0 -w /tmp/router-eth0.pcap -f "icmp"
```

Open the pcap with Wireshark or tcpdump for analysis.

### Diagnostics

```bash
sudo nlink-lab diagnose mylab
sudo nlink-lab diagnose mylab router
```

Checks interface state, address assignment, route tables, and connectivity for all nodes or a specific node.

### Hot-Reload with Apply

Edit the topology file and apply changes to a running lab:

```bash
# Add a new node or change impairments in the .nll file, then:
sudo nlink-lab apply topology.nll --dry-run   # preview
sudo nlink-lab apply topology.nll             # apply
```

### Process Management

```bash
sudo nlink-lab ps mylab                       # list background processes
sudo nlink-lab kill mylab <pid>               # kill a specific process
```

Background processes are those started with `run background [...]` in the topology.

### Spawn and Wait-For

Start services after deployment and wait for readiness:

```bash
# Spawn a background process (tracked by ps/kill)
sudo nlink-lab spawn mylab server -- /usr/bin/my-service --port 8080

# Wait for TCP port to accept connections
sudo nlink-lab wait-for mylab server --tcp 127.0.0.1:8080 --timeout 30
# Port-only shorthand resolves to 127.0.0.1 inside the namespace
# Use the full IP if the service binds to a specific interface address
sudo nlink-lab wait-for mylab server --tcp 8080 --timeout 30

# Wait for a command to succeed
sudo nlink-lab wait-for mylab server --exec "curl -sf http://localhost:8080/health"

# Wait for a file to exist
sudo nlink-lab wait-for mylab server --file /var/run/service.pid
```

All background processes (both `run background` in NLL and `nlink-lab spawn`)
automatically capture stdout/stderr to log files.

**Default log location:** `~/.local/state/nlink-lab/labs/{lab}/logs/`
**File naming:** `{node}-{command}-{pid}.stdout` and `.stderr`

Use `--log-dir` to override the default location:

```bash
sudo nlink-lab spawn mylab server --log-dir /tmp/logs -- my-service
nlink-lab logs mylab --pid 12345                # view stdout
nlink-lab logs mylab --pid 12345 --stderr       # view stderr
nlink-lab logs mylab --pid 12345 --tail 50      # last 50 lines
```

Log paths are also included in `nlink-lab ps --json` output (`stdout_log`, `stderr_log` fields).

### Exec with JSON Output

Get structured output from commands:

```bash
sudo nlink-lab exec --json mylab host -- ping -c1 10.0.0.1
# {"exit_code": 0, "stdout": "...", "stderr": "", "duration_ms": 1023}
```

### Node IP Discovery

Query node addresses without parsing topology JSON:

```bash
nlink-lab ip mylab server                      # all interfaces
nlink-lab ip mylab server --iface eth0         # bare IP (10.0.0.1)
nlink-lab ip mylab server --iface eth0 --cidr  # with prefix (10.0.0.1/24)
nlink-lab ip mylab server --iface mgmt0        # management IP (host-reachable)
nlink-lab ip --json mylab server               # JSON output
```

### Asymmetric Impairments

Apply different impairments per direction:

```bash
sudo nlink-lab impair mylab router:wan0 --out-delay 50ms --in-delay 200ms
sudo nlink-lab impair mylab router:wan0 --out-loss 0% --in-loss 5%
```

### Partition and Heal

Simulate network partitions with automatic baseline preservation:

```bash
sudo nlink-lab impair mylab router:wan0 --partition  # 100% loss, saves baseline
# ... test failure detection ...
sudo nlink-lab impair mylab router:wan0 --heal       # restores original impairments
```

**Semantics:**
- `--partition` saves the current netem config for the endpoint to the state file, then replaces it with 100% packet loss
- `--heal` restores the saved config (e.g., original `delay 50ms` from the NLL file). If no impairment existed before partition, heal clears the qdisc entirely
- Double partition is a no-op — the original saved config is preserved, not overwritten
- Partition/heal operates on a single endpoint (unidirectional). For bidirectional partition, call it on both endpoints

### CLI Parameter Passing

Parameterize topologies without separate files per scenario:

```nll-ignore
param wan_delay default 10ms
param wan_loss default 0%

link a:eth0 -- b:eth0 { delay ${wan_delay} loss ${wan_loss} }
```

```bash
nlink-lab deploy topology.nll --set wan_delay=50ms --set wan_loss=0.1%
nlink-lab validate topology.nll --set wan_delay=100ms
nlink-lab render topology.nll --set wan_delay=300ms
```

### Host-Reachable Management Network

Create a management bridge in the root namespace so host processes can reach lab nodes directly:

```nll-ignore
lab "my-lab" {
    mgmt 172.20.0.0/24 host-reachable
}
```

After deploy, the bridge IP is `.1` and nodes get `.2`, `.3`, etc.:

```bash
curl http://172.20.0.2:8080/health  # connect from host to lab node
```

---

## Templates

List all templates:

```bash
nlink-lab init --list
```

| Template | Description | Nodes | Key Features |
|----------|-------------|-------|--------------|
| `simple` | Two nodes with one link and netem impairment | 2 | veth, addresses, routes, netem |
| `router` | Router between two subnets with IP forwarding | 3 | profiles, ip-forwarding, default-routes |
| `spine-leaf` | Datacenter fabric: 2 spines, 2 leaves, 2 servers | 6 | profiles, loopback, multi-hop |
| `wan` | Two sites over impaired WAN link | 4 | delay, loss, rate-limiting, jitter |
| `firewall` | Server behind a stateful nftables firewall | 3 | nftables, conntrack, policy |
| `vlan-trunk` | Bridge with VLAN filtering, trunk and access ports | 3 | bridge, vlan-filtering, pvid, tagged |
| `vrf` | PE router with VRF tenant isolation | 3 | vrf, routing-tables, tenant-isolation |
| `wireguard` | Site-to-site WireGuard VPN tunnel | 2 | wireguard, encryption, tunnel |
| `vxlan` | VXLAN overlay between two VTEPs | 2 | vxlan, overlay, underlay |
| `container` | Alpine container connected to a namespace host | 2 | container, mixed-topology, docker |
| `mesh` | Full mesh of 4 nodes (6 links) | 4 | full-mesh, point-to-point |
| `iperf` | Throughput test with iperf3 and rate limiting | 2 | iperf3, rate-limiting, exec |

Create a lab from a template:

```bash
nlink-lab init spine-leaf -n my-dc -o ./labs/
sudo nlink-lab deploy ./labs/my-dc.nll
```

---

## Daemon Mode and TopoViewer

### Zenoh Backend Daemon

The daemon collects per-interface metrics (rx/tx bytes, packets, errors, drops, bitrates) and publishes them over Zenoh.

Start with deployment:

```bash
sudo nlink-lab deploy topology.nll --daemon
```

Or attach to a running lab:

```bash
sudo nlink-lab daemon mylab --interval 2
```

The daemon publishes on these Zenoh key expressions:

- `nlink-lab/<lab>/metrics/snapshot` -- full metrics snapshot (all nodes, all interfaces)
- `nlink-lab/<lab>/metrics/<node>/<iface>` -- per-interface metrics

### Streaming Metrics

Subscribe from any machine (no root required):

```bash
nlink-lab metrics mylab
nlink-lab metrics mylab --node router --format json --count 5
```

Table output shows per-interface rx/tx rates, packet counts, errors, and drops.

### TopoViewer GUI

The topoviewer is an Iced-based GUI that visualizes the live topology. It connects to the daemon via Zenoh and displays:

- Force-directed graph layout of nodes and links
- Live per-interface throughput metrics on link edges
- Pan, zoom, click-select, and drag interaction
- PNG export of the current view

Launch it while a daemon is running to get a real-time view of your lab's network state.
