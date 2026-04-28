# Parametric `import for_each` for fabrics

Build a 3-spine 8-leaf datacenter fabric, or a 6-router ring, by
instantiating a single parametric module N times. The module is
written once; the topology composes them.

## When to use this

- A topology has repeating units (sites, datacenter racks, customer
  edges) and you want to define each unit once.
- A fabric has a parameterized shape (N spines, M leaves) and the
  parameter changes between test runs.
- Multi-site labs where each site has the same internal structure
  but different external addresses.

## Why nlink-lab

containerlab supports topology generation via Go templates and
`generate`, but the templating language sits outside YAML and
requires shell glue to bind. nlink-lab's `import` is part of the
NLL itself: parameters are declared in the imported file, supplied
by the importing file, and resolved at parse time with the same
typed semantics as everything else.

## Parametric module

A reusable module declares its parameters with `param`:

[`examples/imports/parametric-ring.nll`](../../examples/imports/parametric-ring.nll):

```nll
param count default 4

lab "ring"
profile router { forward ipv4 }

for i in 1..${count} {
  node r${i} : router { lo 10.255.0.${i}/32 }
}

for i in 1..${count} {
  link r${i}:right -- r${i % count + 1}:left {
    10.0.${i}.0/31
  }
}
```

`param count default 4` declares a parameter the importer can
override. The `for` loop generates `count` nodes and `count` links
with modulo wrap.

## One-time import

[`examples/imports/use-ring.nll`](../../examples/imports/use-ring.nll):

```nll
import "parametric-ring.nll" as backbone(count=6)

lab "ring-with-monitor"

node monitor { route default via ${backbone.r1.eth0} }
link backbone.r1:mon0 -- monitor:eth0 { 172.16.0.0/30 }
```

Imported nodes are namespaced under the import alias — `backbone.r1`,
`backbone.r2`, etc. The lab gets six ring routers plus one external
monitor host.

## `for_each` — fleet instantiation

To stamp out the same module N times (e.g., 3 datacenters, each
with the same internal structure), use `for_each`:

```nll
import "parametric-ring.nll" for_each {
  dc1(count=4)
  dc2(count=6)
  dc3(count=4)
}

lab "multi-dc"

# Inter-DC links use the prefixed names
link dc1.r1:wan -- dc2.r1:wan { 172.16.1.0/30 }
link dc2.r1:wan -- dc3.r1:wan { 172.16.2.0/30 }
```

Each instance gets its own prefix. `dc1.r1`, `dc2.r1`, `dc3.r1`
are three separate nodes with three separate ring topologies.

## Run

```bash
nlink-lab validate examples/imports/use-ring.nll
nlink-lab render examples/imports/use-ring.nll  # see expanded NLL
sudo nlink-lab deploy examples/imports/use-ring.nll
sudo nlink-lab exec ring-with-monitor monitor -- ping -c 2 backbone.r1
sudo nlink-lab destroy ring-with-monitor
```

## What `param` accepts

| Form | Description |
|------|-------------|
| `param N` | Required. Importer must supply. |
| `param N default V` | Optional with default. |
| `param N default V`, supplied by importer with `as alias(N=other)` | Override per import. |

Values can be integers (used in `for` ranges, arithmetic),
strings, durations (`50ms`), rates (`100mbit`), or percentages
(`0.5%`).

## CLI `--set` overrides

A topology with `param` declarations can also accept overrides
from the command line:

```nll
# wan.nll
param latency default 50ms
param loss default 0.1%

lab "wan"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
impair a:eth0 delay ${latency} loss ${loss}
```

```bash
sudo nlink-lab deploy --set latency=200ms --set loss=5% wan.nll
```

This is the common parameterized-test pattern — same NLL, different
parameter combinations per CI run. See
[ci-matrix-sweep.md](ci-matrix-sweep.md) for the matrix recipe.

## Cross-references into imported nodes

Once imported, the imported topology's nodes are addressable as
`<alias>.<node>`:

```nll
link dc1.r1:wan -- dc2.r1:wan { ... }     # link between fabrics
node monitor { route default via ${dc1.r1.eth0} }  # use cross-ref for IP
```

The cross-reference syntax `${node.iface}` works across the
import boundary.

## Variations

- **Spine-leaf with parametric depth** — `import "spine-leaf.nll"
  for_each { dc1(spines=2, leaves=4)  dc2(spines=4, leaves=8) }`
- **Multi-tier**: import a `rack.nll` from inside a `dc.nll`,
  which is in turn imported from a `region.nll`. Imports nest
  arbitrarily; each level prefixes the names of the level below.
- **Conditional shape**: combine `param` + `if`:
  ```nll
  param redundant default 0
  if ${redundant} == 1 {
    link r1:redundant -- r2:redundant { ... }
  }
  ```

## What nlink-lab does at parse time

1. Parses the importing file.
2. For each `import` (or each instance of `for_each`), reads the
   imported NLL.
3. Substitutes the parameters into the imported AST.
4. Prefixes every node/network/link name with the alias.
5. Merges into the importing topology.

All eagerly at parse time. The validator sees the merged
topology; the deployer sees only the final flat form.

## When this is the wrong tool

- If you need *runtime* dynamism (new nodes appearing while the
  lab is running), `import` doesn't help — it's a parse-time
  expansion. Use `apply` after editing.
- If your fabric is generated from a service catalog (CMDB,
  Terraform state, etc.), generate the NLL via a script in your
  CI rather than fighting `param` to model dynamic data.

## See also

- [NLL: imports](../NLL_DSL_DESIGN.md#2-imports)
- [`render`](../cli/render.md) — see the post-import flat NLL
