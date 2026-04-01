# Plan 128: Per-Pair Impairment Matrix on Shared Networks

**Date:** 2026-04-01
**Status:** Blocked — needs nlink TC filter API (`add_filter`, u32 match support)
**Effort:** Medium-Large (2-3 days)
**Priority:** P2 — enables realistic distance-dependent link simulation

---

## Problem Statement

Shared modem networks apply uniform characteristics to all members. But
in reality, radio/satellite links have different quality per pair:
- C2 ↔ A18 at 5km: low delay, low loss
- C2 ↔ A19 at 50km: higher delay, higher loss
- A18 ↔ A19 direct: even higher delay

Currently nlink-lab can only impair per-interface (netem on one end) or
per-link (netem on both ends of a point-to-point veth). There's no way
to apply different impairment per source-destination pair on a bridge.

## Proposed Syntax

```nll
network radio {
  members [c2-fw:radio, a18-black:radio, a19-black:radio]
  subnet 172.100.3.0/24

  # Per-pair impairment (inside network block)
  impair c2-fw -- a18-black { delay 15ms jitter 5ms loss 1% }
  impair c2-fw -- a19-black { delay 40ms jitter 20ms loss 5% }
  impair a18-black -- a19-black { delay 60ms jitter 30ms loss 8% }
}
```

Pairs without explicit impairment get zero impairment (direct bridge path).

## Implementation

### Linux TC approach

For each bridge port, create TC classes with u32 filters matching by
destination IP. Each class gets its own netem qdisc.

**Per bridge port (e.g., veth connected to c2-fw:radio):**

```bash
# On the bridge-side veth for c2-fw:radio
tc qdisc add dev veth-c2fw-radio root handle 1: prio bands 3
# Class for traffic to a18-black (172.100.3.18)
tc qdisc add dev veth-c2fw-radio parent 1:1 netem delay 15ms jitter 5ms loss 1%
tc filter add dev veth-c2fw-radio parent 1: protocol ip u32 \
  match ip dst 172.100.3.18/32 flowid 1:1
# Class for traffic to a19-black (172.100.3.19)
tc qdisc add dev veth-c2fw-radio parent 1:2 netem delay 40ms jitter 20ms loss 5%
tc filter add dev veth-c2fw-radio parent 1: protocol ip u32 \
  match ip dst 172.100.3.19/32 flowid 1:2
```

### Deploy integration

After Step 14 (netem impairments), add Step 14b for network impairment:

```rust
// Step 14b: Apply per-pair network impairments
for (net_name, network) in &topology.networks {
    if network.impairments.is_empty() { continue; }
    for impairment in &network.impairments {
        // For each direction of the pair, configure TC on the
        // source's bridge port to delay/drop traffic to destination
        apply_bridge_port_impairment(
            &mgmt_ns, net_name,
            &impairment.src, &impairment.dst, &impairment.config,
        ).await?;
        // Reverse direction (impairment is symmetric by default)
        apply_bridge_port_impairment(
            &mgmt_ns, net_name,
            &impairment.dst, &impairment.src, &impairment.config,
        ).await?;
    }
}
```

### Types

```rust
pub struct NetworkImpairment {
    pub src: String,   // node name (endpoint without :iface)
    pub dst: String,   // node name
    pub config: Impairment,
}
```

Add `impairments: Vec<NetworkImpairment>` to `Network`.

### nlink requirements

Need TC operations on bridge-side veth endpoints:
- `conn.add_qdisc()` — already exists
- `conn.add_tc_class()` — may need to check nlink support for prio/htb
- `conn.add_tc_filter()` — u32 filter support needed

Check nlink for: `TcFilter`, `TcClass`, `PrioQdisc` support.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_network_impairment` | Parse impair inside network block |
| `test_lower_network_impairment` | Lower to NetworkImpairment type |
| Integration: `deploy_network_impair` | Deploy, verify per-pair delay differs |

## Documentation Updates

| File | Change |
|------|--------|
| **README.md** | Add "Per-Pair Network Impairment" section |
| **CLAUDE.md** | Add `NetworkImpairment` to types; mention in features |
| **NLL_DSL_DESIGN.md** | Add `impair` syntax inside network blocks |
| **examples/infra-c2-a18-a9.nll** | Add impairment matrix to radio network |

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `NetworkImpairment`, `impairments` on `Network` |
| `ast.rs` | Add impairment to `NetworkDef` |
| `parser.rs` | Parse `impair` inside network blocks |
| `lower.rs` | Lower network impairments |
| `deploy.rs` | Step 14b: TC classes + filters on bridge ports |
| `render.rs` | Render per-pair impairments in network blocks |
