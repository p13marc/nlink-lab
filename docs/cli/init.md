# `nlink-lab init`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Create a topology file from a built-in template

Usage: nlink-lab init [OPTIONS] [TEMPLATE]

Arguments:
  [TEMPLATE]  Template name (e.g., "router", "spine-leaf"). Use --list to see all

Options:
      --json             Output JSON instead of human-readable text (for status, diagnose, ps)
      --list             List available templates
  -o, --output <OUTPUT>  Output directory (default: current directory)
  -v, --verbose          Verbose output (show deployment steps, tracing info)
  -f, --format <FORMAT>  Output format [default: nll]
  -q, --quiet            Quiet output (errors only)
  -n, --name <NAME>      Override the lab name
      --force            Overwrite existing files
  -h, --help             Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
