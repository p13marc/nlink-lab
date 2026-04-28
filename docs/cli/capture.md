# `nlink-lab capture`

Capture packets on a lab interface to a pcap file, with optional
BPF filtering.

## Usage

```text
nlink-lab capture [OPTIONS] <LAB> <ENDPOINT>
```

## Description

Wraps [`netring`](https://crates.io/crates/netring) — a zero-copy
AF_PACKET TPACKET_V3 ring-buffer reader. The capture runs inside
the target node's namespace, so the interface is the per-node
interface name (e.g. `eth0`, not the host-side veth peer).

Without `-w`, capture prints one-line packet summaries to stdout.
With `-w trace.pcap`, packets are written in libpcap format
readable by tcpdump, Wireshark, scapy, and tshark.

## Arguments

| Argument | Description |
|----------|-------------|
| `<LAB>` | Lab name. |
| `<ENDPOINT>` | `node:iface` — for example, `router:eth0`. |

## Options

| Flag | Description |
|------|-------------|
| `-w`, `--write FILE` | Write to pcap. Without this, summaries go to stdout. |
| `-c`, `--count N` | Stop after N packets. |
| `--duration SECS` | Stop after N seconds. |
| `-f`, `--filter EXPR` | BPF filter, e.g. `"tcp port 80"`. Applied in the kernel via `setsockopt`. |
| `--snap-len BYTES` | Truncate each packet to N bytes. Default 262144 (full packet). |
| `--json` | Emit each summary as a JSON line (without `-w`). |

## Examples

### Capture 100 TCP packets on port 80

```bash
sudo nlink-lab capture lab client:eth0 -c 100 -f "tcp port 80" -w http.pcap
```

### Watch traffic live with summaries

```bash
sudo nlink-lab capture lab router:wan -f "icmp"
```

```text
1729001234.567 ICMP 10.0.0.2 → 10.0.0.1 echo-request id=42 seq=1
1729001234.572 ICMP 10.0.0.1 → 10.0.0.2 echo-reply   id=42 seq=1
...
```

### Time-limited capture for a CI artifact

```bash
sudo nlink-lab capture lab server:eth0 \
  --duration 30 \
  -w /tmp/server-eth0.pcap
```

### Capture both sides of a link in parallel

```bash
sudo nlink-lab capture lab a:eth0 -w a-eth0.pcap &
sudo nlink-lab capture lab b:eth0 -w b-eth0.pcap &
wait
```

### Open in Wireshark

```bash
sudo nlink-lab capture lab router:wan --duration 10 -w /tmp/trace.pcap
wireshark /tmp/trace.pcap
```

### Pipe to tshark live

```bash
sudo nlink-lab capture lab router:wan -w - -c 100 | tshark -r -
```

(Some shells need `-w /dev/stdout` instead of `-w -`.)

## Performance notes

- Captures run zero-copy via TPACKET_V3 ring buffers; even at
  10Gbps line rates the CPU cost is small.
- The default snap length (262144) keeps full packets; reduce to
  `--snap-len 96` for header-only captures with much smaller
  files.
- BPF filters apply in the kernel; non-matching packets never
  cross the user/kernel boundary.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Capture finished cleanly (count or duration met, or Ctrl-C) |
| 1 | Bad arguments / bad BPF filter |
| 2 | Lab or interface not found |
| 5 | Insufficient capabilities (need `CAP_NET_RAW` in addition to deploy caps) |

## See also

- [`exec`](exec.md) — to run `tcpdump` inside a node directly
- [`diagnose`](diagnose.md) — per-lab health checks
