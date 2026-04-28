# macvlan: attach a lab node to a host physical NIC

Give a lab node a real address on the host's physical LAN, with its
own MAC, addressable from any other machine on the network. The
node can reach external services and be reached from external
clients — without bridging the entire host network namespace.

## When to use this

- A lab node needs to talk to a real DHCP server, a real upstream
  router, or another machine on your LAN.
- You're testing a service from a real client (your laptop, a
  phone) and want it to point at a lab address.
- Modeling a "data plane" interface that participates in an
  external L2 broadcast domain.

## Why nlink-lab

Docker can do something similar with `--network macvlan`, but the
container loses access to the host's localhost (the macvlan interface
can't talk to the host's own management stack — by design). nlink-lab
sidesteps the issue: the lab node lives in its own namespace where
that limitation doesn't matter, and the rest of the lab (other
nodes) is still wired by ordinary veth pairs.

This is straightforward kernel networking — `ip link add ... type
macvlan` — wrapped in a declarative form.

## NLL

[`examples/macvlan.nll`](../../examples/macvlan.nll):

```nll
lab "macvlan-demo"
profile router { forward ipv4 }

node gateway : router {
  macvlan eth0 parent "enp3s0" mode bridge {
    192.168.1.100/24
  }
  route default via 192.168.1.1
}

node internal { route default via 10.0.0.1 }

link gateway:veth0 -- internal:eth0 { subnet 10.0.0.0/24 }
```

`gateway` has a macvlan interface attached to the host's `enp3s0`
(edit to match your host's NIC name). The gateway also has an
internal-facing veth (`veth0`) that internal traffic uses.

`internal` knows nothing about the host LAN — it routes default via
the gateway's internal address.

## Run

```bash
# Find your host NIC
ip -o link show | awk -F': ' '{print $2}' | grep -v '^lo'

# Edit examples/macvlan.nll and replace "enp3s0" with your NIC name
sudo nlink-lab deploy examples/macvlan.nll
```

### Verify the gateway reaches the LAN

```bash
sudo nlink-lab exec macvlan-demo gateway -- ping -c 3 192.168.1.1
```

If your LAN gateway is at `192.168.1.1`, the ping should succeed.

### Reach the gateway from another machine

From your laptop or another box on the LAN:

```bash
ping 192.168.1.100
```

The gateway responds — its macvlan address is a first-class citizen
on the LAN.

### Verify the internal node still routes through the gateway

```bash
sudo nlink-lab exec macvlan-demo internal -- ping -c 3 192.168.1.1
```

Traffic flows `internal → veth → gateway → macvlan → enp3s0`.

### Tear down

```bash
sudo nlink-lab destroy macvlan-demo
```

The macvlan interface is removed; the host's physical NIC is
unaffected.

## macvlan modes

The `mode` keyword takes one of:

| Mode | Behavior |
|------|----------|
| `bridge` | The default. Macvlan interfaces on the same parent can talk to each other through the kernel's macvlan bridge. |
| `private` | Macvlan interfaces on the same parent cannot reach each other. |
| `vepa` | Virtual Ethernet Port Aggregator — traffic between macvlan peers exits the parent NIC and re-enters via an external bridge. Rare. |
| `passthru` | One-to-one with the parent NIC. Disables filtering. |

For most labs `bridge` is what you want.

## ipvlan alternative

If your environment doesn't permit macvlan (some DHCP servers reject
unknown MACs, some hypervisors block additional MACs on a port),
use `ipvlan` instead:

```nll-ignore
node gateway {
  ipvlan eth0 parent "enp3s0" mode l2 {
    192.168.1.100/24
  }
}
```

ipvlan shares the parent's MAC and only differentiates by IP. Modes:
`l2`, `l3`, `l3s`. See
[NLL: macvlan / ipvlan](../NLL_DSL_DESIGN.md).

## When this is the wrong tool

- If you need the lab to be **reachable from the host's own
  applications** (not from another machine), macvlan won't work for
  the same fundamental reason it doesn't in Docker. Use a
  `host-reachable` mgmt bridge instead — see
  [the user guide](../USER_GUIDE.md).
- If your "physical interface" is actually a Wi-Fi adapter, macvlan
  may not work — many Wi-Fi drivers reject MAC spoofing. Use
  ipvlan.

## See also

- [NLL: macvlan / ipvlan](../NLL_DSL_DESIGN.md)
