# `nlink-lab impair`

> **Stub page** — auto-generated from `--help`. Will get a full
> reference (examples, exit codes, see-also) in [Plan 150 Phase D](../plans/150-documentation-overhaul.md).

```text
Modify link impairment at runtime

Usage: nlink-lab impair [OPTIONS] <LAB> [ENDPOINT]

Arguments:
  <LAB>       Lab name
  [ENDPOINT]  Endpoint (e.g., "router:eth0"). Not required with --show

Options:
      --json                     Output JSON instead of human-readable text (for status, diagnose, ps)
      --show                     Show current impairments on all interfaces
      --delay <DELAY>            Delay (e.g., "10ms")
  -v, --verbose                  Verbose output (show deployment steps, tracing info)
      --jitter <JITTER>          Jitter (e.g., "2ms")
  -q, --quiet                    Quiet output (errors only)
      --loss <LOSS>              Packet loss (e.g., "0.1%")
      --rate <RATE>              Rate limit (e.g., "100mbit")
      --clear                    Remove impairment
      --out-delay <OUT_DELAY>    Egress delay (applied to named endpoint)
      --out-jitter <OUT_JITTER>  Egress jitter
      --out-loss <OUT_LOSS>      Egress packet loss
      --out-rate <OUT_RATE>      Egress rate limit
      --in-delay <IN_DELAY>      Ingress delay (applied to peer endpoint)
      --in-jitter <IN_JITTER>    Ingress jitter
      --in-loss <IN_LOSS>        Ingress packet loss
      --in-rate <IN_RATE>        Ingress rate limit
      --partition                Simulate a network partition (save impairments, apply 100% loss)
      --heal                     Restore pre-partition impairments
  -h, --help                     Print help
```

## See also

- [CLI reference index](README.md)
- [User guide](../USER_GUIDE.md)
