# Sharing labs as `.nlz` archives

You hit a bug in a 12-node satellite mesh. You want a coworker to
reproduce it. You could send them the `.nll` file and a list of
`--set` values you used, but they'll silently mis-deploy if they
have a different nlink-lab version, miss a referenced script, or
get the parameters slightly wrong.

The `.nlz` lab archive is one file that captures everything they
need: NLL source, `--set` values, a rendered Topology snapshot,
and SHA-256 checksums. They `import` it; they get the same lab.

## When to use this

- Bug repros: "send me your topology" turns into "attach the .nlz
  to the GitHub issue."
- Lab sharing: a teammate's CI hit a flake; you want their exact
  topology to investigate.
- Versioned lab archives in CI artifacts: snapshot the deployed
  topology after each test run.
- Time travel: when nlink-lab bumps a version, archives from the
  old version still import (within `format_version`).

## Why nlink-lab

[containerlab](https://containerlab.dev) supports `clab save` for
state and YAML sharing for definitions, but they're separate
flows. The `.nlz` archive is one tarball with manifest, source,
params, rendered snapshot, and integrity checksums — closer to a
hermetic Nix derivation than a `tar`-the-Docker-volumes script.

## Export

From an NLL file (no deploy needed):

```bash
nlink-lab export --archive examples/cookbook/satellite-mesh.nll
# → satellite-mesh.nlz in cwd
```

From a deployed lab:

```bash
sudo nlink-lab deploy examples/cookbook/satellite-mesh.nll
nlink-lab export --archive satellite-mesh -o /tmp/repro.nlz
```

With `--set` values recorded for reproducibility:

```bash
nlink-lab export --archive wan.nll \
  --set delay=200ms \
  --set loss=2% \
  -o wan-laggy.nlz
```

The recipient gets `delay=200ms` applied automatically when they
import.

With live state included (informational only — recipient can't
resume the same PIDs):

```bash
nlink-lab export --archive --include-running-state satellite-mesh
```

## Inspect

Read-only summary, never extracts:

```bash
nlink-lab inspect satellite-mesh.nlz
```

```text
Archive:       satellite-mesh.nlz
Lab:           satellite-mesh
Format:        v1
Exported by:   nlink-lab 0.x.y on 2026-04-28T10:48:39Z
Platform:      linux 6.19.13-200.fc43.x86_64 / x86_64
State:         Definition
Nodes:         12
Links:         0
Networks:      1
Files:
  topology:    topology.nll
  rendered:    rendered.toml
```

JSON output for tooling:

```bash
nlink-lab inspect --json satellite-mesh.nlz | jq '.manifest.lab_name'
```

CI gate — assert an archive matches expectations before importing:

```bash
nlink-lab inspect --json bug-repro.nlz \
  | jq -e '.manifest.format_version == 1 and .node_count == 12'
```

## Import

Default flow: extract + validate + deploy.

```bash
sudo nlink-lab import satellite-mesh.nlz
```

```text
Extracted lab 'satellite-mesh' to satellite-mesh (format v1, ...)
Deployed lab 'satellite-mesh'
```

Extract + validate only (no kernel state changed):

```bash
nlink-lab import --no-deploy satellite-mesh.nlz
ls satellite-mesh/
# manifest.json  rendered.toml  topology.nll
```

Custom extract dir:

```bash
sudo nlink-lab import bug-repro.nlz -d /tmp/repro
```

Use the rendered snapshot directly (skip re-parsing the NLL —
useful if the archive was produced by a newer nlink-lab whose
syntax we don't fully understand):

```bash
sudo nlink-lab import --no-reparse newer-archive.nlz
```

## What's in the archive

`.nlz` is a gzipped tarball. You can inspect it with standard
tools:

```bash
tar tzf satellite-mesh.nlz
# manifest.json
# topology.nll
# rendered.toml
```

The manifest carries:

| Field | What |
|-------|------|
| `format_version` | Bumped on incompatible format changes |
| `lab_name` | The NLL `lab "name"` |
| `exported_at` | RFC 3339 timestamp |
| `exported_by` | `nlink-lab <version>` |
| `deploy_state` | `definition` or `running` |
| `platform` | OS, kernel, arch from the export host |
| `files` | Map of role → filename |
| `checksums` | SHA-256 for every listed file |

Import re-validates the checksums and refuses to extract on
mismatch. This protects against partial downloads, intentional
tampering, and accidental edits-in-place.

## Format versioning

`format_version` is bumped on incompatible changes. A
nlink-lab that supports v1 will:

- Accept v1 archives.
- Reject v2+ archives with a clear "newer than supported" error
  (use `--no-reparse` only if the rendered.toml schema didn't
  change).

Patch-level additions (new optional manifest fields) keep
`format_version` stable. Older importers ignore unknown fields
via `#[serde(default)]`.

## Limitations

The archive captures **the lab definition**, not an arbitrary
snapshot of running state:

- **PIDs and namespace names are informational.** The
  recipient gets a fresh deploy with new PIDs and namespace
  names. There's no CRIU-like checkpoint/restore.
- **Container images aren't bundled.** If the topology
  references `docker.io/foo/bar:latest`, the recipient needs
  network access to pull. (Shipping images would balloon the
  archive to 500MB+ for typical labs.) `--bundle-images`
  is on the roadmap if there's demand.
- **Inline scripts aren't bundled yet.** If your topology's
  `exec` or `spawn` blocks reference external scripts, you
  must ship those alongside the archive. Bundling scripts is
  a Plan 153 follow-up.
- **Imports outside the workspace.** Importing an NLL with
  paths like `/etc/nlink-lab/templates/...` requires those
  paths exist on the recipient. Workaround: inline the
  imports before exporting.

## Secrets

If your topology has WireGuard private keys or password
parameters via `--set`, those values land in the archive
verbatim. **Treat `.nlz` files as secrets** if your params are.
A `--redact-secrets` flag is on the roadmap.

## Use as a CI artifact

Export the deployed topology after each test run:

```bash
sudo nlink-lab deploy --json examples/wan.nll > deploy.json
# ... run tests ...
nlink-lab export --archive --include-running-state wan -o /tmp/wan-${CI_RUN}.nlz
```

Upload `wan-${CI_RUN}.nlz` to your CI artifact store. Failures
get a one-click reproducer.

## When this is the wrong tool

- For checkpointing a lab's full kernel state (in-flight
  conntrack entries, spawned-process internal state), use
  CRIU directly. `.nlz` only captures definition + checksums.
- For sharing a *running* deployed lab between machines, you
  can't — the recipient gets a fresh deploy. To exactly mirror
  a live lab, use container snapshots (containerlab + Docker
  has weak support for this; nlink-lab does not).

## See also

- [`export` CLI page](../cli/export.md)
- [`import` CLI page](../cli/import.md)
- [`inspect` CLI page](../cli/inspect.md)
- [Plan 153](../plans/153-export-import.md) — design + roadmap
