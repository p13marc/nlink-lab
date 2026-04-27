# Plan 154: Polish + Promote the `#[lab_test]` Proc Macro

**Date:** 2026-04-27
**Status:** Proposed
**Effort:** Small (1.5 days)
**Priority:** P2 — the macro mostly exists; the gap is ergonomics
and discoverability. High leverage because it's the
strongest "library-first" claim we can make.

---

## Problem Statement

The `#[lab_test]` proc macro lives in
`crates/nlink-lab-macros/src/lib.rs` (~194 LOC). It works:

```rust
#[lab_test("examples/simple.nll")]
async fn test_ping(lab: RunningLab) {
    let out = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
    assert_eq!(out.exit_code, 0);
}
```

But:

1. **Not exported from the main crate.** Users have to add a
   second dep on `nlink-lab-macros`. Should be `pub use
   nlink_lab_macros::lab_test;` from `nlink-lab` so users get one
   crate.
2. **Not in the README's hero example or USER_GUIDE.md.** The
   library-first claim is the biggest wedge against containerlab,
   and it's invisible.
3. **No `--set` parameter override** at the macro level. Today
   users must build the topology, mutate it manually, then deploy.
   Should support:
   ```rust
   #[lab_test("simple.nll", set = { "delay" = "20ms" })]
   ```
4. **Lab-name collision risk if tests run with `--test-threads=1`
   on the same NLL.** Today's suffix uses `process::id() +
   fn_name`, which is unique per-process but the same within one
   process running the same test twice. Add a thread/test-id
   component.
5. **Skip-on-non-root behavior is "silently pass."** This is
   surprising — a `cargo test` reports green even though the
   test didn't run. Better: emit a libtest "ignored" via
   `#[ignore]`-friendly logic, or print a banner that's hard to
   miss. Users should opt into the silent skip with a flag.
6. **No retry / fixture-style helpers.** Test setup like "deploy
   the lab and wait for routing convergence" is hand-coded each
   time. Provide a `lab.wait_for_route(...)` and similar.
7. **Error messages on parse failure point at the proc macro,
   not the NLL file.** Users see "lab_test failed at line X" but
   X is in the macro expansion, not their NLL.

The macro is real, but rough around the edges and undiscovered.
This plan polishes and promotes it.

## Goals

1. `#[lab_test]` is the obvious way to write integration tests
   against nlink-lab from a Rust project, importable from a
   single dep on `nlink-lab`.
2. Test setup is one line for the common case (deploy NLL, get
   `RunningLab`, run assertions, auto-tear-down).
3. The library-first story is the README's third-paragraph hero
   example.
4. Common test utilities (wait-for-route, wait-for-tcp,
   capture-pings, scenario-runner) are in
   `nlink_lab::test_helpers`.

## Phases

### Phase A — Re-export + minor fixes (0.5 day, P0)

- `crates/nlink-lab/src/lib.rs`:
  ```rust
  #[cfg(feature = "test_macros")]
  pub use nlink_lab_macros::lab_test;
  ```
  Default-on the feature. Single dep for users.

- Fix the silent-skip-on-non-root: emit a clear banner and a
  `--ignored`-style return so `cargo test --include-ignored`
  semantics work. Match the convention nlink itself adopted in
  0.15.0 with `nlink::require_root!()`.

- Add a thread-id / nanos-since-epoch suffix on top of the
  existing process-id suffix to harden against repeated runs
  in a single process.

- Forward source spans from the NLL file in parse errors. The
  `nlink_lab::parser::parse_file` returns a miette diagnostic;
  the macro currently `.expect()`s on it, hiding the diagnostic.
  Use `match ... { Err(e) => panic!("{e:?}", ...) }` so miette
  pretty-prints the NLL location.

### Phase B — Macro feature parity (0.5 day, P1)

Extend the macro syntax:

```rust
#[lab_test("simple.nll")]                                // existing
async fn t1(lab: RunningLab) { ... }

#[lab_test(topology = my_topology_fn)]                   // existing
async fn t2(lab: RunningLab) { ... }

#[lab_test("multi-site.nll", set { wan_delay = "20ms", loss = "0.5%" })]
async fn t3(lab: RunningLab) { ... }                     // NEW

#[lab_test("simple.nll", timeout = 30s)]                 // NEW
async fn t4(lab: RunningLab) { ... }                     // wrap in
                                                          // tokio::time::timeout

#[lab_test("simple.nll", capture = true)]                // NEW — auto-records
async fn t5(lab: RunningLab) { ... }                     // every node:iface
                                                          // for the test
                                                          // duration; saves
                                                          // .pcap on failure
```

The `set { ... }` form mirrors the CLI `--set` flag and threads
through to `Topology::apply_overrides` before validation/deploy.

The `capture = true` form is high-leverage: a failing test
auto-attaches pcaps to the test output. Implementation: spawn
a `tokio::task` per `node:iface` that calls
`lab.capture_to_file(...)`. On `Drop`, only persist files if the
test failed.

The `timeout = 30s` form wraps the test body in
`tokio::time::timeout` and panics with a clear "test exceeded
N seconds" message. Default: 60s. Override per-test or
crate-wide via `NLINK_LAB_TEST_TIMEOUT` env var.

### Phase C — Test helpers module (0.5 day, P2)

Create `nlink_lab::test_helpers` with idioms harvested from real
test cases:

```rust
impl RunningLab {
    /// Block until `node` has a route to `dst`, polling every 100ms.
    /// Errors after `timeout`. Useful for routing-convergence tests.
    pub async fn wait_for_route(
        &self,
        node: &str,
        dst: IpAddr,
        timeout: Duration,
    ) -> Result<()> { ... }

    /// Block until `node:port` accepts TCP. Wraps the existing
    /// `wait-for --tcp` CLI logic.
    pub async fn wait_for_tcp(
        &self,
        ep: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<()> { ... }

    /// Run pings between two nodes for `dur` and return parsed
    /// stats. Errors if all pings dropped.
    pub async fn ping(
        &self,
        src: &str,
        dst: &str,
        count: u32,
    ) -> Result<PingStats> { ... }

    /// Run iperf3 between two nodes, returning the parsed JSON.
    pub async fn iperf3(
        &self,
        client: &str,
        server: &str,
        opts: Iperf3Opts,
    ) -> Result<Iperf3Report> { ... }
}
```

Each helper is a thin wrapper around `lab.exec(...)`. The
benchmark module already has the iperf3 / ping output parsers;
expose them publicly under `nlink_lab::test_helpers::parse_*`.

### Phase D — Documentation + cookbook recipe (0.25 day, P0)

This is the high-leverage outcome. Without it, Phases A–C are
invisible.

- `docs/cookbook/rust-integration-test.md` — tutorial: "Network
  integration tests in `cargo test`."
- `README.md` — promote a 12-line `#[lab_test]` snippet to the
  third paragraph (after the per-pair impair NLL hero).
- `docs/USER_GUIDE.md` — new "Library-first testing" section
  with the three macro forms and the test-helpers module.
- `crates/nlink-lab-macros/README.md` — document the macro
  itself; this becomes the `docs.rs` landing page for the macros
  crate.

Cookbook structure:

```markdown
# Recipe: Network integration tests in `cargo test`

You're writing a P2P protocol in Rust. You want to test that it
recovers from a 5-second link partition. Your options today:

- **A bash script that calls `iptables`.** Fragile. Doesn't run
  on macOS. Doesn't show in `cargo test` output.
- **containerlab + a Pytest fixture.** Three tools, two
  languages, one Docker daemon. Slow.
- **`#[lab_test]`.** One annotation. `cargo test`. Done.

[full example using #[lab_test], scenario block, asserts]

[Section: how it works under the hood]

[Section: capture = true — the killer feature]

[Section: when this is the wrong tool — see comparison page]
```

## Tests

| Test | Description |
|------|-------------|
| `lab_test_skips_on_non_root` | `cargo test` passes even without root, with banner |
| `lab_test_with_set_overrides` | `set { ... }` overrides apply before validation |
| `lab_test_timeout_panics_clearly` | `timeout = 1s` on a 5s test panics with the expected message |
| `lab_test_capture_attaches_pcap_on_failure` | A failing test leaves `.pcap`s in the test output dir |
| `wait_for_route_succeeds_on_convergence` | `wait_for_route` returns ok after a route is added |
| `ping_helper_returns_parsed_stats` | `ping(...)` returns PingStats with sane fields |

## File Changes

| File | Change |
|------|--------|
| `crates/nlink-lab/src/lib.rs` | Re-export `lab_test`; add `test_helpers` module |
| `crates/nlink-lab/src/test_helpers.rs` | New module |
| `crates/nlink-lab/Cargo.toml` | `test_macros` feature on by default |
| `crates/nlink-lab-macros/src/lib.rs` | `set { … }`, `timeout = …`, `capture = …`, banner-on-non-root, source-span forwarding |
| `crates/nlink-lab-macros/README.md` | New |
| `README.md` | Hero example #2 — `#[lab_test]` |
| `docs/USER_GUIDE.md` | New "Library-first testing" section |
| `docs/cookbook/rust-integration-test.md` | New |

## Acceptance

- `cargo add nlink-lab` (no separate macros crate) gets you
  `#[lab_test]`.
- `#[lab_test("topology.nll", set { ... }, timeout = 30s, capture = true)]`
  is documented and tested.
- README's hero pair: per-pair impair NLL + `#[lab_test]` Rust
  test using it.
- Cookbook "Rust integration test" recipe exists, links from
  README, cross-links from comparison page.
- `nlink_lab::test_helpers::{ping, iperf3, wait_for_route,
  wait_for_tcp}` exist and have unit tests.

## Dependencies

This plan is independent of 152 and 153 but benefits from Plan 150
(README rewrite) shipping first, since Phase D wants to add to the
new README structure rather than retrofit the old one.

## Out of scope

- A `criterion`-backed performance regression harness for labs.
  Could be a future plan; not load-bearing for this one.
- Custom test runners (custom-test-frameworks unstable feature).
  Stay on standard `#[tokio::test]`.
- Integration with insta for snapshot-testing topology dumps.
  Could be useful; not in this plan.
