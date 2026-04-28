# `nlink-lab render`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Render a topology file with all loops, variables, and imports expanded

Usage: nlink-lab render [OPTIONS] <TOPOLOGY>

Arguments:
  <TOPOLOGY>  Path to the topology file (.nll)

Options:
      --dot              Output as DOT graph (for Graphviz)
      --json             Output JSON instead of human-readable text (for status, diagnose, ps)
      --ascii            Output as ASCII diagram
  -v, --verbose          Verbose output (show deployment steps, tracing info)
  -q, --quiet            Quiet output (errors only)
      --set <KEY=VALUE>  Set NLL parameters (can be repeated: --set key=value)
  -h, --help             Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
