# `nlink-lab apply`

Reconcile a running lab to an edited NLL file — without redeploying.

## Usage

```text
nlink-lab apply [OPTIONS] <TOPOLOGY>
```

## Description

Diffs the running lab against the desired NLL, then issues only the
add/change/del operations needed to converge. Unchanged links keep
their state; live ping flows continue without packet loss for
unchanged paths.

`apply` is the canonical "edit and reconcile" verb — the alternative
to `destroy` + `deploy`, which incurs full teardown and rebuild.

The current implementation reconciles **nodes, links,
per-endpoint impairments, network-level per-pair impair, and
per-node static routes**. Other resources (sysctls, nftables, NAT)
currently require redeploy until [Plan 152](../plans/152-apply-reconcile.md)
Phase B finishes.

## Arguments

| Argument | Description |
|----------|-------------|
| `<TOPOLOGY>` | Path to the updated NLL file. |

## Options

| Flag | Description |
|------|-------------|
| `--dry-run` | Print the diff and what would change; don't make kernel calls. |
| `--json` | Emit a structured diff: `{nodes_added, nodes_removed, links_added, links_removed, impairments_changed, ...}`. |
| `-v`, `--verbose` | Print every reconcile step. |
| `-q`, `--quiet` | Suppress non-error output. |

## Examples

### Edit and apply

```bash
$EDITOR examples/wan-impairment.nll      # change `delay 50ms` to `delay 100ms`
sudo nlink-lab apply examples/wan-impairment.nll
```

The link's netem qdisc is updated in-place. Existing connections
stay up.

### Preview a diff before applying

```bash
nlink-lab apply --dry-run examples/wan-impairment.nll
```

```text
  ~ change impair router:wan0: delay 50ms → 100ms
  + add node monitor
  + add link router:mon0 -- monitor:eth0
```

### CI gate — fail if the lab has drifted

(After Plan 152 ships, `apply --check` will be the canonical drift
gate. Today, `apply --dry-run --json` and `jq -e '.no_op == true'`
is the equivalent.)

```bash
nlink-lab apply --dry-run --json topo.nll | jq -e '.no_op == true' \
  || { echo "drift detected"; exit 1; }
```

### Auto-apply in a config-watch loop

```bash
inotifywait -m -e modify topology.nll | while read; do
  sudo nlink-lab apply topology.nll
done
```

## What's reconcilable today

| Resource | Status |
|----------|--------|
| Nodes added | ✅ |
| Nodes removed | ✅ |
| Links added | ✅ |
| Links removed | ✅ |
| Per-endpoint netem | ✅ change in place |
| Network-level per-pair impair | ✅ via `PerPeerImpairer::reconcile()` — zero kernel calls when unchanged |
| Rate limits | 🚧 Plan 152 Phase B |
| Routes | ✅ add / replace / remove via reconcile |
| Sysctls | 🚧 Plan 152 Phase B |
| nftables / NAT | 🚧 Plan 152 Phase B |
| Spawned processes | ❌ — apply leaves them; redeploy or `kill` + `spawn` |
| Container nodes | ❌ — image / cmd changes require redeploy |

Behavior under unsupported changes: `apply` warns and applies what
it can. Use `--json` to see the unhandled diff items.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Apply succeeded (or `--dry-run` and the diff was clean) |
| 1 | Bad NLL or validation failure |
| 2 | Reconcile failed mid-way |
| 3 | Lab not running |
| 4 | Lock contention |

## See also

- [`deploy`](deploy.md) — start fresh
- [`destroy`](destroy.md) — tear down
- [`diff`](diff.md) — diff two NLL files (no kernel involvement)
- [Plan 152](../plans/152-apply-reconcile.md) — the full reconcile rollout
