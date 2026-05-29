# Plan 158 — Taking advantage of nlink 0.16 + 0.17 + 0.18

**Date:** 2026-05-27 (rewritten 2026-05-29 after nlink 0.18.0)
**Status:** Proposal — all upstream prerequisites have landed
**Effort:** S to M depending on scope
**Priority:** P1 for the nftables-reconcile arc (PR A); P2 for the rest

---

## TL;DR

nlink shipped three back-to-back releases since nlink-lab last
upgraded (`0.15.1`):

- **`0.16.0` (2026-05-25)** — substantial-but-additive. New
  `nlink-macros` proc-macro crate, DPLL + `net_shaper` families,
  declarative `NftablesConfig` with per-rule USERDATA-keyed
  reconcile, streaming dumps for nftables + conntrack + XFRM +
  TC, nftables multicast events, and richer `NETLINK_EXT_ACK`
  kernel errors.
- **`0.17.0` (2026-05-26)** — internal-quality cycle. Default
  30-second operation timeout on every `Connection<P>` call,
  nine recv-loops audited to the canonical seq-filter +
  timeout shape, `NftablesConfig::diff` idempotence fix.
- **`0.18.0` (2026-05-29)** — purely additive release driven
  entirely by `nlink-upstream-asks.md` (this report). Shipped
  both 158 asks (`DeclaredChainBuilder::chain_type`,
  `list_*_in`) **and the full six-item wishlist**
  (`Error::ext_ack()`,
  `impl Display for NftablesDiff / ConfigDiff`,
  `Ipv4Route::default_route()`,
  `subscribe_all_with_resync(factory)` /
  `into_events_with_resync(factory)`, plus
  `NftablesEvent::NewSet`/`DelSet` for resync completeness).
  Bonus: `Chain::device(name)` for netdev base chains —
  shipped alongside `chain_type` since they share the
  `NFTA_CHAIN_HOOK` nest. **All 158 sub-plans are now
  unblocked across the board.**

**The single highest-impact item for nlink-lab is** Plan 157 in
nlink (`NftablesConfig` with `rule_keyed`). It directly closes
the comment we left in `deploy.rs:2906`:

> A fully-incremental reconcile (rule-by-rule diffing inside the
> table) is doable but requires upstreaming a per-rule diff API
> to nlink.

That API now exists. Switching `apply_nftables_diff` from
"delete-table-then-rebuild" to "diff + atomic batch" eliminates
the brief packet-drop window on rule edits, makes idempotent
re-applies produce zero kernel ops, and surfaces external
drift cleanly.

Three more lower-priority opportunities and one freebie are
detailed below.

---

## What's in 0.16 / 0.17 that nlink-lab can use

### Highest impact — `NftablesConfig` + `rule_keyed` (0.16, Plan 157)

```rust
let cfg = NftablesConfig::new().table("nlink-lab", Family::Inet, |t| {
    t.chain("input", |c| c.hook(Hook::Input).policy(Policy::Drop))
        .rule_keyed("input", "ssh-allow",  |r| r.match_tcp_dport(22).accept())
        .rule_keyed("input", "icmp-allow", |r| r.match_l4proto(1).accept())
});

cfg.diff(&conn).await?.apply(&conn).await?;   // first: 2 adds
cfg.diff(&conn).await?.apply(&conn).await?;   // re-apply: 0 ops
```

- Per-rule identity carried in `NFTA_RULE_USERDATA` (visible as
  `comment "nlink:<key>"` in `nft list ruleset`). Survives
  `nft -f` re-emits — diff stays accurate across foreign-tool
  interaction.
- `NftablesDiff::apply` is **atomic in 0.16**: every
  rule/chain/table/flowtable mutation commits in one
  `BATCH_BEGIN…BATCH_END`. Kernel never sees a half-applied
  ruleset.
- `apply_reconcile(opts)` adds bounded retry-on-conflict for
  race-prone scenarios (e.g. concurrent operator on the same
  table). Defaults: 3 retries, 100 ms initial backoff.
- 0.17 fixed a false-positive in the diff that was flagging
  unchanged rules as needing replacement on every re-apply
  (Plan 178). Without 0.17, idempotent re-apply would churn
  the kernel on every reconcile call.

**Maps to:** `apply_nftables_diff` in
`crates/nlink-lab/src/deploy.rs:2910` — currently does
`del_table` + full rebuild on any change to firewall or NAT.

### High impact — Default 30s `Connection<P>` timeout (0.17, Plan 171)

Every `Connection<P>` round-trip now times out after 30 s by
default. The 22-minute GHA hang that surfaced on the 0.16 cut
would now have been a clean `Error::Timeout`.

- No nlink-lab code change required — inherited automatically
  on upgrade.
- nlink-lab's existing test flakes that hung deploys (we patched
  the IPv6 DAD ping with retry in commit `3505c55a`) get an
  upstream defence-in-depth.
- Per-`Connection` override: `.timeout(Duration)`; opt out with
  `.no_timeout()`.
- Streaming dumps (`stream_links`, `stream_routes`, etc.) apply
  the timeout per-chunk, not over the whole dump.

### Medium impact — Richer kernel errors (`Error::Kernel { ext_ack }`)

`Error::Kernel` and `Error::KernelWithContext` gained an
`ext_ack: Option<String>` field carrying the kernel's
`NETLINK_EXT_ACK` TLV ("operation not permitted because …").
Both variants are now `#[non_exhaustive]` (semver-correct way to
add fields).

- `match` arms on these variants need a wildcard if they
  destructure the existing fields explicitly. nlink-lab has
  ~5 such match arms in `error.rs` / `deploy.rs`; a quick
  audit covers it.
- Surfacing `ext_ack` in nlink-lab's error display would let
  users see "veth0 not allowed in shared netns because …"
  instead of bare `EPERM`.

### Medium impact — nftables multicast events (0.16)

```rust
let mut nft = Connection::<Nftables>::new()?;
nft.subscribe(&[NftablesGroup::All])?;
let mut events = nft.events();
while let Some(evt) = events.next().await {
    match evt? {
        NftablesEvent::NewRule(r) => …,
        NftablesEvent::DelRule(r) => …,
        _ => {}
    }
}
```

- 8 typed event variants
  (`{New,Del}{Table,Chain,Rule,Flowtable}`).
- Combined with `events_with_resync` (also new in 0.16), survives
  `ENOBUFS` automatically by re-snapshotting.

**Maps to:** a future `nlink-lab watch <lab>` /
`bins/nlink-lab-backend` Zenoh-published topology-drift stream.
Today the backend is a periodic poller; with this it becomes
push-driven.

### Low impact (in nlink-lab's domain) but easy wins

- **`From<AddressParseError>` + `From<RouteParseError>` for
  `nlink::Error`** (0.16, Plan 173). Removes `.map_err(|e:
  AddressParseError| nlink::Error::InvalidMessage(e.to_string()))`
  ceremony. nlink-lab has ~3 such call sites in
  `deploy.rs` (look for `IpAddr::V4(v4) =>
  Ipv4Route::from_addr…`).
- **Streaming dumps for `stream_qdiscs / stream_classes /
  stream_filters / stream_conntrack`** (0.16, Plan 149). Mostly
  immaterial for our bounded labs (a 50-node lab has tens of
  qdiscs, not millions). Reach for these only if we ever build a
  `lab.inspect_huge_state()` flow.
- **`NetworkConfig::diff` HTB-default-class detection fix**
  (0.16, Plan 147). If anywhere in nlink-lab we mutate qdisc
  options across an `apply`, the diff now picks it up.

### Out of scope for nlink-lab

- **DPLL** (clock-sync hardware) — labs don't have GNSS-disciplined
  oscillators.
- **`net_shaper`** (per-NIC, per-queue TX hardware shaping) — no
  real NICs inside namespaces.
- **`nlink-macros`** — for projects implementing new GENL
  families. nlink-lab is a consumer.

---

## Migration cost

### Bumps required

- **MSRV** for nlink itself bumped from 1.85 to 1.95
  (0.16). nlink-lab pins `edition = "2024"` and uses
  recent-rustc features already; no toolchain change needed.
- **`Connection<P>::new` vs `new_async`** (0.16): sealed
  `SyncConstructible` / `AsyncConstructible` traits now
  enforce the right constructor. nlink-lab uses
  `Connection::<Route>::new()` (sync) and
  `Connection::<Wireguard>::new_async()` (async, which is
  what we want) — already compliant. The previously-silent
  `Connection::<Wireguard>::new()` footgun would now be a
  compile error.
- **`#[non_exhaustive]` on `Error::Kernel{…}` and
  `KernelWithContext{…}`** (0.16) — add `_ =>` to any
  destructuring `match` on those variants.
- **`Register` enum discriminants changed** (0.17, Plan 178).
  No impact unless you cast `Register::R0 as u32` and rely on
  the literal value `8`; nlink-lab does not.
- **`NftablesDiff::rules_to_delete` tuple shape** (0.17, Plan
  178) — `(table, family, handle)` → `(table, family, chain,
  handle)`. nlink-lab doesn't currently consume the diff
  struct — irrelevant until we adopt the reconcile path
  (and then it's the right shape on day one).

### Workspace edit

```diff
-nlink = { version = "0.15.1", features = ["full"] }
+nlink = { version = "0.17", features = ["full"] }
```

That's the entire surface bump. The 0.16 cycle was
deliberately additive (per its migration guide intro:
"substantial-but-mostly-additive"), and 0.17 is a small
internal-quality release.

---

## Proposed PR breakdown

### PR A — nftables reconcile via `NftablesConfig` (P1, M effort)

The big payoff. Concretely:

1. Bump `Cargo.toml` workspace dep `nlink = "0.17"` (and run
   `cargo update -p nlink`). Verify build + tests.
2. Replace `apply_nftables_diff`
   (`crates/nlink-lab/src/deploy.rs:2910`) — currently
   `del_table` + sequential `apply_firewall` / `apply_nat` —
   with construction of an `NftablesConfig` for the desired
   state + `cfg.diff(&conn).await?.apply(&conn).await?`.
3. Rewrite the initial-deploy `apply_firewall` /
   `apply_nat` paths (`deploy.rs:~1640`, `~1830`) so they
   produce the **same** `NftablesConfig` shape, then we
   just call `apply` directly. Single code path for
   "create" + "reconcile."
4. Synthesize per-rule keys from existing rule identity:
   firewall keys = `fw:<chain>:<idx>:<src>:<dst>`; NAT keys =
   `nat:<chain>:<idx>:<kind>:<args>`. Stable across reapplies
   of the same NLL config.
5. Update `apply_nftables_diff`'s docstring to drop the
   "full-rebuild approach" caveat and reference the
   per-rule USERDATA-keyed reconcile.
6. New integration test
   `apply_nftables_idempotent_reapply_is_zero_ops` — deploy a
   topology with firewall rules, run `apply` on the unchanged
   topology, assert no kernel mutations (verify via
   `nft list ruleset`'s `generation` counter or via timestamp
   on `/proc/net/netfilter/nf_tables/<name>`).

**Lines touched:** ~300. Out-of-scope spawned-process reconcile
stays out-of-scope. The `nft -f` foreign-edit absorption is a
nice side effect; it can become an advertised feature.

### PR B — `Error::Kernel` ext_ack surfacing (P2, S effort)

1. Add `_ =>` arms where we destructure `Error::Kernel{errno}`
   and `Error::KernelWithContext{…}` (≤5 sites).
2. In `Error::Display` for `nlink_lab::Error::*` that wrap
   `nlink::Error::Kernel`, include `ext_ack` in the rendered
   string when present.
3. Sample diff for the test surface — verify a deliberate
   bad operation (e.g. assign IPv4 to non-existent
   interface) now surfaces a useful one-liner.

### PR C — Adopt `From<AddressParseError>` + `From<RouteParseError>` (P2, XS effort)

Pure cosmetic. ~3 `.map_err` ceremonies in `deploy.rs`
collapse into bare `?`.

### PR D — Optional: `nlink-lab watch` via nftables multicast (P3, L effort)

A new subcommand that subscribes to `NftablesGroup::All` on
every node in a lab and streams typed events. Useful for
debug-loops where users edit rulesets out-of-band via
`nlink-lab exec node -- nft -f` and want a confirmation of
what landed. Would compose with the existing
`bins/nlink-lab-backend` Zenoh publisher to become a
real-time topology view in `bins/topoviewer`.

Probably overkill for an immediate ship — float it after
PR A is in.

---

## Recommendation

Order: **PR A → PR B → PR C**. PR A is the natural Plan 158
and resolves a TODO comment that has lived in the deploy
path since Plan 152 (April 2026). PR B and PR C are quick
cleanup sweeps that should ride alongside the same nlink
version bump commit so we don't have a "0.17 upgrade with
half the new affordances unused" intermediate state.

Skip PR D unless an actual user use case shows up. The
watch-mode would be the right primitive for an `nlink-lab
topoviewer` live drift indicator, but topoviewer already
polls and that's fine for now.

After PR A ships, the nlink-lab deploy story becomes:

| Resource | Reconcile granularity |
|----------|----------------------|
| Per-pair impair | Per-tree (HTB+netem+flower), atomic, no packet loss |
| nftables rules  | **Per-rule, atomic, no packet loss** ← new |
| Rate-limits     | Per-endpoint, coarse (full HTB rebuild) |
| Routes / sysctls / links / addresses | Per-object |

That leaves rate-limits as the last full-rebuild reconcile
path. Whether that's worth upstreaming a
`PerHostLimiter::reconcile()` to nlink (mirror of
`PerPeerImpairer::reconcile`) is a separate conversation —
the current rebuild is correct and rate-limits don't usually
churn at runtime, so the priority is low.

---

## References

- nlink CHANGELOG 0.16.0: `/home/mpardo/git/rip/CHANGELOG.md:197`
- nlink CHANGELOG 0.17.0: `/home/mpardo/git/rip/CHANGELOG.md:7`
- nlink migration guides:
  - `/home/mpardo/git/rip/docs/migration_guide/0.15.1-to-0.16.0.md`
  - `/home/mpardo/git/rip/docs/migration_guide/0.16.0-to-0.17.0.md`
- nlink recipes (referenced patterns):
  - `nftables-declarative-config.md` — `diff + apply + reconcile`
  - `events-with-resync.md` — ENOBUFS-tolerant multicast
- nlink-lab call site to retire:
  `crates/nlink-lab/src/deploy.rs:2900-2947`
  (`apply_nftables_diff` full-table-rebuild).

## Upstream prerequisites (asks for the nlink author) — ALL SHIPPED IN 0.18.0

Originally this section listed one real blocker and two
nice-to-haves. nlink 0.18.0 (2026-05-29) shipped every item
plus the six wishlist entries. The block-level summary below
is preserved as historical record. **Action: bump
`nlink = "0.18"` workspace dep and remove Phase 0
coordination from 158a; everything else just inherits the
new ergonomics automatically.**

### ✅ Required for Plan 158a Phase 2 (NAT reconcile) — landed as Plan 180 in nlink

- **`DeclaredChainBuilder::chain_type(ChainType)`** —
  `DeclaredChain` (`crates/nlink/src/netlink/nftables/config/
  types.rs:216-221`) carries `name`, `hook`, `priority`,
  `policy` but **not** `chain_type`. The apply path
  (`config/apply.rs:100-112`) reconstructs a runtime
  `Chain` from the declared form without threading
  `chain_type` either. Result: declared NAT chains default
  to `ChainType::Filter`, which is wrong for
  `prerouting`/`postrouting`. Fix:
  1. Add `pub(crate) chain_type: Option<ChainType>` field
     to `DeclaredChain`.
  2. Add `.chain_type(ChainType)` method on
     `DeclaredChainBuilder`.
  3. Wire it into the runtime `Chain` reconstruction in
     `config/apply.rs:100-112` (~5 LOC).
  Total: ~25–40 LOC + one round-trip unit test
  (declare a NAT chain, assert the emitted
  `NFTA_CHAIN_TYPE` attribute).

  Without this, nlink-lab Phase 1 (firewall reconcile)
  ships standalone and Phase 2 (NAT reconcile) is
  deferred. Phase 1 is the majority of the win — NAT
  chains can stay imperative for one more cycle.

### ✅ Nice-to-have for Plan 158d resync efficiency — landed as Plan 181 in nlink

- **`list_tables_in(family)` / `list_chains_in(table, family)`
  / `list_flowtables_in(table, family)` /
  `list_sets_in(table, family)`** — server-side filtered
  dump methods on `Connection<Nftables>`. Tiny efficiency
  win, now available, with the bonus `list_sets_in` for the
  resync snapshot.

### ✅ Wishlist — all six items landed in 0.18.0

| Wishlist | nlink plan | Status |
|----------|-----------|--------|
| W1: `impl Display for NftablesDiff` + `ConfigDiff` | Plan 183 | ✅ Shipped |
| W2: `subscribe_all_with_resync(factory)` + `into_events_with_resync(factory)` | Plan 185 | ✅ Shipped |
| W3: `Error::ext_ack()` + `ext_ack_offset()` accessors | Plan 182 | ✅ Shipped |
| W4: `for_each_namespace_async` | — | Not done (lab-flavored helper — punted) |
| W5: `Ipv4Route::default_route()` / `Ipv6Route::default_route()` | Plan 184 | ✅ Shipped |
| W6: `NetworkConfig` per-object reconcile parity | — | Direction noted; multi-cycle work, not in 0.18 |

Plus a bonus pair landed alongside Plan 180:
- `Chain::device(name)` (imperative + declarative) for
  netdev base chains hooked at `ingress`/`egress` with
  `NFTA_HOOK_DEV`. Useful for future nlink-lab work on
  ingress-side filtering inside namespaces.
- `NftablesEvent::NewSet(SetInfo)` + `DelSet(SetInfo)` —
  Plan 185 bundled them since `events_with_resync` enumerates
  sets during the snapshot replay; consumers benefit
  immediately even without using the resync wrapper.

### Nice-to-have for nlink-lab rate-limit reconcile (already done, was false alarm)

- ~~**`PerHostLimiter::reconcile`** mirroring
  `PerPeerImpairer::reconcile`~~ — **already exists** in nlink
  at `crates/nlink/src/netlink/ratelimit.rs:749`. The
  stale comment in nlink-lab's `deploy.rs:2956-2961`
  claiming this was missing should be removed in PR A's
  cleanup pass.

### False alarms (no upstream work needed)

- The `"nlink:"` USERDATA prefix is **not** a namespace
  collision risk for nlink-lab. The library auto-wraps
  user-supplied keys
  (`crates/nlink/src/netlink/nftables/userdata.rs:47-58`)
  and strips on parse — user-side keys can contain any
  bytes that fit the 121-byte budget after the `"nlink:"`
  prefix and trailing NUL.
- All listing methods needed for the 158d resync snapshot
  exist (`list_tables`, `list_chains`,
  `stream_rules`, `list_flowtables`).
- `Connection::<Nftables>::subscribe` is callable from a
  current-thread runtime — no `tokio::spawn` or blocking
  primitives in the path
  (`connection.rs:757-762`).
- `Error::Kernel { ext_ack }` already lands in 0.16 with
  `Display` rendering; nlink-lab inherits it for free
  after the dep bump.
