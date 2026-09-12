# nlink-lab JSON output schemas

`--json` output for the CLI commands with a contractual envelope —
schemas in JSON Schema draft-07. Hand-written; the source of truth is
the code. If a field name disagrees, **the code is correct and the
schema is stale** — please file a fix (generation from the Rust types
via `schemars` is tracked as issue #60).

| Command | Schema | Documents |
|---------|--------|-----------|
| `nlink-lab deploy --json` | `deploy.schema.json` | one object per deploy |
| `nlink-lab status --json` (no lab) | `status-list.schema.json` | array of running labs |
| `nlink-lab status --scan --json` | `status-scan.schema.json` | running labs + orphans + stale |
| `nlink-lab status --json <LAB>` | `status-lab.schema.json` | per-lab topology + addresses + host_resources |
| `nlink-lab spawn --json` | `spawn.schema.json` | one object per spawn |
| `nlink-lab ps --json` | `ps.schema.json` | array of tracked processes |
| `nlink-lab impair --show --json` | `impair-show.schema.json` | per-endpoint qdisc state |
| `nlink-lab proc-stat --json` | `proc-stat.schema.json` | per-process resource snapshot |
| `nlink-lab apply --check --json` / `--dry-run --json` | `layered-diff.v3.schema.json` | schema v3 envelope: `network` + `nftables` typed per-namespace diffs |

The `inspect`, `exec`, `diagnose`, `render` and `diff` JSON shapes are
documented inline in each subcommand's `--help`. Open a PR adding a
schema here if you need a contractual interface. The v1 and v2
`layered-diff` schemas were removed with the 0.7.0 envelope change
(their fields no longer exist in the output).

## Validating output

```bash
nlink-lab status --json | jq . | python -c '
import json, sys
import jsonschema
schema = json.load(open("docs/json-schemas/status-list.schema.json"))
data = json.load(sys.stdin)
jsonschema.validate(data, schema)
print("ok")
'
```

(Same idea with `ajv` or any other JSON Schema validator.)
