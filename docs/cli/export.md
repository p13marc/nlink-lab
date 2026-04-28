# `nlink-lab export`

Export a lab's topology — either as plain TOML/JSON for inspection,
or as a portable `.nlz` archive for sharing.

## Usage

```text
# Plain TOML/JSON serialization (default)
nlink-lab export <LAB> [-o FILE]

# Portable .nlz archive (with --archive)
nlink-lab export --archive <LAB|FILE> [-o FILE.nlz]
                                      [--set KEY=VALUE]...
                                      [--include-running-state]
                                      [--no-rendered]
```

## Description

Two modes:

- **Plain export** (no `--archive`): serializes the rendered
  Topology to TOML (default) or JSON (`--json`). Useful for
  diffing topologies, feeding into other tools, or quickly
  inspecting what the parser produced.
- **Archive export** (`--archive`): produces a `.nlz` file —
  a gzipped tar bundle with manifest, NLL source, optional
  `--set` params, optional rendered snapshot, and SHA-256
  checksums. Suitable for sharing repros via email, GitHub
  issues, or CI artifacts.

## Arguments

| Argument | Description |
|----------|-------------|
| `<LAB>` (plain) | Name of a deployed lab. |
| `<LAB\|FILE>` (`--archive`) | Lab name OR path to a `.nll` file. Auto-detected — paths that exist or end in `.nll` are treated as files. |

## Options

| Flag | Description |
|------|-------------|
| `-o`, `--output FILE` | Output path. Plain mode default: stdout. Archive mode default: `<lab>.nlz` in cwd. |
| `--archive` | Produce a `.nlz` archive instead of plain serialization. |
| `--set KEY=VALUE` | (with `--archive`) Record an NLL `param` override in the archive. Repeatable. |
| `--include-running-state` | (with `--archive`) Include live PIDs / namespace names from a deployed lab. Informational only. |
| `--no-rendered` | (with `--archive`) Skip the rendered.toml snapshot. Smaller archive; recipient must have a parser-compatible nlink-lab. |
| `--json` | (plain mode) Emit JSON instead of TOML. |

## Examples

### Plain TOML to stdout

```bash
sudo nlink-lab deploy examples/simple.nll
nlink-lab export simple
```

### JSON to a file

```bash
nlink-lab export --json simple -o simple.json
```

### Archive from an NLL file (no deploy needed)

```bash
nlink-lab export --archive examples/cookbook/satellite-mesh.nll
# → satellite-mesh.nlz in cwd
```

### Archive from a deployed lab with params

```bash
sudo nlink-lab deploy --set delay=200ms wan.nll
nlink-lab export --archive wan \
  --set delay=200ms \
  -o /tmp/wan-laggy.nlz
```

The recipient runs `nlink-lab import wan-laggy.nlz` and gets the
same lab with `delay=200ms` applied automatically.

### Bug repro for a GitHub issue

```bash
nlink-lab export --archive --include-running-state mybug
# → mybug.nlz; attach to the issue
```

The recipient inspects without deploying:

```bash
nlink-lab inspect mybug.nlz
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Bad args / NLL parse failure / serialize error |
| 2 | Lab not found (deployed-lab mode) |

## Notes

- **`.nlz` is a tarball**. `tar tzf <file>.nlz` lists contents;
  `tar xzf <file>.nlz` extracts manually. The manifest is a
  human-readable JSON file at the root.
- **`--set` values are stored verbatim**. If they contain
  secrets (WG keys, passwords), treat the archive as a secret.
- **Archive format is versioned**. See [Plan 153](../plans/153-export-import.md).

## See also

- [`import`](import.md) — extract + deploy
- [`inspect`](inspect.md) — summarize an archive without extracting
- [Cookbook: lab portability](../cookbook/lab-portability.md)
