# Bridge VLAN trunks with access/trunk ports

A vlan-aware bridge enforcing 802.1Q segmentation: hosts on VLAN
100 (sales) cannot reach hosts on VLAN 200 (engineering), even
though all three hosts share one physical bridge.

## When to use this

- Validating that a service correctly tags / strips VLAN headers.
- Modeling a campus access switch with one VLAN per department.
- Testing VLAN-aware applications (DHCP relay agents, voice/data
  separation).

## Why nlink-lab

containerlab can do VLAN tagging via a dedicated NOS image (SR
Linux, vJunos), but with `kind: linux` containers and the default
Docker network, VLAN filtering on the bridge is awkward — Docker
manages the bridge and discourages userspace messing with it.

nlink-lab declares VLANs and per-port roles inline, then writes
the bridge VLAN configuration via the kernel's bridge VLAN
netlink API directly.

## NLL

[`examples/vlan-trunk.nll`](../../examples/vlan-trunk.nll):

```nll
lab "vlan-trunk" { description "Bridge with VLAN trunking" }

node host1
node host2
node host3

network fabric {
  vlan-filtering                       # turn on the bridge's VLAN filter
  members [host1:eth0, host2:eth0, host3:eth0]

  vlan 100 "sales"
  vlan 200 "engineering"

  port host1 { pvid 100  untagged }    # access port on VLAN 100
  port host2 { pvid 100  untagged }    # same
  port host3 { pvid 200  untagged }    # access port on VLAN 200
}
```

`vlan-filtering` enables 802.1Q on the bridge. `pvid` sets the
default VLAN for untagged ingress; `untagged` sets the egress
treatment. host1 and host2 are on the same broadcast domain
(VLAN 100); host3 is alone on VLAN 200.

## Run

```bash
sudo nlink-lab deploy examples/vlan-trunk.nll
```

The lab has no L3 addresses by default — VLAN segmentation happens
at L2. Add IPs manually for the test:

```bash
sudo nlink-lab exec vlan-trunk host1 -- ip addr add 10.0.0.1/24 dev eth0
sudo nlink-lab exec vlan-trunk host2 -- ip addr add 10.0.0.2/24 dev eth0
sudo nlink-lab exec vlan-trunk host3 -- ip addr add 10.0.0.3/24 dev eth0
```

### Verify same-VLAN connectivity

```bash
# host1 and host2 share VLAN 100 — connectivity works
sudo nlink-lab exec vlan-trunk host1 -- ping -c 3 10.0.0.2
```

### Verify cross-VLAN isolation

```bash
# host1 (VLAN 100) cannot reach host3 (VLAN 200), even though same subnet
sudo nlink-lab exec vlan-trunk host1 -- ping -c 3 -W 2 10.0.0.3
# expect 100% packet loss
```

### Inspect the bridge VLAN configuration

```bash
sudo bridge -d vlan show
```

You'll see the per-port PVID and untagged-egress entries.

### Tear down

```bash
sudo nlink-lab destroy vlan-trunk
```

## Trunk ports

For a node that needs to see multiple VLANs (e.g. a router doing
inter-VLAN routing), declare a trunk port:

```nll
node router

network fabric {
  vlan-filtering
  members [router:eth0, host1:eth0]

  vlan 100 "sales"
  vlan 200 "engineering"

  port router { vlans [100, 200] tagged }    # trunk port
  port host1  { pvid 100  untagged }          # access port
}
```

The router sees tagged frames on VLAN 100 and 200; host1 sees
untagged frames on VLAN 100. The router can configure
sub-interfaces (`eth0.100`, `eth0.200`) to handle each VLAN.

## What nlink-lab built

The `network` block creates a bridge with `vlan_filtering=1`.
Per-port VLAN entries are written via netlink's
[bridge VLAN API](https://docs.rs/nlink/latest/nlink/netlink/bridge_vlan/).
Each port's PVID + tagged/untagged flags are an entry in the
bridge's VLAN database.

## Variations

- **Native VLAN with tagged additional**: `port x { pvid 100 vlans [200, 300] tagged }`.
- **VXLAN extension**: terminate a VXLAN tunnel into a bridge VLAN
  for L2 over WAN.
- **Multiple bridges**: declare two `network` blocks on the same
  set of nodes for two independent L2 domains.

## When this is the wrong tool

If you're testing a vendor switch's VLAN behavior specifically
(Cisco IOS access/trunk port modes, switchport native vlan), use
containerlab with that NOS image. nlink-lab uses the Linux bridge
implementation only.

## See also

- [NLL: networks (bridges)](../NLL_DSL_DESIGN.md#5-networks-bridges)
