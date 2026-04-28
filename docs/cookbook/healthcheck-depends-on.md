# Healthcheck-gated container startup with `depends-on`

A multi-tier topology that starts the database first, waits for it
to be ready, *then* starts the application. The lab is fully up
only when every node reports healthy.

## When to use this

- Multi-service test labs where node A's startup depends on node
  B's readiness (database before app, message broker before
  consumers).
- CI gating: the deploy command should not return until the lab is
  actually serving traffic.
- Modeling a production startup ordering for chaos tests.

## Why nlink-lab

containerlab supports startup ordering via `pre-deploy` hooks and
shell scripts. nlink-lab makes ordering declarative — `depends-on
[db]` is a single line, healthchecks are a single line, and the
deploy scheduler topo-sorts and polls in step 16.

## NLL

[`examples/container-lifecycle.nll`](../../examples/container-lifecycle.nll):

```nll
lab "container-lifecycle" {
  description "Container nodes with lifecycle management"
}

profile router { forward ipv4 }

node router : router {
  image "alpine:latest"
  cmd ["sleep", "infinity"]
}

/* Database: starts first; deploy waits for the healthcheck. */
node db image "postgres:16" {
  env ["POSTGRES_PASSWORD=secret"]
  cpu 1
  memory 512m
  healthcheck "pg_isready -U postgres"
  startup-delay 3s
}

/* App: starts after db is healthy. Pip install runs as a
   pre-start exec; app starts only when that succeeds. */
node app image "python:3-slim" {
  cmd ["python", "-m", "http.server", "8080"]
  workdir "/app"
  depends-on [db]
  exec "pip install psycopg2-binary"
}

link router:eth0 -- db:eth0  { subnet 10.0.1.0/24 }
link router:eth1 -- app:eth0 { subnet 10.0.2.0/24 }
```

Three things to notice:

1. **`healthcheck "pg_isready -U postgres"`** — exec'd inside the
   container with the standard interval/timeout/retries (defaults
   apply if not specified).
2. **`startup-delay 3s`** — fixed delay before the first
   healthcheck attempt, for services that need a moment before
   they accept probes.
3. **`depends-on [db]`** — `app` won't start until `db` is healthy.

## Run

Requires Docker or Podman:

```bash
sudo nlink-lab deploy examples/container-lifecycle.nll
```

Deploy logs show the ordering:

```text
step 16/18: spawning processes
  starting db (postgres:16)
  waiting for healthcheck: pg_isready -U postgres
    attempt 1: not yet
    attempt 2: ready
  db is healthy
  starting app (python:3-slim)
    pre-exec: pip install psycopg2-binary
  app is up
```

By the time `deploy` returns, the lab is actually serving:

```bash
sudo nlink-lab exec container-lifecycle app -- curl -fsS http://localhost:8080/
```

### Inspect health state

```bash
nlink-lab containers container-lifecycle
nlink-lab stats container-lifecycle
```

### Check the dependency-ordering edges

```bash
nlink-lab graph container-lifecycle | dot -Tpng > deps.png
```

The graph rendering shows the topo order.

### Tear down

```bash
sudo nlink-lab destroy container-lifecycle
```

## Healthcheck options

```nll
healthcheck "command"            # default: 30s interval, 10s timeout, 3 retries
healthcheck "command" {
  interval 5s
  timeout 2s
  retries 5
}
```

The healthcheck runs inside the container via `docker exec` /
`podman exec`. Exit code 0 = healthy.

## Dependency types

| Form | Meaning |
|------|---------|
| `depends-on [a]` | Wait for `a` to be healthy before starting |
| `depends-on [a, b]` | Wait for both |
| `depends-on [a, b]` (cycle) | Validator rejects with `cyclic-dependencies` error |

The deploy scheduler topo-sorts the dependency graph; any cycle is
caught at validate time.

## CI integration

```bash
sudo nlink-lab deploy --json examples/container-lifecycle.nll
# → exits 0 only when every node is healthy and every healthcheck passes
```

This is the test:

```bash
#!/bin/bash
set -euo pipefail
sudo nlink-lab deploy examples/container-lifecycle.nll
trap 'sudo nlink-lab destroy container-lifecycle' EXIT
sudo nlink-lab exec container-lifecycle app -- curl -fsS http://localhost:8080/
```

If anything along the chain (db boot, healthcheck, app startup,
HTTP probe) fails, the script aborts.

## Variations

- **Backoff on healthcheck failure**: increase `interval` per
  attempt — not yet a first-class NLL feature, but you can model
  it via a `scenario` block.
- **Multi-stage init**: `exec "step1" exec "step2"` runs in
  order before `cmd`.
- **Ephemeral helper containers**: a node with `cmd ["sleep",
  "1"]` and `exec` block is a one-shot init container that runs
  before its dependents. Common pattern for migrations / seed
  data.

## When this is the wrong tool

- If your real production uses Kubernetes, `depends-on` here
  doesn't model k8s init-container semantics exactly. For
  k8s-faithful behavior, use kind/minikube + your real manifests.
- If you need vendor-NOS lifecycle hooks (Junos commit, IOS-XR
  commit), you need that NOS — use containerlab.

## See also

- [NLL: containers + healthchecks](../NLL_DSL_DESIGN.md)
- [`logs --follow`](../cli/logs.md) for tailing container output
- [`stats`](../cli/stats.md) for live CPU/memory
