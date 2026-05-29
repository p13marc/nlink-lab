# Plan 158f — `Display`-driven diff rendering

**Date:** 2026-05-29
**Status:** Proposed (PR F of the Plan 158 arc — new with 0.18
expansion)
**Effort:** Small (0.5 day)
**Priority:** P2 — ergonomic cleanup; lets us delete custom
diff renderers in favor of upstream's, and exposes structured
diffs natively in `apply --check --json`.

---

## TL;DR

nlink 0.18 shipped `impl Display for NftablesDiff` (Plan 183)
and `impl Display for ConfigDiff` (the `NetworkConfig` diff
type — same plan). With Plans 158a + 158e in flight, those
upstream diffs become the load-bearing structures inside
`nlink-lab apply --check` and `apply --dry-run`.

nlink-lab today carries its own `impl Display for
TopologyDiff` in `crates/nlink-lab/src/diff.rs:154`. It
renders 12 different change kinds via hand-rolled
`writeln!` calls. Most of those (links, addresses, routes,
qdiscs, firewall, NAT) are now covered upstream. The
nlink-lab side keeps the small set that genuinely lives at
the lab level (nodes added/removed, sysctls, per-pair
impair, spawned processes — the "topology graph" diff,
not the "kernel resource" diff).

After this plan:

```
nlink-lab apply --check
  TopologyDiff: 2 graph changes
    + add node: server-2
    + add link: router:eth2 -- server-2:eth0

  NetworkConfig diff (server-2 ns): 3 changes        ← upstream Display
    + link  veth eth0 → router:eth2
    + addr  10.0.2.2/24 on eth0
    + route default via 10.0.2.1

  NftablesDiff (server-2): 0 changes                  ← upstream Display
```

The three diffs are rendered through their respective
`Display` impls. `--json` serializes each as a structured
sub-object.

---

## Audit

### nlink-lab today

`crates/nlink-lab/src/diff.rs:154-280` — hand-rolled
`Display for TopologyDiff` that walks 12 fields and emits
one line per change.

`crates/nlink-lab/src/diff.rs:120-152` — `is_empty()` /
`change_count()` helpers (kept).

`bins/lab/src/main.rs:1126` — `apply --check` body prints
`diff` via `{diff}` (calls our Display).

### nlink 0.18 surface

- `impl Display for NftablesDiff` —
  `crates/nlink/src/netlink/nftables/config/diff.rs`.
- `impl Display for ConfigDiff` (the `NetworkDiff`
  alias) — `crates/nlink/src/netlink/config/diff.rs`.
- Both render compact `+ add` / `- remove` / `~ replace`
  per change.

---

## Goals

1. **Shrink `TopologyDiff::Display`** — keep node/link/
   sysctl/spawn-config rendering (lab-graph concerns);
   drop link-resource / address / route / qdisc /
   firewall / NAT lines (now upstream's job).
2. **Introduce `LayeredDiff`** — a small struct that
   bundles the three diffs (`TopologyDiff` +
   `NetworkDiff` + `NftablesDiff`) with a single
   `Display` impl that renders each one in turn.
3. **`apply --check --json` output is layered** — top-
   level JSON with `topology`, `network`, `nftables`
   subobjects, each carrying the upstream diff's
   serialized shape (or our pruned `TopologyDiff` for
   the lab-graph part).
4. **Internal flag for the layered renderer** — pass
   through `apply --check` and `apply --dry-run` so they
   share the same code path.

---

## Phases

### Phase 1 — Trim `TopologyDiff::Display` (0.15 day)

After 158a + 158e land, six of the twelve fields in
`TopologyDiff` either:
- Disappear because the resource lives in upstream's
  `NetworkConfig` / `NftablesConfig` and is diffed there
  (links, addresses, routes, firewall, NAT, qdiscs).
- Stay because they're lab-only concerns (nodes,
  sysctls, per-pair impair, network impair, rate
  limits, spawned processes).

Trim `Display` to the second list only. ~80 lines deleted.

### Phase 2 — `LayeredDiff` struct (0.2 day)

```rust
// crates/nlink-lab/src/diff.rs

use nlink::netlink::config::NetworkDiff;
use nlink::netlink::nftables::config::NftablesDiff;

/// A full apply-time diff bundling the three layers:
/// lab graph, RTNETLINK resources, and nftables resources.
///
/// Each layer has its own `Display` impl. `LayeredDiff`'s
/// `Display` renders them in order with a one-line header
/// per non-empty layer.
pub struct LayeredDiff<'a> {
    pub topology: &'a TopologyDiff,
    /// Per-node `NetworkConfig` diffs, keyed by node name.
    /// Only entries with at least one change are kept.
    pub network: HashMap<String, NetworkDiff>,
    /// Per-node `NftablesDiff`, same shape.
    pub nftables: HashMap<String, NftablesDiff>,
}

impl<'a> LayeredDiff<'a> {
    pub fn is_empty(&self) -> bool {
        self.topology.is_empty()
            && self.network.values().all(|d| d.is_empty())
            && self.nftables.values().all(|d| d.is_empty())
    }

    pub fn change_count(&self) -> usize {
        self.topology.change_count()
            + self.network.values().map(|d| d.change_count()).sum::<usize>()
            + self.nftables.values().map(|d| d.change_count()).sum::<usize>()
    }
}

impl<'a> std::fmt::Display for LayeredDiff<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.topology.is_empty() {
            writeln!(f, "TopologyDiff: {} graph changes",
                self.topology.change_count())?;
            write!(f, "{}", self.topology)?;
        }
        for (node, diff) in &self.network {
            if diff.is_empty() { continue; }
            writeln!(f, "NetworkConfig diff ({node} ns): {} changes",
                diff.change_count())?;
            write!(f, "{diff}")?;     // upstream Display
        }
        for (node, diff) in &self.nftables {
            if diff.is_empty() { continue; }
            writeln!(f, "NftablesDiff ({node}): {} changes",
                diff.change_count())?;
            write!(f, "{diff}")?;     // upstream Display
        }
        if self.is_empty() {
            writeln!(f, "no changes")?;
        }
        Ok(())
    }
}
```

### Phase 3 — `apply --check --json` layered envelope (0.15 day)

JSON shape:

```json
{
  "topology": {
    "is_empty": false,
    "change_count": 2,
    "nodes_added": ["server-2"],
    "nodes_removed": [],
    "links_added": [{"endpoints": ["router:eth2", "server-2:eth0"]}],
    "links_removed": [],
    "sysctls_changed": [],
    "network_impairs_changed": [],
    "rate_limits_changed": [],
    "spawn_changed": []
  },
  "network": {
    "server-2": { /* serialized NetworkDiff */ }
  },
  "nftables": {
    "server-2": { /* serialized NftablesDiff */ }
  },
  "total_change_count": 5
}
```

The two upstream diffs need `serde::Serialize`. nlink ships
them as `#[non_exhaustive]` already (Plan 163). Three
options:

- **A.** Ask nlink to add a `serde` feature that derives
  `Serialize` on the diff types. Small upstream PR.
- **B.** Hand-roll a `SerializeAsJson` shim in nlink-lab
  that walks the diff's public getters and produces
  the JSON object.
- **C.** Use the `Display` shape and put the human-
  readable string as the JSON value.

Recommendation: A. The upstream report
(`nlink-upstream-asks.md`) was generous to nlink-lab — one
more small ergonomic ask is acceptable. As a fallback,
land B in nlink-lab's repo as a stopgap.

### Phase 4 — Wire into CLI (0.1 day)

`bins/lab/src/main.rs` — apply --check / --dry-run paths:

```rust
let layered = LayeredDiff { topology, network, nftables };
if cli.json {
    serde_json::to_writer_pretty(stdout, &layered)?;
} else {
    write!(stdout, "{layered}")?;
}
```

`layered.is_empty()` drives the exit code (0 = no changes,
1 = drift detected, matches `apply --check` CI semantics).

---

## Tests

### Unit

| Test | Description |
|------|-------------|
| `layered_diff_empty_shows_no_changes` | Display of all-empty `LayeredDiff` says "no changes". |
| `layered_diff_renders_three_sections_when_present` | Topology + network + nftables all carry one change each; assert all three section headers in output. |
| `layered_diff_skips_empty_subdiffs` | Only nftables has changes; network + topology sections omitted from output. |
| `layered_diff_json_envelope_shape` | Serialized JSON has the documented top-level keys. |

### Integration (root-gated)

| Test | Description |
|------|-------------|
| `apply_check_on_unchanged_topology_exits_zero` | Deploy lab; run `nlink-lab apply --check`; assert exit code 0 + "no changes" output. |
| `apply_check_on_edited_nft_exits_one` | Deploy lab; manually edit a rule via `nft -f`; assert `apply --check` exits 1 + reports the drift. |

---

## Acceptance

- `crates/nlink-lab/src/diff.rs` `Display for TopologyDiff`
  is ~80 lines shorter, covers only lab-graph concerns.
- New `LayeredDiff` struct in `crates/nlink-lab/src/diff.rs`.
- `apply --check` and `apply --dry-run` produce three-
  section human output via `LayeredDiff::Display`.
- `apply --check --json` emits the documented top-level
  envelope.
- JSON schema in `docs/json-schemas/layered-diff.schema.json`.
- 6 new tests pass.
- If option A (nlink `serde` feature) is the path chosen,
  open the upstream PR concurrently — it's tiny.

---

## Out of scope

- **A `Diff` trait abstracting over the three sources.**
  Overkill for three users.
- **A "compact" / "verbose" toggle.** Upstream's Display
  is compact by default; the `{:#}` alternate form is the
  verbose path. Pass through unchanged.
- **Coloured terminal output.** No, the Display
  implementations are uncoloured by convention.

---

## Files

| File | Change |
|------|--------|
| `crates/nlink-lab/src/diff.rs` | Shrink `Display for TopologyDiff` by ~80 LOC; add `LayeredDiff` struct + `Display` impl + tests. ~+80 / −80 LOC net. |
| `bins/lab/src/main.rs` | Wire `LayeredDiff` into `apply --check` + `apply --dry-run`. ~+30 LOC. |
| `docs/json-schemas/layered-diff.schema.json` | NEW. |
| `crates/nlink-lab/tests/integration.rs` | 2 new root-gated tests. |
| (upstream `nlink`) | Optional small PR adding a `serde` feature that derives `Serialize` on `NetworkDiff` + `NftablesDiff`. |
