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
per-endpoint impairments, network-level per-pair impair, per-node
static routes, per-node sysctls, per-endpoint rate-limits, and
per-node nftables / NAT**. Plan 152 Phase B is complete; Phase D
adds full e2e tests. Spawned processes still need explicit
respawn — apply leaves them alone.

## Arguments

| Argument | Description |
|----------|-------------|
| `<TOPOLOGY>` | Path to the updated NLL file. |

## Options

| Flag | Description |
|------|-------------|
| `--dry-run` | Print the diff and what would change; don't make kernel calls. |
| `--json` | Emit a structured report. With `--dry-run`: the schema-v3 envelope (`{schema_version: 3, lab, no_op, change_count, topology: {…}, network: {…}, nftables: {…}, wireguard: {…}}`, see `docs/json-schemas/layered-diff.v3.schema.json`) suitable for CI. Traffic-control changes live in `topology`. |
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

```bash
nlink-lab verify --file topo.nll
# exit 0: no drift
# exit non-zero: drift detected; the diff is printed to stderr
```

Or with structured output:

```bash
nlink-lab --json verify --file topo.nll \
  | jq -e '.no_op == true' \
  || { echo "drift detected"; exit 1; }
# or simply rely on the exit code: `nlink-lab verify --file topo.nll` exits 2 on drift
```

The `--json` flag with `--dry-run` (and `verify --json`) emits the schema-v3 envelope
(`docs/json-schemas/layered-diff.v3.schema.json`): the lab-graph diff,
plus per-namespace typed diffs from nlink keyed by node name, with empty
layers elided.

```json
{
  "schema_version": 3,
  "lab": "check-demo",
  "no_op": false,
  "change_count": 6,
  "topology": {
    "impairments_changed": [
      { "endpoint": "alpha:radio", "old": { "delay": "10ms" }, "new": { "delay": "40ms" } }
    ],
    "nftables_changed": [
      {
        "node": "hq",
        "desired_firewall": {
          "policy": "drop",
          "rules": [
            { "match": "ct state established,related", "action": "accept" },
            { "match": "tcp dport 8080", "action": "accept" }
          ]
        },
        "was_present": true
      }
    ]
  },
  "network": {
    "alpha": {
      "addresses-to-add": [ { "dev": "radio", "address": "172.100.0.3/24" } ],
      "addresses-to-remove": [ { "dev": "radio", "address": "172.100.0.2/24" } ]
    }
  },
  "nftables": {
    "hq": {
      "blocked": [],
      "changes": [
        {
          "type": "rule", "op": "add",
          "family": "inet", "table": "nlink-lab", "chain": "input",
          "key": "nlink-lab/fw/input/1", "handle": null,
          "placement": { "after": 4 },
          "nft": "tcp dport 8080 accept"
        }
      ],
      "generation": 7
    }
  }
}
```

Trimmed from a real run: `network` and `nftables` are nlink's own diffs.
`network` is serialized with kebab-case keys, and every key it defines is
present (empty ones are left out above), among them `links-to-recreate`
(a declared link whose kind parameters changed, see below).

`nftables` is nlink's `NftablesDiff` (nlink 0.33): `changes` lists one
entry per table, chain, rule, set, batch of elements, object or flowtable
it changes. Each names its `type` and its `op`: `add` and `remove` carry
the value's fields, `update` carries `from` (what the kernel holds) and
`to`, and `recreate` adds a `reason` and a `cascade` (a set's elements and
the rules it moves, a chain's rules) that is not listed again. A rule
carries `nft`, the rule as `nft -s list` prints it, and where it goes
(`"append"`, `{"before": handle}`, `{"after": handle}`; a rule moved back
into declared order is an `update` with a placement). `blocked` lists what
the diff cannot do; an entry that refuses (a set that a rule nlink-lab did
not write uses, recreated) fails the apply before anything changes.
`generation` is the ruleset generation the diff was taken from (an apply
of it is refused if the ruleset moved since).

`links-to-remove` lists a declared `vlan`/`vxlan`/`bond`/`vrf`/`dummy`
the topology no longer has, which `apply` deletes. `wireguard` is nlink's
`WireguardConfigDiff` per node: a peer removed by hand shows as

```json
"wireguard": {
  "gw-a": {
    "devices-to-modify": [
      [ "wg0", {
          "private-key-set": false, "listen-port-set": false, "fwmark-set": false,
          "peers-to-add": [
            { "public-key": "4XjmA/FZ+ScYXFEIYPAIoPEqmtZDCfxPsGgrWpUdrjQ=",
              "endpoint": "10.0.0.2:51820",
              "allowed-ips": [ { "addr": "192.168.255.2", "cidr": 32 } ], … }
          ],
          "peers-to-modify": [], "peers-to-remove": []
      } ]
    ],
    "devices-to-add": []
  }
}
```

Keys never appear in it: peers by public key, private and preshared keys
as `*-set` flags.

**Traffic control is in both.** `topology` says which impairment, `qdisc`
block, rate limit or per-pair matrix the file changed; `network` has what
the kernel needs for it: `qdiscs-to-add`/`-replace`/`-remove` and the
`tc` trees (classes, leaves, filters) of each device. A qdisc deleted
by hand shows only under `network`, as drift.

`nlink-lab verify --file FILE` exits 2 when `no_op` is false: the CI drift gate.

### Repair drift

`apply` with the file the lab was deployed from is not a no-op when the
kernel has drifted from it — a rule deleted by hand, an address someone
added, or what a newer nlink-lab plans differently for the same file. It
reports the drift (`topology unchanged; the kernel has drifted from it`)
and converges it, exactly the changes `verify` lists.

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
| Rate limits | ✅ add / replace / remove (a changed limit is removed and re-added — brief unshaped window; removal takes the ingress qdisc and IFB too) |
| Routes | ✅ add / replace / remove via reconcile |
| Sysctls | ✅ add / change in place; removed entries warned (kernel default not auto-restored) |
| nftables / NAT | ✅ per rule, in one atomic batch: an edited rule is replaced in place, a new one inserted where it is declared, rules out of declared order moved; foreign rules left alone |
| Declared interfaces (vlan, vxlan, bond, vrf, dummy) | ✅ in place where the kernel allows it; a changed VLAN id/protocol/parent, VNI, VXLAN port or address family, bond mode, VRF table or kind recreates the link, with its impairment, `qdisc`, rate limit and VRF routes put back. Refused before any change if the link carries state the topology cannot declare (FRR routes, permanent neighbours, hand-added FDB entries) |
| Spawned processes | ❌ — apply leaves them alone; redeploy or use `kill` + `spawn` |
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
- **Plan 152** — the full reconcile rollout

## Reference

<!-- cli-ref:start (generated by `nlink-lab docs-gen`; do not edit) -->

```text
Apply topology changes to a running lab.

Reconciles the live lab with a changed topology, issuing only the deltas. Without FILE, the file the lab was deployed from (or last applied) is read again, with its params. `--dry-run` shows the changes (`--json` for the machine-readable diff); `verify --file FILE` is the CI drift gate.

JSON OUTPUT: { "schema_version": 1, "lab", "ops", "removed", "applied", "duration_ms", "assertions"?, "assertions_failed"? }

Usage: nlink-lab apply [OPTIONS] [TOPOLOGY]

Arguments:
  [TOPOLOGY]
          The changed topology file (`.nll` or `.nlz`). Default: the file the lab was deployed from (or last applied), with its params

Options:
  -L, --lab <LAB>
          The lab. Default: `$NLINK_LAB`, then the one `nlink-lab use` selected, then the only running lab

      --set <KEY=VALUE>
          Set a `param` value (repeatable): --set wan_delay=50ms. Default: the values the lab was deployed with, when the file is too

      --dry-run
          Show what would change without applying (`verify --file FILE` is the CI drift gate)

      --strict
          Fail (exit 2) when any `validate { … }` assertion fails after the changes are applied. The lab stays as applied for inspection

      --skip-validate
          Do not run the topology's `validate { … }` assertions after applying

      --reset-impairments
          Drop the impairments set at runtime with `nlink-lab impair` and converge on the topology's `impair` declarations. Without this, a runtime impairment survives `apply` unless the topology changes that endpoint

  -h, --help
          Print help (see a summary with '-h')
```

<!-- cli-ref:end -->
