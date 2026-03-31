# Plan 113: Context-Sensitive Keywords (Fix Identifier Stealing)

**Date:** 2026-03-31
**Status:** Ready
**Effort:** Medium (1-2 days)
**Priority:** P0 — blocks any topology using common words as names

---

## Problem Statement

Every keyword added to the lexer (`wifi`, `up`, `down`, `at`, `ssid`, `log`,
`clear`, `scenario`, `benchmark`, etc.) becomes a reserved token that can never
be used as a node name, interface name, profile name, or identifier in NLL.

Examples of breakage:
- `leaf1:up0` — `up` tokenized as `Token::Up` (now fixed ad-hoc)
- `c2-fw:wifi` — `wifi` tokenized as `Token::Wifi`
- A user can't name a node `log`, `at`, `clear`, `scenario`, etc.

This will get worse with every feature we add. It's a systemic design issue.

## Root Cause

The logos lexer tokenizes keywords before the parser runs. The parser has no
chance to interpret context — `wifi` is always `Token::Wifi`, whether it appears
as `wifi wlan0 mode ap { ... }` (keyword) or `node:wifi` (interface name).

## Design: Reserved vs Context-Sensitive Keywords

**Reserved keywords** (always tokens — used at top level or to start blocks):
```
lab, node, profile, link, network, defaults, pool, validate, scenario,
benchmark, import, as, param, let, for, in
```

**Context-sensitive keywords** (parsed as `Token::Ident`, matched by string):
```
# Node properties (only valid inside node blocks)
forward, sysctl, lo, route, firewall, vrf, wireguard, vxlan, dummy,
macvlan, ipvlan, wifi, run, image, cmd

# Sub-keywords (only valid inside their parent blocks)
ssid, wpa2, mesh-id, channel, mode, parent,
policy, accept, drop, reject, ct, tcp, udp, dport, sport,
src, dst, icmp, icmpv6, mark,
delay, jitter, loss, rate, corrupt, reorder,
mtu, subnet, via, dev, metric, table,
pvid, tagged, untagged, vlan-filtering,
description, prefix, version, author, tags, mgmt, dns, runtime,
default, address, key, listen, peers,
reach, no-reach, tcp-connect, latency-under, route-has, dns-resolves,
samples, timeout, retries, interval,
at, down, up, clear, log,
count, hub, spokes, mesh, ring, star,
assert, duration, streams, udp,
background, exec,
cpu, memory, privileged, cap-add, cap-drop, entrypoint, hostname,
workdir, labels, pull, env-file, config, overlay, depends-on,
healthcheck, startup-delay
```

## Implementation

### Phase 1: Move sub-keywords to ident matching

For each context-sensitive keyword, remove its `#[token("...")]` from the
lexer and match on `Token::Ident(s) if s == "..."` in the parser.

**Lexer changes:** Remove ~60 keyword token definitions, leaving only the ~15
reserved keywords.

**Parser changes:** Every `Some(Token::Foo) =>` match arm becomes
`Some(Token::Ident(s)) if s == "foo" =>`. This is mechanical.

**Helper macro** to reduce boilerplate:
```rust
macro_rules! eat_kw {
    ($tokens:expr, $pos:expr, $kw:literal) => {
        matches!(at($tokens, *$pos), Some(Token::Ident(s)) if s == $kw)
            && { *$pos += 1; true }
    };
}
```

### Phase 2: Fix Display impl

The `Token` Display impl currently prints keyword names for each token variant.
With fewer token variants, this simplifies naturally. Idents display as their
string value.

### Phase 3: Update tests

Parser tests that match on specific token types need updating. Lexer tests
that check for specific keyword tokens need updating.

## Risks

- **Performance:** Matching `Ident(s) == "keyword"` is marginally slower than
  matching a specific token variant, but parsing is not a bottleneck (NLL files
  are small).
- **Error messages:** Currently, errors say "expected Token::Wifi, found ...".
  With ident matching, errors say "expected 'wifi', found ...". This is actually
  better for users.

## File Changes

| File | Change |
|------|--------|
| `lexer.rs` | Remove ~60 keyword tokens, keep ~15 reserved |
| `parser.rs` | Replace `Token::Foo` matches with `Token::Ident(s) if s == "foo"` |
| `parser.rs` | Add `eat_kw!` / `expect_kw!` helper macros |
| `lexer.rs` tests | Update keyword token tests |
| `parser.rs` tests | Minimal changes (tests use `parse_nll()` which is parser-level) |
