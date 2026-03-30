# DNS and DHCP in nlink-lab — Feasibility Report

**Date:** 2026-03-30

## Current State

nlink-lab has **no DNS or DHCP support**. All IP addressing is static (explicit CIDRs or
subnet/pool auto-assignment at parse time). The design document (`docs/NLINK_LAB.md`) lists
DHCP/DNS as a **low-priority gap**: *"Auto-assign addresses, resolve names"*.

There are zero references to dnsmasq, dhclient, resolv.conf, or nameserver anywhere in the
codebase or examples.

---

## Does DNS Work Today?

**No.** Lab nodes cannot resolve each other by name. Since network namespaces share the host
filesystem, processes inherit the host's `/etc/resolv.conf`, which points to the host's DNS
resolver (often `127.0.0.53` via systemd-resolved). That stub resolver is unreachable from
inside a network namespace because it binds to the host's loopback.

Users can work around this manually by running a DNS server (e.g., dnsmasq) inside a namespace
via an `exec` block, but there is no built-in support.

## Does DHCP Work Today?

**No.** There is no dynamic address assignment. All addresses are determined at parse time.

Users could technically spawn a DHCP server (dnsmasq, dhcpd) and client (dhclient) via `exec`
blocks, but they'd also need to leave interface addresses unconfigured — which the deployer
doesn't support (it always assigns addresses from the topology).

---

## How Do Other Lab Tools Handle This?

| Tool | DNS | DHCP |
|------|-----|------|
| **Containerlab** | Injects entries into host `/etc/hosts`; per-node `dns:` config passes `--dns` to Docker; Docker's embedded DNS (127.0.0.11) resolves container names on the management network | Static IPs from Docker subnet (no DHCP) |
| **netlab** | Relies on underlying provider | First-class `dhcp` module: auto-generates pools from topology, ships dnsmasq as a built-in device kind, supports relays and VRF-aware DHCP |
| **Mininet** | Inherits host `/etc/resolv.conf` | None — deterministic static IPs (`10.0.0.x`) |
| **GNS3** | User-configured per device (full OS images) | User-configured per device |

**Key takeaway:** most tools skip DHCP entirely and use static addressing. Only netlab treats
DHCP as a first-class feature, and that's because it targets network-engineer training
scenarios where DHCP configuration is part of the exercise.

---

## Technical Challenges in Network Namespaces

### DNS

A network namespace isolates the network stack but **not the filesystem**. `/etc/resolv.conf`
is shared across all namespaces unless extra steps are taken:

- **`ip netns exec` convention:** Files placed in `/etc/netns/<name>/` are bind-mounted over
  `/etc/` when a process is started via `ip netns exec`. So
  `/etc/netns/myns/resolv.conf` overrides `/etc/resolv.conf` for that namespace.
- **`setns(2)` path (what nlink uses):** When entering a namespace via `setns()` directly,
  the bind-mount trick does **not** apply. A mount namespace must be created manually and the
  bind mount performed explicitly.
- **systemd-resolved:** On modern distros, `/etc/resolv.conf` is a symlink to
  `127.0.0.53` (the stub resolver), which is unreachable from inside a netns. A real
  upstream DNS IP must be written into per-namespace resolv.conf.

### DHCP

DHCP works normally inside namespaces — it's pure L2/L3. A DHCP server binds to an interface
and listens for broadcasts; a client sends DHCPDISCOVER on its interface. Both work as long
as L2 connectivity (bridge or veth) exists. The main challenge is **integration with the
deployer**: currently, the deployer always assigns static addresses (step 9), so supporting
DHCP means allowing interfaces to remain unconfigured and delegating addressing to runtime.

---

## Implementation Options

### Tier 1 — Static /etc/hosts (DNS only, no new dependencies)

Generate a hosts file with all node name→IP mappings during deployment:

```
# /etc/netns/<lab>-<node>/hosts
10.0.1.1   router
10.0.2.2   server
10.0.1.2   client
```

**Pros:**
- Zero external dependencies
- Deterministic, reproducible
- Fits the existing deployment pipeline (add as a step after address assignment)
- This is essentially what containerlab does

**Cons:**
- Only covers lab-internal name resolution
- Doesn't help with external DNS (e.g., `apt install` inside a namespace)
- Requires creating a mount namespace for processes launched via `setns()`

**NLL syntax idea:**
```nll
lab "example" {
  dns hosts   # auto-generate /etc/hosts from topology
}
```

### Tier 2 — Spawn dnsmasq as Infrastructure (DNS + DHCP)

Add NLL blocks to declare DNS/DHCP services. Under the hood, spawn dnsmasq in a namespace:

```nll
node infra {
  dns {
    upstream 8.8.8.8
    zone lab.local   # auto-populated from topology
  }
  dhcp eth0 {
    range 10.0.0.100 10.0.0.200
    gateway 10.0.0.1
  }
}
```

The deployer would:
1. Generate a dnsmasq config from the topology
2. Spawn dnsmasq inside the designated namespace
3. Write per-namespace `resolv.conf` pointing to the dnsmasq instance

**Pros:**
- Full DNS + DHCP in one lightweight process (~1-2 MB)
- dnsmasq is battle-tested (used by libvirt, LXC, netlab, OpenWrt)
- `--bind-interfaces` flag designed for multi-namespace environments
- Supports static leases, TFTP, PXE, DNSSEC

**Cons:**
- External dependency (dnsmasq must be installed)
- More complex lifecycle management (need to track and kill dnsmasq on destroy)
- DHCP requires the deployer to skip address assignment for DHCP-enabled interfaces

### Tier 3 — Embedded Rust DNS/DHCP Server (maximum integration)

Use Rust crates to embed DNS/DHCP directly in nlink-lab:

| Crate | Purpose | Maturity |
|-------|---------|----------|
| **hickory-server** (fka trust-dns) | Authoritative DNS server library | Mature, async/Tokio, MIT/Apache 2.0 |
| **dhcproto** | DHCPv4/v6 message parser/encoder | Maintained by BlueCat Networks |
| **dhcp4r** | Simple DHCP server library | Includes working server example |

**Pros:**
- No external dependencies — pure Rust, single binary
- Full programmatic control; auto-populate DNS zones from topology at deploy time
- Tight integration with the deployer lifecycle

**Cons:**
- Significant implementation effort (especially DHCP: lease management, broadcast sockets, option handling)
- Adds dependency weight to the library crate
- Reinventing what dnsmasq already does well

---

## Should This Be in Scope?

### DNS name resolution — Yes (Tier 1)

Auto-generating `/etc/hosts` from the topology is low-effort, high-value, and fits naturally
into the deployer pipeline. Every lab user benefits from being able to `ping server` instead
of `ping 10.0.2.2`. This should be a built-in feature.

### External DNS forwarding — Maybe (Tier 2)

Useful when lab nodes need internet access (e.g., `apt install` inside a namespace). Could
be solved by writing a `resolv.conf` pointing to the host's upstream DNS. Not critical for
most lab scenarios but nice to have.

### DHCP — Probably not built-in

DHCP is a **testing target**, not infrastructure. Users who need to test DHCP scenarios
(server config, relay agents, failover) should configure it explicitly — just like they'd
configure BGP or OSPF. nlink-lab already supports this via `exec` blocks to spawn dnsmasq.

Making DHCP a first-class deployer feature adds complexity for a niche use case. The
declarative static addressing model is a strength, not a limitation — it makes topologies
reproducible and deterministic.

**Exception:** if nlink-lab adds container support with dynamic IPs (Docker-style), a
built-in DHCP mechanism may become necessary. But for namespace-based labs with explicit
topologies, static addressing is the right default.

---

## Recommended Roadmap

| Phase | Feature | Effort | Priority |
|-------|---------|--------|----------|
| **1** | Auto-generate `/etc/hosts` per namespace from topology | Small | High |
| **2** | Auto-generate `/etc/resolv.conf` per namespace (configurable upstream DNS) | Small | Medium |
| **3** | NLL `dns` block to spawn dnsmasq with auto-populated zone | Medium | Low |
| **4** | NLL `dhcp` block to spawn dnsmasq with auto-generated pool config | Medium | Low |

Phases 1–2 require no external dependencies and could be implemented as additional deployer
steps. Phases 3–4 would add an optional dnsmasq dependency and new NLL syntax.
