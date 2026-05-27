# Plan 158b — Surface kernel `ext_ack` in nlink-lab errors

**Date:** 2026-05-27
**Status:** Proposed (PR B of the Plan 158 arc)
**Effort:** Small (0.5 day)
**Priority:** P2 — quality-of-life win that turns a class of
mystery `EPERM` / `EINVAL` failures into actionable one-liners.

---

## TL;DR

nlink 0.16 added `ext_ack: Option<String>` and
`ext_ack_offset: Option<u32>` to both `Error::Kernel` and
`Error::KernelWithContext`. The kernel populates these from
`NETLINK_EXT_ACK` TLVs (`NLMSGERR_ATTR_MSG`,
`NLMSGERR_ATTR_OFFS`) and they are *dramatically* more
actionable than the bare errno. nlink's `Display` already
renders them in the format:

```text
kernel error: <strerror>(errno <N>): <ext_ack> (at request offset <K>)
```

In nlink-lab today every kernel error is wrapped in one of
our higher-level variants (`Error::Firewall`, `Error::Route`,
`Error::NetlinkOp`, etc.) by formatting the inner error with
`{e}` — which routes through nlink's `Display` impl and
**already includes the ext_ack text** in the rendered
string. So step one is just: bump nlink to 0.17 and
verify the new content appears in our existing error
messages.

The work in this PR is:

1. **Add the `_ =>` wildcard arms** that 0.16's
   `#[non_exhaustive]` markers on `Error::Kernel` and
   `Error::KernelWithContext` require. Without these the
   code stops compiling.
2. **Audit our few `match` sites** that inspect kernel
   errors (e.g. `running.rs:1026`
   `Err(nlink::Error::QdiscNotFound { .. }) => {}`) and add
   the same wildcards if any of them destructure the
   affected variants.
3. **Optional:** add a top-level `Error::display_chain()`
   helper that walks the source chain and surfaces
   `ext_ack` even when the error was wrapped through more
   than one layer of `Error::Firewall { detail }` strings —
   this is useful for the `--json` error paths where we
   want the structured `ext_ack` separately from the
   already-flattened display string.

---

## Audit

### nlink 0.17 surface (citations to `/home/mpardo/git/rip/`)

- `Error::Kernel` — `crates/nlink/src/netlink/error.rs:31`
  (`#[non_exhaustive]`).
- `Error::KernelWithContext` —
  `crates/nlink/src/netlink/error.rs:49`
  (`#[non_exhaustive]`).
- Both variants carry `ext_ack: Option<String>` and
  `ext_ack_offset: Option<u32>`.
- Display rendering: `error.rs:268-302`
  (`format_kernel` / `format_kernel_ctx`).
- Helper getters on `nlink::Error`:
  - `.errno() -> Option<i32>` — `error.rs:484`
  - `.is_busy() -> bool` — `error.rs:474`
  - `.is_try_again() -> bool` — `error.rs:576`
  - **No** built-in `.ext_ack() -> Option<&str>` — consumers
    pattern-match the variants.
- Source parser: `crates/nlink/src/netlink/message.rs:329`
  (`NlMsgError::parsed_ext_ack`) reads
  `NLMSGERR_ATTR_MSG` (TLV type 1) and
  `NLMSGERR_ATTR_OFFS` (TLV type 2).

### nlink-lab sites that touch `nlink::Error` directly

```
crates/nlink-lab/src/error.rs:22
    Nlink(#[from] nlink::Error)

crates/nlink-lab/src/running.rs:1026
    Err(nlink::Error::QdiscNotFound { .. }) => {}
```

The `QdiscNotFound` match arm at `running.rs:1026` is on a
variant that is **not** `#[non_exhaustive]` in 0.17, so it
keeps compiling. There are no other direct destructuring
matches on `nlink::Error::Kernel{…}` or
`Error::KernelWithContext{…}` in nlink-lab — the
`Nlink(#[from] nlink::Error)` `#[from]` derive on
`nlink_lab::Error` propagates the whole variant
opaquely. Stays compatible without change.

### Where ext_ack already shows up "for free" after the bump

Every `.map_err(|e| Error::Firewall { detail: format!("…: {e}"), … })`
pattern in `deploy.rs` walks through `nlink::Error`'s
`Display` impl on its way to our `detail` string. That impl
already includes `ext_ack` if present — so an existing
firewall failure log like:

```text
apply firewall on node 'router': diff: kernel error: operation not permitted (errno 1)
```

becomes (after the bump, no nlink-lab code change):

```text
apply firewall on node 'router': diff: kernel error: operation not permitted (errno 1): netlink: Could not process rule: Operation not permitted (at request offset 16)
```

That's the main win. PR B is mostly verifying it lands
without regressions + the small audit below.

### `--json` error paths

In a few places nlink-lab emits JSON-shaped error envelopes
(`bins/lab/src/main.rs` — `deploy --json`, `inspect --json`,
`render --json`, `diagnose --json`). Today these flatten the
error to a single `error_message` string. After the bump,
the string is richer, but the structured `ext_ack` is still
buried inside it.

**Optional improvement:** add an
`ext_ack: Option<String>` field to the JSON error envelope
when the underlying error is a `nlink::Error::Kernel{…}` or
`KernelWithContext{…}`. This is a downstream consumer
ergonomic — tools like `jq` can then surface
`.error.ext_ack` directly.

---

## Goals

1. **Compile against nlink 0.17** with no new `match`-arm
   ceremony beyond what's strictly required.
2. **Verify ext_ack flows through** existing error messages
   by running an integration test that triggers a kernel
   `EINVAL` deliberately (e.g. attempt to add a route to a
   non-existent interface) and asserts the human-readable
   error string contains the kernel's `NLMSGERR_ATTR_MSG`
   text.
3. **Decide on the structured-ext_ack JSON surface.** Either
   add it (small) or document why we punt.

---

## Phases

### Phase 1 — nlink bump + compile sanity (0.1 day)

Already done as part of the umbrella Plan 158 dep bump.
This phase is just: build, run existing tests, confirm
nothing breaks. No code change.

### Phase 2 — Integration smoke test (0.2 day)

Add a single root-gated integration test that proves
`ext_ack` is being surfaced end-to-end.

```rust
// crates/nlink-lab/tests/integration.rs

#[lab_test(topology = ext_ack_smoke_topology)]
async fn kernel_ext_ack_surfaces_in_error(lab: RunningLab) {
    // Deliberately add a route via a non-existent interface.
    // The kernel rejects with EINVAL + a human-readable
    // NLMSGERR_ATTR_MSG explaining which attribute failed.
    let result = lab
        .exec("a", "ip", &["route", "add", "10.99.0.0/24", "dev", "nope0"])
        .unwrap();
    assert_ne!(result.exit_code, 0);
    // The `ip` command renders kernel errors itself; this
    // path verifies our infrastructure isn't filtering them.
    assert!(
        result.stderr.contains("Cannot find device") || result.stderr.contains("No such device"),
        "expected kernel detail in stderr, got: {}",
        result.stderr
    );
}

fn ext_ack_smoke_topology() -> nlink_lab::Topology {
    Lab::new("ext-ack-smoke").node("a", |n| n).build()
}
```

This test doesn't actually exercise nlink-lab's *own* error
path — it exercises `iproute2` inside the namespace. For a
library-internal test, see Phase 4.

### Phase 3 — Optional `--json` ext_ack surface (0.1 day)

Walk every `--json` error path in `bins/lab/src/main.rs`
and emit (where applicable) a structured shape:

```json
{
  "error": "apply firewall on node 'router': …",
  "ext_ack": "netlink: Could not process rule: Operation not permitted",
  "ext_ack_offset": 16,
  "errno": 1
}
```

The helper:

```rust
// crates/nlink-lab/src/error.rs (additions)
impl Error {
    /// Extract `(errno, ext_ack, ext_ack_offset)` if this error
    /// (or any error in its source chain) is a kernel error
    /// carrying NLMSGERR_ATTR_MSG / NLMSGERR_ATTR_OFFS.
    pub fn kernel_detail(&self) -> Option<KernelDetail<'_>> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                match e {
                    nlink::Error::Kernel { errno, ext_ack, ext_ack_offset, .. }
                    | nlink::Error::KernelWithContext { errno, ext_ack, ext_ack_offset, .. } => {
                        return Some(KernelDetail {
                            errno: *errno,
                            ext_ack: ext_ack.as_deref(),
                            ext_ack_offset: *ext_ack_offset,
                        });
                    }
                    _ => {}
                }
            }
            match src.source() {
                Some(next) => src = next,
                None => return None,
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KernelDetail<'a> {
    pub errno: i32,
    pub ext_ack: Option<&'a str>,
    pub ext_ack_offset: Option<u32>,
}
```

Caveat: nlink-lab's `Error::Firewall { detail: String }` and
similar variants stringify the inner error early — by the
time we have an `Error::Firewall`, the `nlink::Error` is
*gone*. To preserve the typed chain we'd need to switch
the variant to carry `source: Box<dyn Error>`. Out of
scope for PR B (too invasive); document as Plan 158b
follow-up.

For now: only the top-level `Nlink(#[from] nlink::Error)`
variant carries the typed chain. JSON envelopes can route
through `kernel_detail()` and it'll succeed for that one
variant — better than nothing, and the firewall/route paths
get the ext_ack via the flattened `Display` string anyway.

### Phase 4 — Library-internal integration test (0.1 day)

Verify `kernel_detail()` works on a real failure that
flows through `nlink_lab::Error::Nlink(…)`.

```rust
// crates/nlink-lab/tests/integration.rs

#[lab_test(topology = lib_ext_ack_topology)]
async fn library_kernel_error_carries_ext_ack(lab: RunningLab) {
    use nlink::netlink::link::DummyLink;

    // Open a connection inside the lab's namespace.
    let handle = lab.namespace_handle_for("a").unwrap();
    let conn: nlink::Connection<nlink::Route> = handle.connection().unwrap();

    // Add a dummy link, then try to add it again — the second
    // call yields EEXIST + a kernel ext_ack message.
    let link = DummyLink::new("dup-iface");
    conn.add_link(link).await.unwrap();
    let link2 = DummyLink::new("dup-iface");
    let err = conn.add_link(link2).await.unwrap_err();

    // Wrap into our error type, then extract.
    let lab_err: nlink_lab::Error = err.into();
    let detail = lab_err.kernel_detail()
        .expect("kernel error must surface detail");
    assert_eq!(detail.errno, 17, "EEXIST");
    assert!(
        detail.ext_ack.is_some(),
        "ext_ack should be populated by NLMSGERR_ATTR_MSG"
    );
    // The display string should also contain the ext_ack text
    // (passthrough through nlink::Error's Display impl).
    let rendered = format!("{lab_err}");
    let ack = detail.ext_ack.unwrap();
    assert!(
        rendered.contains(ack),
        "Display should include ext_ack: {rendered}"
    );
}

fn lib_ext_ack_topology() -> nlink_lab::Topology {
    Lab::new("lib-ext-ack").node("a", |n| n).build()
}
```

Skip this if Phase 3's `kernel_detail()` helper is descoped.

---

## Tests

| Test | Phase | Description | Gated |
|------|-------|-------------|-------|
| Existing tests (regression) | 1 | All 393 lib tests + ~43 integration tests pass against nlink 0.17. | (uses CI's existing matrix) |
| `kernel_ext_ack_surfaces_in_error` | 2 | Run `iproute2` inside lab, assert ENOENT for "Cannot find device" surfaces. | root |
| `library_kernel_error_carries_ext_ack` | 4 (opt.) | Trigger EEXIST via library API, assert `kernel_detail().ext_ack` populated. | root |
| `kernel_detail_walks_source_chain` | 3 (opt.) | Unit test on the helper using a hand-rolled `nlink::Error::Kernel { ext_ack: Some(…), … }`. | none |

---

## Acceptance

- nlink-lab compiles against `nlink = "0.17"`.
- An apparent existing firewall error message now includes
  the kernel's `ext_ack` text — verified by intentionally
  breaking a deploy (e.g. inject a rule the kernel will
  reject) and reading the error.
- If Phase 3 lands: `kernel_detail()` helper documented in
  `Error`'s rustdoc + JSON error envelopes in `bins/lab`
  carry `errno` / `ext_ack` / `ext_ack_offset` fields when
  the underlying error is a `nlink::Error::Kernel*` variant.
- CHANGELOG entry under **Changed** (umbrella with 158a):
  > Kernel error messages now include `NETLINK_EXT_ACK`
  > detail strings inline. Failed `apply` and `deploy`
  > operations surface the kernel's actionable text
  > (e.g. "netlink: Could not process rule: Operation not
  > permitted") instead of the bare errno.

---

## Out of scope

- **Typed-chain `nlink_lab::Error` rework.** Switching
  `Error::Firewall { detail: String }` to
  `Error::Firewall { source: Box<dyn Error + Send + Sync> }`
  is the right answer for full ext_ack fidelity but breaks
  every caller. Defer to Plan 159+ if there's demand.
- **Localizing ext_ack.** Kernel emits English strings;
  nlink-lab is also English. No translation surface.
- **Stripping ext_ack from `--quiet` mode.** Current
  `--quiet` only suppresses informational output; errors
  still go to stderr. No change.

---

## Files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | `nlink = "0.17"` bump (shared with 158a). |
| `crates/nlink-lab/src/error.rs` | (Optional Phase 3) Add `kernel_detail()` + `KernelDetail` struct. ~+40 LOC. |
| `bins/lab/src/main.rs` | (Optional Phase 3) Thread `kernel_detail()` into the 4 `--json` error envelopes. ~+20 LOC. |
| `crates/nlink-lab/tests/integration.rs` | 1–2 new `#[lab_test]` integration tests (Phase 2 + Phase 4). |
| `CHANGELOG.md` | Shared umbrella entry with 158a (see Acceptance). |
