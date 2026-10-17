# nlink-lab JSON output schemas

`--json` output for the CLI commands with a contractual envelope, as
JSON Schema. Most files are **generated from the Rust types** by
`nlink-lab docs-gen --schemas docs/json-schemas` (`schemars`, issue #60)
and diffed in CI — do not hand-edit those; change the type. The
remaining hand-written ones document `serde_json::json!` payloads that
have no typed struct yet.

| Command | Schema | Source |
|---------|--------|--------|
| `nlink-lab apply --dry-run --json` / `verify --json` | `layered-diff.v3.schema.json` | generated (`DryRunReport`) |
| `nlink-lab validate --json` | `validate.schema.json` | generated (`ValidateReport`) |
| `nlink-lab validate --list-rules --json` | `validate-rules.schema.json` | generated (`Vec<RuleInfo>`) |
| `nlink-lab deploy --json` | `deploy.schema.json` | generated (`DeployReport`) |
| `nlink-lab ls --json` (with `--scan`: `orphans`) | `ls.schema.json` | generated (`LsReport`) |
| `nlink-lab status --json` | `status.schema.json` | generated (`StatusReport`) |
| `nlink-lab use --json` | `use.schema.json` | generated (`UseReport`) |
| `nlink-lab snapshot ls --json` | `snapshot-list.schema.json` | generated (`SnapshotList`) |
| `nlink-lab ps --json` | `ps.schema.json` | generated (`PsReport`) |
| `nlink-lab stats --json` | `stats.schema.json` | generated (`StatsReport`) |
| `nlink-lab stats NODE --pid PID --json` / `--procs` | `stats-procs.schema.json` | generated (`ProcsReport`) |
| `nlink-lab doctor --json` | `doctor.schema.json` | generated (`DoctorReport`) |
| `nlink-lab lint --json` | `lint.schema.json` | generated (`LintReport`) |
| `nlink-lab metrics --format json` (one line per snapshot) | `metrics-snapshot.schema.json` | generated (`MetricsSnapshot`) |
| `nlink-lab spawn --json` | `spawn.schema.json` | hand-written |
| `nlink-lab impair --show --json` | `impair-show.schema.json` | hand-written |
| daemon API, client → daemon ([API.md](../API.md)) | `api-client.v1.schema.json` | generated (`api::ClientMsg`) |
| daemon API, daemon → client | `api-server.v1.schema.json` | generated (`api::ServerMsg`) |

The upstream `nlink::ConfigDiff` / `nlink::NftablesDiff` values inside
the layered diff are free-form objects in the schema (their shape is
nlink's). The shapes without a schema are described in prose: `exec`'s
in [docs/cli/exec.md](../cli/exec.md), `diagnose`'s and `stats`'s in their
`--help`; `archive inspect --json` prints the archive summary, `export
--format json` and `render --json` a topology, and `diff --json` the
topology diff with `total_changes`.

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
