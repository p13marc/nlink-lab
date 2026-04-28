# `nlink-lab import`

Import a `.nlz` lab archive — verify checksums, extract, validate,
and (by default) deploy.

## Usage

```text
nlink-lab import <FILE.nlz> [-d DIR] [--no-deploy] [--no-reparse]
```

## Description

The default flow:

1. Open the archive, verify SHA-256 checksums for every listed
   file. Reject the archive on any mismatch (partial download,
   tampering, accidental edit-in-place).
2. Validate the `format_version` is supported.
3. Extract to `./<lab-name>/` (or `-d DIR`).
4. Re-parse the NLL source (or read the rendered snapshot with
   `--no-reparse`).
5. Run the validator.
6. Deploy (or skip with `--no-deploy`).

## Arguments

| Argument | Description |
|----------|-------------|
| `<FILE.nlz>` | Path to the archive. |

## Options

| Flag | Description |
|------|-------------|
| `-d`, `--dir DIR` | Extract to this directory. Default: `./<lab-name>/`. |
| `--no-deploy` | Extract + validate only; don't touch the kernel. |
| `--no-reparse` | Use the archive's `rendered.toml` directly. Required if the archive's NLL syntax is newer than this nlink-lab supports. |

## Examples

### Default: extract + deploy

```bash
sudo nlink-lab import bug-repro.nlz
```

```text
Extracted lab 'bug-repro' to ./bug-repro (format v1, exported by nlink-lab 0.x.y)
Deployed lab 'bug-repro'
```

### Extract + validate only

```bash
nlink-lab import --no-deploy bug-repro.nlz
ls bug-repro/
# manifest.json  rendered.toml  topology.nll
```

Doesn't require root. Useful as a CI gate before deciding to
deploy.

### Custom extract dir

```bash
sudo nlink-lab import bug-repro.nlz -d /tmp/repro
sudo nlink-lab import bug-repro.nlz -d ./labs/issue-42/
```

### Use the rendered snapshot (skip parse)

```bash
sudo nlink-lab import --no-reparse newer-archive.nlz
```

When the archive was produced by a newer nlink-lab whose NLL
syntax this version doesn't fully understand, `--no-reparse`
uses the bundled `rendered.toml` (post-parse, post-lower
Topology) directly.

This requires the archive to have been exported without
`--no-rendered`. If `rendered.toml` is missing, `--no-reparse`
errors.

### CI gate: validate before deploying

```bash
nlink-lab import --no-deploy attached.nlz   # validate
RC=$?
if [ $RC -eq 0 ]; then
  sudo nlink-lab deploy attached/topology.nll
fi
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success (extracted, validated, deployed if not `--no-deploy`) |
| 1 | Bad args, parse failure, or validation failure |
| 2 | Archive missing, malformed, or checksum mismatch |
| 3 | `format_version` newer than supported |
| 5 | Insufficient capabilities for deploy step (need `CAP_NET_ADMIN`+`CAP_SYS_ADMIN`) |

## What gets extracted

The archive contents (see [`inspect`](inspect.md)):

- `manifest.json` — metadata + checksums
- `topology.nll` — NLL source
- `params.json` — (optional) `--set` values from export time
- `rendered.toml` — (optional) post-parse Topology snapshot
- `state.json` — (optional) live state from a deployed export

`params.json`, if present, is automatically applied during
re-parse — the recipient gets the same `--set` overrides the
exporter used.

## Notes

- **State files are informational.** PIDs, namespace names, and
  container IDs in `state.json` are from the export host. The
  recipient gets a fresh deploy with new IDs.
- **Container images aren't bundled.** If the topology references
  images, the recipient needs network access to pull.
- **Inline scripts aren't bundled (yet).** External `exec`/`spawn`
  scripts must be shipped alongside the archive.
- **Format compat**: nlink-lab maintains backward compatibility
  for archive `format_version` 1+. Newer versions are rejected
  with a clear error.

## See also

- [`export`](export.md) — produce a `.nlz` archive
- [`inspect`](inspect.md) — summarize without extracting
- [Cookbook: lab portability](../cookbook/lab-portability.md)
