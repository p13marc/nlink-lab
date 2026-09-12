# nlink 0.19 — post-adoption feedback

**Date:** 2026-06-01
**Audience:** nlink maintainer.
**Author:** nlink-lab maintainer (the canonical downstream consumer).
**nlink version reviewed:** `0.19.0` (released 2026-05-30).
**Scope:** items discovered while implementing the seven-plan 159
arc against 0.19 in nlink-lab — bugs we'd like fixed, gaps we'd
like closed, features we'd like added. Everything here is
grounded in a concrete downstream workaround or test failure;
nothing speculative.

> **2026-06-08 status update against 0.21.0.** nlink shipped 0.20.0
> (emergency wire-format fixes), 0.20.1 (additive typed-API
> tightening), and 0.21.0 (the typed-API closeout) together on
> 2026-06-04. nlink-lab is now on 0.21.0 (commit `777ef52`).
>
> Per-item status:
>
> - **#1 WireguardConfigDiff Serialize** — still open in 0.21.
>   `WireguardConfigDiff` at `config.rs:629` has no
>   `#[cfg_attr(feature = "serde", derive(serde::Serialize))]`.
> - **#2 WireguardConfig::diff requires the device to exist** —
>   still open in 0.21. The same `get_device_by_name(&ifname).await?`
>   line is at `config.rs:247`. nlink-lab's imperative
>   `add_link(WireguardLink::new(name))` prelude in `deploy.rs`
>   step 6c and `apply_diff` Phase 6 stay imperative.
> - **#3 DeclaredLinkType::Wireguard variant** — still open.
> - **#4 Stack::apply_in_namespace name-only** — still open.
> - **#5 RateLimiter::reconcile** — still open. Plan 158g
>   remains parked.
> - **#6 RTNETLINK `_if_exists` family** — still open.
> - **#7 facade::apply WG `_in_namespace_path/_pid`** — still open.
> - **#8 `NETNS_RUN_DIR` not re-exported** — still open.
> - **#9 `DeclaredWgPeerBuilder::endpoint_hostname`** — still open.
> - **#10 `StackDiff::change_count()`** — still open.
>
> The 0.20/0.21 cycle was scoped to emergency wire-format
> corrections + typed-API closeout, not the feedback items. The
> 10 items here are still open for the next cycle.

> **First — thank you.** 0.19 closed 14 of the 16 numbered items,
> 4 of the 9 wishlist items, and **all 6 documentation
> suggestions** from `nlink-feedback.md` (2026-05-30). Several
> closures exceeded what I asked for — `WireguardConfig`
> instead of a `LinkBuilder::wireguard`, `Error::chain_walk`
> instead of just a docstring, normalization at source instead
> of doc-only fixes. The post-cycle audit also caught five
> silent-corruption bugs (TC filter `tcm_info` packing,
> `Verdict::Jump`/`Goto` constants, IPv6 NAT register drop,
> F1 `Connection<P>` request lock, N1 `namespace::create`
> thread-bleed) that nlink-lab silently inherited as
> correctness wins. Net win, no question.
>
> The items below are the next layer down — what surfaced when
> nlink-lab tried to actually USE the new 0.19 APIs at the
> declarative level.

---

## TL;DR — read this table first

| # | Item | Severity | Suggested fix size | Where nlink-lab hit it |
|---|------|----------|--------------------|------------------------|
| 1 | `WireguardConfigDiff` has no `serde::Serialize` derive | **HIGH (correctness/UX)** | 2 LOC + feature gate | blocked our `apply --check --json` schema v2 from including the WG layer |
| 2 | `WireguardConfig::diff` requires the device to exist on the kernel side | **HIGH (ergonomic blocker)** | ~30 LOC | forces an imperative `add_link(WireguardLink::new(name))` prelude in every downstream apply path |
| 3 | `DeclaredLinkType::Wireguard` variant missing | medium (feature) | ~20 LOC + GENL link kind | related to #2 — declarative WG link creation isn't possible today |
| 4 | `Stack::apply_in_namespace(&str)` is name-only — no `_path`/`_pid`/`_fd` variants | medium (ergonomic blocker) | ~30 LOC | nlink-lab uses `Connection<P>` opened via `connection_for_pid` for container nodes; couldn't adopt upstream Stack as-is, built our own `apply_stack_for_node` mirroring the shape |
| 5 | `RateLimiter::reconcile` missing (only `PerHostLimiter` has it) | medium (feature) | mirror existing `PerHostLimiter::reconcile` | blocks nlink-lab's Plan 158g — rate limits are still rebuilt destructively on every apply |
| 6 | No `_if_exists` family for RTNETLINK (`del_route_v4`, `del_link`, `del_qdisc`) | medium (ergonomic) | 6 thin wrappers | 5 `let _ = conn.del_*(...).await;` ignore patterns in nlink-lab; replacement to `del_*_if_exists` would tighten error semantics |
| 7 | `WireguardConfig` doesn't ship `_in_namespace_path`/`_pid` apply helpers in facade | low (ergonomic) | mirror existing nft helpers | same shape as #4 but for WG specifically |
| 8 | `NETNS_RUN_DIR` const not re-exported at crate root | low (ergonomic) | 1 line | downstream has to walk `nlink::netlink::namespace::NETNS_RUN_DIR` or hardcode `/var/run/netns/` |
| 9 | `DeclaredWgPeerBuilder::endpoint` takes `SocketAddr` only — no `hostname:port` | low (feature) | ~50 LOC + DNS resolver | nlink-lab doesn't need hostname yet but it's a common ask |
| 10 | `StackDiff::change_count()` / `StackApplyReport::change_count()` missing | low (ergonomic) | 8 LOC | callers have to sum per-layer counts manually |

**Highest leverage:** **#1 (WG diff Serialize)** — 2-line fix that
unblocks the WG layer in every downstream typed-JSON workflow.
**#2 (WG diff needs existing device)** is right behind — it
forces an awkward imperative prelude that breaks the "all
declarative" promise of the Stack pattern.

Everything else is medium-to-low impact and survivable with
workarounds.

---

## 1. (HIGH — correctness/UX) `WireguardConfigDiff` has no `serde::Serialize` derive

**Severity:** high — blocks the WG layer in every downstream typed-JSON pipeline.

**Where in nlink (0.19):**
`crates/nlink/src/netlink/genl/wireguard/config.rs:627-633`:

```rust
#[derive(Debug, Clone, Default)]
#[must_use = "Diffs do nothing unless passed to `.apply()` or inspected"]
pub struct WireguardConfigDiff {
    pub devices_to_modify: Vec<(String, DeviceChanges)>,
}
```

No `#[cfg_attr(feature = "serde", derive(serde::Serialize))]`.

**Background:** Plan 189 (the 0.19 release plan that added serde
derives to `ConfigDiff` and `NftablesDiff`) missed `WireguardConfigDiff`.
nlink-lab's Plan 159d shipped a typed-JSON envelope for
`apply --check --json` with `.network` and `.nftables` typed
fields. We wanted a `.wireguard` field too but couldn't add it
because the type isn't serializable.

**Workaround:** none. We omit the WG layer from the JSON
envelope entirely. `apply --check --json` users who care about
WG drift have to fall back to parsing the human-readable
`layered_summary` text — which is exactly what schema v2 was
supposed to eliminate.

**Suggested fix:**

```rust
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Default)]
pub struct WireguardConfigDiff {
    pub devices_to_modify: Vec<(String, DeviceChanges)>,
}
```

…and mirror on `DeviceChanges`, `DeclaredWgPeer`, `AllowedIp`,
`PublicKey`. Match the kebab-case shape `ConfigDiff` /
`NftablesDiff` use.

**Test:** a single roundtrip assertion in
`tests/wireguard_config_serde.rs` would be enough — build a
`WireguardConfigDiff` with one device-modify, serialize, assert
JSON contains the expected fields.

---

## 2. (HIGH — ergonomic blocker) `WireguardConfig::diff` requires the device to exist on the kernel side

**Severity:** high — forces an imperative prelude that breaks
the declarative-Stack pattern.

**Where in nlink (0.19):**
`crates/nlink/src/netlink/genl/wireguard/config.rs:233-252`:

```rust
pub async fn diff(&self, conn: &Connection<Wireguard>) -> Result<WireguardConfigDiff> {
    let mut diff = WireguardConfigDiff::default();

    for declared in &self.devices {
        // Fetch current device state. If the interface
        // doesn't exist or isn't a WG link, propagate
        // the kernel error — the caller is expected to
        // ensure the link exists (typically via
        // NetworkConfig with a `wg`-kind link).
        let current = conn.get_device_by_name(&declared.ifname).await?;
        // ...
    }
    Ok(diff)
}
```

The docstring acknowledges the issue — *"the caller is expected
to ensure the link exists (typically via NetworkConfig with a
`wg`-kind link)"* — but `NetworkConfig` doesn't currently have a
`wg`-kind link variant (see item #3).

**Where in nlink-lab:**
`crates/nlink-lab/src/deploy.rs:636-655` (step 6c — imperative
WG link create) AND `crates/nlink-lab/src/deploy.rs:3201-3224`
(`apply_diff` Phase 6 — newly-added node WG link create). Both
sites have to imperatively `conn.add_link(WireguardLink::new(name))`
BEFORE the declarative `WireguardConfig` path can take over.

**Background:** this asymmetry surfaced as a real bug. nlink-lab
shipped commit `bfe8744` (apply_diff Phase 6 WG support) that
added the declarative WG path; the next CI run failed because
`WireguardConfig::diff` returned an error like
`"NetworkConfig::apply on 'c': interface not found: wg0"`. The
fix (commit `3ea2cb7`) was an imperative `add_link` prelude.

**Workaround:** the imperative prelude works, but it means the
"all-declarative" Stack pattern has an asymmetric hole for WG.
Every downstream consumer that wires WG through Stack will hit
this and reinvent the same prelude.

**Suggested fix (option A — recommended):** make
`WireguardConfig::diff` treat absent devices as "device to add":

```rust
pub async fn diff(&self, conn: &Connection<Wireguard>) -> Result<WireguardConfigDiff> {
    let mut diff = WireguardConfigDiff::default();

    for declared in &self.devices {
        match conn.get_device_by_name(&declared.ifname).await {
            Ok(current) => {
                let device_changes = declared.diff_against(&current);
                if !device_changes.is_empty() {
                    diff.devices_to_modify
                        .push((declared.ifname.clone(), device_changes));
                }
            }
            Err(e) if e.is_not_found() => {
                // Plan 196 — declarative bootstrap: an absent
                // device becomes a "device to add" so callers
                // don't need an imperative add_link prelude.
                diff.devices_to_add.push(declared.clone());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(diff)
}
```

Then `apply` creates the device via `add_link` internally before
running the per-peer config — same atomic semantics as today,
just declarative-bootstrapped.

**Suggested fix (option B — explicit):** add
`WireguardConfig::ensure_devices(conn).await` that creates any
missing devices, then call `diff` / `apply` as before. Downstream
calls `ensure_devices().await?; diff(...).await?; apply(...).await?`.

Option A is cleaner but option B preserves the "diff is
read-only" invariant if you care about that.

**Test:** the regression that hit us:
```rust
let cfg = WireguardConfig::new().device("wg0", |d| d.private_key([0; 32]));
let conn: Connection<Wireguard> = Connection::new_async().await?;
// wg0 doesn't exist yet
let result = cfg.apply(&conn).await;
assert!(result.is_ok(), "WireguardConfig::apply should bootstrap missing devices");
```

---

## 3. (medium — feature) `DeclaredLinkType::Wireguard` variant missing

**Severity:** medium — related to #2 but separately useful.

**Where in nlink (0.19):**
`crates/nlink/src/netlink/config/types.rs:182-238` — the
`DeclaredLinkType` enum lists Dummy, Veth, Bridge, Vlan, Vxlan,
Macvlan, Bond, **Vrf** (new in 0.19!), but no `Wireguard`
variant.

**Background:** WG is GENL, not RTNETLINK — so adding a
`Wireguard` variant to `DeclaredLinkType` is more invasive than
the VRF case. But the absence means downstream can't declare
`NetworkConfig::new().link("wg0", |b| b.wireguard())` and rely
on the shared declarative diff/apply pipeline for the LINK CREATE
step.

**Workaround:** nlink-lab imperatively creates the WG link first
(see #2), then declares peers/keys via `WireguardConfig`.

**Suggested fix:** add `DeclaredLinkType::Wireguard` that creates
the WG interface via the standard RTNETLINK link-create path
(WG links are created via RTNETLINK; only peer/key state is
GENL). This would close the asymmetry.

If implementing this is too invasive, **closing #2 alone is
sufficient** — option A there subsumes the create-on-apply
behavior. #3 is a "do it if cheap" follow-up.

---

## 4. (medium — ergonomic blocker) `Stack::apply_in_namespace(&str)` is name-only

**Severity:** medium — blocks container nodes from upstream Stack.

**Where in nlink (0.19):**
`crates/nlink/src/facade/stack.rs:113`:

```rust
pub async fn apply_in_namespace(&self, ns: &str) -> Result<StackApplyReport>;
```

`namespace::connection_for(ns: &str)` resolves to
`/var/run/netns/<ns>` — works for bare namespaces but not for
container nodes whose namespace lives at `/proc/<pid>/ns/net`.

**Where in nlink-lab:** Plan 159c was supposed to adopt upstream
Stack but couldn't because nlink-lab's `NodeHandle` enum has
two variants:

- `Namespace { ns_name }` — opens via `connection_for(ns_name)`
- `Container { pid, .. }` — opens via `connection_for_pid(pid)`

Adopting upstream Stack would break containers. We built our
own `apply_stack_for_node` (a 30-line orchestrator) that mirrors
Stack's shape but routes through `NodeHandle::connection<P>()`.
~30 LOC duplicated work that should ideally live upstream.

**Workaround:** in-house Stack pattern. Works. Costs ~30 LOC and
loses the pre-flight validation that upstream Stack provides
(which we then have to reimplement).

**Suggested fix:** add three sibling APIs mirroring
`namespace::connection_for*`:

```rust
impl Stack {
    pub async fn apply_in_namespace(&self, ns: &str) -> Result<StackApplyReport>;
    pub async fn apply_in_namespace_path<P: AsRef<Path>>(&self, path: P) -> Result<StackApplyReport>;
    pub async fn apply_in_namespace_pid(&self, pid: u32) -> Result<StackApplyReport>;

    pub async fn diff_in_namespace(&self, ns: &str) -> Result<StackDiff>;
    pub async fn diff_in_namespace_path<P: AsRef<Path>>(&self, path: P) -> Result<StackDiff>;
    pub async fn diff_in_namespace_pid(&self, pid: u32) -> Result<StackDiff>;
}
```

Mirrors the three `connection_for*` variants already in
`crates/nlink/src/netlink/namespace.rs:222-302`.

Same for the `facade::apply::*_in_namespace` and
`facade::diff::*_in_namespace` family — they'd benefit from
sibling `_path` / `_pid` variants too.

---

## 5. (medium — feature) `RateLimiter::reconcile` missing

**Severity:** medium — blocks nlink-lab Plan 158g (the last
"destructive rebuild" reconcile path in nlink-lab's deploy).

**Where in nlink (0.19):**
`crates/nlink/src/netlink/ratelimit.rs:148..` shows
`impl RateLimiter` has `new()`, `egress()`, `ingress()`, `apply()`,
and `remove()` — but no `reconcile()`. By contrast,
`impl PerHostLimiter` (line 558..) has the full set including
`reconcile()` (line 749), `reconcile_dry_run()` (line 758), and
`reconcile_with_options()` (line 767).

**Background:** I asked for this back in 2026-05-29 (the original
Plan 158 audit). My report assumed `PerHostLimiter::reconcile`
WAS `RateLimiter::reconcile` — that was an error on my part.
Once you ship `RateLimiter::reconcile`, the closure that's been
pending since 0.18 (Plan 158g in nlink-lab) drops in trivially.

**Where in nlink-lab:**
`crates/nlink-lab/src/deploy.rs:851` — `RateLimiter::new(&ep.iface)`
followed by `apply()`. Every redeploy destroys-and-rebuilds the
qdisc/class/filter tree even when no rate limit changed.

**Workaround:** none. Plan 158g stays parked.

**Suggested fix:** mirror the `PerHostLimiter::reconcile` shape
on `RateLimiter`. The qdisc/class shape is simpler — just root
HTB + two leaves (egress/ingress) — so the diff logic should
be ~50% smaller than `PerHostLimiter::reconcile`'s.

```rust
impl RateLimiter {
    pub async fn reconcile(&self, conn: &Connection<Route>) -> Result<ReconcileReport>;
    pub async fn reconcile_dry_run(&self, conn: &Connection<Route>) -> Result<ReconcileReport>;
    pub async fn reconcile_with_options(
        &self,
        conn: &Connection<Route>,
        opts: ReconcileOptions,
    ) -> Result<ReconcileReport>;
}
```

Same `ReconcileReport` and `ReconcileOptions` types — reuse what
PerHostLimiter already exposes.

**Test:** the existing `PerHostLimiter::reconcile` tests should
port over almost verbatim.

---

## 6. (medium — ergonomic) No `_if_exists` family for RTNETLINK

**Severity:** medium — five `let _ = conn.del_*(...).await;`
sites in nlink-lab.

**Where in nlink (0.19):** Plan 188 §2.7 shipped
`del_table_if_exists`, `del_chain_if_exists`, `del_rule_if_exists`
on `Connection<Nftables>`. No corresponding family on
`Connection<Route>` for `del_link`, `del_route_v4`, `del_route_v6`,
`del_qdisc`, `del_filter`, `del_addr`.

**Where in nlink-lab:** five sites use the "ignore the result"
pattern:

```
crates/nlink-lab/src/deploy.rs:3228:   let _ = conn.del_route_v4("0.0.0.0", 0).await;
crates/nlink-lab/src/deploy.rs:3229:   let _ = conn.del_route_v6("::", 0).await;
crates/nlink-lab/src/scenario.rs:238:  let _ = conn.del_qdisc(&ep.iface, ...).await;
crates/nlink-lab/src/running.rs:1184:  let _ = root_conn.del_link(&peer).await;
crates/nlink-lab/src/running.rs:1187:  let _ = root_conn.del_link(&bridge_name).await;
```

The `let _ = ... .await;` shape swallows EVERY error including
EPERM, EINVAL, etc. — not just the intended ENOENT. The
`_if_exists` shape (returns `Ok(bool)`) would let us write
`let _ = conn.del_*_if_exists(...).await?;` — still ignoring
the bool but propagating real errors.

**Workaround:** the current shape works. Doesn't hide real bugs
because the surrounding paths are best-effort cleanup. But it's
ugly and could mask a regression where the kernel actually fails
the delete for a non-ENOENT reason.

**Suggested fix:** six wrappers, each one a 5-LOC match on
`is_not_found()` (analogous to the nftables shape):

```rust
impl Connection<Route> {
    pub async fn del_link_if_exists(&self, name: &str) -> Result<bool>;
    pub async fn del_route_v4_if_exists(&self, dst: &str, prefix: u8) -> Result<bool>;
    pub async fn del_route_v6_if_exists(&self, dst: &str, prefix: u8) -> Result<bool>;
    pub async fn del_qdisc_if_exists(&self, dev: &str, handle: TcHandle) -> Result<bool>;
    pub async fn del_addr_if_exists(&self, name: &str, addr: &str) -> Result<bool>;
    pub async fn del_filter_if_exists(&self, ...) -> Result<bool>;
}
```

Implementation: call existing `del_*`, match on `is_not_found()`,
return `Ok(false)` on ENOENT, propagate other errors.

---

## 7. (low — ergonomic) `WireguardConfig` doesn't ship `_in_namespace_path`/`_pid` apply helpers

**Severity:** low (related to #4).

**Where in nlink (0.19):**
`crates/nlink/src/facade/apply.rs:83-89`:

```rust
pub async fn wireguard_in_namespace(
    ns: &str,
    cfg: &WireguardConfig,
) -> Result<crate::netlink::genl::wireguard::WireguardApplyResult> {
    let conn = namespace::connection_for_async::<Wireguard>(ns).await?;
    cfg.apply(&conn).await
}
```

No `wireguard_in_namespace_path` or `wireguard_in_namespace_pid`.
Same ergonomic gap as `Stack` (#4) but at the per-layer level.

**Suggested fix:** mirror the shape from #4.

---

## 8. (low — ergonomic) `NETNS_RUN_DIR` const not re-exported at crate root

**Severity:** low.

**Where in nlink (0.19):**
`crates/nlink/src/netlink/namespace.rs:205`:

```rust
pub const NETNS_RUN_DIR: &str = "/var/run/netns";
```

`pub` at the module level but not re-exported at
`nlink::NETNS_RUN_DIR`. Downstream has to know the deep path or
hardcode `/var/run/netns/`.

**Where in nlink-lab:** we don't actually use it (we route through
the connection_for helpers) but a debug-log path in
`crates/nlink-lab/src/state.rs` could use it.

**Suggested fix:**

```rust
// In crates/nlink/src/lib.rs:
pub use netlink::namespace::NETNS_RUN_DIR;
```

One line. Trivial.

---

## 9. (low — feature) `DeclaredWgPeerBuilder::endpoint` takes `SocketAddr` only

**Severity:** low — nlink-lab doesn't need it yet, but it's a
common downstream ask.

**Where in nlink (0.19):**
`crates/nlink/src/netlink/genl/wireguard/config.rs:595`:

```rust
pub fn endpoint(mut self, addr: SocketAddr) -> Self {
    self.endpoint = Some(addr);
    self
}
```

`SocketAddr` only — no `hostname:port` resolution.

**Background:** Real-world WG configs frequently use
`vpn.example.com:51820` shape, where the hostname needs DNS
resolution. Today every downstream consumer that wants this
shape has to do the DNS lookup themselves before constructing
the `SocketAddr`. nlink-lab doesn't surface hostname endpoints
in NLL yet so we don't hit it, but if/when we do, we'll
reimplement the same lookup.

**Suggested fix:** add a sibling method that resolves hostname:

```rust
impl DeclaredWgPeerBuilder {
    pub fn endpoint(mut self, addr: SocketAddr) -> Self { /* existing */ }

    /// Resolve `hostname:port` via the system resolver and use
    /// the first matching `SocketAddr`. Fails if the hostname
    /// has no A/AAAA records or doesn't include a port.
    pub fn endpoint_hostname(mut self, host_port: &str) -> Result<Self> {
        let addr = host_port.to_socket_addrs()?
            .next()
            .ok_or_else(|| ...)?;
        self.endpoint = Some(addr);
        Ok(self)
    }
}
```

Note: hostname resolution is sync-blocking by default
(`ToSocketAddrs`); document this. Async resolver users can
resolve themselves and pass the result through the existing
`endpoint(SocketAddr)`.

---

## 10. (low — ergonomic) `StackDiff` / `StackApplyReport` `change_count()` missing

**Severity:** low.

**Where in nlink (0.19):**
`crates/nlink/src/facade/stack.rs:184-200`:

```rust
pub struct StackDiff {
    pub network: Option<ConfigDiff>,
    pub nftables: Option<NftablesDiff>,
    pub wireguard: Option<WireguardConfigDiff>,
}

impl StackDiff {
    pub fn is_empty(&self) -> bool;
    // No change_count()
}
```

Each per-layer diff exposes `change_count()`, but the bundle
doesn't sum them.

**Where in nlink-lab:** `LayeredDiff::change_count()` in
nlink-lab manually sums per-layer counts — every downstream
consumer that adopts Stack will reimplement the same sum.

**Suggested fix:**

```rust
impl StackDiff {
    pub fn change_count(&self) -> usize {
        self.network.as_ref().map_or(0, |d| d.change_count())
            + self.nftables.as_ref().map_or(0, |d| d.change_count())
            + self.wireguard.as_ref().map_or(0, |d| d.change_count())
    }
}

impl StackApplyReport {
    pub fn change_count(&self) -> usize {
        self.network.as_ref().map_or(0, |r| r.changes_made)
            + self.nftables_change_count.unwrap_or(0)
            + self.wireguard.as_ref().map_or(0, |r| r.device_writes + r.peer_writes + r.peer_removals)
    }
}
```

Five LOC. Cosmetic but every consumer reinvents it.

---

## Triage table for the next release cycle

If you're picking which to ship, I'd suggest:

| Priority | Items | Why |
|---|---|---|
| **Should-ship** | #1 (WG diff Serialize), #2 (WG diff bootstrap) | Both close real gaps in the declarative story. #1 is 2 LOC. #2 is the asymmetry that makes Stack feel incomplete. |
| **Nice-to-ship** | #4 (Stack namespace variants), #5 (RateLimiter::reconcile), #6 (RTNETLINK `_if_exists`) | Real ergonomic wins. #5 unblocks one of our two remaining parked plans. |
| **Wishlist** | #3 (DeclaredLinkType::Wireguard), #7 (facade::apply::wireguard_in_namespace_path), #8 (NETNS_RUN_DIR), #9 (endpoint_hostname), #10 (StackDiff change_count) | Cosmetic / small. Ship if free. |

---

## What landed great in 0.19 (just for context)

So this doesn't read like only criticism — the wins from 0.19
nlink-lab benefits from every day:

- **`WireguardConfig` declarative.** Replaced our two-pass
  imperative `set_device` loop entirely. Plan 159a Phase 2 was a
  delight to implement.
- **`Connection<Route>::subscribe_all_with_resync`.** Unblocked
  the `nlink-lab watch` CLI's RTNETLINK side. ResyncStream
  combinators are clean.
- **`Error::chain_walk` + `root_cause`.** Refactored three
  hand-rolled `downcast_ref` loops in nlink-lab's error.rs to
  three one-liners (Plan 159f). The `Box<Error>` trap I
  reported in nlink-feedback.md item #4 is permanently closed.
- **`Error::from_errno*` `.abs()` normalization.** Plan 159f
  flipped two test assertions from `Some(-1)` to `Some(1)` and
  moved on — clean.
- **`ApplyOptions` builder methods.** Adopted in nlink-lab Plan
  158f Phase 2's `compute_layered_diff`. Cleaner than the
  struct-literal shape.
- **TC filter `tcm_info` packing fix (post-cycle).** Every flower
  filter nlink-lab's `PerPeerImpairer` emits silently had the
  wrong protocol field pre-0.19. We never noticed; our tests
  never caught it. Pure correctness win from the bump.
- **`Verdict::Jump`/`Goto` constants (post-cycle).** Every
  nft jump/goto rule was actually emitting `NFT_BREAK = -2`
  pre-0.19. nlink-lab's firewall paths use jump heavily. **Real
  silent semantic bug that ships fixed by the bump.**
- **F1 — `Connection<P>` request lock.** Concurrent dumps on
  shared `Arc<Connection>` no longer steal each other's
  responses. nlink-lab is single-threaded per namespace today
  but the lock is the right default.
- **N1 — `namespace::create` thread-bleed.** `unshare` on a
  tokio worker no longer bleeds the new netns to other tasks on
  that worker. nlink-lab's `LabNamespace::new` callers
  transitively benefit.

Net: **0.19 is the most consequential nlink release for
nlink-lab so far.** The items in this doc are next-layer
follow-ups, not regressions.

Thanks for shipping the cycle.

---

## Maintenance details

- **nlink version reviewed:** 0.19.0, source at
  `/home/mpardo/git/rip`. nlink-lab commit `3ea2cb7`.
- **Cross-reference:** `nlink-feedback.md` (2026-05-30, against
  0.18.0) and `nlink-0.19-realignment.md` (2026-05-31,
  per-item closeout against 0.19).
- **How to reach me:** the nlink-lab repo
  (`p13marc/nlink-lab`) carries every detail of every plan I've
  written against your APIs; commit messages link back to nlink
  Plan numbers where applicable. Most of the 159 arc is one
  PR each in `docs/plans/`.
