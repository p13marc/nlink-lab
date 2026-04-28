# `nlink-lab logs`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Show container logs

Usage: nlink-lab logs [OPTIONS] <LAB> [NODE]

Arguments:
  <LAB>   Lab name
  [NODE]  Node name (for container logs)

Options:
      --json         Output JSON instead of human-readable text (for status, diagnose, ps)
      --pid <PID>    Process ID (for background process logs)
      --stderr       Show stderr instead of stdout (with --pid)
  -v, --verbose      Verbose output (show deployment steps, tracing info)
      --follow       Stream logs (tail -f style, container only)
  -q, --quiet        Quiet output (errors only)
      --tail <TAIL>  Show last N lines
  -h, --help         Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
