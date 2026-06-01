# Plan 159d — serde derive on LayeredDiff; drop `layered_summary` fallback

**Date:** 2026-05-31
**Status:** Proposed
**Effort:** Small (1 day)
**Priority:** P2 — schema bump; ship early so downstream consumers
have a deprecation period. Independent of every other 159 sub-plan.

---

## TL;DR

Plan 158f Phase 2's `apply --check --json` / `apply --dry-run --json`
output today carries a `layered_summary: String` field with the
formatted `Display` output. This shape was chosen because nlink
0.18's `ConfigDiff` / `NftablesDiff` didn't derive `Serialize`,
so we couldn't emit them as typed JSON.

nlink 0.19 ships opt-in `serde` derives on every diff type
(`ConfigDiff`, `NftablesDiff`, `WireguardConfigDiff`,
`LinkChanges`, the per-link / per-rule sub-structs) under a new
`serde` cargo feature (Plan 189 upstream).

Plan 159d enables `nlink/serde`, derives `Serialize` on
nlink-lab's `LayeredDiff` + `TopologyDiff`, and updates the
JSON schema for `apply --check --json` to a typed shape:

```json
{
  "lab": "my-lab",
  "no_op": false,
  "change_count": 7,
  "topology": { "nodes_added": [...], "links_added": [...], ... },
  "network": {
    "router": {
      "links_to_add": [{ "name": "eth1", "kind": "Veth", ... }],
      "addresses_to_add": [...],
      "routes_to_add": [...]
    }
  },
  "nftables": {
    "router": {
      "tables_to_add": [...],
      "rules_to_add": [...]
    }
  },
  "wireguard": {
    "router": {
      "devices_to_modify": [...]
    }
  }
}
```

This lets `jq '.network.router.links_to_add[]'` work directly
— no parsing of `layered_summary` text.

Schema bump is **backwards-incompatible** for downstream tools
that read `.layered_summary`. We keep that field for one
release as a deprecation period (with a
`"layered_summary_deprecated": true` marker), then remove it.

---

## Audit — what 0.19 ships for serde (citations to `/home/mpardo/git/rip/`)

### Cargo feature

`crates/nlink/Cargo.toml`:

```toml
[features]
serde = ["dep:serde"]
```

`dep:serde` makes the dependency optional + name-collision-safe.
Downstream just adds `features = ["serde"]` to enable.

### Derived types

Audit `crates/nlink/src/netlink/config/types.rs`,
`crates/nlink/src/netlink/nftables/config/types.rs`,
`crates/nlink/src/netlink/genl/wireguard/config.rs` for
`#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]`.

Expected coverage (Plan 189 upstream):

- `ConfigDiff` + every sub-field type (`LinkChanges`,
  `DeclaredLink`, `DeclaredAddress`, `DeclaredRoute`, `DeclaredQdisc`)
- `NftablesDiff` + every sub-field type (`NftablesTable`, `NftablesChain`, `NftablesRule`, `NftablesSet`, `NftablesElement`)
- `WireguardConfigDiff` + sub-fields (`DeviceChanges`, `DeclaredWgDevice`, `DeclaredWgPeer`, `AllowedIp`, `PublicKey`)
- `ApplyOptions`, `ApplyResult`, `ApplyError`
- `StackDiff`, `StackApplyReport`

What's NOT derived (intentional):

- `LinkMessage`, `AddressMessage`, `RouteMessage`, `NeighborMessage`,
  `TcMessage` — raw kernel-message types with byte-level layouts;
  serializing as JSON would just produce opaque blobs.
- `Connection<P>` — non-serializable I/O handle.
- `nlink::Error` — has `Box<dyn Error>` source; not serializable
  by design.

### Format choice — kebab-case vs snake_case

0.19's upstream rename style is `#[serde(rename_all = "kebab-case")]`
on most diff types. Confirm in Phase 1 audit. nlink-lab's existing
JSON output uses snake_case (`nodes_added`, `links_added`,
`layered_summary`). If kebab is the upstream choice, we have
two options:

1. **Keep snake_case downstream.** Override via
   `#[serde(rename = "...")]` on our own struct, accept the
   layer-mismatch (we read snake, deserialize from kebab in
   tests).
2. **Adopt kebab-case at the boundary.** Schema v2 uses kebab.
   Slightly more churn for downstream consumers.

Recommend option 1 — keep snake_case for backwards consistency
with the existing `TopologyDiff` shape.

---

## What changes — file-by-file

### `Cargo.toml` (workspace)

```diff
- nlink = { version = "0.19", features = ["full"] }
+ nlink = { version = "0.19", features = ["full", "serde"] }
```

If `full` already pulls `serde` in upstream, no diff. Confirm
in Phase 1.

### `crates/nlink-lab/src/diff.rs`

`LayeredDiff` gains `#[derive(Serialize)]`:

```rust
#[derive(Debug, Default, Serialize)]
pub struct LayeredDiff {
    pub topology: TopologyDiff,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub network: HashMap<String, ConfigDiff>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub nftables: HashMap<String, NftablesDiff>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub wireguard: HashMap<String, WireguardConfigDiff>,
}
```

`#[serde(default, skip_serializing_if = "HashMap::is_empty")]`
keeps the JSON tight — a deploy with no nftables / WG emits
just `topology` + (maybe) `network`. Smaller payloads on big
labs.

`TopologyDiff` already derives `Serialize` (existing shape).
Audit for any sub-field that doesn't.

`Display for LayeredDiff` — unchanged. The `layered_summary`
field in the legacy JSON envelope uses this.

### `bins/lab/src/main.rs`

The `apply --check --json` / `apply --dry-run --json` paths
today emit:

```rust
let envelope = serde_json::json!({
    "lab": lab_name,
    "no_op": layered.is_empty(),
    "change_count": layered.change_count(),
    "diff": layered.topology,         // existing TopologyDiff JSON
    "layered_summary": format!("{}", layered),  // Display string
});
```

After 159d:

```rust
let envelope = serde_json::json!({
    "schema_version": 2,
    "lab": lab_name,
    "no_op": layered.is_empty(),
    "change_count": layered.change_count(),
    "topology": layered.topology,        // typed (existing)
    "network": layered.network,          // typed (NEW)
    "nftables": layered.nftables,        // typed (NEW)
    "wireguard": layered.wireguard,      // typed (NEW; populated post-159a)

    // Deprecation: 159d ships v2 typed shape; v1 fields are
    // retained for one release for backwards compat.
    "diff": layered.topology,            // = .topology (alias for v1 consumers)
    "layered_summary": format!("{}", layered),  // deprecated
    "layered_summary_deprecated": true,
});
```

Alternative cleaner shape (recommend) — emit the
`#[derive(Serialize)] LayeredDiffEnvelope` struct directly:

```rust
#[derive(Serialize)]
struct LayeredDiffEnvelope<'a> {
    schema_version: u32,
    lab: &'a str,
    no_op: bool,
    change_count: usize,
    #[serde(flatten)]
    diff: &'a LayeredDiff,

    // Deprecated v1 mirror fields
    #[serde(rename = "diff")]
    legacy_topology: &'a TopologyDiff,
    layered_summary: String,
    layered_summary_deprecated: bool,
}
```

`#[serde(flatten)]` inlines the typed fields directly into the
envelope. `legacy_topology` is a renamed mirror of
`diff.topology` to preserve v1 path `.diff.nodes_added`.

### `docs/json-schemas/layered-diff.schema.json`

Bump `$id` to `.../layered-diff.schema.json` v2 (add `.v2` to
the URL or bump a version field).

New schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "https://nlink-lab.example/schemas/layered-diff.v2.schema.json",
  "title": "nlink-lab apply --check / --dry-run --json output (v2)",
  "description": "Typed v2 emitted by `nlink-lab apply --check --json` from nlink-lab >= 0.6.0. Replaces v1's `layered_summary: String` fallback with typed per-layer diffs.",
  "type": "object",
  "required": ["schema_version", "lab", "no_op", "change_count", "topology"],
  "properties": {
    "schema_version": { "const": 2 },
    "lab": { "type": "string" },
    "no_op": { "type": "boolean" },
    "change_count": { "type": "integer", "minimum": 0 },
    "topology": { "$ref": "#/definitions/TopologyDiff" },
    "network": {
      "type": "object",
      "additionalProperties": { "$ref": "#/definitions/ConfigDiff" }
    },
    "nftables": {
      "type": "object",
      "additionalProperties": { "$ref": "#/definitions/NftablesDiff" }
    },
    "wireguard": {
      "type": "object",
      "additionalProperties": { "$ref": "#/definitions/WireguardConfigDiff" }
    },
    "diff": {
      "description": "DEPRECATED — v1 mirror of `topology`. Removed in v3.",
      "$ref": "#/definitions/TopologyDiff"
    },
    "layered_summary": {
      "description": "DEPRECATED — v1 human-readable rendering. Use `topology`/`network`/`nftables`/`wireguard` typed fields directly. Removed in v3.",
      "type": "string"
    },
    "layered_summary_deprecated": { "const": true }
  },
  "definitions": {
    "TopologyDiff": { /* existing v1 shape, unchanged */ },
    "ConfigDiff": {
      "type": "object",
      "additionalProperties": true,
      "description": "nlink::ConfigDiff serialized — see https://docs.rs/nlink/0.19/nlink/netlink/config/struct.ConfigDiff.html",
      "properties": {
        "links_to_add": { "type": "array", "items": { "$ref": "#/definitions/DeclaredLink" } },
        "links_to_modify": { "type": "array" },
        "links_to_remove": { "type": "array" },
        "addresses_to_add": { "type": "array" },
        "addresses_to_remove": { "type": "array" },
        "routes_to_add": { "type": "array" },
        "routes_to_remove": { "type": "array" },
        "qdiscs_to_add": { "type": "array" },
        "qdiscs_to_remove": { "type": "array" }
      }
    },
    "NftablesDiff": {
      "type": "object",
      "additionalProperties": true,
      "description": "nlink::NftablesDiff serialized — see https://docs.rs/nlink/0.19/nlink/netlink/nftables/config/struct.NftablesDiff.html",
      "properties": {
        "tables_to_add": { "type": "array" },
        "tables_to_remove": { "type": "array" },
        "chains_to_add": { "type": "array" },
        "chains_to_remove": { "type": "array" },
        "rules_to_add": { "type": "array" },
        "rules_to_remove": { "type": "array" },
        "sets_to_add": { "type": "array" },
        "sets_to_remove": { "type": "array" },
        "elements_to_add": { "type": "array" },
        "elements_to_remove": { "type": "array" }
      }
    },
    "WireguardConfigDiff": {
      "type": "object",
      "additionalProperties": true,
      "description": "nlink::WireguardConfigDiff serialized — see https://docs.rs/nlink/0.19/nlink/netlink/genl/wireguard/config/struct.WireguardConfigDiff.html",
      "properties": {
        "devices_to_add": { "type": "array" },
        "devices_to_modify": { "type": "array" },
        "devices_to_remove": { "type": "array" }
      }
    },
    "DeclaredLink": {
      "type": "object",
      "properties": {
        "name": { "type": "string" },
        "kind": { "type": "string", "enum": ["dummy", "veth", "bridge", "vlan", "vxlan", "bond", "macvlan", "ipvlan", "vrf", "wireguard"] },
        "mtu": { "type": "integer", "nullable": true },
        "master": { "type": "string", "nullable": true }
      }
    }
  }
}
```

**Sub-type stability** — `ConfigDiff`'s inner shape is owned by
upstream nlink; we ship `additionalProperties: true` so the
schema doesn't break on upstream field additions. Downstream
consumers writing `jq` queries are advised to use defensive
defaults (`.links_to_add // []`).

### `crates/nlink-lab/src/bins/lab/src/main.rs` — schema docs

Add to `apply --help`:

```text
--check / --dry-run output (--json):
  Emits a typed v2 envelope per docs/json-schemas/layered-diff.v2.schema.json.
  Use `jq '.network.<node>.links_to_add[]'` to extract per-node typed deltas.
  The v1 `.diff` and `.layered_summary` fields are deprecated and
  will be removed in nlink-lab 0.7.0.
```

### CHANGELOG

```markdown
### Changed
- **JSON schema v2 for `apply --check --json` / `apply --dry-run --json`.**
  The envelope now emits typed per-layer diffs at `.network.<node>`,
  `.nftables.<node>`, `.wireguard.<node>` derived directly from
  nlink 0.19's serde derives (Plan 189 upstream). The v1
  `.layered_summary: string` (human-readable Display output) and
  `.diff` (TopologyDiff alias) fields are retained for one
  release as a deprecation period; both will be removed in
  nlink-lab 0.7.0. `schema_version: 2` is now an explicit envelope
  field. See `docs/json-schemas/layered-diff.v2.schema.json` and
  Plan 159d.
```

---

## Phases

### Phase 1 — feature toggle + audit

1. Audit `crates/nlink/Cargo.toml` for the exact serde feature
   name (`serde` vs `with-serde` vs included in `full`).
2. Confirm `Serialize` lands on all diff types via
   `cargo doc --no-deps -p nlink --features serde` →
   read the rustdoc.
3. Bump `Cargo.toml` workspace dep to include the feature if
   not already in `full`.
4. Verify build clean.
5. Write a smoke unit test:

   ```rust
   #[test]
   fn nlink_config_diff_serializes_under_serde_feature() {
       let diff = ConfigDiff::default();
       let json = serde_json::to_string(&diff).unwrap();
       assert!(json.contains("links_to_add"));
   }
   ```

### Phase 2 — `LayeredDiff::Serialize` + envelope rewrite

1. Add `#[derive(Serialize)]` to `LayeredDiff`. Make sure every
   field is `Serialize`-able. The four fields use HashMap, all
   inner types derived in Phase 1.
2. Define `LayeredDiffEnvelope<'a>` (or inline JSON build).
3. Replace the existing envelope build in `bins/lab/src/main.rs`.
4. Write the schema v2 JSON file
   (`docs/json-schemas/layered-diff.v2.schema.json`).
5. Keep the existing v1 schema file with a deprecation banner.
6. Unit test: `apply_check_json_includes_typed_network_field` —
   construct a `LayeredDiff` with a populated network HashMap,
   serialize, assert the JSON has `.network.<node>.links_to_add`.

### Phase 3 — integration tests + downstream guidance

1. Root-gated integration test:
   - `apply_check_json_schema_v2_matches_typed_layout` — run
     `apply --check --json` against a deployed lab, validate
     the output against the v2 JSON schema (with
     `jsonschema` crate or `cargo jsonschema-validate`).
   - `apply_check_json_v1_aliases_still_present_for_one_release`
     — assert `.diff` and `.layered_summary` are still in the
     output during the deprecation period.
2. Add a sample `jq` recipe to `docs/NLINK_LAB.md`:

   ```bash
   # Get every link to be added across the lab:
   nlink-lab apply --check --json my-lab \
     | jq -r '.network | to_entries[] | .key as $node | .value.links_to_add[] | "\($node): \(.name)"'
   ```

3. CHANGELOG entry under `[Unreleased]`.

### Phase 4 — v3 cleanup (next release, NOT this PR)

After one release of warning, delete `.diff` and
`.layered_summary` from the envelope. Bump schema to v3. Out
of scope for 159d; tracked here so future-me remembers.

---

## Deprecation policy

| Schema version | When | Shape |
|----------------|------|-------|
| **v1** | < 0.6.0 | `.diff` + `.layered_summary` only |
| **v2** | 0.6.0 (this plan) | adds `.topology`/`.network`/`.nftables`/`.wireguard`; keeps v1 fields with `"layered_summary_deprecated": true` marker |
| **v3** | 0.7.0+ | drops `.diff` + `.layered_summary`; pure typed shape |

The `schema_version` envelope field is the disambiguator —
consumers branch on it:

```bash
nlink-lab apply --check --json | jq '
  if .schema_version >= 2
  then .network
  else .layered_summary | "PARSE_REQUIRED"
  end
'
```

---

## Test plan

### Unit tests

- `nlink_config_diff_serializes_under_serde_feature` — Phase 1 smoke.
- `layered_diff_serialize_round_trip` — populate every layer with
  one entry; serialize; deserialize; assert structural equality.
- `layered_diff_serialize_skips_empty_layers` — empty layer
  HashMaps emit no key (via `skip_serializing_if`).
- `apply_check_json_envelope_includes_schema_version_2` — string
  search the rendered envelope.
- `apply_check_json_envelope_includes_v1_deprecation_marker` —
  v1 fields still present + `"layered_summary_deprecated": true`.

### JSON schema tests

- `layered_diff_v2_validates_against_schema` — round-trip a
  populated envelope through the schema validator.
- `layered_diff_v2_schema_documents_every_field` — every field
  in the envelope shape has a matching `properties` entry in
  the schema.
- (Optional) `layered_diff_v1_envelope_still_passes_v1_schema`
  — confirm v1 schema validators still pass our v2 output (the
  v1 fields haven't moved or changed types).

### CLI tests

- Add a CLI test that invokes `apply --check --json` on a
  deployed lab and parses the output through the v2 schema.

### Integration tests

- `apply_check_json_schema_v2_matches_typed_layout` — Phase 3.
- `apply_check_json_v1_aliases_still_present_for_one_release` —
  Phase 3.

---

## Risks

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| nlink's serde derive uses kebab-case; nlink-lab uses snake | Low | Medium | Plan 159d Phase 1 confirms; use `#[serde(rename = "...")]` on the LayeredDiff fields if needed (only for the wrapper; nlink's internal shape is downstream-of-nlink) |
| Downstream `jq` queries break unexpectedly | High for any external consumer | Medium | Deprecation period of one release; CHANGELOG callout; sample recipe in docs |
| Schema size growth (typed layers can be verbose) | Low — only on labs with deep diffs | Low | `skip_serializing_if = "HashMap::is_empty"` keeps the common no-op case tight |
| nlink's diff sub-fields churn between 0.19 and 0.20 | Medium — schema would break | Medium (upstream evolves) | `additionalProperties: true` on inner types; document upstream version compat in the schema description |
| `#[serde(flatten)]` for envelope creates field-name collisions | Low | Low — fields don't overlap | Build env structure manually if collision; unit test asserts no collision |

---

## Out of scope

- **Schema migration tool** — `nlink-lab schema-convert v1 v2`
  to upgrade old captures. Out of scope; deprecation period is
  the migration vector.
- **Wire-format negotiation** — `--json-schema v1` to opt back
  into the v1 shape. Out of scope; consumers branch on
  `schema_version`.
- **Strict mode** — `--json-strict` to reject the deprecated
  fields. Out of scope.
- **Hooking into Plan 159b `watch`** — `nlink-lab watch --json`
  uses NDJSON `WatchEvent` records, not `LayeredDiff`. Separate
  schema; not 159d's scope.

---

## Success criteria

- [ ] `apply --check --json` emits `{.schema_version: 2, .topology, .network, .nftables, .wireguard, …}`.
- [ ] `.network.<node>.links_to_add[].name` is a usable `jq` path.
- [ ] `.layered_summary` + `.diff` still present with
  `"layered_summary_deprecated": true` marker.
- [ ] `docs/json-schemas/layered-diff.v2.schema.json` ships
  alongside the v1 file.
- [ ] CHANGELOG documents the schema bump + deprecation.
- [ ] `docs/NLINK_LAB.md` has a `jq` recipe.
- [ ] All tests green.

---

## Cross-references

- [Plan 159 umbrella](159-nlink-0.19-adoption.md)
- Plan 158f Phase 2 (shipped — commits `4115099`, `4581be3`)
  — original `layered_summary` design
- [Plan 159a Phase 3](159a-declarative-vrf-wg-vxlan.md) — adds
  the `wireguard` field
- [`nlink-0.19-realignment.md`](../../nlink-0.19-realignment.md)
  — item #9 closure cited
- nlink 0.19 sources at `/home/mpardo/git/rip`:
  - `Cargo.toml` — `serde` feature
  - Plan 189 upstream — `Serialize` derives across diff types
