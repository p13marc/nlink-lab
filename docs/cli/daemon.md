# `nlink-lab daemon`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Start the Zenoh backend daemon for a running lab

Usage: nlink-lab daemon [OPTIONS] <LAB>

Arguments:
  <LAB>  Lab name (must be deployed)

Options:
  -i, --interval <INTERVAL>            Metrics collection interval in seconds [default: 2]
      --json                           Output JSON instead of human-readable text (for status, diagnose, ps)
  -v, --verbose                        Verbose output (show deployment steps, tracing info)
      --zenoh-mode <ZENOH_MODE>        Zenoh mode: peer or client [default: peer]
  -q, --quiet                          Quiet output (errors only)
      --zenoh-listen <ZENOH_LISTEN>    Zenoh listen endpoint
      --zenoh-connect <ZENOH_CONNECT>  Zenoh connect endpoint
  -h, --help                           Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
