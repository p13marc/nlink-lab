# nlink-lab JSON output schemas

`--json` output for the four high-traffic CLI commands — schemas
in JSON Schema draft-07. Hand-written; the source of truth is the
code. If a field name disagrees, **the code is correct and the schema
is stale** — please file a fix.

| Command | Schema | Documents |
|---------|--------|-----------|
| `nlink-lab deploy --json` | `deploy.schema.json` | one object per deploy |
| `nlink-lab status --json` (no lab) | `status-list.schema.json` | array of running labs |
| `nlink-lab status --scan --json` | `status-scan.schema.json` | running labs + orphans + stale |
| `nlink-lab spawn --json` | `spawn.schema.json` | one object per spawn |
| `nlink-lab ps --json` | `ps.schema.json` | array of tracked processes |

The `inspect`, `exec`, `diagnose`, `render`, `diff` and `apply` JSON
shapes are documented inline in each subcommand's `--help`. Open a PR
adding a schema here if you need a contractual interface.

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
