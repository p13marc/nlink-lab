# Healthcheck-gated container startup with `depends-on`

A multi-tier topology that creates the database first, waits for it
to be ready, *then* runs the application's setup. The lab is fully
up only when every healthcheck has passed.

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
deploy topo-sorts the nodes and polls in its Processes stage.

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

/* Database: created first; the deploy waits for its health check,
   which runs inside the container. */
node db image "postgres:16" {
  env ["POSTGRES_PASSWORD=secret"]
  cpu 1
  memory 512m
  healthcheck "pg_isready -U postgres"
  startup-delay 3s
}

/* App: created after db. Its `cmd` starts with the container; its
   `exec` runs only once db is healthy. The lab has no internet, so a
   setup step cannot download anything. */
node app image "python:3-slim" {
  cmd ["python", "-m", "http.server", "8080"]
  /* A workdir must already exist in the image: docker creates a
     missing one, podman refuses with "workdir does not exist". */
  workdir "/tmp"
  depends-on [db]
  exec "echo db is up > /tmp/ready"
}

link router:eth0 -- db:eth0  { subnet 10.0.1.0/24 }
link router:eth1 -- app:eth0 { subnet 10.0.2.0/24 }
```

Three things to notice:

1. **`healthcheck "pg_isready -U postgres"`** — runs as `sh -c …`
   inside the container (`docker exec` / `podman exec`), every
   second until it exits 0, for 30 s at most.
2. **`startup-delay 3s`** — a fixed pause before the node's `exec`
   commands and its first healthcheck, for services that need a
   moment before they accept probes.
3. **`depends-on [db]`** — `app` is created after `db`, and its
   `exec` and `run` commands and healthcheck wait until `db`'s
   healthcheck has passed. `app`'s own `cmd` is not held back: it
   starts when the container is created, before the lab's links
   exist, so a `cmd` that needs `db` must retry until it is up.

## Run

Requires Docker or Podman:

```bash
sudo nlink-lab deploy examples/container-lifecycle.nll
```

With `--verbose` the deploy logs each wait:

```text
INFO waiting for healthcheck on 'db': pg_isready -U postgres
INFO healthcheck passed for 'db'
```

By the time `deploy` returns, `db` accepts connections and `app`'s
setup has run:

```bash
sudo nlink-lab exec container-lifecycle app -- cat /tmp/ready
sudo nlink-lab exec container-lifecycle app -- \
  python -c "import urllib.request; print(urllib.request.urlopen('http://localhost:8080/').status)"
```

### Inspect health state

```bash
nlink-lab containers container-lifecycle
nlink-lab stats container-lifecycle
```

### Check the order

```bash
nlink-lab deploy --dry-run examples/container-lifecycle.nll
```

The plan lists `db`'s container before `app`'s, and `db`'s
healthcheck before `app`'s `exec`.

### Tear down

```bash
sudo nlink-lab destroy container-lifecycle
```

## Healthcheck options

```nll-ignore
healthcheck "command"            # probe every 1s, give up after 30s
healthcheck "command" {
  interval 5s                    # time between probes
  timeout 60s                    # how long the deploy waits, in all
  retries 5                      # or give up after 5 failed probes
}
```

The healthcheck runs inside the container via `docker exec` /
`podman exec` (on a namespace node, `sh -c` in its namespace). Exit
code 0 = healthy; a probe still running at the timeout is killed.

## Dependency types

| Form | Meaning |
|------|---------|
| `depends-on [a]` | Created after `a`; `exec`, `run` and healthcheck wait for `a`'s healthcheck |
| `depends-on [a, b]` | Wait for both |
| `depends-on [a, b]` (cycle) | Validator rejects with a `depends-on-cycle` error |

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
sudo nlink-lab exec container-lifecycle app -- cat /tmp/ready
```

If anything along the chain (db boot, healthcheck, app setup,
the final check) fails, the script aborts.

## Variations

- **Backoff on healthcheck failure**: increase `interval` per
  attempt — not yet a first-class NLL feature, but you can model
  it via a `scenario` block.
- **Multi-stage init**: `exec "step1" exec "step2"` runs in
  order once the container is up (after its `cmd` started), and
  a failing step fails the deploy.
- **Helper containers**: a node with `cmd ["sleep", "infinity"]`
  and `exec` steps is an init container whose steps finish before
  its dependents' `exec` and `run` start. Common pattern for
  migrations / seed data. (Its `cmd` must keep the container
  running: an `exec` into an exited container fails.)

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
