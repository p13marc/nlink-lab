# Troubleshooting

This guide covers the most common issues when using nlink-lab.

---

## Permission Errors

**Symptom:** `operation not permitted` when deploying or destroying labs.

nlink-lab requires root privileges (or `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`) to
create network namespaces, veth pairs, and configure interfaces.

```bash
# Deploy with root
sudo nlink-lab deploy examples/simple.nll

# Run tests with root (preserve environment for cargo/rustup)
sudo -E cargo test -p nlink-lab --lib
```

If you use `sudo`, make sure the nlink-lab binary is in root's `$PATH` or use
an absolute path.

---

## Namespace Already Exists

**Symptom:** `namespace "lab-router" already exists` during deploy.

This happens when a previous deploy crashed or was interrupted before cleanup.

```bash
# Check what labs are currently tracked
nlink-lab status

# Force-destroy the stale lab
sudo nlink-lab destroy --force <name>
```

If the state file is gone but namespaces remain, clean up manually:

```bash
sudo ip netns list
sudo ip netns delete <ns-name>
```

Repeat for every namespace belonging to the lab (they share the lab prefix).

---

## NLL Parse Errors

nlink-lab uses miette for rich diagnostics. Errors include source spans pointing
to the exact problem:

```
Error: NLL parse error
  x Expected string literal for lab name
   ,-[examples/broken.nll:1:5]
 1 | lab simple
   :     ^^^^^^
   `----
  help: Wrap the lab name in quotes: lab "simple"
```

**Common mistakes:**

| Problem | Wrong | Correct |
|---------|-------|---------|
| Unquoted lab name | `lab simple` | `lab "simple"` |
| Sysctl with `=` | `sysctl net.ipv4.ip_forward=1` | `sysctl net.ipv4.ip_forward 1` |
| Missing braces | `node router : router` (with properties after) | `node router : router { ... }` |
| Bad endpoint | `router-eth0` | `router:eth0` |
| Missing address separator | `10.0.0.1/24 10.0.0.2/24` | `10.0.0.1/24 -- 10.0.0.2/24` |

Endpoints must always use the `node:interface` format. Interface names follow
Linux conventions (alphanumeric, max 15 characters).

---

## Deploy Failures

### Namespace limit reached

```
Error: exceeded maximum number of user namespaces
```

Check and raise the limit:

```bash
sysctl user.max_user_namespaces
sudo sysctl -w user.max_user_namespaces=65536
```

### Interface name too long

Linux limits interface names to 15 characters (`IFNAMSIZ`). If the lab prefix
plus the interface name exceeds this, deployment fails. Use shorter names or a
shorter lab prefix.

### Duplicate addresses

Each IP address must be unique within the topology. If two nodes share the same
address on the same subnet, the deployer will reject it during validation.

### MTU mismatch

When a link specifies an MTU that conflicts with an overlay (VXLAN adds 50 bytes
of overhead), the deploy may fail or produce silent packet drops. Ensure inner
MTU is at least 50 bytes less than the outer link MTU.

---

## Container Errors

**"no container runtime found"** -- Install Docker or Podman. nlink-lab checks
for `docker` and `podman` in `$PATH`.

**Image pull failures** -- Verify network connectivity and that the image name
is correct. Private registries require prior authentication (`docker login`).

**Container networking** -- Container nodes use `--network=none` automatically.
All networking is handled by nlink-lab through veth pairs into the container's
network namespace. Do not override the network mode.

---

## State Corruption

Lab state is stored in `~/.local/state/nlink-lab/labs/`. Each lab has a TOML
file tracking its namespaces, interfaces, and processes.

If state becomes inconsistent (e.g., after a system crash):

```bash
# Force-destroy to clean up kernel resources
sudo nlink-lab destroy --force <name>

# Remove the corrupted state file
rm -rf ~/.local/state/nlink-lab/labs/<name>
```

Then verify no orphaned namespaces remain:

```bash
sudo ip netns list
# Delete any that match the lab prefix
sudo ip netns delete <orphaned-ns>
```

---

## WireGuard Issues

- **Kernel support:** WireGuard requires Linux 5.6+ for in-kernel support, or
  the `wireguard-dkms` package on older kernels. Check with `modprobe wireguard`.

- **Ephemeral keys:** `key auto` generates keys at deploy time. They are not
  persisted across redeploys. For stable keys, specify them explicitly.

- **Peer references:** WireGuard peer blocks must reference node names that
  exist in the topology. Misspelled names cause a validation error.

---

## Performance

- **Parsing:** 500-node topologies parse in under 1 second.
- **Deployment:** Scales linearly with node count. A 100-node lab typically
  deploys in 2-3 seconds.
- **File descriptors:** Large labs open many sockets. Check your limit with
  `ulimit -n` and raise it if needed (`ulimit -n 65536`).
- **Kernel memory:** Each namespace uses approximately 4KB of kernel memory.
  A 1000-node lab consumes roughly 4MB.

---

## Getting More Information

```bash
# Verbose output during deploy
sudo nlink-lab deploy -v examples/simple.nll

# Validate without deploying
nlink-lab validate examples/simple.nll

# Inspect a running lab
sudo nlink-lab exec <lab> <node> -- ip addr
sudo nlink-lab exec <lab> <node> -- ip route
sudo nlink-lab exec <lab> <node> -- ping -c 1 <target>
```

---

## Apply / Reconcile

### `apply` says "drift detected" but I didn't change anything

`nlink-lab apply --check` exits non-zero if the live lab differs
from the NLL. False positives can happen when:

- **Routes auto-generated post-deploy** drift from the source NLL.
  The auto-routing pass writes inferred routes (default gateways,
  directly-connected subnets) at deploy time but doesn't update
  the topology snapshot. After `apply --check` reports the diff
  once, run `apply` to converge — it's safe.
- **Sysctls touched outside the lab.** If something else on the
  host modified `net.*` sysctls in the namespace, the diff will
  show changes you didn't make. Run `apply` to revert to the NLL
  values.
- **Container image pull updated metadata.** Containers store an
  image-id digest in state. A pull between deploy and apply can
  change it without a topology diff — apply doesn't touch this.

```bash
# Inspect the structured diff before deciding
nlink-lab apply --check --json topology.nll | jq '.diff'
```

### `apply` won't reconcile my spawned process change

By design. `apply` doesn't touch long-running processes — it
would otherwise have to either kill them (data loss) or accept
that the new command is wrong (state divergence). Use:

```bash
nlink-lab kill <lab> <pid>
nlink-lab spawn <lab> <node> -- /usr/bin/new-cmd
```

Or redeploy.

### `apply` rebuilt my whole nftables ruleset for a one-line change

Expected. nftables reconcile is coarse: any change to a node's
firewall or NAT block triggers an atomic `del_table` +
`apply_firewall` + `apply_nat` cycle. The kernel transaction is
atomic — packets in flight are never matched against a half-built
ruleset — but conntrack state is preserved across the swap.

A fully-incremental rule-by-rule reconcile is on the roadmap.

---

## `.nlz` Archive Errors

### `import` reports "checksum mismatch"

The archive was modified after export. Either:

- **Truncated download:** re-download.
- **Edited in place:** archives are immutable by design. Export a
  fresh one from the source NLL.
- **Bit rot on storage:** check the source.

### `import` reports "format version newer than supported"

The archive was produced by a newer nlink-lab whose format you
don't support. Two options:

- **Upgrade nlink-lab.** The archive metadata says which version
  produced it (`nlink-lab inspect <archive>.nlz`).
- **Use `--no-reparse`** to read the bundled `rendered.toml`
  directly. Works as long as the rendered Topology schema didn't
  change incompatibly between versions.

### `import` deploy fails with "address already in use"

The recipient host has a stale lab from a previous import. Check:

```bash
nlink-lab status
nlink-lab status --scan      # also lists orphans
sudo nlink-lab destroy --orphans
```

---

## Scenario / Benchmark

### Scenario step fired late

The scenario engine targets ±100ms timing per step. Drift larger
than that usually means:

- **Long-running validate steps.** A `tcp-connect` with retries
  can block for seconds. Reduce `retries` or `timeout`.
- **Heavy `exec` workloads.** If a step's `exec` spawns a
  process that takes longer than the next step's offset, the
  next step queues. Move long execs into `spawn` (background)
  if the test logic doesn't depend on completion.
- **CPU contention on shared CI runners.** GitHub Actions
  runners are often single-core; 100ms drift is not unusual on
  a busy host.

### Benchmark assertion fails on first run only

The first ping on a freshly-deployed namespace pays for ARP
resolution. Either:

- Add a warm-up ping in a `validate { reach a b }` block
  before the benchmark.
- Increase the assertion threshold by ~5ms to swallow the first
  packet's delay.

---

## Library Use (`#[lab_test]`)

### Test silently passes without running

You're not running as root. The macro skips with a loud banner —
look for `*** SKIPPING #[lab_test] '...' ***` in the test
output. Run with `sudo cargo test` or grant `CAP_NET_ADMIN`+
`CAP_SYS_ADMIN` to the cargo binary.

### Tests interfere with each other

Each `#[lab_test]` gets a unique lab name suffix, but namespaces
share the host kernel. If multiple tests use overlapping address
ranges:

- **Use distinct subnets** per test (10.0.1.0/24, 10.0.2.0/24,
  etc.).
- **Or run with `--test-threads=1`** for serial execution.
- **Or use the `set { … }` macro arg** to parameterize the
  subnet:
  ```rust
  #[lab_test("base.nll", set { subnet = "10.0.1.0/24" })]
  ```

### `cargo test` is slower than expected

Each test deploys + destroys a topology. A 3-node lab takes
200–500ms; a 12-node ring 1–2s. CI runners (especially GitHub
Actions) can double this. To reduce per-test cost:

- **Share a topology across tests** with an explicit
  `Lab::deploy` call in `#[ctor::ctor]` (advanced).
- **Use smaller topologies.** A 2-node lab boots in ~100ms.
- **Profile with `--no-capture`** to see which step is slow.

---

## Common Misconfigurations

### Pings work but iperf3 doesn't

iperf3 binds to a specific interface; check that:
- The server is listening on the namespace's interface (not
  loopback only — pass `-B <iface-ip>` if needed).
- `node.forward ipv4` is set on intermediate routers.
- The sender's MTU isn't oversized (PMTUD blackholes can drop
  iperf3's larger frames silently).

### TCP connections work but UDP doesn't

Often firewall + conntrack:
- A default-drop policy with only `accept ct established,related`
  doesn't match unsolicited UDP — UDP has no handshake, so
  conntrack treats every flow as new.
- Add `accept udp dport <port>` for the specific service.

### A node can ping itself but not the gateway

The route default is missing or points at a wrong gateway. Check:

```bash
sudo nlink-lab exec <lab> <node> -- ip route
```

Common fixes:

- Add `route default via <gw>` to the node block.
- Use `${router.eth0}` cross-references to ensure the gateway
  matches the actual peer address: `route default via ${router.eth0}`.
- Set `routing auto` in the lab block to compute defaults from
  the topology graph.

### Wi-Fi nodes can't see each other

`mac80211_hwsim` is loaded but the radios aren't grouped:
- Wi-Fi nodes share a *radio space* by SSID. Make sure all peers
  declare the same SSID + same passphrase.
- Channel mismatches silently fail. Set `channel` consistently.
- For mesh mode, use `mode mesh` on every node — APs and stations
  don't mesh.

---

## Filing a Bug

A good bug report includes:

1. **Output of `nlink-lab inspect <lab>` or `--show-ips
   <topology.nll>`.**
2. **The NLL** (or the `.nlz` archive — `nlink-lab export
   --archive --include-running-state <lab>`).
3. **Kernel version** (`uname -r`) — many edge cases are
   kernel-version-specific.
4. **What you expected vs. what happened.** "Couldn't ping"
   isn't enough; include the failing command and its output.
5. **Whether it reproduces from a fresh deploy** or only after
   apply / scenario / impair.

For network behavior bugs, also include:

```bash
# A short pcap from the failing path
sudo nlink-lab capture <lab> <ep> -w bug.pcap --duration 5
```

The pcap + the `.nlz` archive are usually enough to repro
locally.
