# VRF customer separation in a single namespace

A provider-edge (PE) router serving two customers, with each
customer's routes living in a separate VRF. Customer A and customer
B can use overlapping IP space and remain isolated at L3 — tenant-A
cannot reach tenant-B even though both ride the same PE.

VRFs are a Linux kernel feature; they're invisible to Docker's
network model.
[containerlab](https://containerlab.dev) requires a
vendor NOS image (cEOS, SR Linux, vMX) for VRF — you cannot build
this with `kind: linux` containers and a YAML topology.

## When to use this

- Multi-tenant lab where two customers share infrastructure but
  must remain isolated.
- Testing a routing daemon that imports/exports between VRFs.
- Modeling MPLS L3VPN PE behavior without paying the vendor-image
  tax.

## NLL

[`examples/vrf-multitenant.nll`](../../examples/vrf-multitenant.nll):

```nll
lab "vrf-multitenant" { description "PE router with VRF tenant isolation" }

profile router { forward ipv4 }

node pe : router {
  vrf red  table 10 { interfaces [eth1]  route default dev eth1 }
  vrf blue table 20 { interfaces [eth2]  route default dev eth2 }
}

node tenant-a { route default via 10.10.0.1 }
node tenant-b { route default via 10.20.0.1 }

link pe:eth1 -- tenant-a:eth0 { 10.10.0.1/24 -- 10.10.0.10/24 }
link pe:eth2 -- tenant-b:eth0 { 10.20.0.1/24 -- 10.20.0.10/24 }
```

Two VRFs (`red` table 10, `blue` table 20). Each owns one PE
interface. Tenant hosts route default through the PE.

## Run

```bash
sudo nlink-lab deploy examples/vrf-multitenant.nll
```

### Verify isolation

Tenant-A should reach the PE in its VRF:

```bash
sudo nlink-lab exec vrf-multitenant tenant-a -- ping -c 2 10.10.0.1
# 0% packet loss
```

Tenant-A should NOT reach tenant-B even though both addresses
exist on the same PE node:

```bash
sudo nlink-lab exec vrf-multitenant tenant-a -- ping -c 2 10.20.0.10
# 100% packet loss — destination unreachable
```

Inspect the VRFs on the PE:

```bash
sudo nlink-lab exec vrf-multitenant pe -- ip vrf show
sudo nlink-lab exec vrf-multitenant pe -- ip route show vrf red
sudo nlink-lab exec vrf-multitenant pe -- ip route show vrf blue
```

### Tear down

```bash
sudo nlink-lab destroy vrf-multitenant
```

## What nlink-lab built

For each `vrf` block, nlink-lab uses
[`nlink::netlink::link::VrfLink`](https://docs.rs/nlink/latest/nlink/netlink/link/struct.VrfLink.html)
to create a kernel VRF device, then enslaves the named interfaces
into it. The VRF table number drives the routing table — `ip route
show table 10` shows the red VRF's routes.

This is standard kernel networking. No userspace routing daemon,
no vendor stack.

## Variations

- **Add a third VRF for management traffic**, with its own route to
  an out-of-band ssh jump host.
- **Cross-VRF leaking** via a route in both tables — useful when a
  shared service (DNS, NTP) needs to be reachable from both
  customers.
- **VRF + WireGuard**: terminate a per-customer WireGuard tunnel
  inside the customer's VRF for site-to-site VPN. See
  [wireguard-mesh.md](wireguard-mesh.md).

## When this is the wrong tool

If you're testing a Cisco IOS XR or Juniper Junos VRF
implementation specifically, you need that NOS's image — use
containerlab.

## See also

- [NLL: VRF syntax](../NLL_DSL_DESIGN.md#8-vrf)
