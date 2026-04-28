# Multi-tenant WAN: VRF + WireGuard + nftables on one box

Two customers (red and blue) share a single PE router at each
site. They need:

- **L3 isolation**: red can't see blue, period — even though both
  ride the same physical box.
- **End-to-end encryption** between sites.
- **Different policy** per VRF (red sshable from red's mgmt
  subnet, blue sshable from blue's).

This recipe builds it in 90 lines of NLL. The composition
demonstrates three primitives working together that
[containerlab](https://containerlab.dev) can't compose with
`kind: linux` containers, even with `exec:` shell hooks: VRFs,
auto-keygen WireGuard, and stateful nftables — written via
netlink directly, not via shell scripts inside containers.

## When to use this

- Modeling MPLS L3VPN PE behavior without the vendor-image tax.
- Testing a multi-tenant network function (a VPN concentrator,
  a service-chain router, a tenant-aware proxy) under realistic
  per-tenant traffic.
- Validating cross-VRF leak prevention as part of a CI gate.
- Building a reproducible repro for a customer's "the tenants
  are bleeding into each other" bug.

## Why nlink-lab

Pure Linux composition is the wedge here. VRFs are kernel
primitives; WireGuard is a kernel module; nftables is a kernel
subsystem. nlink-lab declares the three together inline in NLL
and writes the live config via netlink. containerlab needs a
vendor NOS image (cEOS, SR Linux, vMX) for VRF, can't easily
declare WireGuard, and shells out for nftables. The end result
in clab is multi-page YAML + multi-page startup-config scripts;
in nlink-lab, it's one file.

## NLL

[`examples/cookbook/multi-tenant-wan.nll`](../../examples/cookbook/multi-tenant-wan.nll) (site B abridged):

```nll-no-validate
lab "multi-tenant-wan" { dns hosts }

profile router { forward ipv4 }

# Site A hub — VRF red, VRF blue, two WG tunnels, nftables policy.
node hub-a : router {
  vrf red  table 10 {
    interfaces [eth1, wg-red]
    route 10.20.0.0/16 dev wg-red
  }
  vrf blue table 20 {
    interfaces [eth2, wg-blue]
    route 10.21.0.0/16 dev wg-blue
  }

  wireguard wg-red {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [hub-b]
  }
  wireguard wg-blue {
    key auto
    listen 51821
    address 192.168.255.3/32
    peers [hub-b]
  }

  firewall policy drop {
    accept ct established,related
    accept udp dport 51820
    accept udp dport 51821
    accept tcp dport 22 src 10.10.0.0/16
    accept tcp dport 22 src 10.20.0.0/16
  }
}

# Site B mirrors site A.
node hub-b : router {
  vrf red  table 10 {
    interfaces [eth1, wg-red]
    route 10.10.0.0/16 dev wg-red
  }
  vrf blue table 20 {
    interfaces [eth2, wg-blue]
    route 10.11.0.0/16 dev wg-blue
  }
  # ... wireguard wg-red, wg-blue, firewall — same shape ...
}

# Customer hosts.
node red-a   { route default via 10.10.0.1 }
node blue-a  { route default via 10.11.0.1 }
node red-b   { route default via 10.20.0.1 }
node blue-b  { route default via 10.21.0.1 }

# Per-VRF site-local links.
link hub-a:eth1 -- red-a:eth0  { 10.10.0.1/16 -- 10.10.0.10/16 }
link hub-a:eth2 -- blue-a:eth0 { 10.11.0.1/16 -- 10.11.0.10/16 }
link hub-b:eth1 -- red-b:eth0  { 10.20.0.1/16 -- 10.20.0.10/16 }
link hub-b:eth2 -- blue-b:eth0 { 10.21.0.1/16 -- 10.21.0.10/16 }

# Shared WAN underlay between hubs (carries both WG tunnels).
link hub-a:wan0 -- hub-b:wan0 { 10.0.0.1/30 -- 10.0.0.2/30 }
impair hub-a:wan0 delay 30ms jitter 8ms loss 0.3%

# Build-time isolation checks.
validate {
  reach red-a   red-b      # red end-to-end via WG
  reach blue-a  blue-b     # blue end-to-end via WG
  no-reach red-a blue-a    # cross-VRF same-site isolated
  no-reach red-a blue-b    # cross-VRF cross-site isolated
}
```

(Site B's hub block is symmetric to site A — see the example file
for the complete declaration.)

The `validate` block at deploy time asserts the composition is
correct. Three things have to be right for it to pass:

1. **VRF isolation**: red's host has no route to blue's, even
   though they share the hub.
2. **WireGuard tunnels up**: red's WG between hubs handles
   `red-a → red-b` traffic; blue's likewise.
3. **nftables doesn't drop intended traffic**: the WG underlay
   port (UDP 51820/51821) is on the accept list.

## Run

```bash
sudo nlink-lab deploy examples/cookbook/multi-tenant-wan.nll
```

Deploy runs the 18-step sequence, including step 17 (validation).
If the validate block fails, deploy returns non-zero and the
diff is printed.

### Verify customer red

```bash
# red-a → red-b through wg-red on hub-a → wg-red on hub-b
sudo nlink-lab exec multi-tenant-wan red-a -- ping -c 3 10.20.0.10
```

Latency reflects the 30ms WAN underlay impairment plus WG
overhead.

### Verify VRF isolation

```bash
# red-a should NOT reach blue-a (same site, different VRF)
sudo nlink-lab exec multi-tenant-wan red-a -- ping -c 3 -W 2 10.11.0.10
# Expected: 100% packet loss

# red-a should NOT reach blue-b (different site, different VRF)
sudo nlink-lab exec multi-tenant-wan red-a -- ping -c 3 -W 2 10.21.0.10
# Expected: 100% packet loss
```

### Inspect the live composition

```bash
# VRF tables on hub-a
sudo nlink-lab exec multi-tenant-wan hub-a -- ip vrf show
sudo nlink-lab exec multi-tenant-wan hub-a -- ip route show vrf red
sudo nlink-lab exec multi-tenant-wan hub-a -- ip route show vrf blue

# WG tunnels
sudo nlink-lab exec multi-tenant-wan hub-a -- wg show

# Firewall policy
sudo nlink-lab exec multi-tenant-wan hub-a -- nft list ruleset
```

### Tear down

```bash
sudo nlink-lab destroy multi-tenant-wan
```

## What containerlab would need

The closest containerlab equivalent runs into three structural
walls:

1. **VRFs aren't first-class.** You'd need an SR Linux / cEOS /
   vMX image and the corresponding vendor-specific VRF config.
2. **WireGuard with auto-keygen needs custom logic.** clab
   doesn't generate WG keypairs; you'd manage keys offline,
   then `exec:` them into the container at startup.
3. **nftables rules go in the container's startup script.** Not
   in the topology file.

Approximate clab line count for the equivalent: ~300 lines (YAML
+ vendor configs + keygen script + startup-exec hooks). nlink-lab:
~90 lines, all in one place, no shell scripts.

If you NEED a vendor's VRF implementation specifically (say you're
testing how SR Linux handles route leaking), use containerlab. If
you want to test *your application* under realistic per-tenant
encrypted traffic, nlink-lab is the lighter setup.

## Variations

- **Cross-VRF leak**: drop a route in red that points at blue's
  subnet via a `lo` device. Useful for service-chain models.
- **Asymmetric per-VRF impair**: declare different impair on
  `wg-red` vs `wg-blue` — easy with [per-pair impair](satellite-mesh.md)
  if both tunnels share an L2 underlay.
- **Per-tenant rate limits**: add `rate hub-a:eth1 egress 100mbit`
  for red, `rate hub-a:eth2 egress 50mbit` for blue. Enforced by
  the kernel HTB shaper.
- **Rotate WG keys mid-test**: edit the NLL to change `key auto`
  to a fixed key, then `nlink-lab apply` (Plan 152 reconcile
  doesn't handle WG yet — full redeploy required for this one).

## Performance

On a 4-core laptop, this deploys in under 1 second:

- 6 namespaces (4 customer hosts + 2 hubs)
- 5 veth pairs
- 4 WireGuard interfaces (with key generation)
- 4 VRF devices
- ~20 nftables rules
- 8 routes (default + WG-via-vrf in each direction)

containerlab with vendor NOS images for the equivalent: typically
30+ seconds per deploy after image pull.

## Composing with `apply`

This is the lab where `nlink-lab apply` shines. Most of the
declarations — routes, VRF route entries, customer host routes —
all reconcile in place via Plan 152. Editing a route entry on hub-a
and running `nlink-lab apply` reconverges with zero packet loss
on the unchanged paths.

WireGuard config edits and nftables changes still need a redeploy
today (Plan 152 Phase B residual).

## When this is the wrong tool

- For testing a vendor's VRF/MPLS interop specifically, you need
  the vendor's image — use containerlab.
- For real-world MPLS L3VPN scale (hundreds of VRFs, BGP
  signaling), you need an actual MPLS data plane. Linux's VRF
  isn't an MPLS substitute — it's L3 isolation only.
- For multi-host setups, this whole topology runs on one host;
  splitting across hosts isn't supported.

## See also

- [Cookbook: VRF multi-tenant](vrf-multitenant.md) — the
  simpler 2-tenant single-PE version
- [Cookbook: WireGuard mesh](wireguard-mesh.md) — WG without VRF
- [Cookbook: nftables firewall](nftables-firewall.md) — stateful
  policy without VRF
- [NLL: VRF, WireGuard, nftables](../NLL_DSL_DESIGN.md)
