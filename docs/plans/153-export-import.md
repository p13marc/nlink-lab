# Plan 153: `export` / `import` — Lab Portability for Repros and Sharing

**Date:** 2026-04-27
**Status:** Proposed
**Effort:** Small (1.5–2 days)
**Priority:** P2 — solves the "send me your repro" problem cleanly,
unblocks GitHub-issue triage, and is cheap to build.

---

## Problem Statement

Today, sharing a lab requires:

1. Copy the .nll file (and any `import`-ed files).
2. Maybe copy `--set` values used during deploy.
3. Maybe copy spawned-process scripts referenced by the topology.
4. Tell the recipient "run `nlink-lab deploy ...`."

Steps 2 and 3 are silent failures: the recipient runs the same NLL
but their lab behaves differently because they used different
parameters or are missing a script.

For bug repros and lab sharing, we need a single artifact that
captures everything needed to reproduce the lab byte-identical:

- The NLL source (with all imports inlined OR shipped as a tree)
- All `--set` parameter values
- All inline scripts referenced by `exec` blocks
- nlink-lab version it was created with
- Optionally: the rendered (post-loop-expansion) Topology, so
  recipients can inspect without running the parser

containerlab has `clab save` for runtime state and `.clab.yml`
sharing for definition. We can do better: a single `.nlz` (nlink
lab zip) file with everything.

## Goals

1. `nlink-lab export <lab-name> [-o file.nlz]` produces a single
   archive that can be sent over Slack/email/GitHub Issue.
2. `nlink-lab import file.nlz` reconstructs the lab in the
   recipient's environment.
3. The archive is reproducible: same source NLL + same params =
   byte-identical archive.
4. Archives include enough metadata that an old archive in the
   future tells the recipient if their nlink-lab version is too
   new/old to deploy it.
5. Optionally: archive a *running* lab's state (PIDs, namespace
   names, IPs assigned) so a recipient can `inspect` without
   re-deploying.

## Format

`.nlz` = a tarball with a fixed structure:

```
manifest.json              # see below
topology.nll               # the entry-point NLL (with imports preserved)
imports/                   # any imported NLL files, preserving paths
  spine-leaf.nll
  modules/router.nll
params.json                # --set values used at deploy time
scripts/                   # inline exec scripts (referenced by topology)
  router-init.sh
  client-bench.py
rendered.toml              # post-loop, post-import, post-interpolation
                           # Topology snapshot (TOML serialization).
                           # Optional. Used by `--no-reparse` import.
state.json                 # OPTIONAL: present if exported from a
                           # running lab. PIDs are preserved as
                           # info-only; recipient cannot resume them.
```

`manifest.json`:

```json
{
  "format_version": 1,
  "lab_name": "satellite-mesh",
  "exported_at": "2026-04-27T14:23:00Z",
  "exported_by": "nlink-lab 0.x.y",
  "nlink_version": "0.15.1",
  "deploy_state": "definition" | "running",
  "platform": {
    "os": "linux",
    "kernel": "6.13.4-200.fc43.x86_64",
    "arch": "x86_64"
  },
  "files": {
    "topology": "topology.nll",
    "params": "params.json",
    "rendered": "rendered.toml",
    "state": "state.json"
  },
  "checksums": {
    "topology.nll": "sha256:...",
    "rendered.toml": "sha256:...",
    "...": "..."
  }
}
```

The `format_version` field lets us evolve the archive format
without breaking existing artifacts. The recipient validates
checksums on import.

## Commands

### `nlink-lab export`

```text
nlink-lab export <LAB|FILE> [-o FILE.nlz]
                            [--include-running-state]
                            [--no-rendered]
                            [--source-only]

Args:
  LAB    Name of a deployed lab (read from ~/.nlink-lab/<name>/)
  FILE   Path to an .nll file (export from definition only)

Flags:
  -o FILE                   Output path. Default: <lab>.nlz in cwd.
  --include-running-state   Snapshot live PIDs, IPs, namespace names
                            (default: only definition).
  --no-rendered             Skip rendered.toml (smaller archive,
                            recipient must have a parser-compatible
                            nlink-lab to import).
  --source-only             Skip everything except topology + imports +
                            params + scripts. The minimal "send to
                            someone for them to deploy" form.
```

### `nlink-lab import`

```text
nlink-lab import FILE.nlz [-d DIR] [--no-deploy] [--no-reparse]

Args:
  FILE.nlz   Archive to import.

Flags:
  -d DIR             Extract sources to DIR. Default: <lab-name>/.
  --no-deploy        Just extract and validate; don't deploy.
  --no-reparse       Use rendered.toml as-is, skip the parser.
                     Useful when the archive was produced by a
                     newer nlink-lab whose NLL syntax we don't
                     understand.
```

The default flow:

```bash
$ nlink-lab import bug-repro.nlz
extracted to ./satellite-mesh/
nlink-lab version match: 0.x.y == 0.x.y
nlink version match:    0.15.1 == 0.15.1
topology validates:     ok
running deploy...
```

### `nlink-lab inspect FILE.nlz`

Read-only — summarize an archive without extracting:

```bash
$ nlink-lab inspect bug-repro.nlz
Lab:           satellite-mesh
Exported by:   nlink-lab 0.x.y on 2026-04-27
Created by:    Linux 6.13.4-200.fc43 / x86_64
Nodes:         12 (in 1 network)
Links:         24
Impair rules:  24 per-pair on `radio`
State:         definition only (not running)
Imports:       imports/distance-matrix.nll
Scripts:       scripts/run-protocol.sh
Rendered:      yes (3.2 KB Topology TOML)
```

Useful in CI: don't import an archive without first sanity-checking
its claims.

## Implementation outline

```rust
// crates/nlink-lab/src/portability.rs (new file)

pub struct ExportOptions {
    pub include_running_state: bool,
    pub include_rendered: bool,
    pub source_only: bool,
}

pub fn export_lab(
    name_or_path: &str,
    out: &Path,
    opts: ExportOptions,
) -> Result<()> {
    // 1. Resolve to (NLL source path, deployed-state path or None).
    // 2. Walk NLL imports recursively; collect file list.
    // 3. Walk topology for `exec`/`spawn` blocks with `script: "..."` paths
    //    and collect script files.
    // 4. Read params (from state.json if running, else empty).
    // 5. If --include-running-state: snapshot LabState.
    // 6. If --include-rendered: render Topology to TOML.
    // 7. Build manifest with checksums.
    // 8. Write a `tar.gz` (or zstd-compressed tar) to `out`.
}

pub struct ImportReport {
    pub extracted_to: PathBuf,
    pub manifest: Manifest,
    pub validation: ValidationResult,
}

pub fn import_lab(
    archive: &Path,
    extract_to: &Path,
    skip_reparse: bool,
) -> Result<ImportReport> {
    // 1. Open archive. Verify format_version.
    // 2. Verify checksums for every listed file.
    // 3. Extract to `extract_to`.
    // 4. Validate: parse topology.nll (or read rendered.toml if
    //    --no-reparse), run validator.
    // 5. Return report.
}
```

The `tar.gz` format keeps things simple: standard tools work,
shell-readable. zstd compression would shrink slightly but adds a
dep — defer unless someone asks.

## Edge cases

1. **Imports outside the workspace.** If an NLL `import`s a file
   from `/etc/nlink-lab/templates/...`, that path won't exist on
   the recipient's machine. Export must inline absolute-path
   imports into `imports/` and rewrite the source.
2. **`--set` values containing secrets.** WireGuard private keys,
   passwords. Add a `--redact-secrets` flag that replaces values
   matching common secret patterns with `<REDACTED-N>`; the import
   side errors with a clear message naming each redacted param.
   Don't auto-redact (false positives).
3. **Running state is ephemeral.** PIDs, namespace names, IP
   leases from a host-reachable mgmt bridge — none survive a
   reimport on a different host. Document this clearly: running
   state is informational, used by `inspect` and never resumed.
4. **Container images.** If a topology references `docker.io/...`
   images, the archive contains the reference but not the image.
   Document: recipient needs network access to pull. Optionally
   add `--bundle-images` later — but it's a >500MB tarball
   easily, defer.
5. **Backward compat.** When format_version changes, the new
   nlink-lab must still be able to import format_version 1
   archives. Tests should cover this from day one.

## Tests

| Test | Description |
|------|-------------|
| `roundtrip_definition` | Export → import → diff topology equals original |
| `roundtrip_with_imports` | NLL with parametric imports survives roundtrip |
| `roundtrip_with_scripts` | Inline `exec`-script files are bundled and extracted |
| `import_checksum_mismatch_rejects` | Tampering with archive triggers checksum failure |
| `import_format_v1_compat` | Hand-written v1 archive still imports in v2+ |
| `redact_secrets_on_export` | `--redact-secrets` replaces matching params; import errors clearly |
| `inspect_no_extract` | `inspect FILE.nlz` doesn't write anything to disk |

## Documentation Updates

| File | Change |
|------|--------|
| `docs/cli/export.md` | New CLI page (Plan 150) |
| `docs/cli/import.md` | New CLI page (Plan 150) |
| `docs/cli/inspect.md` | Update — adds the archive form |
| `docs/cookbook/lab-portability.md` | Recipe: "How to share a lab repro" |
| `docs/USER_GUIDE.md` | New section under "Working with labs" |

## File Changes

| File | Change |
|------|--------|
| `crates/nlink-lab/src/portability.rs` | New module |
| `crates/nlink-lab/src/lib.rs` | Re-export `export_lab`, `import_lab`, `inspect_archive` |
| `bins/lab/src/main.rs` | Three new subcommands |
| `Cargo.toml` | Add `tar`, `flate2`, `serde_json` (already present) |

## Acceptance

- `export <lab>` produces a file the recipient can `import` and
  deploy without further input.
- `inspect FILE.nlz` summarizes an archive without extracting.
- Archive format is documented at `docs/archive-format.md` and
  versioned.
- A backward-compat test pins the v1 format; future format
  versions cannot break it without warning.
- Round-trip is byte-stable for a topology + params + scripts.

## Out of scope

- **Restoring a *running* lab to its exact prior state** (PIDs,
  spawned process state, conntrack tables). That's a different,
  much harder problem (CRIU territory). For now, "running state"
  in the archive is informational only.
- **Image bundling.** Container images aren't included; the
  archive only references them.
- **Encrypted archives.** Use `gpg` on the resulting tarball if you
  need this.
