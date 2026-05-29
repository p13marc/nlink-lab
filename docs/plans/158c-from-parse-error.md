# Plan 158c — Parse-error ergonomics + `default_route()` + 0.18 cleanup sweep

**Date:** 2026-05-27 (expanded 2026-05-29 — 0.18 default_route
helpers + larger sweep enabled by BC-break freedom)
**Status:** Proposed (PR C of the Plan 158 arc)
**Effort:** S (3–4 hours)
**Priority:** P3 — janitorial; ships alongside the nlink bump.

---

## TL;DR

nlink 0.16 added `From<AddressParseError>` and
`From<RouteParseError>` `impl`s on `nlink::Error` (Plan 173 in
nlink). The pitch in nlink's CHANGELOG:

```rust
// before
let addr: Address = "10.0.0.1/24"
    .parse()
    .map_err(|e: AddressParseError| nlink::Error::InvalidMessage(e.to_string()))?;

// after
let addr: Address = "10.0.0.1/24".parse()?;
```

nlink-lab doesn't use `nlink::Address::parse` directly — it
parses everything through `std::net::Ipv4Addr` / `Ipv6Addr` /
`IpAddr` and pre-strips the CIDR prefix via its own
`parse_v4_cidr` / `parse_cidr` helpers. So the immediate
`?`-collapse pitch from the nlink CHANGELOG doesn't apply
literally.

What we *can* do is:

1. **Add `From<std::net::AddrParseError>` and
   `From<std::num::ParseIntError>` on `nlink_lab::Error`**,
   routing them to `Error::InvalidTopology`. This collapses
   the ~30 `.map_err(|e| Error::invalid_topology(format!("…: {e}")))`
   ceremonies in `deploy.rs` to bare `?` calls (when the
   surrounding context is descriptive enough).
2. **For sites where the surrounding context matters (the
   node name, the CIDR string), keep the explicit
   `.map_err(...)` with the context-rich message** — those
   shouldn't collapse to `?`.

This PR is mostly a janitorial sweep. Net `-50` lines, more
readable code, no behavior change.

---

## Audit

### Sites in nlink-lab that could collapse to `?`

Grep result for `.parse().map_err` and similar in
`crates/nlink-lab/src/`:

```text
deploy.rs:571   let addr: Ipv4Addr = local.parse().map_err(|e| { …(format!("invalid …: {e}")) })?;
deploy.rs:579   let addr: Ipv4Addr = remote.parse().map_err(|e| { …(format!("invalid …: {e}")) })?;
deploy.rs:1758  && let Ok(ip) = addr_str.parse::<IpAddr>()       // not a ? site, can stay
deploy.rs:1888  let addr: Ipv4Addr = target.parse().map_err(|e| { …(format!("invalid DNAT target '{target}': {e}")) })?;
deploy.rs:1906  let addr: Ipv4Addr = target.parse().map_err(|e| { …(format!("invalid SNAT target '{target}': {e}")) })?;
deploy.rs:1971  let port: u16 = tokens[i + 2].parse().map_err(|_| { …(format!("invalid port in match '{expr}'")) })?;
deploy.rs:1989  let port: u16 = tokens[i + 2].parse().map_err(|_| { …(format!("invalid port in match '{expr}'")) })?;
deploy.rs:2004  let icmp_type: u8 = tokens[i + 2].parse().map_err(|_| { … })?;
deploy.rs:2015  let icmp_type: u8 = tokens[i + 2].parse().map_err(|_| { … })?;
deploy.rs:2026  let mark: u32 = tokens[i + 1].parse().map_err(|_| { … })?;
deploy.rs:2069  let addr: Ipv4Addr = addr_str.parse().map_err(|e| format!("{e}"))?;
deploy.rs:2070  let prefix: u8 = prefix_str.parse().map_err(|e| format!("{e}"))?;
deploy.rs:2281  Some(via.parse().map_err(|e| { …(format!("invalid gateway '{via}' for route '{dest}' on node '{node_name}': {e}")) })?)
deploy.rs:2365  Some(via.parse().map_err(|e| { …(format!("invalid gateway '{via}' …)) })?)
```

Plus ~10 similar sites elsewhere (e.g. `bins/lab/src/main.rs`
when parsing `--set` values, NLL lexer integer parses).

### Classification

| Group | Sites | Treatment |
|-------|-------|-----------|
| **A. Identity wrappers** (no context added) | `deploy.rs:2069-2070`, a few internal helpers | Collapse to bare `?` via new `From` impls. |
| **B. Context-bearing wrappers** | `deploy.rs:571, 579, 1888, 1906, 2281, 2365` | Keep `.map_err(|e| …)` — the messages include `node_name`, `dest`, `via`, `target` etc. that you genuinely want in the error. Just add a `_:` type annotation if the new `From` causes inference ambiguity. |
| **C. Match-expression token parses** | `deploy.rs:1971, 1989, 2004, 2015, 2026` | Keep — these point at a specific malformed token in user input; the error needs the position. |

Group A is the only group that collapses. Realistic LOC
saving: ~10 lines across the codebase. The bigger
maintainability win is groups B+C — they get a uniform
treatment: every parse error has rich context, and the
"identity wrapper" anti-pattern (which adds noise without
context) becomes literally unrepresentable via `?`.

---

## Goals

1. **`nlink_lab::Error: From<std::net::AddrParseError>`** —
   routes to `Error::InvalidTopology(format!("invalid IP
   address: {e}"))`.
2. **`nlink_lab::Error: From<std::num::ParseIntError>`** —
   routes to `Error::InvalidTopology(format!("invalid
   integer: {e}"))`.
3. **Apply where it's a true wash** — group A sites only.
   Leave B and C verbatim.
4. **Document the convention** in `error.rs`: "Use bare `?`
   when the surrounding `Result<_, Error>` context is
   descriptive enough; use `.map_err(|e| Error::* {…, detail:
   …})` when the error needs to carry node / endpoint /
   field-name context."

---

## Phases

### Phase 1 — Add the `From` impls (5 min)

`crates/nlink-lab/src/error.rs`:

```rust
impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Self {
        Self::InvalidTopology(format!("invalid IP address: {e}"))
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Self::InvalidTopology(format!("invalid integer: {e}"))
    }
}
```

Add corresponding unit tests:

```rust
#[test]
fn from_addr_parse_error_routes_to_invalid_topology() {
    let e: Error = "not-an-ip".parse::<std::net::IpAddr>().unwrap_err().into();
    assert!(matches!(e, Error::InvalidTopology(_)));
    let rendered = e.to_string();
    assert!(rendered.contains("invalid IP address"));
}

#[test]
fn from_int_parse_error_routes_to_invalid_topology() {
    let e: Error = "abc".parse::<u32>().unwrap_err().into();
    assert!(matches!(e, Error::InvalidTopology(_)));
    let rendered = e.to_string();
    assert!(rendered.contains("invalid integer"));
}
```

### Phase 2 — Convert identity wrappers (15 min)

For the ~3-5 group-A sites (none in deploy.rs's hottest
paths — mostly in tiny private helpers like `parse_v4_cidr`
internals at `deploy.rs:2069-2070`), replace:

```rust
let addr: Ipv4Addr = addr_str.parse().map_err(|e| format!("{e}"))?;
let prefix: u8 = prefix_str.parse().map_err(|e| format!("{e}"))?;
```

with:

```rust
let addr: Ipv4Addr = addr_str.parse()?;
let prefix: u8 = prefix_str.parse()?;
```

(when `parse_v4_cidr` is changed to return `Result<_,
Error>` instead of `Result<_, String>` — see Phase 3).

### Phase 2b — Adopt `Ipv4Route::default_route()` / `Ipv6Route::default_route()` (5 min)

nlink 0.18 (Plan 184) ships:

```rust
nlink::netlink::route::Ipv4Route::default_route()  // 0.0.0.0/0
nlink::netlink::route::Ipv6Route::default_route()  // ::/0
```

nlink-lab today uses the literal-string idiom at
`deploy.rs:2294` and `deploy.rs:2322` (and two more in the
diff/route code):

```rust
nlink::netlink::route::Ipv4Route::new("0.0.0.0", 0)
nlink::netlink::route::Ipv6Route::new("::", 0)
```

Replace the four sites with `::default_route()`. The
`Ipv4Route::new("0.0.0.0", 0)` form still works — this is
purely a readability win.

### Phase 3 — Normalize `Result<_, String>` private helpers to `Result<_, Error>` (15 min)

Helpers in `deploy.rs` that return `Result<_, String>`
predate the `From` derive. Convert:

```rust
// Before
fn parse_v4_cidr(s: &str) -> std::result::Result<(Ipv4Addr, u8), String> { … }

// After
fn parse_v4_cidr(s: &str) -> Result<(Ipv4Addr, u8)> { … }
```

Then every caller's `.map_err(|e| Error::deploy_failed(…))`
becomes either `?` (if no extra context needed) or
`.map_err(|e| Error::* { node, detail: … })`.

This is the bulk of the wash — ~30 LOC saved.

### Phase 4 — Verify with `cargo +nightly fmt --check` + `clippy` (5 min)

No semantic behavior change. Should pass on first try.

---

## Tests

| Test | Description |
|------|-------------|
| `from_addr_parse_error_routes_to_invalid_topology` | New unit test — `IpAddr::parse` failure converts to `Error::InvalidTopology`. |
| `from_int_parse_error_routes_to_invalid_topology` | Same shape for `u32::parse`. |
| Existing 393 lib tests + ~43 integration tests | All pass unchanged (this PR has no behavior delta beyond error text wording). |
| Existing `deploy_simple`, `deploy_firewall`, `deploy_vrf` etc. | Catch any accidental error-shape regression in the converted helpers. |

No new root-gated integration tests needed — the deploy
suite exercises every parse helper end-to-end already.

---

## Acceptance

- `nlink_lab::Error` has both `From` impls + matching unit
  tests.
- `cargo clippy --workspace --all-targets -- -D warnings`
  clean.
- `cargo +nightly fmt --check` clean.
- Diff is net negative LOC (target: −30 to −50).
- No `.map_err(|e| format!("{e}"))?` patterns remain in
  `crates/nlink-lab/src/deploy.rs`.
- CHANGELOG entry under **Changed** (folded into 158a/b's
  umbrella entry, or a one-liner):
  > Internal cleanup — parse-error wrappers in `deploy.rs`
  > use bare `?` via new `From<AddrParseError>` /
  > `From<ParseIntError>` impls on `nlink_lab::Error`.

---

## Out of scope

- **The 0.16 `nlink::Error: From<AddressParseError>` impl
  itself.** That impl is on `nlink::Error`. nlink-lab never
  parses through `nlink::Address::parse` (it uses
  `std::net`), so the upstream impl is irrelevant in our
  call paths. The PR title still references it for plan-
  arc continuity — the *idea* is the same, the impl
  surface is just shifted to `std::net` / `std::num`
  errors instead.
- **Switching `parse_v4_cidr` to return `nlink::Address`**
  instead of `(Ipv4Addr, u8)`. That'd let us reuse
  upstream's `From<AddressParseError>` — but the
  surrounding NAT-rule lowering code wants a separated
  `(addr, prefix)` tuple, not a fused `Address`. Punt.
- **Context-bearing `.map_err` sites (groups B+C).**
  Keep as-is. The convention to be documented in
  `error.rs` is exactly: don't collapse those.

---

## Files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | `nlink = "0.17"` bump (shared with 158a/b). |
| `crates/nlink-lab/src/error.rs` | 2 new `From` impls + 2 new unit tests + a documentation paragraph. ~+30 LOC. |
| `crates/nlink-lab/src/deploy.rs` | Convert ~5 group-A `.map_err` sites + normalize ~3 `Result<_, String>` helpers to `Result<_, Error>`. ~−40 LOC. |
| `CHANGELOG.md` | Optional one-liner under **Changed** (or absorbed by 158a/b's umbrella entry). |
