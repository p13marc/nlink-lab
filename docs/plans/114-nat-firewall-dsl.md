# Plan 114: NAT Support in Firewall DSL

**Date:** 2026-03-31
**Status:** Ready
**Effort:** Medium (2-3 days)
**Priority:** P1 — blocks realistic firewall/router topologies
**Depends on:** Nothing (nlink has snat/dnat/masquerade on Rule type)

---

## Problem Statement

nlink-lab's firewall DSL only generates **filter** chains (input + forward).
Real infrastructure requires NAT — SNAT, DNAT, and masquerade — for routing
between internal networks and WAN links.

Current workaround: `run ["iptables", "-t", "nat", ...]` commands, which:
- Bypass the declarative topology model
- Don't render in `nlink-lab render`
- Can't be validated or diffed
- Depend on `iptables` being installed

nlink already has `Rule::snat()`, `Rule::dnat()`, `Rule::masquerade()` on
its nftables `Rule` type. nlink-lab just doesn't create NAT chains or expose
these in the DSL.

## NLL Syntax

### Option A: Separate `nat` block (recommended)

```nll
node firewall {
  forward ipv4

  firewall policy accept {
    accept ct established,related
    drop tcp dport 22 src 10.0.0.0/8
  }

  nat {
    masquerade src 10.2.0.0/16
    dnat dst 144.18.1.0/24 to 172.100.1.18
    dnat dst 144.18.2.0/24 to 172.100.2.18
    snat src 10.2.0.0/16 to 172.100.1.2
  }
}
```

**Pros:** Clean separation between filter and NAT. Easy to read.
**Cons:** New top-level node property block.

### Option B: NAT rules inside firewall block

```nll
node firewall {
  firewall policy accept {
    accept ct established,related
    masquerade src 10.2.0.0/16
    dnat dst 144.18.1.0/24 to 172.100.1.18
  }
}
```

**Pros:** No new block type.
**Cons:** Mixes filter and NAT in one block, which is confusing since they use
different nftables chains (filter vs nat).

**Recommendation:** Option A — separate `nat` block.

## NAT Rules

| Rule | NLL Syntax | nftables equivalent |
|------|-----------|-------------------|
| Masquerade | `masquerade src CIDR` | `ip saddr CIDR masquerade` in postrouting |
| SNAT | `snat src CIDR to IP` | `ip saddr CIDR snat to IP` in postrouting |
| DNAT | `dnat dst CIDR to IP` | `ip daddr CIDR dnat to IP` in prerouting |
| DNAT with port | `dnat dst CIDR tcp dport PORT to IP:PORT` | `ip daddr CIDR tcp dport PORT dnat to IP:PORT` |
| Redirect | `redirect tcp dport PORT to PORT` | `tcp dport PORT redirect to :PORT` in prerouting |

## Implementation

### 1. Types (`types.rs`)

```rust
pub struct NatConfig {
    pub rules: Vec<NatRule>,
}

pub struct NatRule {
    pub action: NatAction,
    /// Source CIDR match (for masquerade/snat).
    pub src: Option<String>,
    /// Destination CIDR match (for dnat).
    pub dst: Option<String>,
    /// Protocol + port match (optional).
    pub proto_port: Option<(String, u16)>,
    /// Target address (for snat/dnat).
    pub target: Option<String>,
    /// Target port (for dnat with port).
    pub target_port: Option<u16>,
}

pub enum NatAction {
    Masquerade,
    Snat,
    Dnat,
    Redirect,
}
```

Add `nat: Option<NatConfig>` to `Node`.

### 2. Lexer

New context-sensitive keywords (parsed as idents, per Plan 113):
`nat`, `masquerade`, `snat`, `dnat`, `redirect`, `to`.

### 3. AST + Parser

```
nat_block  = "nat" "{" nat_rule* "}"
nat_rule   = "masquerade" match_clause*
           | "snat" match_clause* "to" IP
           | "dnat" match_clause* "to" IP (":" INT)?
           | "redirect" match_clause* "to" INT
match_clause = "src" CIDR | "dst" CIDR | "tcp" "dport" INT | "udp" "dport" INT
```

### 4. Deploy (`deploy.rs`)

Extend `apply_firewall()` to also create NAT chains when `node.nat` is set:

```rust
// Create nat table (reuse "nlink-lab" table, different chain types)
let pre_chain = Chain::new(table_name, "prerouting")
    .family(Family::Inet)
    .hook(Hook::Prerouting)
    .priority(Priority::DstNat)
    .chain_type(ChainType::Nat);
nft_conn.add_chain(pre_chain).await?;

let post_chain = Chain::new(table_name, "postrouting")
    .family(Family::Inet)
    .hook(Hook::Postrouting)
    .priority(Priority::SrcNat)
    .chain_type(ChainType::Nat);
nft_conn.add_chain(post_chain).await?;

for rule in &nat.rules {
    match rule.action {
        NatAction::Masquerade => {
            let nft_rule = Rule::new(table_name, "postrouting")
                .family(Family::Inet)
                .match_saddr_v4(addr, prefix)
                .masquerade();
            nft_conn.add_rule(nft_rule).await?;
        }
        NatAction::Dnat => {
            let nft_rule = Rule::new(table_name, "prerouting")
                .family(Family::Inet)
                .match_daddr_v4(addr, prefix)
                .dnat(target_addr, target_port);
            nft_conn.add_rule(nft_rule).await?;
        }
        // ...
    }
}
```

### 5. nlink Requirements

Check that nlink's nftables API supports:
- `ChainType::Nat` — for NAT chains
- `Priority::DstNat` / `Priority::SrcNat` — correct priorities
- `Hook::Prerouting` / `Hook::Postrouting` — NAT hooks

If any of these are missing in nlink's types, a feature request is needed.

### 6. Render

Render `nat` block inside node definitions.

### 7. Tests

| Test | Description |
|------|-------------|
| `test_parse_nat_masquerade` | Parser: `nat { masquerade src 10.0.0.0/8 }` |
| `test_parse_nat_dnat` | Parser: `nat { dnat dst 144.18.0.0/16 to 172.100.1.18 }` |
| `test_parse_nat_snat` | Parser: `nat { snat src 10.0.0.0/8 to 172.100.1.2 }` |
| `test_parse_nat_dnat_port` | Parser: `dnat dst ... tcp dport 80 to 10.0.0.1:8080` |
| `test_lower_nat` | Lower: AST to typed NatConfig |
| `test_render_nat` | Render: roundtrip |
| Integration: `deploy_nat` | Deploy with masquerade, verify NAT via conntrack |

### File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `NatConfig`, `NatRule`, `NatAction`, `nat` field on `Node` |
| `lexer.rs` | No changes (context-sensitive per Plan 113) |
| `ast.rs` | Add `NatDef`, `NatRuleDef`, `NodeProp::Nat` |
| `parser.rs` | Parse `nat { ... }` block in node properties |
| `lower.rs` | Lower to typed NatConfig |
| `deploy.rs` | Create NAT chains + rules in `apply_firewall()` |
| `render.rs` | Render `nat` blocks |
| `validator.rs` | Validate NAT rules (valid CIDRs, valid target IPs) |
| `builder.rs` | Add `nat()` method to `NodeBuilder` |
