# Container DSL Improvement Report

A detailed analysis of nlink-lab's container support — what exists, what
competitors offer, and concrete proposals for improvement.

## 1. Current State

nlink-lab supports 4 container properties on nodes:

```nll
node server image "nginx:latest" {
    cmd ["nginx", "-g", "daemon off;"]
    env ["PORT=8080", "DEBUG=true"]
    volumes ["/data:/data", "/logs:/logs:ro"]
    route default via ${router.eth0}
}
```

That's it. Under the hood, containers are created with `--network=none --privileged`
and all networking is managed via netlink (same as bare namespaces). This is the
right architecture — but the DSL surface is thin.

### What works well

- **Unified networking**: Containers and namespaces share the same link/address/route
  model. A container node looks identical to a namespace node from the network
  perspective.
- **Runtime detection**: Auto-detects docker/podman, or explicit selection via
  `lab "name" { runtime "docker" }`.
- **Image pulling**: Automatic pull-on-demand with local cache check.

### What's missing

| Feature | Containerlab | Docker Compose | Kubernetes | nlink-lab |
|---------|-------------|----------------|------------|-----------|
| Resource limits (CPU/memory) | `cpu`, `memory` | `deploy.resources` | `resources.limits` | -- |
| Capabilities | via kind defaults | `cap_add`/`cap_drop` | `securityContext` | Always `--privileged` |
| Health checks | `startup-delay` | `healthcheck` | 3 probe types | -- |
| Post-deploy exec | `exec` list | -- | `lifecycle.postStart` | -- |
| Entrypoint override | via kind | `entrypoint` | `command` | -- |
| Working directory | -- | `working_dir` | `workingDir` | -- |
| User/UID | `user` | `user` | `securityContext.runAsUser` | -- |
| Hostname | implicit | `hostname` | `hostname` | -- |
| Restart policy | -- | `restart` | `restartPolicy` | -- |
| Pull policy | -- | `pull_policy` | `imagePullPolicy` | Always pull-if-missing |
| Labels | `labels` | `labels` | `metadata.labels` | -- |
| Startup config injection | `startup-config` | `configs` | ConfigMap mount | -- |
| env from file | `env-files` | `env_file` | `envFrom` | -- |
| Device mounts | -- | `devices` | -- | -- |
| Sysctls (container-level) | -- | `sysctls` | -- | via nlink (namespace-level) |
| Dependency ordering | -- | `depends_on` | init containers | -- |

## 2. Competitive Analysis

### Containerlab

The most relevant competitor. Their container config is YAML-based:

```yaml
nodes:
  router:
    kind: linux
    image: frrouting/frr:latest
    startup-config: configs/router.cfg
    memory: 2g
    cpu: 1.5
    binds:
      - configs/frr.conf:/etc/frr/frr.conf:ro
    env:
      FRR_OPTS: "--daemon"
    exec:
      - "vtysh -c 'show ip route'"
    ports:
      - "8080:80"
```

**Key patterns to adopt:**

1. **`exec` list**: Post-deploy commands run inside the container. This is
   extremely useful for one-shot setup (installing packages, configuring
   services) without rebuilding images.

2. **`memory`/`cpu`**: Simple resource limits as top-level properties, not
   nested under a `resources` block.

3. **`startup-config`**: A file mounted and executed at container start. For
   the `linux` kind, this is just a shell script. Simple and effective.

### Docker Compose

The richest container configuration model:

```yaml
services:
  web:
    image: nginx
    cap_add: [NET_ADMIN, NET_RAW]
    cap_drop: [ALL]
    sysctls:
      net.ipv4.ip_forward: 1
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost"]
      interval: 10s
      timeout: 5s
      retries: 3
    deploy:
      resources:
        limits:
          cpus: '0.5'
          memory: 512M
```

**Key patterns to adopt:**

1. **`cap_add`/`cap_drop`**: Fine-grained capabilities instead of blanket
   `--privileged`. Most lab nodes only need `NET_ADMIN` and `NET_RAW`.

2. **`healthcheck`**: Know when a container is actually ready, not just running.

3. **`sysctls`**: Container-level sysctls (our namespace-level approach
   already handles this via nlink, but explicit container sysctls have
   different semantics for containerized daemons).

### Kathara

The simplest model — and sometimes the best:

```
router/
  etc/frr/frr.conf          # overlay file placed at /etc/frr/frr.conf
  etc/network/interfaces     # overlay file placed at /etc/network/interfaces
router.startup               # shell script executed at boot
```

**Key pattern to adopt:**

1. **Overlay directory**: Instead of listing individual bind mounts, just
   create a directory tree that mirrors the container filesystem. Every file
   in `node_name/` gets overlaid into the container. This is dramatically
   more intuitive than explicit volume listings.

## 3. Proposals

### Tier 1: High impact, low effort

#### 3.1 Resource Limits (`cpu`, `memory`)

**Problem**: Can't constrain container resource usage. Large labs exhaust
host resources.

**Syntax**:
```nll
node router image "frr:latest" {
    cpu 1.5
    memory 512m
}
```

**Implementation**:
- Lexer: Add `Cpu`, `Memory` tokens (or reuse existing typed literals)
- AST: Add `NodeProp::Cpu(String)` and `NodeProp::Memory(String)` variants
- Types: Add `cpu: Option<String>` and `memory: Option<String>` to `Node`
- Container: Pass `--cpus` and `--memory` to docker/podman create
- Parser: Parse after other node properties

**Effort**: Low — 4 files changed, straightforward plumbing.

#### 3.2 Post-Deploy Exec (`exec` with background support)

nlink-lab already has `run` for processes inside namespace nodes. But for
container nodes, `run` uses namespace exec — not `docker exec`. The
existing `exec` support in deploy.rs (line 871) already handles container
nodes separately using `docker exec`.

**Problem**: The current `run` syntax works but has two issues:
1. No way to run a one-shot setup command (install package, configure service)
   that doesn't need to stay running
2. Container commands should use `docker exec` for full container FS access

The existing `run` in NLL already supports both `background` and foreground
modes. The deploy code at line 871 already dispatches container commands
through `docker exec`. So this is **already working** for the basic case.

**What's missing**: A dedicated `exec` keyword for post-deploy one-shot
commands (distinct from `run` which is a tracked process).

**Syntax**:
```nll
node router image "frr:latest" {
    exec "apk add iperf3"
    exec "sysctl -w net.ipv4.ip_forward=1"
    run "nginx" background
}
```

**Implementation**:
- AST: Add `NodeProp::Exec(Vec<String>)` — one-shot commands
- Deploy: Execute after container start, before link setup
- Distinct from `run` which creates tracked background processes

**Effort**: Low — the container exec path already exists in deploy.rs.

#### 3.3 Capabilities (`cap_add`, `cap_drop`, `privileged`)

**Problem**: All containers run with `--privileged`. This is a security
concern and isn't necessary for most lab nodes.

**Syntax**:
```nll
# Default: only NET_ADMIN + NET_RAW (sufficient for most networking)
node host image "alpine"

# Explicit capabilities
node router image "frr:latest" {
    cap_add [NET_ADMIN, NET_RAW, SYS_PTRACE]
}

# Full privileges (backward compat)
node debugger image "ubuntu" {
    privileged
}
```

**Default behavior change** (breaking): Switch from `--privileged` to
`--cap-add=NET_ADMIN --cap-add=NET_RAW` as the default. Nodes that need
full privileges must explicitly say `privileged`.

Most lab containers only need `NET_ADMIN` (for ip/tc/nftables commands)
and `NET_RAW` (for ping/iperf). The blanket `--privileged` flag grants
full host access which is unnecessary and risky.

**Implementation**:
- Lexer: Add `Privileged`, `CapAdd`, `CapDrop` tokens
- AST/Types: Add capability fields to Node
- Container: Build `--cap-add`/`--cap-drop` flags from config
- Default: `[NET_ADMIN, NET_RAW]` when no explicit caps and not privileged

**Effort**: Medium — requires deciding on default capability set.

### Tier 2: Medium impact, medium effort

#### 3.4 Health Checks / Readiness

**Problem**: No way to know when a container is actually ready (control plane
booted, daemon listening). Tests fail intermittently because they run before
the service is ready.

**Syntax**:
```nll
node router image "frr:latest" {
    healthcheck "pgrep zebra" interval 5s timeout 30s
}
```

Or for simple delay-based readiness:
```nll
node router image "frr:latest" {
    startup-delay 10s
}
```

**Implementation**:
- Lexer: Add `Healthcheck`, `Interval`, `Timeout`, `StartupDelay` tokens
- Types: Add `healthcheck` field to Node
- Deploy: After container start, poll health check command until success
  or timeout. Only proceed to link setup after all health checks pass.
- CLI: `nlink-lab wait <lab>` already exists — integrate health check status

**Effort**: Medium — need polling loop and timeout handling in deploy.

#### 3.5 Startup Config File

**Problem**: Configuring services inside containers requires either
rebuilding the image or listing individual volume mounts.

**Syntax**:
```nll
node router image "frr:latest" {
    config "configs/frr.conf" "/etc/frr/frr.conf"
    config "configs/daemons" "/etc/frr/daemons"
}
```

Or using a directory overlay (Kathara-style):
```nll
node router image "frr:latest" {
    overlay "configs/router/"    # maps to container root
}
```

The overlay approach mounts a host directory tree into the container,
mirroring its structure. A file at `configs/router/etc/frr/frr.conf`
appears at `/etc/frr/frr.conf` inside the container.

**Implementation**:
- Lexer: Add `Config`, `Overlay` tokens
- Types: Add config/overlay fields to Node
- Container: Convert to `--volume` flags with appropriate paths
- For `config`: each entry becomes `--volume host:container:ro`
- For `overlay`: recursively enumerate files and create bind mounts

**Effort**: Medium — overlay requires directory walking.

#### 3.6 Pull Policy

**Problem**: Default behavior always checks local cache then pulls.
Sometimes you want to force a pull (CI) or never pull (air-gapped).

**Syntax**:
```nll
node router image "frr:latest" {
    pull always     # always pull, even if local
    # pull never    # never pull, fail if not local
    # pull missing  # default: pull only if not local
}
```

**Implementation**:
- Lexer: Add `Pull`, `Always`, `Never`, `Missing` tokens (or use strings)
- Types: Add `pull_policy: Option<PullPolicy>` to Node
- Container: Skip or force `ensure_image()` based on policy

**Effort**: Low.

### Tier 3: Nice-to-have

#### 3.7 Hostname

```nll
node router image "frr:latest" {
    hostname "core-router-01"
}
```

Sets `--hostname` on container create. Default: node name.

#### 3.8 Working Directory

```nll
node app image "node:18" {
    workdir "/app"
    cmd ["npm", "start"]
}
```

Sets `--workdir` on container create.

#### 3.9 Entrypoint Override

```nll
node debug image "ubuntu" {
    entrypoint "/bin/bash"
    cmd ["-c", "while true; do sleep 3600; done"]
}
```

Sets `--entrypoint` on container create. Currently only `cmd` is supported.

#### 3.10 Labels

```nll
node router image "frr:latest" {
    labels [
        "nlink.role=router",
        "nlink.tier=core"
    ]
}
```

Sets `--label` on container create. Useful for filtering with `docker ps`.

#### 3.11 Env from File

```nll
node app image "myapp" {
    env-file "configs/app.env"
}
```

Reads environment variables from a file (one `KEY=VALUE` per line).
Passes `--env-file` to docker/podman.

#### 3.12 Dependency Ordering

```nll
node db image "postgres:16" {
    healthcheck "pg_isready" interval 2s timeout 30s
}
node app image "myapp" {
    depends-on db
    env ["DATABASE_URL=postgres://db:5432/app"]
}
```

Deploy nodes in dependency order. Wait for health check to pass before
deploying dependent nodes.

**Implementation complexity**: High — requires topological sort of nodes
and changing the sequential deployment to a dependency-aware scheduler.

## 4. Prioritized Roadmap

| Priority | Proposal | Effort | Breaking |
|----------|----------|--------|----------|
| **P0** | 3.1 Resource limits (cpu/memory) | Low | No |
| **P0** | 3.3 Capabilities (cap_add/cap_drop) | Medium | Yes (default changes) |
| **P1** | 3.2 Post-deploy exec | Low | No |
| **P1** | 3.6 Pull policy | Low | No |
| **P1** | 3.9 Entrypoint override | Low | No |
| **P1** | 3.7 Hostname | Low | No |
| **P1** | 3.8 Working directory | Low | No |
| **P1** | 3.10 Labels | Low | No |
| **P2** | 3.5 Startup config / overlay | Medium | No |
| **P2** | 3.4 Health checks | Medium | No |
| **P2** | 3.11 Env from file | Low | No |
| **P3** | 3.12 Dependency ordering | High | No |

### Suggested phases

**Phase A** (1-2 days): P0 + P1 — resource limits, capabilities, exec,
pull policy, entrypoint, hostname, workdir, labels. These are all simple
`--flag value` additions to the container create command.

**Phase B** (2-3 days): P2 — startup config, health checks, env-file.
These require more logic (file handling, polling loops).

**Phase C** (future): P3 — dependency ordering. Requires architectural
changes to the deploy sequence.

## 5. Design Principles for Container DSL

1. **Containers are nodes, not a separate concept.** Container properties
   extend the node, they don't replace it. Routes, firewall, impairments
   all work the same regardless of whether the node is a container or
   bare namespace.

2. **`--network=none` is non-negotiable.** nlink-lab owns the network
   stack. Container runtime networking is disabled. This is the core
   architectural decision that makes containers work like namespaces.

3. **Sensible defaults, explicit overrides.** Default capabilities should
   be `NET_ADMIN + NET_RAW` (not `--privileged`). Default pull policy
   should be `missing`. Default hostname should be the node name.

4. **NLL properties, not YAML nesting.** Container config should use the
   same flat property syntax as other node features. No deeply nested
   resource blocks — just `cpu 1.5` and `memory 512m`.

5. **Profile inheritance applies.** Container properties should be
   inheritable via profiles, enabling patterns like
   `profile web-server { image "nginx"; cpu 0.5; memory 256m }`.
