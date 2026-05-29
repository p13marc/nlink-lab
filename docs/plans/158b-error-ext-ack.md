# Plan 158b — Typed `Error::source` chain + `ext_ack` surfacing

**Date:** 2026-05-29 (rewritten, BC-break freedom granted)
**Status:** Proposed (PR B of the Plan 158 arc)
**Effort:** Medium (1–1.5 days, breaking-change refactor)
**Priority:** P2 — quality-of-life; turns mystery `EPERM` /
`EINVAL` failures into actionable one-liners with full
typed-chain fidelity (was P3 before BC-break freedom).

---

## TL;DR

The original (pre-BC-freedom) version of 158b only added a
`kernel_detail()` helper that walked
`Error::Nlink(#[from] nlink::Error)` — the only variant
that still carried a typed `nlink::Error`. Every other
variant
(`Error::Firewall { detail: String }`, `Error::Route {
detail: String }`, `Error::NetlinkOp { detail: String }`,
`Error::Namespace { detail: String }`,
`Error::Container { detail: String }`) stringified the
inner error at construction — so by the time we had an
`Error::Firewall`, the kernel's `ext_ack` was already
flattened into the human-readable string and the structured
attribute was lost.

With BC-break freedom we can do the right thing:

```rust
// Before
pub enum Error {
    Firewall { node: String, detail: String },
    Route    { dest: String, node: String, detail: String },
    NetlinkOp { op: String, node: String, detail: String },
    Namespace { op: &'static str, ns: String, detail: String },
    Container { op: &'static str, name: String, detail: String },
    // ...
}

// After
pub enum Error {
    Firewall { node: String, #[source] source: nlink::Error },
    Route    { dest: String, node: String, #[source] source: nlink::Error },
    NetlinkOp { op: String, node: String, #[source] source: nlink::Error },
    Namespace { op: &'static str, ns: String, #[source] source: nlink::Error },
    Container { op: &'static str, name: String, #[source] source: Box<dyn std::error::Error + Send + Sync> },
    // ...
}
```

This gives us:

1. **Full source-chain fidelity** — `err.source()`
   recursively yields the underlying `nlink::Error` from
   any nlink-lab variant, so `err.kernel_detail()` works
   from the top-level error regardless of which wrapper
   variant fired.
2. **`Error::ext_ack()` callable on `nlink_lab::Error`
   directly** — a 3-line forwarder that walks `source()`
   until it finds an `nlink::Error` and returns
   `e.ext_ack()` (the accessor that nlink 0.18 ships).
3. **Cleaner `.map_err` sites** — `format!("…: {e}")` no
   longer needed because `thiserror` renders the chain
   automatically via `#[error("…")]`. About 50 `.map_err`
   ceremonies in `deploy.rs` simplify.
4. **No string-clobbering of typed data** — `errno`,
   `ext_ack`, and offset are preserved through every
   error wrapper for the JSON paths in `bins/lab` to
   surface structurally.

Container errors don't always wrap an nlink::Error (could
be `std::io::Error` from `docker exec`), so they get
`Box<dyn Error + Send + Sync>` as a generalized source.

---

## Audit

### nlink 0.18 ext_ack accessors (citations to `/home/mpardo/git/rip/`)

- `Error::Kernel { errno, message, ext_ack, ext_ack_offset }`
  — `crates/nlink/src/netlink/error.rs:31` (`#[non_exhaustive]`).
- `Error::KernelWithContext { operation, errno, message,
  ext_ack, ext_ack_offset }` — `error.rs:49`.
- `Error::ext_ack(&self) -> Option<&str>` — landed in
  nlink 0.18 (Plan 182). Walks `Kernel` and
  `KernelWithContext` variants.
- `Error::ext_ack_offset(&self) -> Option<u32>` — same.
- `Error::errno(&self) -> Option<i32>` — `error.rs:484`.
- `Display` impl renders `ext_ack` inline already
  (`error.rs:268-302`).

### nlink-lab error variants that wrap kernel errors today

`crates/nlink-lab/src/error.rs` defines 17 variants. The
ones currently using a `detail: String` shape that should
flip to typed sources:

| Variant | Today | After |
|---------|-------|-------|
| `Nlink(#[from] nlink::Error)` | typed | unchanged |
| `Namespace { op, ns, detail }` | string | `{ op, ns, source: nlink::Error }` |
| `NetlinkOp { op, node, detail }` | string | `{ op, node, source: nlink::Error }` |
| `Route { dest, node, detail }` | string | `{ dest, node, source: nlink::Error }` |
| `Firewall { node, detail }` | string | `{ node, source: nlink::Error }` |
| `Container { op, name, detail }` | string | `{ op, name, source: Box<dyn Error + Send + Sync> }` (heterogeneous) |
| `State { op, detail, path }` | string | leave — wraps mostly `io::Error` (already typed in chain) |
| `Capture(String)` | string | leave — netring errors, not nlink |
| `DeployFailed(String)` | string | leave — generic catch-all |
| `Validation(String)` | string | leave — domain error, no source |
| `InvalidTopology(String)` | string | leave — domain error |
| `Timeout(Duration)` | typed | unchanged |
| `Io(#[from] io::Error)` | typed | unchanged |
| `Json(#[from] serde_json::Error)` | typed | unchanged |
| `AlreadyExists` / `NotFound` / `NodeNotFound` / `InvalidEndpoint` | typed | unchanged |
| `NllParse(String)` / `NllDiagnostic(Box<…>)` | typed | unchanged |

**Five variants flip**: `Namespace`, `NetlinkOp`, `Route`,
`Firewall`, `Container`. Each currently has ~5-15
`.map_err(|e| Error::* { …, detail: format!("…: {e}") })`
call sites in `deploy.rs` / `running.rs`. Total ~50 call
sites to simplify.

### Call-site shape today

```rust
nft_conn.add_table(...).await.map_err(|e| {
    Error::Firewall {
        node: node_name.into(),
        detail: format!("failed to create nftables table: {e}"),
    }
})?;
```

### Call-site shape after

```rust
nft_conn.add_table(...).await.map_err(|e| Error::Firewall {
    node: node_name.into(),
    source: e,
})?;
```

The `failed to create nftables table` context moves into
the variant's `#[error]` template:

```rust
#[error("apply firewall on node '{node}'")]
Firewall { node: String, #[source] source: nlink::Error },
```

`Display` renders as `apply firewall on node 'router'`
plus the source chain (`thiserror` walks `source()`
automatically with `{:#}` / `Report` shaping).

---

## Goals

1. **Five variants flip** to typed-source shape.
2. **`Error::ext_ack(&self) -> Option<&str>`** + **
   `Error::ext_ack_offset(&self) -> Option<u32>`** + **
   `Error::errno(&self) -> Option<i32>`** inherent methods
   walk the source chain.
3. **JSON envelope** in `bins/lab` emits structured
   `errno` / `ext_ack` / `ext_ack_offset` / `error_chain`
   fields for all 4 commands using `--json` error
   reporting (deploy, status, render, diagnose).
4. **All ~50 affected `.map_err` sites** simplified to
   match the new variant shapes.
5. **Public API change** documented as a breaking change
   in CHANGELOG `[Unreleased]` under "Library API breaks."

---

## Phases

### Phase 1 — Variant refactor + inherent accessors (0.5 day)

#### 1.1 `crates/nlink-lab/src/error.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ... unchanged variants ...

    #[error("{op} namespace '{ns}'")]
    Namespace {
        op: &'static str,
        ns: String,
        #[source]
        source: nlink::Error,
    },

    #[error("{op} on node '{node}'")]
    NetlinkOp {
        op: String,
        node: String,
        #[source]
        source: nlink::Error,
    },

    #[error("add route '{dest}' on node '{node}'")]
    Route {
        dest: String,
        node: String,
        #[source]
        source: nlink::Error,
    },

    #[error("apply firewall on node '{node}'")]
    Firewall {
        node: String,
        #[source]
        source: nlink::Error,
    },

    #[error("{op} container '{name}'")]
    Container {
        op: &'static str,
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    // ... unchanged ...
}

impl Error {
    /// Walk the source chain looking for a kernel
    /// `NLMSGERR_ATTR_MSG` payload. Returns the first
    /// `ext_ack` string found, or `None` if no kernel
    /// error is in the chain.
    pub fn ext_ack(&self) -> Option<&str> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                if let Some(s) = e.ext_ack() {
                    return Some(s);
                }
            }
            src = src.source()?;
        }
    }

    /// Companion to `ext_ack()` — returns the offset (if
    /// any) into the request payload where the kernel said
    /// the rejected attribute lives.
    pub fn ext_ack_offset(&self) -> Option<u32> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                if let Some(o) = e.ext_ack_offset() {
                    return Some(o);
                }
            }
            src = src.source()?;
        }
    }

    /// Returns the kernel errno from the source chain, if
    /// any.
    pub fn errno(&self) -> Option<i32> {
        let mut src: &dyn std::error::Error = self;
        loop {
            if let Some(e) = src.downcast_ref::<nlink::Error>() {
                if let Some(n) = e.errno() {
                    return Some(n);
                }
            }
            src = src.source()?;
        }
    }
}
```

The three accessors deliberately mirror nlink 0.18's
inherent shape, so consumers calling `err.ext_ack()` get
the same semantic on either type.

#### 1.2 Update tests

```rust
#[test]
fn ext_ack_walks_through_firewall_variant() {
    let kernel = nlink::Error::Kernel {
        errno: 1,
        message: "operation not permitted".into(),
        ext_ack: Some("netlink: Could not process rule".into()),
        ext_ack_offset: Some(16),
    };
    let lab_err = Error::Firewall {
        node: "router".into(),
        source: kernel,
    };
    assert_eq!(lab_err.ext_ack(), Some("netlink: Could not process rule"));
    assert_eq!(lab_err.ext_ack_offset(), Some(16));
    assert_eq!(lab_err.errno(), Some(1));
}

#[test]
fn ext_ack_walks_through_container_variant_with_io_source() {
    let io = std::io::Error::new(std::io::ErrorKind::Other, "docker exec failed");
    let lab_err = Error::Container {
        op: "exec",
        name: "router".into(),
        source: Box::new(io),
    };
    assert_eq!(lab_err.ext_ack(), None);  // no nlink::Error in chain
    assert_eq!(lab_err.errno(), None);
}

#[test]
fn ext_ack_none_when_no_kernel_in_chain() {
    let lab_err = Error::Validation("bad name".into());
    assert_eq!(lab_err.ext_ack(), None);
}
```

### Phase 2 — Migrate all ~50 `.map_err` call sites (0.5 day)

Mechanical sweep. Grep for each of the 5 affected
variants:

```bash
rg 'Error::Firewall \{' crates/nlink-lab/src bins/lab/src
rg 'Error::Route \{'    crates/nlink-lab/src bins/lab/src
rg 'Error::NetlinkOp \{' crates/nlink-lab/src bins/lab/src
rg 'Error::Namespace \{' crates/nlink-lab/src bins/lab/src
rg 'Error::Container \{' crates/nlink-lab/src bins/lab/src
```

For each:

- Replace `detail: format!("…: {e}")` with `source: e`
  (drop the format-string context since the variant's
  `#[error]` template already carries node/op/dest).
- If the context was load-bearing (rare — e.g. the format
  string distinguishes between two `add_link` failures
  in the same function), introduce a more specific
  variant. Audit: there are probably ≤5 such call sites.
- `Container { detail }` sites either route through
  `nlink::Error` (use the typed source directly) or
  through `std::io::Error` (`Box::new(e)`); pick per
  site.

Expected cleanup: ~50 `format!("…: {e}")` ceremonies
removed; ~150 lines net.

### Phase 3 — JSON envelope wiring in `bins/lab` (0.25 day)

In each `--json` error path
(`deploy --json`, `status --json`, `render --json`,
`diagnose --json`), serialize a structured error:

```rust
fn render_error_json(err: &nlink_lab::Error) -> serde_json::Value {
    let mut chain = Vec::new();
    let mut src: &dyn std::error::Error = err;
    loop {
        chain.push(src.to_string());
        match src.source() {
            Some(next) => src = next,
            None => break,
        }
    }
    serde_json::json!({
        "error": err.to_string(),
        "error_chain": chain,
        "errno": err.errno(),
        "ext_ack": err.ext_ack(),
        "ext_ack_offset": err.ext_ack_offset(),
    })
}
```

Document this shape under `docs/json-schemas/error.schema.json`.

### Phase 4 — Documentation (0.25 day)

- `docs/TROUBLESHOOTING.md` — new section "Reading nlink-lab
  error output" with examples of human-readable + JSON
  shapes.
- `CHANGELOG.md` `[Unreleased]` entry under "Library API
  breaks":
  > `Error::{Namespace, NetlinkOp, Route, Firewall,
  > Container}` now carry `#[source] source: nlink::Error`
  > (or `Box<dyn Error>` for `Container`) instead of
  > `detail: String`. Match arms that destructure these
  > variants need updating. New inherent methods
  > `Error::ext_ack()` / `ext_ack_offset()` / `errno()`
  > walk the source chain.
- `CHANGELOG.md` under "Added":
  > `Error::ext_ack()`, `Error::ext_ack_offset()`,
  > `Error::errno()` inherent accessors mirror the
  > nlink 0.18 shape but walk through nlink-lab's
  > wrapper variants.

---

## Tests

### Unit

5 new tests in `crates/nlink-lab/src/error.rs`:

| Test | Description |
|------|-------------|
| `ext_ack_walks_through_firewall_variant` | Top-level `Error::Firewall { source }` returns the kernel's `ext_ack` string. |
| `ext_ack_walks_through_route_variant` | Same for `Error::Route`. |
| `ext_ack_walks_through_netlinkop_variant` | Same for `Error::NetlinkOp`. |
| `ext_ack_walks_through_container_variant_with_io_source` | `Box<dyn Error>` chain that doesn't contain `nlink::Error` returns `None`. |
| `ext_ack_none_when_no_kernel_in_chain` | `Error::Validation("...")` returns `None`. |

### Integration (root-gated)

| Test | Description |
|------|-------------|
| `ext_ack_surfaces_from_real_kernel_error` | Trigger a real kernel `EEXIST` (add a duplicate dummy link). Assert the resulting `nlink_lab::Error::NetlinkOp.ext_ack()` is `Some(_)` and contains kernel text. |
| `ext_ack_json_envelope_shape` | Run `nlink-lab deploy --json` on a topology that fails with a kernel error. Assert stdout JSON has `errno`, `ext_ack`, `ext_ack_offset` fields. |

---

## Acceptance

- All `.map_err(|e| Error::* { ..., detail: format!("...: {e}") })`
  patterns in `crates/nlink-lab/src` and `bins/lab/src` for
  the 5 affected variants are gone.
- `Error::ext_ack()` / `ext_ack_offset()` / `errno()`
  walk the source chain.
- `nlink-lab deploy --json` on a deliberately-failing
  topology surfaces structured `errno` / `ext_ack`.
- 7 new tests pass (5 unit + 2 integration).
- CHANGELOG entries under both "Library API breaks" and
  "Added".

---

## Out of scope

- **Variant unification.** The 5 variants stay distinct
  (`Firewall`, `Route`, `NetlinkOp`, `Namespace`,
  `Container`) — they carry different context fields and
  serve different rendering needs. A `Source` variant
  that collapses them was considered; rejected because
  losing the per-variant context (`node`, `dest`, `op`)
  is a downgrade.
- **Migrating `DeployFailed(String)` / `Capture(String)` /
  `Validation(String)` to typed sources.** These are
  catch-all / domain variants without a natural source.
  Stay as-is.
- **`#[non_exhaustive]` on `nlink_lab::Error`.** We
  already break BC here — adding `#[non_exhaustive]`
  defensively for future-proofing is a separate
  judgment call (and yes, we should — but it's a one-
  line addition that can land alongside or after).

---

## Files

| File | Change |
|------|--------|
| `crates/nlink-lab/src/error.rs` | Refactor 5 variants; add 3 inherent accessors; 5 new unit tests. ~+80 / −20 LOC. |
| `crates/nlink-lab/src/deploy.rs` | Migrate ~30 call sites. ~−100 LOC (format-string cleanup). |
| `crates/nlink-lab/src/running.rs` | Migrate ~10 call sites. |
| `crates/nlink-lab/src/state.rs` | Migrate ~5 call sites. |
| `bins/lab/src/main.rs` | JSON envelope helper + 4 dispatch path updates. ~+30 / −10 LOC. |
| `crates/nlink-lab/tests/integration.rs` | 2 new root-gated tests. |
| `docs/json-schemas/error.schema.json` | NEW — JSON Schema for the structured error envelope. |
| `docs/TROUBLESHOOTING.md` | New section. |
| `CHANGELOG.md` | New entries (Library API breaks + Added). |
