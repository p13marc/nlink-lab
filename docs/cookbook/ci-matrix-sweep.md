# CI matrix: parameter sweeps from a shell loop

Run the same topology N times with different parameter values per
run. Each run is a fresh lab with a unique name, so they can run in
parallel on the same CI runner without colliding.

## When to use this

- You have one test scenario but want to explore a 2D matrix of
  parameters (latency × loss × bandwidth).
- A regression test needs to assert "for any reasonable WAN delay,
  this still works."
- A CI gate that catches edge cases by sweeping the input space.

## Why nlink-lab

This pattern is shell-glue around any lab tool, but nlink-lab makes
the inner loop fast — sub-second deploy means a 25-cell matrix
finishes in under a minute on a laptop. With containerlab's image
pull and Docker overhead, the same matrix is multi-minute.

## Setup

A topology with declared `param`s:

```nll
# wan-test.nll
param latency default 10ms
param loss default 0%
param rate default 1gbit

lab "wan-test"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
impair a:eth0 delay ${latency} loss ${loss} rate ${rate}
```

Validate before sweeping (this is the cheap fail-fast):

```bash
nlink-lab validate wan-test.nll
```

## The sweep

```bash
#!/bin/bash
set -euo pipefail

LATENCIES=(10ms 50ms 200ms 500ms 1000ms)
LOSSES=(0% 0.1% 1% 5%)

failed=()

for latency in "${LATENCIES[@]}"; do
  for loss in "${LOSSES[@]}"; do
    name="wan-${latency//[^0-9]/}-${loss//[^0-9]/}"
    echo "── $latency / $loss ──"

    if ! sudo nlink-lab deploy --json \
         --suffix "${latency}-${loss}" \
         --set "latency=$latency" \
         --set "loss=$loss" \
         wan-test.nll
    then
      failed+=("$name")
      continue
    fi

    # Run the test inside the lab.
    if ! sudo nlink-lab exec "wan-test-${latency}-${loss}" a -- \
         /usr/bin/my-test --target b
    then
      failed+=("$name")
    fi

    sudo nlink-lab destroy "wan-test-${latency}-${loss}"
  done
done

if [ ${#failed[@]} -gt 0 ]; then
  echo "FAILED cells: ${failed[*]}"
  exit 1
fi
```

20 cells. Roughly 20 × (deploy + test + destroy) seconds. Most
networks deploy in under a second; a typical run is well under a
minute.

## Parallelism with `--unique`

To sweep in parallel, give each lab a unique name. `--unique`
appends `-pid<PID>` automatically:

```bash
for latency in 10ms 50ms 200ms; do
  (
    sudo nlink-lab deploy --unique --set latency=$latency wan-test.nll
    name="wan-test-pid$$"   # not exactly $$ — see below
    # ... run test inside, get name from --json output ...
    sudo nlink-lab destroy "$name"
  ) &
done
wait
```

Or use `--suffix STR` if you want predictable names. `--suffix`
and `--unique` are mutually exclusive.

For machine-controlled parallelism, capture the lab name from
`--json`:

```bash
NAME=$(sudo nlink-lab deploy --json --unique wan.nll | jq -r '.lab')
echo "started lab $NAME"
```

## Pitfalls

1. **State directory races.** Each parallel deploy writes to
   `~/.nlink-lab/<name>/`. With `--unique` or distinct
   `--suffix`, this is fine. Without, two processes race the
   lock and one fails — file an issue if this matters for your
   workflow.
2. **veth name length.** Each lab prefixes its bridge / veth
   names. Parallel labs with very long base names can hit
   `IFNAMSIZ` (15 chars). Keep base lab names ≤ 8 chars to leave
   room for the `-pid12345` suffix.
3. **Resource limits.** 20 parallel labs = 20 × N namespaces and
   bridges. Linux's defaults can handle thousands but check
   `sysctl net.core.netdev_max_backlog` and friends if you push
   into the hundreds.
4. **Cleanup on CTRL-C.** Add a `trap 'sudo nlink-lab destroy
   --all' EXIT` to ensure orphaned labs don't pile up between
   runs.

## CI matrix output

For GitHub Actions / GitLab CI, emit JUnit XML and let the runner
render the matrix:

```bash
sudo nlink-lab test --junit results.xml \
  --set latency=10ms wan-test.nll \
  --set latency=50ms wan-test.nll \
  --set latency=500ms wan-test.nll
```

(`test` is the all-in-one verb: deploy + run validate blocks +
destroy.)

## When this is the wrong tool

- For 100+ cells, parallel by `xargs -P` or GNU `parallel` is
  brittle. Use a real CI matrix (GitHub Actions strategy.matrix,
  GitLab parallel:matrix) where each cell is a separate runner job
  invoking nlink-lab with one parameter set.
- For property-based testing (random parameters), generate the
  parameter set in the test framework (proptest, quickcheck) and
  deploy from `#[lab_test]` directly. See
  [TESTING_GUIDE.md](../TESTING_GUIDE.md).

## See also

- [parametric-imports.md](parametric-imports.md) — for
  shape-parametric topologies (count of spines, count of leaves)
- [`deploy --set` and `--unique`](../cli/deploy.md)
- [`test`](../cli/test.md) — the all-in-one CI verb
