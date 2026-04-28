# WireGuard with auto-key generation

A site-to-site WireGuard tunnel between two gateways, with the
underlay carrying realistic WAN impairment. Keys are generated at
deploy time — there's no offline key-management dance.

## When to use this

- Testing a service that needs to traverse a real WG tunnel under
  realistic packet conditions (loss, jitter, MTU).
- Validating that a userspace VPN client interoperates with the
  kernel's `wireguard` driver.
- Mesh routing experiments where each pair of nodes needs an
  encrypted underlay.

## Why nlink-lab

WireGuard is a kernel module; spinning it up in containerlab
requires either a vendor-NOS image with WireGuard support, or
shelling out to `wg setconf` from a `kind: linux` container with
hand-managed keys. nlink-lab declares the tunnel inline, generates
keypairs at deploy time, and writes them via the kernel's
WireGuard genetlink API directly.

## NLL

[`examples/wireguard-vpn.nll`](../../examples/wireguard-vpn.nll):

```nll
lab "wireguard-vpn" { description "Site-to-site WireGuard VPN over WAN" }

profile gateway { forward ipv4 }

node gw-a : gateway {
  wireguard wg0 {
    key auto                       # generate at deploy time
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]                   # nlink-lab fills in keys + endpoints
  }
  route 192.168.2.0/24 dev wg0
  route 192.168.255.2/32 dev wg0
}

node gw-b : gateway {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.2/32
    peers [gw-a]
  }
  route 192.168.1.0/24 dev wg0
  route 192.168.255.1/32 dev wg0
}

node host-a { route default via 192.168.1.1 }
node host-b { route default via 192.168.2.1 }

# WAN underlay (with realistic impairment)
link gw-a:eth0 -- gw-b:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }
impair gw-a:eth0 delay 50ms jitter 5ms loss 0.1%

# LAN sides
link gw-a:eth1 -- host-a:eth0 { 192.168.1.1/24 -- 192.168.1.10/24 }
link gw-b:eth1 -- host-b:eth0 { 192.168.2.1/24 -- 192.168.2.10/24 }
```

The `peers [gw-b]` declaration on `gw-a` is symmetric: nlink-lab
resolves it at deploy time, plugs in `gw-b`'s public key, and points
the WG endpoint at `gw-b`'s WAN address. No manual key copying.

## Run

```bash
sudo nlink-lab deploy examples/wireguard-vpn.nll
```

### Verify the tunnel

```bash
sudo nlink-lab exec wireguard-vpn gw-a -- wg show
```

You should see `gw-b`'s public key listed as a peer with a recent
handshake.

### Cross-site ping over WG

`host-a` reaches `host-b` through the encrypted WG tunnel:

```bash
sudo nlink-lab exec wireguard-vpn host-a -- ping -c 5 192.168.2.10
```

The latency reflects the 50ms underlay impairment plus WG's small
overhead.

### Tear down

```bash
sudo nlink-lab destroy wireguard-vpn
```

## What nlink-lab built

For each `wireguard` block with `key auto`, nlink-lab generates an
X25519 keypair using `x25519-dalek` at deploy time, creates the WG
interface via netlink (`WireguardLink::new`), and pushes the
configuration (private key, listen port, peer entries with public
keys + endpoints + allowed-IPs) via the kernel's WireGuard
genetlink interface.

The keys exist only for the lab's lifetime; teardown removes them.
For tests that need stable keys (multi-deploy reproducibility),
replace `key auto` with an inline `key "<base64-private-key>"`.

## Variations

- **Three-node mesh**: every node lists every other node in
  `peers [...]`. nlink-lab generates 3 keypairs and 6 peer
  configs (3 nodes × 2 peers each).
- **Inside a VRF** for per-customer L3VPN: declare the
  WireGuard interface and slot it into a VRF block. See
  [vrf-multitenant.md](vrf-multitenant.md).
- **PreSharedKey for post-quantum**: add `preshared-key auto` to
  the peer entries.
- **WG underlay with packet loss spikes**: use a `scenario { }`
  block to inject mid-test loss on `gw-a:eth0`.

## Performance notes

WireGuard in the kernel costs roughly 5–10% over native veth in
this setup. Throughput tests should use [`benchmark`](../NLL_DSL_DESIGN.md)
blocks with iperf3 to measure overhead in your specific scenario.

## When this is the wrong tool

If your goal is testing a vendor's WG implementation (e.g. some
NOS's WG configlet syntax), you need that NOS's image. nlink-lab
uses the mainline kernel WG driver only.

## See also

- [NLL: WireGuard syntax](../NLL_DSL_DESIGN.md#9-wireguard)
- [vrf-multitenant.md](vrf-multitenant.md) — combine VRF + WG for L3VPN-style isolation
