# nlink issues spotted during the 158 arc integration

**Date:** 2026-05-30
**Audience:** nlink maintainer.
**Author:** nlink-lab maintainer.
**nlink version reviewed:** `0.18.0`.

While implementing the 158 arc (Plans 158a + 158b + 158c + 158e
Slices 1+2+3 + 158f + Phase 3) against nlink 0.18, I ran into
the following issues. Two are real bugs / footguns. Three are
ergonomic gaps. One is a feature request that already lives in
the nlink-lab plan files (158e Slice 4 candidate).

None of these block the 158 arc — nlink-lab worked around each
one downstream. Pasting them here in case any are quick wins
upstream.

---

## 1. (Footgun) `Error::from_errno_ext_ack` sign convention isn't obvious

**Severity:** medium — silently produces wrong errno values for
direct callers.

**Location:** `crates/nlink/src/netlink/error.rs:321-333`.

```rust
pub fn from_errno_ext_ack(
    errno: i32,
    ext_ack: Option<String>,
    ext_ack_offset: Option<u32>,
) -> Self {
    let message = io::Error::from_raw_os_error(-errno).to_string();
    Self::Kernel {
        errno: -errno,         // <-- negates the input
        ...
    }
}
```

The factory negates the input internally. The intended use is
"pass the kernel's signed errno as it appears in `nlmsgerr.error`
(which is negative — `-EEXIST = -17`)" → the stored value is
positive 17 → `.errno()` returns 17.

That's correct for the canonical parse path. But the parameter
name `errno: i32` doesn't telegraph the convention. A direct
caller (e.g. me, writing a unit test) reasonably reads "errno=1"
as "POSIX EPERM" and gets back `Some(-1)` from
`.errno()`. Surfaces in this test:

```rust
let kernel = nlink::Error::from_errno_ext_ack(1, ..., Some(16));
let lab_err = Error::Namespace { ..., source: kernel };
assert_eq!(lab_err.errno(), Some(-1));  // <-- counter-intuitive
```

**Suggested fixes** (pick whichever fits):

a. Rename the parameter to `errno_signed` or `errno_negated` so the
   convention is in the signature.
b. Add one sentence to the rustdoc: "Pass the kernel's negative-form
   errno; the factory negates it internally to produce the standard
   positive form on `Self::Kernel.errno`."
c. Normalize: `errno: -errno.abs()` to make both 1 and -1 produce
   stored 17 (for EEXIST). Slightly opinionated but eliminates the
   footgun entirely.

Same applies to `from_errno_with_context_ext_ack`.

---

## 2. (Real bug — downstream impact) `NetworkConfig::apply` iterates `links_to_add` in declared order

**Severity:** high for declarative consumers — silently fails with
kernel `ENODEV` on VLAN-parent ordering.

**Location:** `crates/nlink/src/netlink/config/apply.rs:118-142`.

The apply walks `diff.links_to_add` in the order links were
appended via `NetworkConfig::link()`. For a config that declares
a VLAN sub-interface AND its parent Dummy at the same
declaration level, the order is whatever the caller chose.

Downstream code that builds the config from a `HashMap` (very
common — our `node.interfaces` field is a HashMap, like most
Rust data structures of this shape) gets non-deterministic
order, and the kernel returns `ENODEV` for any VLAN created
before its parent.

We hit this in `topology_to_network_config` and worked around
it with a two-pass iteration (`crates/nlink-lab/src/deploy.rs`
`Pass 1` = Dummy + Bond + bond-member master ops; `Pass 2` =
VLANs). The regression test
`network_config_vlan_parent_dummy_declared_first_regardless_of_hashmap_order`
asserts the workaround is sound.

**Suggested fixes:**

a. **Documentation-only**: add a paragraph to `NetworkConfig::link`
   rustdoc stating that LinkBuilder kinds with parent dependencies
   (VLAN, possibly future bridge-slave shapes) must be declared
   *after* their parents in the same `NetworkConfig` if both are
   new.

b. **Topological sort**: inside `compute_diff`, detect VLAN
   parent dependencies (read `DeclaredLinkType::Vlan(parent,
   vid)`) and reorder `links_to_add` so parents come before
   children. ~40 LOC. Catches the bug at apply time without the
   downstream caller having to know about it.

c. **Both**: ship (a) immediately; ship (b) when next touching the
   apply path.

The same issue could in principle bite Bond + member shapes —
but in our Slice 2, members declared with `.master(bond)` end
up in `links_to_modify` (because veth members already exist),
which is phase 2 of apply, after the bond creation in phase 1.
So bond ordering happens to be correct by coincidence. VLAN is
the cleanly-broken case.

---

## 3. (Ergonomic gap) `nlink::netlink::config::apply::apply_diff` not re-exported

**Severity:** low — affects diff-then-apply consumers like our
`compute_layered_diff`.

**Location:** `crates/nlink/src/netlink/config/mod.rs:46-51`.

```rust
mod apply;             // <-- private
pub use apply::{ApplyOptions, ApplyResult};  // only these two
```

The free function `apply_diff(&ConfigDiff, &Connection<Route>,
ApplyOptions) -> Result<ApplyResult>` (apply.rs:98) is the
natural primitive for consumers that already hold a `ConfigDiff`
(e.g. dry-run preview workflows that compute the diff first,
display it, then optionally commit). Today the only public path
is `NetworkConfig::apply(&conn)` which re-runs `compute_diff`
internally — one wasted dump round-trip.

We worked around it by calling `cfg.apply(&conn)` and accepting
the redundant compute_diff. Real cost is small (one round-trip
per node per apply) but the asymmetry with `NftablesDiff::apply`
(which IS callable on the diff) is the kind of paper-cut that
adds up.

**Suggested fix:** `pub use apply::apply_diff;` in `config/mod.rs`,
or make `ConfigDiff::apply(&self, &conn, opts)` an inherent
method that internally calls the same function.

---

## 4. (Ergonomic gap) `ConfigDiff` has no inherent `apply()` method

**Severity:** low — naming asymmetry.

`NftablesDiff::apply` and `NftablesDiff::apply_reconcile` are
inherent methods (`nftables/config/diff.rs`). `ConfigDiff` (the
RTNETLINK one) has neither. The above-mentioned `apply_diff` free
function does the work, but it's not re-exported.

Consequence: consumers writing
`cfg.diff(&conn).await?.apply(&conn, opts).await?` — the
intuitive chain — get a method-not-found error and have to
either go through `cfg.apply(&conn)` (re-runs diff) or import
the private path.

**Suggested fix:** add `impl ConfigDiff { pub async fn apply(&self,
&Connection<Route>, ApplyOptions) -> Result<ApplyResult> { ... } }`
as a thin wrapper over the free function. Matches `NftablesDiff`'s
shape.

---

## 5. (Ergonomic gap) `Box<nlink::Error>` as `#[source]` breaks chain-walk downcast

**Severity:** medium — silent breakage for downstream code using
`Error::ext_ack` style accessors that downcast through
`std::error::Error::source()`.

**Background:** In nlink-lab Plan 158b we added `Error::ext_ack`
that walks the source chain via `std::error::Error::source` and
does `src.downcast_ref::<nlink::Error>()` at each step. nlink
0.18's `Error::ext_ack` accessor uses the same pattern.

Clippy (`result_large_err`) flagged our outer `Error` enum as
too big and suggested boxing the inner `nlink::Error` in
`#[source]`. We tried it. The chain walk silently stopped finding
the inner `nlink::Error` because thiserror's generated `source()`
for `Box<nlink::Error>` returns the `Box` first; downcasting
`&dyn Error` whose concrete type is `Box<nlink::Error>` doesn't
match `nlink::Error`.

The result is: `err.ext_ack()` returns `None` even though the
chain DOES contain a kernel error. Tests caught it. We backed
out the box and crate-allowed the lint.

**This isn't strictly nlink's bug** — it's how Rust's downcast +
thiserror's source emission compose. But it would help downstream
if `nlink::Error`'s rustdoc on the `Error::Kernel` variant
mentioned: "consumers boxing this variant on `#[source]` must
unwrap before downcast; the chain-walk accessor pattern depends
on the `nlink::Error` appearing directly in `source()`."

---

## 6. (Feature gap, already documented downstream) `LinkBuilder::vxlan` missing `local()` and `port()`

Plan 158e Slice 3 in nlink-lab calls this out explicitly:

> Vxlan stays imperative — upstream `LinkBuilder::vxlan` doesn't
> yet expose `local` / `port` setters our existing topology shape
> supports. Slice 4+ pending upstream extension.

The downstream nlink-lab `InterfaceConfig` carries `local:
Option<String>` (tunnel source IP) and `port: Option<u16>` (UDP
encap port). Current `LinkBuilder::vxlan(vni)` +
`.vxlan_remote(addr)` covers VNI + remote endpoint only.

**Suggested addition** (mirrors the imperative `VxlanLink`
shape):

```rust
impl LinkBuilder {
    pub fn vxlan_local(self, addr: IpAddr) -> Self { ... }
    pub fn vxlan_port(self, port: u16) -> Self { ... }
    // also useful: vxlan_underlay_dev(name) for the kernel
    // `IFLA_VXLAN_LINK` field nlink-lab's `parent` doesn't
    // currently surface.
}
```

Tiny upstream PR; would let Slice 4 fold VXLAN creation into the
declarative path.

---

## Summary triage

| # | Issue | Severity | Suggested fix size |
|---|-------|----------|-------------------|
| 1 | errno sign convention footgun | medium | 1 line rustdoc OR 1 line code |
| 2 | VLAN-parent ordering silent failure | **high** | docstring + optional ~40 LOC topo-sort |
| 3 | `apply_diff` free fn not re-exported | low | 1 line |
| 4 | `ConfigDiff` has no inherent `.apply()` | low | thin wrapper |
| 5 | `Box<Error>` source downcast trap | medium | 1 paragraph rustdoc |
| 6 | `LinkBuilder::vxlan_local` / `_port` | low | feature add, ~15 LOC |

Issues 1, 2, and 5 are the ones whose fix would have caught real
bugs in nlink-lab earlier. The others are paper-cuts.

I'm happy to open upstream PRs for any of these if it'd help.
Just flagging them here so they don't fall off the radar.
