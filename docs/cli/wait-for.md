# `nlink-lab wait-for`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Wait for a service or condition inside a lab node

Usage: nlink-lab wait-for [OPTIONS] <LAB> <NODE>

Arguments:
  <LAB>   Lab name
  <NODE>  Node name

Options:
      --json                 Output JSON instead of human-readable text (for status, diagnose, ps)
      --tcp <TCP>            Wait for TCP port (e.g., "127.0.0.1:8080" or just "8080" for localhost)
      --exec <EXEC_CMD>      Wait for command to succeed (exit 0)
  -v, --verbose              Verbose output (show deployment steps, tracing info)
      --file <FILE>          Wait for file to exist
  -q, --quiet                Quiet output (errors only)
  -t, --timeout <TIMEOUT>    Timeout in seconds (default: 30) [default: 30]
      --interval <INTERVAL>  Poll interval in milliseconds (default: 500) [default: 500]
  -h, --help                 Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
