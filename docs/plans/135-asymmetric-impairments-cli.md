# Plan 135: Asymmetric Impairments via CLI

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (half day)
**Priority:** P2 — enables satellite/mobile WAN simulation from CLI

---

## Problem Statement

NLL supports asymmetric impairments with `->` / `<-` syntax, and the lowerer already
applies them to separate endpoints. But the CLI `nlink-lab impair` only takes a single
set of `--delay`, `--loss`, etc. flags — applied to one endpoint at a time.

While you *can* call `nlink-lab impair` twice (once per endpoint), there's no
ergonomic way to set different values per direction in a single call.

## Proposed CLI

```bash
# Existing (symmetric on one endpoint — unchanged)
nlink-lab impair my-lab router:eth0 --delay 50ms

# New: per-direction flags
nlink-lab impair my-lab router:eth0 --out-delay 50ms --in-delay 200ms
nlink-lab impair my-lab router:eth0 --out-loss 0% --in-loss 5%
```

`--out-*` applies to the egress qdisc on the named endpoint (traffic leaving the node).
`--in-*` applies to the ingress path — which in practice means applying netem to the
**peer** endpoint on the other side of the veth pair.

## Design Decisions

### In-direction implementation

Linux netem only works on egress. To shape "incoming" traffic on `router:eth0`, we need
to apply netem on the **peer** interface (the other end of the veth in the connected
node's namespace). This means `--in-delay 200ms` on `router:eth0` actually calls
`set_impairment()` on the peer endpoint.

This requires resolving the peer endpoint from the topology. The topology links store
both endpoints: if the user specifies `router:eth0`, we look up which link contains
that endpoint and apply the in-direction impairment on the opposite endpoint.

### Mixing `--delay` and `--out-delay`

These are mutually exclusive. If the user provides both `--delay` and `--out-delay`,
error with a clear message.

### Library API

Add a convenience method:

```rust
impl RunningLab {
    pub async fn set_asymmetric_impairment(
        &self,
        endpoint: &str,
        egress: Option<&Impairment>,
        ingress: Option<&Impairment>,
    ) -> Result<()>;
}
```

This resolves the peer endpoint internally and applies egress to the named endpoint,
ingress to the peer.

## Implementation

### Step 1: CLI flags (`bins/lab/src/main.rs`)

Add directional flags to the `Impair` command:

```rust
Impair {
    // ... existing fields ...

    /// Egress delay.
    #[arg(long)]
    out_delay: Option<String>,
    #[arg(long)]
    out_jitter: Option<String>,
    #[arg(long)]
    out_loss: Option<String>,
    #[arg(long)]
    out_rate: Option<String>,

    /// Ingress delay (applied to peer endpoint).
    #[arg(long)]
    in_delay: Option<String>,
    #[arg(long)]
    in_jitter: Option<String>,
    #[arg(long)]
    in_loss: Option<String>,
    #[arg(long)]
    in_rate: Option<String>,
},
```

### Step 2: Peer resolution (`running.rs`)

Add a method to find the peer endpoint for a given endpoint:

```rust
/// Given "nodeA:eth0", find the other end of the link → "nodeB:eth0".
pub fn peer_endpoint(&self, endpoint: &str) -> Result<String> {
    let ep = EndpointRef::parse(endpoint)
        .ok_or_else(|| Error::InvalidEndpoint { endpoint: endpoint.to_string() })?;
    for link in &self.topology.links {
        if link.left.node == ep.node && link.left.iface == ep.iface {
            return Ok(format!("{}:{}", link.right.node, link.right.iface));
        }
        if link.right.node == ep.node && link.right.iface == ep.iface {
            return Ok(format!("{}:{}", link.left.node, link.left.iface));
        }
    }
    Err(Error::deploy_failed(format!("no link found for endpoint '{endpoint}'")))
}
```

### Step 3: CLI handler update (`bins/lab/src/main.rs`)

In the impair handler, detect directional flags and handle accordingly:

```rust
let has_directional = out_delay.is_some() || out_jitter.is_some()
    || out_loss.is_some() || out_rate.is_some()
    || in_delay.is_some() || in_jitter.is_some()
    || in_loss.is_some() || in_rate.is_some();

let has_symmetric = delay.is_some() || jitter.is_some()
    || loss.is_some() || rate.is_some();

if has_directional && has_symmetric {
    return Err("cannot mix --delay/--loss with --out-delay/--in-delay".into());
}

if has_directional {
    let endpoint = endpoint.as_ref().ok_or("endpoint required")?;
    let egress = Impairment {
        delay: out_delay, jitter: out_jitter, loss: out_loss, rate: out_rate, ..default()
    };
    let ingress = Impairment {
        delay: in_delay, jitter: in_jitter, loss: in_loss, rate: in_rate, ..default()
    };

    if egress != Impairment::default() {
        running.set_impairment(endpoint, &egress).await?;
    }
    if ingress != Impairment::default() {
        let peer = running.peer_endpoint(endpoint)?;
        running.set_impairment(&peer, &ingress).await?;
    }
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_impair_asymmetric_delay` | integration.rs | `--out-delay 10ms --in-delay 50ms` applies different netem |
| `test_impair_symmetric_unchanged` | integration.rs | Existing `--delay` still works |
| `test_impair_mixed_flags_error` | main.rs (unit) | `--delay` + `--out-delay` → error |
| `test_peer_endpoint_resolution` | running.rs | Finds peer for both link directions |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +40 | Directional flags + handler logic |
| `running.rs` | +25 | `peer_endpoint()` method |
| Tests | +35 | 4 test functions |
| **Total** | ~100 | |
