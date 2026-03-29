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
