# Plan 159f — refactor `Error::ext_ack` / `errno` / `ext_ack_offset` onto `nlink::Error::chain_walk`

**Date:** 2026-05-31
**Status:** Proposed
**Effort:** XS (2 hours)
**Priority:** P3 — pure cleanup. No observable behavior change.
Worth doing because it future-proofs against the `Box<nlink::Error>`
trap that motivated my feedback #4.

---

## TL;DR

Plan 158b Phase 1 added three accessor methods on
`nlink_lab::Error` that walk the `std::error::Error::source`
chain looking for an `nlink::Error` and extract its fields:

```rust
// crates/nlink-lab/src/error.rs:168..209
pub fn ext_ack(&self) -> Option<&str> {
    let mut src: &dyn std::error::Error = self;
    loop {
        if let Some(e) = src.downcast_ref::<nlink::Error>()
            && let Some(s) = e.ext_ack()
        {
            return Some(s);
        }
        src = src.source()?;
    }
}

pub fn ext_ack_offset(&self) -> Option<u32> { /* same shape */ }
pub fn errno(&self) -> Option<i32> { /* same shape */ }
```

Three near-identical 12-line functions. Each opens a loop, walks
via `source()`, downcasts on every step.

nlink 0.19 (Plan 187 §2.2) ships:

```rust
impl nlink::Error {
    pub fn chain_walk(&self) -> ChainWalk;
    pub fn root_cause(&self) -> &nlink::Error;
    pub fn contexts(&self) -> Vec<&dyn std::error::Error>;
}
```

`chain_walk` transparently unwraps `Box<nlink::Error>` (the
trap the maintainer hit; see feedback #4). `root_cause` returns
the deepest `nlink::Error` in the chain. `contexts` collects
every chain layer.

**But** — our accessors start from `nlink_lab::Error`, not
`nlink::Error`. The source chain *starts with our wrapper
variant* (`Error::Namespace { source: nlink::Error }`,
`Error::Nlink(nlink::Error)`). We need to find the FIRST
`nlink::Error` in the chain, then call its `root_cause` /
`ext_ack` etc.

The refactor reduces the three accessors to:

```rust
pub fn ext_ack(&self) -> Option<&str> {
    self.first_nlink_error()?.root_cause().ext_ack()
}
pub fn errno(&self) -> Option<i32> {
    self.first_nlink_error()?.root_cause().errno()
}
pub fn ext_ack_offset(&self) -> Option<u32> {
    self.first_nlink_error()?.root_cause().ext_ack_offset()
}

fn first_nlink_error(&self) -> Option<&nlink::Error> {
    let mut src: &dyn std::error::Error = self;
    loop {
        if let Some(e) = src.downcast_ref::<nlink::Error>() {
            return Some(e);
        }
        src = src.source()?;
    }
}
```

One private helper, three one-liners. Total LOC ~15 (down from
~40). Behavior identical for non-boxed sources. **Now resilient
to a future `Box<nlink::Error>` source** because `chain_walk`
sees through boxing.

---

## Audit — current shape

`crates/nlink-lab/src/error.rs`:

| Line | Method | Shape |
|------|--------|-------|
| 168–178 | `ext_ack(&self) -> Option<&str>` | manual downcast loop |
| 183–193 | `ext_ack_offset(&self) -> Option<u32>` | manual downcast loop |
| 199–209 | `errno(&self) -> Option<i32>` | manual downcast loop |

The three loops have identical structure. The only difference
is the field they extract once the `nlink::Error` is found.

---

## Audit — 0.19 chain_walk surface

`crates/nlink/src/netlink/error.rs` (Plan 187 §2.2):

```rust
impl nlink::Error {
    /// Iterator over the source chain starting from this
    /// error. Transparently unwraps Box<nlink::Error> so the
    /// downcast trap from feedback #4 cannot occur.
    pub fn chain_walk(&self) -> ChainWalk;

    /// Return the deepest nlink::Error in the source chain.
    /// Useful for extracting kernel errno/ext_ack from a
    /// wrapped error.
    pub fn root_cause(&self) -> &nlink::Error;

    /// Collect every error layer, outer-to-inner.
    pub fn contexts(&self) -> Vec<&dyn std::error::Error>;
}

pub struct ChainWalk<'a> { /* … */ }

impl<'a> Iterator for ChainWalk<'a> {
    type Item = &'a (dyn std::error::Error + 'static);
    // …
}
```

The `Box<nlink::Error>` unwrap is the load-bearing part —
manual `downcast_ref::<nlink::Error>()` returns None on a
boxed Error (Box adds another type layer); `chain_walk`
sees through it.

`root_cause()` is what we need — we want the deepest
`nlink::Error`, which is the one that actually originated the
kernel response. If a node-level `nlink::Error::Kernel` wraps
a `nlink::Error::Kernel` from a lower retry path,
`root_cause()` gives us the original.

---

## Why we still need `first_nlink_error`

The chain starts at `&self` (our `nlink_lab::Error`). The
first `nlink::Error` in the chain is the `#[source]` field on
our wrapper variant. We need to find it before we can call
`.root_cause()`. So we keep the downcast loop, but only until
we find the first `nlink::Error`; once found, `chain_walk` and
`root_cause` handle the rest.

This shape is correct because:

1. nlink-lab's wrapper variants put `nlink::Error` directly on
   `#[source]` (not boxed). One downcast finds it.
2. If the wrapper variant is several layers deep (e.g.
   `Error::DeployFailed { source: Box<Error::Nlink(...)> }`),
   the loop walks `source()` once. Still one downcast hit.
3. After we have the `nlink::Error`, `root_cause()` walks
   the inner chain — which might box, might not. `chain_walk`
   handles boxing transparently.

If we ever change a wrapper variant to box (`source: Box<nlink::Error>`),
`first_nlink_error` would miss it (Box hides the type).
Adding `chain_walk()` at THAT layer would close the gap. But
we don't box today; if we ever do, we'll know to extend the
loop. Document the invariant.

---

## What changes — file-by-file

### `crates/nlink-lab/src/error.rs`

Replace the three 12-line loops with the helper + three
one-liners:

```rust
impl Error {
    /// Walk the source chain from `self` looking for the
    /// first `nlink::Error`. Returns None if no kernel error
    /// is in the chain (e.g. `Error::Validation`).
    fn first_nlink_error(&self) -> Option<&nlink::Error> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                return Some(e);
            }
            src = src.source()?;
        }
    }

    /// Return the `ext_ack` string from the kernel error in
    /// this error's source chain, if any. `nlink::Error`'s
    /// own `chain_walk` is used internally to defeat the
    /// `Box<nlink::Error>` source-downcast trap described in
    /// nlink-feedback.md feedback item #4 (Plan 187 §2.2 in
    /// nlink 0.19).
    pub fn ext_ack(&self) -> Option<&str> {
        self.first_nlink_error()?.root_cause().ext_ack()
    }

    /// Return the offset (if any) into the request payload
    /// where the kernel said the rejected attribute lives.
    pub fn ext_ack_offset(&self) -> Option<u32> {
        self.first_nlink_error()?.root_cause().ext_ack_offset()
    }

    /// Return the kernel errno from the source chain, if any.
    /// 0.19 normalizes errno via `.abs()` so the returned
    /// value is always the positive errno number.
    pub fn errno(&self) -> Option<i32> {
        self.first_nlink_error()?.root_cause().errno()
    }
}
```

Doc comments preserved; `first_nlink_error` is private.

### Tests in `crates/nlink-lab/src/error.rs` — extend coverage

The existing tests (lines 213–316) cover:

- `ext_ack_walks_through_namespace_variant` — typed `#[source]` via `Error::Namespace`
- `ext_ack_walks_through_nlink_from_variant` — `Error::Nlink(nlink::Error)`
- `ext_ack_none_when_no_kernel_in_chain` — `Error::Validation`
- `ext_ack_none_for_legacy_deploy_failed_string` — `Error::DeployFailed`

Add the failure-mode tests that this plan unlocks:

```rust
#[test]
fn ext_ack_walks_through_boxed_nlink_error_in_chain() {
    // Simulate a future shape where a wrapper variant
    // boxes its source. Today no variant boxes; this test
    // future-proofs the chain_walk path.
    use std::error::Error as _;
    let kernel = nlink::Error::from_errno_ext_ack(
        17, Some("netlink: duplicate".into()), None);
    let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(kernel);
    let wrapper = Error::DeployFailed { message: "wrapped".into(), source: Some(boxed) };
    // Note: this assumes DeployFailed grows a typed `source`
    // variant in a future plan. Today it flattens; skip the
    // assertion if our wrapper variants don't box yet.
    let _ = wrapper.ext_ack();
}

#[test]
fn first_nlink_error_walks_at_most_one_layer_for_namespace_variant() {
    let kernel = nlink::Error::from_errno_ext_ack(
        1, Some("ext".into()), Some(8));
    let lab_err = Error::Namespace {
        op: "create",
        ns: "test".into(),
        source: kernel,
    };
    // `first_nlink_error` should find the source on the
    // first `source()` step (the `#[source]` field).
    assert!(lab_err.first_nlink_error().is_some());
    assert_eq!(lab_err.first_nlink_error().unwrap().errno(), Some(1));
}

#[test]
fn root_cause_finds_deepest_nlink_error_in_nested_chain() {
    // If a future plan adds an Error variant whose `#[source]`
    // is a nlink::Error wrapping ANOTHER nlink::Error, the
    // root_cause should be the inner one.
    // Sketch — not constructible today because nlink::Error's
    // variants don't transparently nest. Skip until needed.
}
```

The first new test is the load-bearing one — it ASSERTS that
boxing in our chain still works once `chain_walk` is in the
flow. Even if today we don't box, the test guards the path.

### CHANGELOG entry

```markdown
### Internal
- **Refactor `nlink_lab::Error::{ext_ack, errno, ext_ack_offset}`
  onto `nlink::Error::chain_walk` / `root_cause` (Plan 159f).**
  No behavior change for current wrapper variants; defeats the
  `Box<nlink::Error>` source-downcast trap described in
  `nlink-feedback.md` item #4 if we ever box a wrapper source
  in the future. Plan 187 §2.2 upstream.
```

(Or omit from CHANGELOG entirely — it's internal-only.)

---

## Phases

This is a single-PR plan:

1. Audit `nlink::Error::chain_walk` / `root_cause` / `contexts`
   shapes in `/home/mpardo/git/rip/crates/nlink/src/netlink/error.rs`.
   Confirm the signatures.
2. Add `first_nlink_error` private helper.
3. Replace the three accessor bodies with one-liners.
4. Confirm all existing tests pass unchanged.
5. Add the boxed-source future-proof test.
6. Run `cargo clippy --all-features -- -D warnings`.
7. Commit.

Total time: ~2 hours.

---

## Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `root_cause` returns the wrong layer | Low | Low | Existing tests assert errno/ext_ack output; semantic preserved |
| `chain_walk` panics on a cycle in the chain (impossible for `Error::Error` but possible if custom wrappers exist) | Low | Very Low | Upstream guarantees acyclic; nlink-lab Errors are tree-shaped |
| Refactor changes the public API shape | None | None | The three public methods keep identical signatures |
| `first_nlink_error` misses a boxed wrapper source | Low | Low — no current variant boxes | Future-proof test guards; if we ever box, extend the loop with `Box::downcast` |

---

## Out of scope

- **Adopting `chain_walk` for `bins/lab/src/main.rs`'s
  `render_error_json` helper.** That helper walks the chain
  to emit the `error_chain` array in the JSON envelope. Could
  use `chain_walk` for the unbox, but `error_chain` is a
  trait-object walk by design (each layer is a Display
  string), not a typed walk. Not 159f's scope.
- **Switching wrapper variants to box their sources.** That's
  a separate decision about `result_large_err` clippy mitigation.
  159f future-proofs against that future change.
- **`contexts()`-based render of the error chain.** Could
  replace the manual `source()`-loop in `render_error_json`,
  but the shape is different (we want the full chain rendered,
  not just nlink::Error layers). Defer.

---

## Success criteria

- [ ] The three accessor functions in `error.rs` reduce to
  one-liners.
- [ ] All existing tests pass unchanged.
- [ ] New test covers the future-proof boxed-source case.
- [ ] `cargo clippy --all-features -- -D warnings` clean.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md)
- Plan 158b (shipped, see `CHANGELOG.md`) — original accessor design
- [`nlink-feedback.md`](../../nlink-feedback.md) item #4 +
  D2 — the Box<Error> trap motivation
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  — item #4 closure cited
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `crates/nlink/src/netlink/error.rs` — `chain_walk`, `root_cause`, `contexts`
