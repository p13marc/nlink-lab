# Stateful nftables firewall

A node with a default-drop policy that accepts only established
connections, specific ports, and SSH from a trusted subnet. The
ruleset uses the kernel's nftables — same engine production
firewalls use.

## When to use this

- Testing how a service handles being firewalled (connection
  refused vs. connection timeout vs. RST).
- Validating that your application correctly opens the right ports
  and nothing else.
- Modeling a DMZ host or a production backend that should reject
  everything except a small whitelist.
- Conntrack-aware tests where return traffic must match an outbound
  state.

## Why nlink-lab

containerlab's
[generic Linux containers](https://containerlab.dev/manual/kinds/linux/)
can run nftables, but you have to ship the rules as a startup
script and manage them out-of-band. nlink-lab declares the firewall
inline in NLL, with the same `src`/`dst`/`ct` matchers nftables
itself takes — and writes the ruleset directly via the netlink
nftables interface (no shell exec, no `nft -f`).

## NLL

[`examples/firewall.nll`](../../examples/firewall.nll):

```nll
lab "firewall" {
  description "Server with stateful nftables firewall"
  dns hosts                          # /etc/hosts injection so we can
                                     # ping by name
}

profile router { forward ipv4 }
node router : router
node client { route default via ${router.eth0} }

node server {
  route default via ${router.eth1}

  firewall policy drop {
    accept ct established,related      # return traffic for outbound
    accept tcp dport 8080              # public service
    accept udp dport 53                # DNS replies (if server is also
                                       # the resolver)
    accept tcp dport 22 src 10.0.1.0/24  # SSH only from trusted LAN
  }
}

link router:eth0 -- client:eth0 { subnet 10.0.1.0/24 }
link router:eth1 -- server:eth0 { subnet 10.0.2.0/24 }
```

The `firewall` block declares an `inet filter input` chain with
the named policy. Each `accept` line is a single rule. The
matchers (`ct`, `tcp dport`, `src`) compile to standard nftables
expressions.

## Run

```bash
sudo nlink-lab deploy examples/firewall.nll
```

### Verify the rules from inside the server

```bash
sudo nlink-lab exec firewall server -- nft list ruleset
```

You'll see the chain with all five rules.

### Validate the policy

Allowed: TCP port 8080.

```bash
# Spawn a quick listener
sudo nlink-lab spawn firewall server -- python3 -m http.server 8080 --bind 0.0.0.0

# Client should connect
sudo nlink-lab exec firewall client -- curl -fsS --max-time 3 http://server:8080/
```

Blocked: anything not on the accept list.

```bash
# Server isn't running on port 9999, but firewall blocks before that:
# Connection times out (drop), not refused (accept-then-rst).
sudo nlink-lab exec firewall client -- timeout 2 nc -v server 9999
echo "exit code: $?"   # non-zero (timeout)
```

Conditional: SSH from trusted subnet only.

```bash
# Trusted client (10.0.1.0/24): would connect (no ssh server is
# running here, but the SYN gets through and connection is
# refused at the application layer).
sudo nlink-lab exec firewall client -- timeout 2 nc -v server 22

# An "untrusted" simulator (drops outside 10.0.1.0/24 don't apply
# in this lab — to test, add a third client in a different subnet).
```

### Inspect conntrack

```bash
sudo nlink-lab exec firewall server -- conntrack -L
```

Active connections appear; the `ESTABLISHED` state is what `accept
ct established,related` matches against.

### Tear down

```bash
sudo nlink-lab destroy firewall
```

## What nlink-lab built

The `firewall` block lowers to a sequence of nftables rules under a
table named after the node. nlink-lab uses
[`nlink::netlink::nftables`](https://docs.rs/nlink/latest/nlink/netlink/nftables/)
to write them transactionally — the table appears atomically, with
no in-between state.

```text
table inet filter {
  chain input {
    type filter hook input priority 0; policy drop;
    ct state established,related accept
    tcp dport 8080 accept
    udp dport 53 accept
    ip saddr 10.0.1.0/24 tcp dport 22 accept
  }
}
```

(The exact rule order matches the NLL order. Add/reorder as
needed; nftables evaluates top-to-bottom.)

## Variations

- **Conntrack zones**: assign different conntrack zones to
  different VRFs to track them independently. Use
  `ct zone N` matchers.
- **NAT**: separate from `firewall`, the `nat { }` block handles
  masquerade/SNAT/DNAT. See [NLL: NAT syntax](../NLL_DSL_DESIGN.md).
- **Per-direction policy**: declare a second `firewall` block on the
  same node for the `output` chain.
- **Default-accept with explicit drops**: invert the policy
  (`policy accept`) and use `drop` rules for blacklist style.

## When this is the wrong tool

If you're testing a vendor firewall product specifically (Palo
Alto, Fortinet, Cisco ASA), you need that vendor's image — use
containerlab. nlink-lab uses the kernel's nftables only.

## See also

- [NLL: firewall syntax](../NLL_DSL_DESIGN.md#7-firewall)
- [vrf-multitenant.md](vrf-multitenant.md) — combine VRF + firewall for per-tenant policy
