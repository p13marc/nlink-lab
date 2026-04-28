# Plan 151: Killer Examples — "What containerlab can't do, in 30 seconds"

**Date:** 2026-04-27
**Status:** Proposed
**Effort:** Medium (3–4 days for all four; each example is ~half a day's work + writeup)
**Priority:** P1 — these are the marketing artifacts. Without them, Plan 150's docs have no anchors.

---

## Problem Statement

Plan 150 ships the docs scaffolding. This plan ships the load-bearing
content that proves the wedge:

- **Per-pair impair on shared L2.** Already implemented (Plan 128).
  Needs a polished, reproducible 30-second walkthrough.
- **Composed primitives** (VRF + WireGuard + nftables + macvlan) in a
  single topology that demonstrates depth containerlab/Docker
  network model can't reach.
- **Mid-test fault injection** via the scenario engine. The
  scenario DSL is implemented but underused in examples.
- **Library-first CI testing.** A real Rust test that deploys, runs
  assertions, and tears down — showing the `#[tokio::test]` use
  case.

Each example is a polished, runnable artifact that doubles as a
blog post. The goal is a Hacker News / r/networking / r/rust
submission per example, with the walkthrough as the link.

## Goals

1. Four examples, each with a paired markdown writeup, runnable in
   under 60 seconds from clone to first interesting output.
2. Each example demonstrates something containerlab cannot express,
   or expresses far more verbosely.
3. Each writeup is shareable as a standalone article (no
   nlink-lab-internal jargon, full context).
4. Each example has an integration test (root-gated) that asserts
   the walkthrough's claims still hold after future changes.

## The four examples

### Example A: Distance-dependent satellite mesh (Plan 128 showcase)

**File:** `examples/cookbook/satellite-mesh.nll`,
`docs/cookbook/satellite-mesh.md`

**Hook:** "A 12-node mesh with realistic per-hop satellite links —
delays from 8ms to 280ms, loss from 0.1% to 5%, on a single laptop,
in 200ms of deploy time. Each pair is independent. There is no way
to express this in a single containerlab/Docker topology."

**Build:**

- 12 nodes arranged in a Iridium-like ring (each node has 2
  neighbors)
- One shared `network mesh` with a subnet
- An NLL `for` loop generates 24 directional impair rules from a
  distance table (`let distances = [[0,8,15,...]]`) — proving the
  DSL's expressiveness.
- Run `ping` from each node to each other; assert the latency
  distribution matches the configured matrix.

**Writeup structure:**

```markdown
# A 12-node satellite mesh in 30 seconds

The packet you send takes a different number of hops depending on
which neighbor you're talking to. So why are most network labs
flat?

[image: ring topology]

Here's the entire definition:

[NLL embedded — show the for-loop magic]

That `for src in [...] { for dst in [...]` block computes 24
directional impair rules at parse time. The lab boots in 200ms,
each pair sees its own latency profile, and you can tear it down
with one command.

containerlab can't express this because Docker's bridge network
gives every container the same egress queue — there's no
per-destination netem on a shared L2. nlink-lab uses the kernel's
HTB+netem+flower TC primitives directly via netlink.

[Section: how the NLL `for` loop expands]

[Section: what the kernel actually built — `tc -s qdisc show`
output annotated]

[Section: variations — asymmetric, per-pair rate caps, partition]

[Section: where this is useful — P2P/mesh protocol testing,
satellite app development, embedded networking CI]
```

**Test:** `tests/satellite_mesh_e2e.rs` (root-gated) — deploys the
12-node mesh, pings every pair 20 times, asserts mean RTT for
each pair is within ±20% of the configured value, tears down.

### Example B: Multi-tenant WAN with VRF + WireGuard + nftables

**File:** `examples/cookbook/vrf-wg-wan.nll`,
`docs/cookbook/vrf-wg-wan.md`

**Hook:** "Two customer VRFs sharing a physical-link emulation,
encrypted between sites with WireGuard, with per-VRF stateful
firewall rules. Twelve namespaces, three VRFs, four WG tunnels,
two nftables rule sets, six static routes. 25 lines of NLL."

**Build:**

- 2 sites, each with: hub node (has the VRFs), 2 customer nodes
  (one per VRF)
- Hub uses 2 VRFs to keep customer-A and customer-B traffic
  separate at L3
- Inter-site uses WireGuard (auto-keygen) — one tunnel per VRF
- nftables on each hub: stateful conntrack, `customer-A` cannot
  reach `customer-B` even though they share infrastructure
- A single `link` between hub-A and hub-B with delay 30ms loss
  0.1%

**Writeup structure:**

- Show the NLL — emphasize how `vrf` and `wireguard` blocks
  compose with regular interfaces
- Show what containerlab's equivalent would look like (a long
  `exec:` block with 30 lines of `ip vrf add`, `ip route add`,
  `wg setconf`, `nft add rule` — and you'd have to manually
  generate WG keys outside the lab)
- Run `nlink-lab exec` from a customer-A node to verify it can
  reach customer-A on the other site, and to verify it can NOT
  reach customer-B
- Annotated `ip route show vrf customer-a` output

**Test:** `tests/vrf_wg_wan_e2e.rs` — deploy, assert WG handshake
succeeds, assert cross-VRF isolation, tear down.

### Example C: Mid-test fault injection with the scenario engine

**File:** `examples/cookbook/p2p-partition.nll`,
`docs/cookbook/p2p-partition.md`

**Hook:** "Run your distributed system under realistic failure
modes: a 5-second link partition mid-test, recovery, then a
30-second 50% loss spike. The scenario engine runs as part of
deploy — your test doesn't even know it's happening."

**Build:**

- 3 nodes running an example consensus protocol (we'll use a tiny
  Rust binary that does heartbeat + leader election; ship the
  binary or use `python3 -m http.server` as a stand-in)
- A `scenario` block:
  ```nll-ignore
  scenario partition-test {
    at 0s   { exec node1 -- ./run-test.sh }
    at 5s   { down node1:eth0 }
    at 10s  { up node1:eth0 }
    at 15s  { impair node2:eth0 loss 50% }
    at 45s  { clear node2:eth0 }
    at 60s  { validate { reach node1 node2 } }
  }
  ```
- Show how to run the scenario, capture the resulting timing data,
  and assert the consensus protocol recovered

**Writeup structure:**

- "Most network failure tests are a Bash script that runs `iptables
  -A INPUT -j DROP` then `sleep`s. That's not a test; it's a
  fragile script."
- Show the scenario block
- Show what the scenario engine actually does (timeline diagram)
- Show the `--json` output that you can pipe to a CI report
- Compare to: containerlab + a separate `clab-tools` script + a
  Pytest fixture

**Test:** `tests/scenario_engine_e2e.rs` — run the scenario,
assert each timed step fires within ±100ms of its scheduled time.

### Example D: `#[nlink_lab::test]` — Rust integration test

**File:** `examples/cookbook/rust-integration-test.rs`,
`docs/cookbook/rust-integration-test.md`

**Note:** depends on Plan 154 (`#[nlink_lab::test]` proc macro)
landing first. If Plan 154 is not yet shipped, this example uses
the manual API:

```rust
#[tokio::test]
async fn my_protocol_handles_partition() {
    let topo = include_topology!("examples/p2p-partition.nll");
    let lab = topo.deploy().await.unwrap();
    let node1 = lab.exec("node1", "./run-protocol.sh").await.unwrap();
    let node2 = lab.exec("node2", "./run-protocol.sh").await.unwrap();
    lab.scenario("partition-test").run().await.unwrap();
    let metrics = lab.scenario_metrics("partition-test").await.unwrap();
    assert!(metrics.recovered_within(Duration::from_secs(10)));
    // Drop = auto-destroy
}
```

**Hook:** "Network integration tests as part of `cargo test`. No
Docker daemon, no compose file, no Makefile. The lab is a value;
when it goes out of scope it tears down. CI sees a normal test
result."

**Writeup structure:**

- "Why isn't there a network testing crate that just works in
  `#[tokio::test]`? Because every existing tool was built CLI-first
  and the library API is an afterthought. nlink-lab inverts that."
- Show the test, the topology, and `cargo test` output
- Show how to integrate with `criterion` for performance
  regression tests
- Show the `--features deploy_in_test` cfg gate to keep the lib
  tests dependency-free for users who don't need it

**Test:** the example IS the test. Lives in `examples/cookbook/`
as a Rust file plus an NLL file.

## Implementation order

1. **Example A first** (1 day). It's already 80% there from Plan 128
   — needs the 12-node generation via `for` loop, the writeup, and
   the e2e test.
2. **Example C second** (1 day). The scenario engine is implemented
   but underused; this is its showcase.
3. **Example B third** (1 day). Most complex topology; requires
   working VRF+WG+nftables composition, which is implemented but
   has not been stress-tested in one topology.
4. **Example D last** (0.5–1 day). Depends on Plan 154; if Plan 154
   slips, ship the manual-API version of D in parallel and revise
   when 154 lands.

Each example can be a separate PR and a separate blog post. Don't
bundle.

## Tests

| Test | Description |
|------|-------------|
| `tests/satellite_mesh_e2e.rs` | Root-gated; deploys + asserts ±20% latency match |
| `tests/vrf_wg_wan_e2e.rs` | Root-gated; WG handshake + cross-VRF isolation |
| `tests/scenario_engine_e2e.rs` | Root-gated; timed step assertions |
| `examples/cookbook/rust-integration-test.rs` | Compiles+runs as a `cargo test` example |

All e2e tests use `#[ignore]` by default; CI flips on the privileged
runner via `cargo test -- --ignored`.

## Documentation Updates

| File | Change |
|------|--------|
| `docs/cookbook/satellite-mesh.md` | New — example A writeup |
| `docs/cookbook/vrf-wg-wan.md` | New — example B writeup |
| `docs/cookbook/p2p-partition.md` | New — example C writeup |
| `docs/cookbook/rust-integration-test.md` | New — example D writeup |
| `docs/cookbook/README.md` | Add the four to the index, mark as "highlighted" |
| `README.md` | Link the satellite-mesh writeup from the hero example |

## Out of scope

- A web UI / TUI to visualize the scenario engine's timeline. (Nice
  to have. Future plan if there's demand.)
- Vendor-NOS interop examples. Off-strategy per Plan 150.
- Multi-host examples. Not supported, by design.

## Acceptance

- Four polished examples, each runnable in under 60 seconds from
  clone.
- Four writeups, each suitable as a standalone blog post.
- Four e2e tests; all pass on the privileged runner.
- Each writeup links to the relevant cookbook recipe(s) and CLI
  reference pages from Plan 150.
- README.md hero example points to the satellite-mesh writeup.
