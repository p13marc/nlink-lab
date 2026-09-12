# nlink-lab JSON output schemas

`--json` output for the CLI commands with a contractual envelope, as
JSON Schema. Most files are **generated from the Rust types** by
`nlink-lab docs-gen --schemas docs/json-schemas` (`schemars`, issue #60)
and diffed in CI — do not hand-edit those; change the type. The
remaining hand-written ones document `serde_json::json!` payloads that
have no typed struct yet.

| Command | Schema | Source |
|---------|--------|--------|
| `nlink-lab apply --check --json` / `--dry-run --json` / `verify --json` | `layered-diff.v3.schema.json` | generated (`DryRunReport`) |
| `nlink-lab validate --json` | `validate.schema.json` | generated (`ValidateReport`) |
| `nlink-lab validate --list-rules --json` | `validate-rules.schema.json` | generated (`Vec<RuleInfo>`) |
| `nlink-lab status --json` (no lab) | `status-list.schema.json` | generated (`Vec<LabInfo>`) |
| `nlink-lab status --scan --json` | `status-scan.schema.json` | generated (`StatusScanReport`) |
| `nlink-lab ps --json` | `ps.schema.json` | generated (`Vec<ProcessInfo>`) |
| `nlink-lab proc-stat --json` | `proc-stat.schema.json` | generated (`ProcStat`) |
| `nlink-lab doctor --json` | `doctor.schema.json` | generated (`DoctorReport`) |
| `nlink-lab lint --json` | `lint.schema.json` | generated (`LintReport`) |
| `nlink-lab metrics --format json` (one line per snapshot) | `metrics-snapshot.schema.json` | generated (`MetricsSnapshot`) |
| `nlink-lab deploy --json` | `deploy.schema.json` | hand-written |
| `nlink-lab status --json <LAB>` | `status-lab.schema.json` | hand-written |
| `nlink-lab spawn --json` | `spawn.schema.json` | hand-written |
| `nlink-lab impair --show --json` | `impair-show.schema.json` | hand-written |

The upstream `nlink::ConfigDiff` / `nlink::NftablesDiff` values inside
the layered diff are free-form objects in the schema (their shape is
nlink's). The `inspect`, `exec`, `diagnose`, `render` and `diff` JSON
shapes are documented inline in each subcommand's `--help`.

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
