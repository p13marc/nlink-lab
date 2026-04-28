# `nlink-lab inspect`

Show comprehensive lab details, OR summarize a `.nlz` archive
without extracting.

## Usage

```text
nlink-lab inspect [OPTIONS] <LAB-OR-PATH>
```

## Description

Two modes, auto-detected:

- **Lab mode**: argument is a deployed lab name. Output mirrors
  `status` but with topology details — interfaces, addresses,
  routes, impairments, networks. Reads from
  `~/.nlink-lab/<name>/`.
- **Archive mode**: argument is a path ending in `.nlz` (or any
  existing path that isn't a known lab). Output is the archive
  manifest + node/link/network counts. Read-only — never
  extracts.

## Arguments

| Argument | Description |
|----------|-------------|
| `<LAB-OR-PATH>` | Lab name or path to a `.nlz` archive. |

## Options

| Flag | Description |
|------|-------------|
| `--json` | Machine-parseable JSON. |

## Examples

### Inspect a deployed lab

```bash
sudo nlink-lab deploy examples/simple.nll
nlink-lab inspect simple
```

### Inspect an archive

```bash
nlink-lab inspect bug-repro.nlz
```

```text
Archive:       bug-repro.nlz
Lab:           bug-repro
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

### CI gate: assert archive contents

```bash
nlink-lab inspect --json archive.nlz | jq -e '
  .manifest.format_version == 1
  and .manifest.deploy_state == "definition"
  and .node_count == 12
'
```

If the assertion fails, the archive isn't what you expected
before you import + deploy.

### Inspect from a different directory

```bash
nlink-lab inspect /tmp/labs/issue-42.nlz
```

Path detection works on any extension that ends in `.nlz`. For
non-standard extensions, also: any existing path that isn't a
known deployed-lab name is treated as an archive.

## Output schema (`--json`, archive mode)

```json
{
  "manifest": {
    "format_version": 1,
    "lab_name": "bug-repro",
    "exported_at": "2026-04-28T10:48:39Z",
    "exported_by": "nlink-lab 0.x.y",
    "deploy_state": "definition",
    "platform": {"os": "linux", "kernel": "6.19.13-200.fc43.x86_64", "arch": "x86_64"},
    "files": {"topology": "topology.nll", "rendered": "rendered.toml"},
    "checksums": { "topology.nll": "...", "rendered.toml": "..." }
  },
  "node_count": 12,
  "link_count": 0,
  "network_count": 1
}
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Bad args / archive malformed |
| 2 | Lab not found (lab mode) |

## Disambiguation

If you have a deployed lab named `foo.nlz` (unusual, but
possible), `inspect foo.nlz` will treat it as the archive path
because the suffix matches. Avoid naming labs with `.nlz`
extensions; otherwise rename the file before inspection.

## See also

- [`export`](export.md) — produce a `.nlz` archive
- [`import`](import.md) — extract + deploy
- [`status`](status.md) — quick lab listing
- [Cookbook: lab portability](../cookbook/lab-portability.md)
