# `nlink-lab metrics`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Stream live metrics from a lab via Zenoh (no root required)

Usage: nlink-lab metrics [OPTIONS] <LAB>

Arguments:
  <LAB>  Lab name

Options:
      --json                           Output JSON instead of human-readable text (for status, diagnose, ps)
  -n, --node <NODE>                    Filter to specific node
  -f, --format <FORMAT>                Output format: table (default), json [default: table]
  -v, --verbose                        Verbose output (show deployment steps, tracing info)
  -c, --count <COUNT>                  Number of samples then exit
  -q, --quiet                          Quiet output (errors only)
      --zenoh-connect <ZENOH_CONNECT>  Zenoh connect endpoint
  -h, --help                           Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
