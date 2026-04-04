# nlink Crate Bug Report

Bugs encountered in the `nlink` crate (netlink library) during nlink-lab development and testing. These are issues in the underlying `nlink` library, not in nlink-lab itself.

**nlink version:** 0.12.1
**Kernel:** Linux 6.17.0-19-generic (Ubuntu)

---

## 1. DNAT nftables rules fail with EAFNOSUPPORT

**Severity: Medium**
**Reproducible: Yes**

### Description

Applying DNAT nftables rules via the nlink API fails with `Address family not supported by protocol (os error 97)`. This occurs when nlink-lab deploys a topology with NAT `translate` rules that are expanded into per-host DNAT rules.

### Reproduction

```rust
// Pseudocode — the actual call chain is:
// deploy.rs → apply_nat() → conn.add_rule(dnat_rule)

let conn: Connection<Nftables> = namespace::connection_for(ns)?;
conn.add_table("nlink-lab", Family::Inet)?;
conn.add_chain(Chain::new("nlink-lab", "prerouting")
    .family(Family::Inet)
    .hook(Hook::PreRouting)
    .chain_type(ChainType::Nat)
    .priority(Priority::Nat))?;

// This rule fails:
conn.add_rule(Rule::new("nlink-lab", "prerouting")
    .family(Family::Inet)
    .match_dst_ip("144.0.1.18/32")  // match destination
    .dnat("172.100.1.18"))?;         // DNAT target
// Error: Address family not supported by protocol (errno 97)
```

### Expected Behavior

DNAT rules should work in the `inet` family table, same as masquerade and SNAT which work correctly.

### Analysis

The error suggests that the nftables netlink message is using an incorrect address family constant or missing a required attribute. Possible causes:

1. **IPv4-specific NAT expressions in inet family** — The DNAT expression may be using `NFT_META_NFPROTO` without specifying IPv4, or the payload expression may reference `NFPROTO_IPV4` fields but the table is `NFPROTO_INET`
2. **Missing `nft_nat` module** — The kernel module for NAT expressions may need to be loaded separately
3. **Expression encoding** — The destination IP match + DNAT combination may need specific expression ordering that nlink doesn't produce correctly

### Workaround

Using `masquerade` (which works) instead of `dnat` for simple NAT scenarios. For 1:1 NAT, the nlink-lab `translate` directive is currently broken due to this issue.

### Affected nlink-lab Examples

- `examples/nat.nll` — DNAT rules fail to deploy
- `examples/multi-site.nll` — `translate` directive (which expands to DNAT rules) would fail if the topology contained translate rules

---

## 2. Connection::new() API inconsistency

**Severity: Low**
**Type: API ergonomics**

### Description

`Connection::<Route>::new()` is synchronous (returns `Result<Connection>`), but `Connection::<Nl80211>::new()` returns a different type/behavior. The API documentation doesn't clearly distinguish which protocol connections are sync vs async.

### Impact

Minor — developers need to check by trial-and-error whether `.await` is needed. We hit this when implementing the mgmt bridge (tried `Connection::new().await` and got a compiler error).

### Suggestion

Document the sync/async nature of `Connection::new()` per protocol type, or make all constructors consistently async.

---

## 3. No `set_link_netns_by_name` convenience method

**Severity: Low**
**Type: Missing API**

### Description

Moving a link to a different namespace requires opening the namespace FD first:

```rust
let ns_fd = namespace::open(ns_name)?;
conn.set_link_netns_fd(ifname, ns_fd.as_raw_fd())?;
```

A convenience method `set_link_netns_by_name(ifname, ns_name)` would reduce boilerplate, especially since `VethLink::new().peer_netns_fd()` already accepts an FD (mixing abstraction levels).

### Suggestion

```rust
// Current (requires manual FD management)
let ns_fd = namespace::open("my-ns")?;
conn.set_link_netns_fd("eth0", ns_fd.as_raw_fd())?;

// Proposed convenience
conn.set_link_netns("eth0", "my-ns")?;
```

---

## Summary

| # | Bug | Severity | Blocking? |
|---|-----|----------|-----------|
| 1 | DNAT nftables EAFNOSUPPORT | Medium | Blocks `translate`/`dnat` NAT rules |
| 2 | Connection::new() sync/async inconsistency | Low | No (cosmetic) |
| 3 | Missing set_link_netns_by_name | Low | No (workaround exists) |

Bug #1 is the only one that affects functionality. The others are API ergonomics.
