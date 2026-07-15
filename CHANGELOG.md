# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.7.0] - 2026-07-15

The "Plan 160 / nlink 0.25" arc. Adopts nlink `0.21 → 0.25` — four minor
versions of upstream correctness work, taken clean — and turns the new
declarative/ergonomic APIs into three long-standing "pending" closeouts:
WireGuard is now **fully declarative** (the imperative device pre-create is
gone, bootstrapped via `WireguardConfig::ensure_devices` — closes feedback item
#3 against 0.19); rate limits **reconcile** instead of rebuild
(`RateLimiter::reconcile`, closing Plan 158g); and teardown/removal paths move to
the typed `del_*_if_exists` family. Deploy now **self-heals a stale namespace
marker** left by an unclean shutdown (`namespace::is_namespace`). Two new
observability wins: `watch` shows per-rule nftables counters, and live metrics
gain a **per-process TCP bandwidth** dimension (sockdiag goodput + process
attribution). The bump alone also fixes latent runtime bugs nlink-lab could not
reach — **traffic shaping is now correct** (psched/HTB/TBF tick fixes, previously
15–125× off) and **firewall rules install in declared order** (the reversed
insertion is fixed upstream).

**Migration impact for 0.6.0 → 0.7.0 upgraders:**

- **Shaping numbers change — to the correct value.** Impairment RTT/loss and
  rate-limit caps on existing labs were silently mis-programmed by nlink's old
  psched-tick math; they now match what the topology declares. Re-check any
  hand-tuned burst you set to *compensate* for the old behaviour.
- **Firewall rule order is now honored.** If a lab "worked" by accident under
  the old reversed insertion, its policy changes; rules now take effect in the
  order written (first-match-wins).
- **`apply --check --json` is schema v3 (breaking for `jq` consumers of the v1
  fields).** The v1 `diff`, `layered_summary`, and `layered_summary_deprecated`
  fields — deprecated in 0.6.0 for a one-release window — are **removed**. Branch
  on `schema_version == 3` and read the typed `network` / `nftables` maps. The
  human-readable `apply --check` output is unchanged.

### Removed
- **v1 `apply --check --json` fields (schema v3).** `diff`, `layered_summary`,
  and `layered_summary_deprecated` are gone from the `--check --json` /
  `--dry-run --json` envelope; `schema_version` is now `3`. This lands the
  one-release deprecation announced in 0.6.0. The typed per-namespace `network`
  (`ConfigDiff`) and `nftables` (`NftablesDiff`) maps — the v2 replacement — plus
  `schema_version` / `lab` / `no_op` / `change_count` are unchanged, and the
  human-readable path never used the removed fields.

### Added
- **Per-process TCP bandwidth in live metrics (nlink 0.24 sockdiag).**
  The backend metrics collector now dumps each bare-namespace node's
  TCP sockets (`Connection<SockDiag>` in the node's netns), diffs
  consecutive dumps with a per-node `SocketRateTracker` for goodput,
  and attributes each flow to its owning process via one amortized
  `SocketOwnerMap` `/proc` walk. The top flows per node ride on a new
  `NodeMetrics.sockets` field (`SocketRateMetric`: comm/pid,
  local/remote, tx/rx goodput, retransmit ratio) — `#[serde(default)]`
  for wire-compat — and render in the `metrics` stream. Container nodes
  and the one-shot `stats` snapshot report none (goodput needs two
  samples over time).
- **`watch` nftables rule drift shows per-rule counters.** `NewRule`
  events decode the typed `(packets, bytes)` counter via nlink 0.24's
  `RuleInfo::counter()` and render `counter pkts=… bytes=…` (and a
  `counter` JSON field) when the rule carries one. (The `Store`/
  reflector watch-cache was evaluated and deferred — the `watch`
  command is a pure per-drift printer with no snapshot consumer, so a
  cache would add bookkeeping nothing reads.)

### Changed
- **Rate limits now reconcile instead of rebuild (closes Plan 158g).**
  Deploy step 15 and the live `apply --check`/`apply` rate-limit path
  use `RateLimiter::reconcile` (nlink 0.24) instead of the destructive
  `RateLimiter::apply` (delete-root-qdisc-then-rebuild). An unchanged
  rate limit now makes zero kernel calls on re-deploy, and an
  egress/ingress edit mutates only the drifted class instead of tearing
  down the whole HTB tree — no packet-drop window. The removal path uses
  `del_qdisc_if_exists`.
- **Deploy self-heals a stale namespace marker (nlink 0.25).** The
  bare-namespace pre-create guard now rejects only a *live* namespace
  (`namespace::is_namespace`, an nsfs bind-mount check). A leftover
  `/var/run/netns/<name>` marker with no live mount — the residue of an
  unclean shutdown — is cleared and the deploy proceeds, instead of
  hard-failing "already exists" and forcing a manual
  `destroy --orphans`. Destroy/removal paths keep the plain `exists`
  check so they still sweep stale markers.
- **Teardown/removal paths use `del_*_if_exists` (nlink 0.24).**
  `clear_impairment`, mgmt veth/bridge teardown, `apply_diff` link
  removal, and static-route removal now use the typed
  `del_qdisc/link/route_*_if_exists` helpers instead of hand-rolled
  `QdiscNotFound` matching or `let _ = del_*` error swallowing —
  already-absent resources are `Ok(false)` and genuine failures are
  surfaced (logged on best-effort paths, propagated otherwise).
- **WireGuard deployment is now fully declarative.** The imperative
  `add_link(WireguardLink::new(...))` pre-create loop (deploy step
  6c, and its mirror in the live-reconcile `apply_diff` path) is
  gone. The WG interface is bootstrapped by
  `WireguardConfig::ensure_devices` (nlink 0.24 #169) inside
  `apply_stack_for_node`, before the `NetworkConfig` apply so the
  tunnel addresses land on an existing link. Idempotent, so
  re-deploys create nothing. Closes the long-standing "feedback
  item #3 against 0.19".
- **Bumped workspace `nlink` dep `0.21` → `0.25`.** Four minor
  versions of upstream correctness work, adopted clean — no
  nlink-lab-side compile change (verified: the sole `NamespaceGuard`
  is confined to a synchronous capture thread so 0.25's `!Send`
  guard is a non-issue; the `Chain::new` `Result`, `LinkStats`
  accessor, and `#[non_exhaustive]` diff/nl80211/sockdiag changes
  touch no nlink-lab call site). nlink-lab's value path was already
  immune to 0.25's silent tc-string unit changes (`mbps`→bytes,
  bare-number→µs) because it hands nlink only typed
  `Rate::bits_per_sec` / `Duration` / `Percent::new`, never a
  tc-string.

  **Behaviour-visible upstream fixes now in effect (no config
  change needed):**
  - **Traffic shaping is now correct.** nlink 0.25 fixed the
    psched-tick conversion for HTB/TBF token buckets (#191–#218);
    previously every `RateLimiter` and `PerPeerImpairer` shape was
    mis-programmed by 15.6–125×. Impairments and rate-limits now
    shape at the configured rate for the first time. Measured
    RTT/rate on existing labs will change — to the correct value.
  - **Firewall rules install in declared order.** nlink 0.25 fixed
    the reversed rule insertion (#195, first-match-wins was
    inverted). nlink-lab emits rules in written order and does not
    compensate, so ordered `accept`/`drop` policies now take effect
    as written. `reject` really rejects (#205), and the flower
    filters behind per-pair impairments now match IPv6/ARP/VLAN too
    (#201).

## [0.6.0] - 2026-06-08

The "159 arc" release. Closes the seven-plan declarative-netlink
adoption arc against nlink 0.19/0.20/0.21 — every netlink resource
nlink-lab models (VRF, VXLAN, WireGuard, bridges, dummies, bonds,
VLANs, addresses, routes, qdiscs, nftables) now commits through
upstream declarative reconcile paths. New `nlink-lab watch <lab>`
power-user CLI surfaces RTNETLINK + nftables drift across every
node (bare namespaces AND containers) with rich event detail. JSON
schema v2 for `apply --check --json` exposes typed per-namespace
diffs.

**Migration impact for upgraders from 0.5.0 with existing labs:**
The first `nlink-lab apply` on a lab deployed pre-0.20 nlink will
diff non-empty for nftables — upstream nlink 0.20 fixed phantom-diff
bugs (matchers that didn't byte-compare against the kernel's
canonical dump form), so the in-kernel rules get rewritten with the
canonical attributes on first reapply. Subsequent reapplies converge
to zero. Fresh deploys are unaffected.

The JSON schema v2 for `apply --check --json` is additive — v1
fields (`.diff`, `.layered_summary`) are retained for one release
with `"layered_summary_deprecated": true`; both are removed in 0.7.

### Changed
- **Bumped workspace `netring` dep `0.11` → `0.18`.** Seven minor
  versions worth of API growth — none of the breaking changes
  (`ProtocolEvent` variants, `Severity::Default`, flowscope 0.10
  bumps) hit nlink-lab's surface, since we only consume the
  capture-path types (`Capture`, `BpfFilter`, `BpfInsn`,
  `RingProfile`, `CaptureStats`, `Timestamp`). Compiled clean
  with no nlink-lab-side migration.

  New CLI capability adopted from the bump: `nlink-lab capture
  --filter-ports 80,443,8080` / `--filter-src-ports` /
  `--filter-dst-ports`. Backed by netring 0.16's
  `BpfFilter::builder::ports()` multi-port shortcut — compiles
  to one BPF branch per port, atomic in-kernel. Pre-bump users
  had to capture all traffic and filter offline if they cared
  about more than one port. Two new unit tests
  (`capture_config_accepts_multi_port_bpf_filter`,
  `capture_config_accepts_icmp_chain_filter`) smoke-test the
  typed-builder integration boundary.

  Not-yet-adopted 0.12–0.18 capabilities (no current need):

  - **0.18 unified-driver `ProtocolMonitor`** — multi-protocol
    L7 monitor sharing one capture across N parsers (collapses
    5×tpacket_v3 rings to one). nlink-lab doesn't currently
    expose protocol-aware capture.
  - **0.15 `StreamSetFilter`** — atomic in-kernel BPF swap on
    a built stream. Useful for long-running monitors that
    change focus; nlink-lab's capture is one-shot.
  - **0.14 per-parser `on_tick`** — flowscope detector lifecycle
    hook. No detectors yet.
  - **0.13 async-stream maturity** — observability + offline
    replay primitives. Future work.
  - **0.12 typed XDP loader** — XDP packet capture path. nlink-lab
    uses tpacket_v3 (AF_PACKET) which is simpler and works inside
    namespaces.

- **Bumped workspace `nlink` dep `0.19` → `0.21`.** The 0.20 emergency
  release shipped critical wire-format fixes — pre-0.20 nlink had two
  classes of bugs that nlink-lab silently inherited:

  - **`Connection<Xfrm>::flush_sp` flushed all SAs instead of policies**
    (XFRM constants were miscounted from the kernel UAPI enum). Not
    a path nlink-lab uses today, but worth noting that anyone
    running nlink-lab pre-0.20 on a kernel with XFRM userspace
    would have seen incorrect behavior.
  - **`NftablesConfig::diff` had latent phantom-diff bugs** —
    several matcher writers emitted shapes that didn't byte-compare
    against the kernel's canonical dump form, so every reapply
    re-emitted the rule. nlink-lab's `apply_nftables_for_node`
    reconcile path uses `NftablesConfig::diff().apply_reconcile()`
    heavily, so this was hitting us on every redeploy.
    **Migration impact:** the first reapply after the bump on an
    EXISTING running lab will diff non-empty as the kernel rules
    get rewritten with the canonical form. Subsequent reapplies
    converge to zero. Fresh deploys (the CI default) are
    unaffected.
  - **`Verdict::Jump` / `Verdict::Goto` were emitting `NFT_BREAK = -2`**
    (already fixed in 0.19 — flagged in `nlink-feedback.md` as
    transitively benefiting nlink-lab's firewall chains).

  The 0.20.1 and 0.21 cycles also tightened the typed-API surface
  (`AddressFamily`, `Percent`, `ChainName`, `Verdict::JumpTo` /
  `GotoTo`, `RuleMessage` and 5 sibling `*Message` types per-field
  accessors). nlink-lab already adopted `Percent` in `build_netem`,
  uses RTNETLINK `*Message` types through accessor methods only
  (e.g. `watch::WatchEventKind::from_network` calls `lm.ifindex()`),
  and doesn't expose raw `Verdict` or raw-`u8` `AddressFamily`
  through its public API — so the bump compiled and tested clean
  with no nlink-lab-side code changes required.

  New 0.21 capabilities not yet adopted but available:
  - **Plan 234 — `Connection<P>::dispatcher()`** for broadcast-side
    `Arc<Connection>` sharing. Could allow multiplexing watch
    subscriptions on one connection per node. Not adopted yet —
    our current per-(node, family) connection-per-subscription
    model works fine for typical lab scale.
  - **Plan 197 — Declarative `OvpnConfig`** for OpenVPN data-
    channel offload. No nlink-lab NLL surface for OpenVPN
    currently; future work if there's demand.
  - **Plan 228 extension** — declarative `QdiscBuilder` netem parity
    setters (`duplicate_pct`, `corrupt_pct`, `reorder_pct`,
    `loss_correlation_pct`, `delay_correlation_pct`). nlink-lab
    uses the imperative `NetemConfig` (which already had these),
    so no migration needed.

### Added
- **`nlink-lab watch` now surfaces enriched RTNETLINK metadata
  across every event variant.** Two-commit follow-on (`06a31b1`
  + `4e08d2e`) to the 0.21 bump that lifts the additional
  accessors that have been on `LinkMessage` / `AddressMessage` /
  `RouteMessage` / `NeighborMessage` / `TcMessage` since 0.19+
  but that the watch loop wasn't reading.

  Per-variant new fields:
  - `NewLink` / `DelLink`: `link_kind` (`vrf`/`vxlan`/
    `wireguard`/`bond`/...), `operstate`, `master`
    (enslave-parent ifindex).
  - `NewAddress` / `DelAddress`: `cidr` (full
    `address/prefix_len`), `scope`.
  - `NewRoute` / `DelRoute`: `dst` (CIDR, or `"default"` for
    the default route — matches `ip route show`), `gateway`,
    `oif`, `table` (the routing table id — surfaces VRF
    routes by their custom table).
  - `NewNeighbor` / `DelNeighbor`: `ifindex`, `dst` (IP),
    `lladdr` (MAC), `state` (`Reachable`/`Stale`/`Failed`/...).
  - `NewFdb` / `DelFdb`: `ifindex`, `lladdr` (MAC, via
    `FdbEntry::mac_str()`).
  - `NewQdisc` / `DelQdisc`: `ifindex`, `handle` (in
    `major:minor` hex form), `tc_kind` (`htb` / `netem` / ...).
  - `NewClass` / `DelClass`: `ifindex`, `handle`, `parent`.
  - `NewFilter` / `DelFilter`: `ifindex`, `handle`, `parent`,
    `tc_kind` (e.g. `flower` for our per-pair impairers).

  All new fields are `Option<>` with serde
  `skip_serializing_if = "Option::is_none"`, so the JSON
  envelope only carries fields the kernel actually emitted —
  consumers reading the 0.19-era 3-field shape (`ifindex`,
  `name`, `mtu` on link events; bare `NewRoute,` for route
  events) keep working unchanged. Backed by nlink 0.21's
  existing `*Message` / `FdbEntry` accessor surface — no new
  upstream APIs needed; just using accessors that already
  shipped.

  Field-name caveat: the new `link_kind` field on
  `NewLink` / `DelLink` is spelled `link_kind` rather than
  `kind` because the `WatchEventKind` enum is tagged with
  `#[serde(tag = "kind")]` — calling the field `kind` would
  collide. Same rationale for `tc_kind` on qdisc/filter
  variants.

  **Nine new unit tests** cover the enriched lifting + the
  JSON elision contract (`new_link_renders_and_serializes_enriched_fields`,
  `new_link_elides_unset_enriched_fields_in_json`,
  `new_address_renders_cidr_when_set`,
  `new_route_renders_dst_default_for_default_route`,
  `new_route_serializes_table_for_vrf_routes`,
  `new_neighbor_renders_ip_mac_state`,
  `new_qdisc_renders_handle_and_kind`,
  `new_filter_renders_parent_and_kind`).
- **WireGuard `fwmark` keyword in NLL.** Surfaces nlink 0.19's
  `DeclaredWgDeviceBuilder::fwmark` so policy-routing setups can
  match WG-encapsulated outbound traffic by routing mark. Syntax:
  `wireguard wg0 { ... fwmark 100 ... }`. Per-peer `preshared_key`
  remains deferred — needs a broader parser refactor since the
  existing `peers [a, b]` shape is a flat `Vec<String>`.
- **Watch CLI now covers container nodes (Plan 159b Phase 4).** New
  `NsResolver` enum (re-exported as `nlink_lab::NsResolver`) bundles
  both name-based (bare namespace) and pid-based (container)
  namespace resolution. The watch loop and ENOBUFS resync factory
  closures both branch through `NsResolver::open_route` /
  `open_nftables`, so `nlink-lab watch <lab>` now tails events from
  every node in the lab regardless of whether the node is a bare
  namespace or a container. The previous "skipping watch for
  container node" warning is gone.
- **VXLAN `underlay` keyword in NLL.** Pins the VXLAN tunnel to a
  specific underlay device via nlink 0.19's
  `LinkBuilder::vxlan_underlay_dev` (upstream Plan 190 §2.1). NLL
  syntax: `vxlan vxlan100 { vni 100; underlay eth0; ... }`. Plumbing
  through AST/parser/interpolator/lower and into the declarative
  step 11c via `b.vxlan_underlay_dev(u)`.
- **`nlink-lab watch <lab> --node <name>` + `--include-snapshot`
  (Plan 159b Phase 3).** Pre-subscription node filter — saves opening
  connections we don't need — and an opt-in flag to render resync
  replay frames (default-silenced) with a `[snapshot]` marker.
- **3 root-gated integration tests for Plan 159a** —
  `slice4_vrf_reapply_is_zero_ops`, `slice4_vxlan_reapply_is_zero_ops`,
  `wireguard_config_reapply_is_zero_ops`. Skip cleanly when the
  kernel `vrf`/`wireguard` modules or the `wg` userspace binary are
  unavailable, so CI runners without them don't fail.
- **`nlink-lab watch <lab>` — kernel-event tail (Plan 159b).**
  Subscribes to nftables + RTNETLINK multicast on every node in
  the running lab and emits one line per kernel mutation. Powered
  by 0.19's `Connection<Route>::subscribe_all_with_resync` +
  `Connection<Nftables>::subscribe_all_with_resync`. Supports
  `--family route|nftables|both` (default `both`) and `--json`
  for NDJSON output. Container nodes are skipped in this phase
  (bare-namespace nodes only); follow-up to extend to
  `/proc/<pid>/ns/net` resolution. Public API:
  `nlink_lab::WatchEvent`, `WatchEventKind`, `WatchFamily`,
  `WatchOpts`, `watch_loop`.
- **Declarative VRF + VXLAN in deploy step 11c (Plan 159a
  Slice 4).** `topology_to_network_config` now emits VRF links
  via `LinkBuilder::vrf(table)` and VXLAN links via
  `LinkBuilder::vxlan + vxlan_local + vxlan_remote + vxlan_port`
  (0.19 upstream Plan 190 §2.1/§2.3). Step 6's VXLAN branch +
  step 6b VRF block + step 10c VRF enslave block become no-op
  markers; re-applies on unchanged topologies make zero kernel
  calls on these layers. Five new unit tests:
  `network_config_vrf_declares_link_with_table`,
  `network_config_vrf_master_enslave_after_vrf_link`,
  `network_config_vxlan_declares_with_local_remote_port`,
  `network_config_vxlan_missing_vni_errors`,
  `network_config_vxlan_bad_local_addr_errors`.
- **Declarative WireGuard in step 10d (Plan 159a Phase 2).**
  Three new helpers replace the two-pass imperative
  `wg_conn.set_device(...)` loops: `build_wg_public_key_map`
  (sync key resolution, returns `(private, public)` per WG
  iface), `topology_to_wireguard_config` (builds a
  `WireguardConfig` from NLL), and `apply_wireguard_for_node`
  (calls `apply_reconcile` per node). Step 10d's body is now
  one declarative apply per node with peer cross-references
  resolved up-front. Four new unit tests:
  `build_wg_public_key_map_decodes_explicit_key`,
  `build_wg_public_key_map_bad_key_errors`,
  `topology_to_wireguard_config_declares_devices_and_peers`,
  `topology_to_wireguard_config_unknown_peer_node_errors`.
- **`apply_stack_for_node` per-node orchestrator (Plan 159c).**
  Bundles `apply_network_config_for_node`,
  `apply_nftables_for_node`, and `apply_wireguard_for_node`
  into a single per-node call site with one unified
  `tracing::info!`. Mirrors upstream `facade::Stack::apply`
  shape but routes through `NodeHandle::connection<P>()` so the
  container case (`connection_for_pid`) keeps working alongside
  bare namespaces — upstream's `Stack::apply_in_namespace(&str)`
  is name-based only. Collapsed three separate per-node loops
  in `deploy()` into one.

### Changed
- **`apply --check --json` / `apply --dry-run --json` ships
  schema v2 (Plan 159d).** The envelope now emits typed
  per-namespace `network` (upstream `ConfigDiff` under
  `nlink/serde`) and `nftables` (upstream `NftablesDiff`)
  fields. `schema_version: 2` is an explicit envelope field;
  downstream `jq` consumers should branch on it. The v1 fields
  `.diff` (alias of topology) and `.layered_summary` (Display
  output) are retained for one release as a deprecation period,
  marked by `"layered_summary_deprecated": true`. Both will be
  removed in schema v3. See
  `docs/json-schemas/layered-diff.v2.schema.json` for the new
  shape and `docs/json-schemas/layered-diff.schema.json` for
  the v1 reference still kept alongside.
- **`Error::ext_ack` / `errno` / `ext_ack_offset` refactored
  onto `nlink::Error::root_cause` (Plan 159f).** Three
  hand-rolled `downcast_ref` loops collapsed to one private
  `first_nlink_error` helper + three one-liners. Behavior
  unchanged for current variants; defeats the
  `Box<nlink::Error>` source-downcast trap described in
  `nlink-feedback.md` item #4 if we ever box a wrapper source
  in the future. Two new tests:
  `root_cause_drills_through_nlink_chain_to_kernel_layer`,
  `root_cause_drills_through_namespace_variant`.
- **Bumped workspace `nlink` dep `0.18` → `0.19`.** The 0.19
  release closes 14 of the 16 numbered items, 4 of the 9
  wishlist items, and all 6 documentation suggestions from
  `nlink-feedback.md` (2026-05-30). The bump itself required
  only two test-assertion flips in
  `crates/nlink-lab/src/error.rs` (`Error::from_errno*` now
  normalizes errno via `.abs()` per upstream Plan 187 §2.1, so
  `errno()` returns the positive errno regardless of input
  sign) and silencing a new `#[must_use]` warning on
  `PerPeerImpairer::reconcile`'s `ReconcileReport` return.
  No other call site changed. The breaking changes in 0.19
  (`ApplyOptions::with_purge` removed, `Hook::Ingress` split,
  `NatExpr.addr` enum, `Connection<P>::events()` async,
  `subscribe` family `&mut self` → `&self`) all targeted call
  sites nlink-lab does not use. Silent-corruption fixes that
  nlink-lab transitively benefits from include the TC filter
  `tcm_info` packing fix (flower filter protocol field was
  silently wrong), `Verdict::Jump`/`Goto` constants (pre-0.19
  emitted `NFT_BREAK = -2` instead of `NFT_JUMP = -3` /
  `NFT_GOTO = -4`, silently terminating instead of jumping),
  IPv6 NAT register drop (PR #6), F1 `Connection<P>` request
  lock (concurrent dumps on shared `Arc<Connection>`), and N1
  `namespace::create` thread-bleed. See
  `nlink-0.19-realignment.md` for the full closeout +
  follow-up work list (Plan 158d RTNETLINK side, Plan 158e
  Slice 4 declarative VRF/WG/Vxlan, `facade::Stack` adoption,
  `serde` derive on `LayeredDiff`, `chain_walk`-based source
  walk).

### Library API breaks
- **Plan 158b — `Error::Namespace` now carries
  `#[source] source: nlink::Error`** instead of
  `detail: String`. Match arms that destructured the old
  `{ op, ns, detail }` shape need to switch to
  `{ op, ns, source, .. }`. The old detail string is recoverable
  via `source.to_string()`, but the typed shape lets new accessors
  walk the source chain to surface kernel
  `NLMSGERR_ATTR_MSG` text.
- **Removed four dead error variants** that were declared in
  `nlink_lab::Error` but never constructed anywhere in nlink-lab
  or downstream code: `Firewall`, `Route`, `NetlinkOp`,
  `Container`. They were leftover from an earlier
  per-resource error taxonomy; the actual wrapping pattern uses
  `Error::deploy_failed(format!(...))` instead. If you matched
  on them in downstream code, replace with a wildcard or
  `Error::DeployFailed`.

### Added
- **`impl From<std::net::AddrParseError> for Error`** and
  **`impl From<std::num::ParseIntError> for Error`** route into
  `Error::InvalidTopology`. The bare `?` operator works on
  `IpAddr::parse()` / `u32::parse()` etc. in any fn returning
  `Result<_, nlink_lab::Error>`, removing
  `.map_err(|e| Error::invalid_topology(format!(...)))` ceremony
  at identity wrap sites. Plan 158c.
- **JSON error envelope on `--json` paths** (Plan 158b Phase 3 —
  `bins/lab/src/main.rs`). When `--json` is in effect, terminal
  errors render to stderr as a structured envelope: `{ error,
  error_chain, errno, ext_ack, ext_ack_offset }`. `error_chain`
  walks `std::error::Error::source` from the top-level error
  down. `errno` / `ext_ack` / `ext_ack_offset` surface kernel
  detail when an `nlink::Error::Kernel` / `KernelWithContext` is
  anywhere in the chain. NLL parse-diagnostic errors keep their
  miette renderer regardless of `--json`.
- **`compute_layered_diff(running, desired) -> Result<LayeredDiff>`**
  public async helper (`nlink_lab::compute_layered_diff`). Walks
  every node in the desired topology, opens per-node `Connection<Route>`
  and `Connection<Nftables>` connections, builds the same
  `NetworkConfig` / `NftablesConfig` the deploy uses, and calls
  upstream `diff()` against the live state. Returns the bundled
  `LayeredDiff` covering all three layers. Cost is one dump
  round-trip per (node, protocol family); only used on
  `apply --check` / `apply --dry-run` paths so normal apply stays
  cheap. Plan 158f Phase 2.
- **`nlink-lab apply --check` and `apply --dry-run` now render
  the layered diff** (lab graph + per-namespace RTNETLINK + per-
  namespace nftables) instead of the TopologyDiff-only view.
  `--check` exits non-zero on layered-level drift, so drift in
  the nftables or RTNETLINK layers that previously slipped past
  `apply --check` (because `TopologyDiff` doesn't model rule-level
  changes) is now caught. The `--json --dry-run` envelope grows
  a `layered_summary` string field carrying the rendered diff. Plan
  158f Phase 2.
- **`LayeredDiff` struct + `Display` impl** (`nlink_lab::diff::LayeredDiff`).
  Bundles the three layers an `apply` call commits against: the
  lab-graph topology, per-namespace RTNETLINK state (links + addresses
  + routes + qdiscs), and per-namespace nftables state. The `Display`
  impl delegates to `TopologyDiff::Display` for the lab-graph diff and
  to `nlink::ConfigDiff`'s / `NftablesDiff`'s upstream `Display` impls
  (Plan 183 in nlink 0.18) for the kernel-resource diffs. Renders each
  non-empty subdiff under its own section header; falls back to "no
  changes" when everything is empty. `is_empty()` and `change_count()`
  aggregate across all three layers. Plan 158f.
- **`Error::ext_ack() -> Option<&str>`,
  `Error::ext_ack_offset() -> Option<u32>`,
  `Error::errno() -> Option<i32>`** inherent accessors on
  `nlink_lab::Error`. They walk the source chain via
  [`std::error::Error::source`], so the kernel's
  `NLMSGERR_ATTR_MSG` payload is reachable from any wrapper
  variant whose `#[source]` ultimately points at an
  `nlink::Error::Kernel` / `KernelWithContext`. Mirrors nlink
  0.18's shape (Plan 182) but routes through nlink-lab's
  wrapper enum.

### Changed
- **Plan 158e Slice 3 — VLAN sub-interfaces now declare via
  `LinkBuilder::vlan(parent, vid)`** in the per-namespace
  NetworkConfig. The imperative branch in step 6 is a no-op
  marker. Vxlan stays imperative — upstream `LinkBuilder` lacks
  the `local` / `port` setters our existing topology shape
  supports.
- **Plan 158e Slice 2 — dummy + bond interface creation (and
  bond member enslave) now go through the declarative
  `NetworkConfig`** built by `topology_to_network_config`. Step
  6's dummy + bond branches and step 10b (bond enslave) are now
  no-op markers; the actual link declarations live in the
  per-namespace apply at step 11c. Re-deploys are idempotent for
  these kinds too (NetworkConfig diff sees the live link and
  emits nothing). Vlan/Vxlan/macvlan/ipvlan/VRF/WG stay
  imperative — Slice 3+ candidates.
- **Plan 158c — default routes now use
  `nlink::Ipv4Route::default_route()` / `Ipv6Route::default_route()`**
  (Plan 184 in nlink 0.18) at the four call sites in `deploy.rs`
  that previously wrote `Ipv4Route::new("0.0.0.0", 0)` /
  `Ipv6Route::new("::", 0)`. Purely cosmetic — the on-wire shape
  is identical — but self-documenting.
- **Plan 158c — `parse_v4_cidr` now returns
  `Result<(Ipv4Addr, u8), Error>`** instead of
  `Result<…, String>`. Internal helper; affected call sites
  preserve their existing context wrappers (`Error::deploy_failed`
  / `Error::invalid_topology`) without change beyond the `e`
  binding becoming `Error` instead of `String` (`Display`
  output is identical).
- **Plan 158e Slice 1 — interface addresses and routes now apply
  declaratively via `nlink::NetworkConfig::apply()`** instead of the
  previous per-source imperative loops. The new step 11c
  (`apply_network_config_for_node`) consumes per-link, per-
  node, network-port, WireGuard, macvlan/ipvlan, and WiFi
  address sources plus manual + auto-generated routes into one
  per-namespace `NetworkConfig`, then calls
  `cfg.apply(&conn)` which computes a `ConfigDiff` and applies
  only the deltas. Idempotent re-deploys make zero kernel
  mutations for the address + route layer. Imperative steps 9
  (addresses) and 12 (routes) are now no-op markers.
  VRF routes (step 12b) remain imperative — `RouteBuilder` does
  not yet expose every VRF table knob.
- **Plan 158a — nftables firewall + NAT now reconcile per-rule
  via `nlink::NftablesConfig::diff().apply_reconcile()`** instead
  of the previous "delete the whole table, rebuild from scratch"
  approach. Both firewall and NAT live in the unified `nlink-lab`
  table and apply as one atomic kernel batch per node. Editing a
  single rule no longer rebuilds the chain; idempotent re-apply
  on an unchanged topology makes zero kernel mutations.
  Per-rule USERDATA keys (`nlink-lab/{fw,nat}/<chain>/<idx>...`,
  auto-prefixed with `nlink:` by the library) drive the diff so
  foreign rules added via `nlink-lab exec NODE -- nft -f ...`
  survive subsequent applies. Closes the TODO that has lived at
  `crates/nlink-lab/src/deploy.rs:2906` since Plan 152.

## [0.5.0] - 2026-05-10

Plan 156 release — `nlink-lab capture` no longer needs `tcpdump`
or `libpcap` at runtime. netring 0.11.0 ships the typed
`BpfFilter::builder()` primitive (proposed in Plan 156a, adopted
upstream); nlink-lab now uses it directly and exposes typed
`--filter-*` CLI flags.

**Library API breaks** (relevant for direct library consumers):
- `nlink_lab::capture::CaptureConfig::bpf_filter` is now
  `Option<netring::BpfFilter>` (was `Option<Vec<netring::BpfInsn>>`).
  Build via `netring::BpfFilter::builder().tcp().dst_port(80).build()?`.
- `nlink_lab::capture::compile_bpf_filter` now returns
  `Result<netring::BpfFilter>` and is gated behind the new
  `legacy-tcpdump-filter` Cargo feature (off by default).

### Added
- `nlink-lab capture --filter-tcp / --filter-udp / --filter-icmp /
  --filter-ip-proto / --filter-ipv4 / --filter-ipv6 / --filter-arp /
  --filter-vlan / --filter-vlan-id / --filter-host / --filter-src-host /
  --filter-dst-host / --filter-net / --filter-src-net /
  --filter-dst-net / --filter-port / --filter-src-port /
  --filter-dst-port / --filter-not` — typed BPF filter flags backed
  by `netring::BpfFilter::builder()`. Compose with implicit AND.
  Pure-Rust compilation; no `tcpdump`/`libpcap` runtime dependency.
  (Plan 156 — round-2 of the C-dependency audit)

### Changed
- nlink-lab no longer shells out to `tcpdump -dd` on the default
  capture path. The legacy `--filter "<tcpdump expr>"` flag still
  exists for parsing-compat, but default builds reject it at parse
  time with a migration suggestion pointing at the new typed
  `--filter-*` flags. Build the CLI with `--features
  legacy-tcpdump-filter` to opt back in to the shell-out behaviour
  (requires `tcpdump` on PATH at runtime). New
  `nlink-lab/legacy-tcpdump-filter` and
  `nlink-lab-cli/legacy-tcpdump-filter` Cargo features (both off
  by default).
- Bumped `netring` workspace dependency to `0.11` for the typed
  `BpfFilter::builder()` API. (Same surface that nlink-lab proposed
  to the netring team in Plan 156a; that proposal landed in
  netring 0.11.0.)

## [0.4.1] - 2026-05-06

Patch release. One bug fix.

### Fixed
- `nlink-lab proc-stat`'s `fd_count` always reported `0` regardless
  of the number of file descriptors actually open. The internal
  implementation exec'd `sh -c "ls /proc/<pid>/fd 2>/dev/null | wc
  -l"` — when `ls` failed (any cause, including the SUID-install
  euid-demotion path that `bash`/`dash` apply when ruid != euid),
  the `2>/dev/null` swallowed the error, `wc -l` read empty stdin
  and emitted `0`, and the trim+parse cleanly returned 0 instead
  of erroring. Direct `ls` exec now (no shell wrapper); errors
  propagate. New `running::count_fd_dir` shared helper. Same bug
  was present in `wait_for_fd_stable` (PR G's heuristic probe);
  fixed alongside. The existing `proc_stat_returns_live_data`
  integration test now asserts `fd_count >= 3` (every spawned
  process inherits stdin/stdout/stderr from `spawn_with_logs`),
  catching the regression. (Round-5 follow-up — same-day report
  on 0.4.0)

The "round-5 wishlist" release — nine PRs from Plan 157 addressing
every item in the harness team's wishlist. New `proc-stat` primitive,
capture rotation, `--wait-port`/`--wait-fd-stable`, `subnet auto/N`
allocator, loopback dedup, `host_pid` alias, fixed parallel-deploy
`/etc/hosts` race, namespace-model docs, harness writer guide.

**Library API breaks** (relevant for direct library consumers):
- `nlink_lab::capture::run_capture` now takes a `CaptureOutput`
  enum instead of `Option<W>`. Construct via
  `CaptureOutput::pcap(path)?` for the simple case.
- `nlink_lab::ProcessInfo` gains a public `host_pid: u32` field.
  Code that constructs `ProcessInfo` directly needs to populate it.

### Added
- `nlink-lab proc-stat <LAB> <NODE> <PID> [--json] [--watch SECS]` —
  single primitive for sampling a spawned process's resource usage.
  Reads `/proc/<pid>/{stat,status}` and `/proc/<pid>/fd/` from inside
  the target namespace via `nlink-lab exec`, so the
  `/proc/<pid>/fd/` permission gymnastics (mode 0700, root-owned)
  go away. `--watch` emits NDJSON at the given interval until
  Ctrl-C. New library API `RunningLab::proc_stat(node, pid)` and
  pure parser `nlink_lab::proc_stat::{parse_stat, parse_status,
  parse_btime, assemble}`. Schema:
  `docs/json-schemas/proc-stat.schema.json`. (Plan 157 PR C —
  round-5 §2.2)
- `nlink-lab capture --max-size <N>` and `--rotate <SECS>` flags —
  rotating pcap segments for long-soak captures. `--keep <N>` (default
  5) caps how many rotated segments are retained; older ones are
  pruned at rotation. Each rotated segment is a complete pcap with
  its own global header. Decimal-SI suffixes accepted on `--max-size`
  (e.g. `100M`, `2G`). Library: new
  `nlink_lab::capture::CaptureOutput` enum (Summaries / Pcap /
  RotatingPcap), `RotatingPcapWriter`. (Plan 157 PR F — round-5 §2.3)
- `nlink-lab capture --dedupe-loopback` flag — sets the kernel's
  `PACKET_IGNORE_OUTGOING` socket option on the AF_PACKET ring, so
  loopback (`lo`) capture no longer reports each packet twice
  (once outgoing, once incoming). Off by default; the historical
  both-directions behavior is preserved when the flag is omitted.
  Requires kernel ≥ 4.20. Library:
  `nlink_lab::capture::CaptureConfig::ignore_outgoing: bool`.
  (Plan 157 PR H — round-5 §2.6)
- `nlink-lab spawn --wait-port <PORT>` and `--wait-fd-stable <SECS>` —
  two new readiness probes joining `--wait-tcp` and `--wait-log`.
  All four AND-compose. `--wait-port` reads `/proc/<pid>/net/tcp{,6}`
  for a `LISTEN` row matching the port (no `connect(2)` attempt;
  works for non-routable binds and avoids logged
  connection-refused noise). `--wait-fd-stable` is a heuristic:
  returns when the spawned process's `/proc/<pid>/fd/` count hasn't
  changed for SECS seconds. Library:
  `RunningLab::wait_for_port(node, pid, port, timeout, interval)`
  and `wait_for_fd_stable(node, pid, stable_for, timeout, interval)`.
  (Plan 157 PR G — round-5 §2.4)
- NLL `subnet auto/<prefix>` (or just `auto`) placeholder for
  network blocks. Resolved at deploy time against a host-wide
  flock-protected pool (`$XDG_STATE_HOME/nlink-lab/subnet-pool.json`,
  `10.0.0.0/8`-derived). Lets parallel labs share a host without
  hard-coding non-colliding subnets in each topology. Allocations
  recorded against the lab name and freed on destroy. New module
  `nlink_lab::subnet_pool` with public `allocate`, `free_for_lab`,
  and `substitute_auto_subnets`. Currently `/24` only; other
  prefixes error clearly. (Plan 157 PR E — round-5 §2.5)
- `nlink-lab status --json <LAB>` now includes a `host_resources`
  block with the lab's mgmt bridge name and declared subnets. Lets
  consumers detect cross-lab collisions client-side without
  netlink. Schema: `docs/json-schemas/status-lab.schema.json`.
  (Plan 157 PR D — round-5 §1.2 bonus)
- `nlink-lab spawn --json` and `nlink-lab ps --json` now also emit
  `host_pid` alongside `pid` — explicit alias documenting that the
  PID is host-side. Required field in both schemas. Equal to `pid`
  today (nlink-lab doesn't use `CLONE_NEWPID`); separate naming
  future-proofs the contract for when/if a NEWPID-based variant
  ships. Library: new `ProcessInfo::host_pid: u32` field.
  (Plan 157 PR B — round-5 §2.1)
- `docs/HARNESS_GUIDE.md` — guide for harness writers building on
  top of nlink-lab (spawn ordering with `--wait-log`/`--wait-port`,
  capture endpoint selection, failure-mode debugging, cleanup
  discipline, parallel-lab concurrency). Linked from README.
  (Plan 157 PR I — round-5 §3.1)
- `docs/ARCHITECTURE.md` — new "Process & namespace model" section
  documenting which `CLONE_NEW*` flags are active (only
  `CLONE_NEWNET` always; `CLONE_NEWNS` when `dns hosts` is set; no
  `CLONE_NEWPID`), the UID model, `/proc` permission rules, and the
  globally-shared state surfaces parallel deploys can race on
  (`/etc/hosts`, mac80211_hwsim). Source-of-truth answer to "what's
  the relationship between host PID and ns PID" — they're equal.
  (Plan 157 PR A — round-5 §1.1)
- README links `CHANGELOG.md` from the Documentation section.
  (Plan 157 PR A — round-5 §3.3)

### Fixed
- Parallel `nlink-lab deploy` invocations on labs that use `dns hosts`
  could lose each other's managed `/etc/hosts` sections. The
  read-modify-write of `/etc/hosts` in `dns::inject_hosts`,
  `remove_hosts`, and `remove_all_hosts` was not synchronised across
  labs. Now serialised by a global blocking flock at
  `$XDG_STATE_HOME/nlink-lab/labs/.hosts.lock` (new
  `state::hosts_lock()`). Concurrent deploys take turns instead of
  racing. (Plan 157 PR D — round-5 §1.2 prime suspect)

### Changed
- **Library API**: `nlink_lab::capture::run_capture` now takes a
  `CaptureOutput` enum instead of `Option<W>`. Use
  `CaptureOutput::pcap(path)?` for the simple case;
  `CaptureOutput::RotatingPcap { ... }` enables `--max-size` /
  `--rotate` rotation. (Plan 157 PR F)
- **Library API**: `nlink_lab::ProcessInfo` gains a public
  `host_pid: u32` field. Direct constructors need to populate it
  (set to `pid`). (Plan 157 PR B)
- Each `--json`-emitting subcommand's `--help` now points at its
  schema file under `docs/json-schemas/` (deploy, status, spawn,
  ps, impair --show, proc-stat). Saves consumers the discovery
  cost. (Plan 157 PR A — round-5 §3.2)

## [0.3.1] - 2026-05-03

Patch release for one bug in 0.3.0. No API changes.

### Fixed
- `nlink-lab impair --show --json` returned `endpoints: {}` for any
  topology built around bridge networks. The first cut only walked
  `topology.links`, so `network { members [...] }`-style endpoints
  were invisible. `collect_impair_show` now collects from
  `links` + `networks.members` + declared `impairments` keys via a
  new pure helper `nlink_lab::impair_parse::topology_endpoints`.
  Two unit tests cover the multi-source collection and a
  network-only topology; a root-gated integration test
  (`impair_show_includes_network_members`) deploys
  `examples/vlan-trunk.nll` and verifies end-to-end that a
  partitioned bridge member's qdisc is visible. Regression guard
  for the harness team's 3-machine config.
  (round-4 §3 follow-up)

## [0.3.0] - 2026-05-03

The "round-4 harness feedback" release — three small PRs from Plan
156 fixing the partition-cycle silent no-op, adding `exec --timeout`,
and adding `impair --show --json`. Together they let the
`des-test-harness` team revert their `--loss 100%` workaround and
their host-side `Command + child.kill()` deadline plumbing.

### Added
- `nlink-lab exec --timeout SECS` — bound the wall-clock time a command
  may run. On expiry the child is sent SIGTERM, then SIGKILL after a
  1-second grace period. Exit code 124 on timeout (matches
  `coreutils timeout(1)`). The CLI prints
  `nlink-lab exec: command timed out after Ns` to stderr. New
  `ExecOpts::timeout: Option<Duration>` field plumbs the value
  through `exec_with_opts` and `exec_attached_with_opts`. New
  `Error::Timeout(Duration)` variant for library consumers.
  (Plan 156 PR B — round-4 §2)
- `nlink-lab impair --show --json` — structured per-endpoint view of
  installed netem state, replacing grep-against-`tc`-text for harness
  consumers. One row per endpoint declared in the topology;
  endpoints with no qdisc serialize as `null`. Each row carries
  `qdisc`, `delay_ms`, `jitter_ms`, `loss_pct`, `rate_bps` (omitted
  when not set), plus a `partition` flag tracking the partition/heal
  lifecycle (distinct from a user installing `--loss 100%`
  directly). New library helper
  `RunningLab::is_partitioned(endpoint)` and pure parser
  `nlink_lab::impair_parse::parse_tc_qdisc_show`. Schema:
  `docs/json-schemas/impair-show.schema.json`.
  (Plan 156 PR C — round-4 §3)
  > **Known bug in 0.3.0**: `endpoints` always returned `{}` for
  > topologies built around bridge networks (the harness team's
  > 3-machine config). The collector only walked `topology.links`
  > and missed `network { members [...] }`-style endpoints
  > entirely. Use 0.3.1 or later for this feature.

### Fixed
- `nlink-lab impair --partition` is no longer a silent no-op on the
  second invocation after `--clear`. `clear_impairment` now prunes
  the endpoint's entry from `saved_impairments` (so the next
  `partition` doesn't short-circuit on the stale "is partitioned"
  flag) and persists state. It is also now idempotent on
  `QdiscNotFound` from the kernel — a missing qdisc is treated as
  "already cleared" instead of erroring. Together this makes
  partition→clear→partition→clear cycles work reliably; previously
  cycle 2's `partition` printed success but installed nothing, and
  cycle 2's `clear` crashed. (Plan 156 PR A — round-4 §1)

### Changed
- **Library API**: `RunningLab::clear_impairment` is now `&mut self`
  (was `&self`) — necessary for the partition-cycle fix above. All
  in-tree callers were already passing `mut RunningLab`; external
  callers (none we know of) need to pass `mut`.

## [0.2.0] - 2026-04-30

The "documentation + reconcile" release. Two big arcs landed since
0.1.0:

1. **Documentation overhaul (Plans 150–154).** README rewritten
   to lead with the wedge, 11 cookbook recipes paired with runnable
   `examples/cookbook/*.nll`, full CLI reference, `COMPARISON.md`
   (vs containerlab), `ARCHITECTURE.md` (contributor on-ramp),
   60-minute USER_GUIDE walkthrough, TROUBLESHOOTING expanded
   193→416 LOC, doc-CI gate.
2. **`apply` reconcile completeness.** Editing any non-process
   topology field (per-endpoint impair, network-level per-pair
   impair, routes, sysctls, rate-limits, nftables, NAT) now
   converges in place via `nlink-lab apply` — no destroy + redeploy
   for non-structural edits. Backed by nlink 0.15.1's
   `PerPeerImpairer::reconcile()`. New `--check` drift gate exits
   non-zero if live state differs from NLL; new `--json` structured
   diff for CI consumption.

Plus: lab portability (`.nlz` archives, Plan 153), library-first
testing polish (`#[lab_test]` `set` / `timeout` / `capture = true`,
Plan 154), per-pair network impair (`impair A -- B { … }` inside
`network`, Plan 128), `for` loops inside network blocks, plus the
round-3 polish from Plan 155 (workdir, status --scan stale
detection, destroy --orphans, spawn --wait-log, ps --alive-only,
ExecOpts/SpawnOpts, JSON schemas).

### Notable bug fixes (this release)

- **Bridge naming collision** (hash-based `nb{hash8}` replaces
  `{prefix}-{net_name}[..15]`). The old truncation silently
  collided whenever the lab prefix grew long enough — surfaced
  by the `#[lab_test]` macro's name-rewriting.
- **Zombie processes treated as alive**: `process_status` now
  reads `/proc/<pid>/stat` and treats state `Z` as not-alive.
  `kill(pid, 0)` returns 0 for zombies; before this fix,
  quick-exiting children stayed "alive" forever from the lab's
  POV.
- **Builder `.port(node, |p| p.interface("eth0"))`** now
  auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, builder-DSL labs silently produced empty
  `members` and a missing veth at deploy time.

### Fixed
- `nlink-lab spawn --env KEY=VALUE` no longer changes the per-process
  log file basename to `env`. Previously the CLI implemented `--env` by
  prepending `/usr/bin/env K=V` to the user's command; the log basename
  is derived from `argv[0]`, so consumers that reconstructed log paths
  from the binary name silently broke. Env vars are now applied via
  `Command::env(k, v)` directly. (Plan 155 PR B — round-3 §3.1)
- `nlink-lab capture -w <pcap>` no longer produces a 0-byte pcap when
  the capture process is terminated by SIGTERM (e.g., `timeout(1)`'s
  default signal) or SIGKILL. The pcap writer now flushes after every
  packet, matching `tcpdump -U`. The CLI also installs a SIGTERM
  handler alongside the existing SIGINT handler so the loop exits
  cleanly and prints the summary line. (Plan 155 PR A — round-3 §2.1)

### Added

#### From Plans 150–154 (this session)

- **Per-pair network impairment**: `impair A -- B { delay … loss …
  rate-cap … }` inside `network { }` blocks, modeling
  distance-dependent radio/satellite/multipoint paths on a shared
  L2. Built on nlink 0.15.1's `PerPeerImpairer`. Deploy step 14b
  builds one HTB+netem+flower TC tree per source interface.
  (Plan 128)
- **`for` loops inside `network { }`** with full arithmetic
  (`${(i+1) % 12}`), modulo, and nested loops (Cartesian product
  expansion). The 12-node satellite-mesh cookbook example uses
  this to generate 32 directional impair rules from ~25 lines of
  NLL.
- **`apply` reconcile completeness** (Plan 152): network-level
  per-pair impair (Phase A), per-node static routes (B/1),
  per-node sysctls (B/2), per-endpoint rate-limits (B/3), per-node
  nftables firewall + NAT (B/4 — atomic flush + rebuild). Phase C
  added `apply --check` (drift gate) and `apply --json --dry-run`
  (structured diff for CI).
- **`.nlz` lab archive** for repros and sharing (Plan 153):
  - `nlink-lab export --archive <lab|.nll> [-o file.nlz]` with
    `--include-running-state`, `--no-rendered`, `--set`.
  - `nlink-lab import file.nlz` — verifies SHA-256 checksums,
    extracts, validates, deploys.
  - `nlink-lab inspect FILE.nlz` — manifest + node/link/network
    counts without extracting.
  - Format: gzipped tarball with `manifest.json` + `topology.nll`
    + optional `params.json` / `rendered.toml` / `state.json`.
    `format_version = 1`.
- **`#[lab_test]` macro polish** (Plan 154):
  - `set { key = "value" }` — apply NLL `param` overrides.
  - `timeout = N` — wrap test body in `tokio::time::timeout`.
  - `capture = true` — start parallel pcaps on every (namespace,
    iface). On panic, persist to
    `target/lab_test_captures/<test>-<pid>/`. On success, discard.
  - `nlink_lab::test_helpers::LabCapture` helper drives the
    implementation.
- **Documentation overhaul** (Plan 150):
  - README rewritten leading with the wedge.
  - `docs/COMPARISON.md` (honest vs containerlab) and
    `docs/ARCHITECTURE.md` (contributor on-ramp).
  - `docs/cookbook/` with 11 recipes: satellite-mesh,
    multi-tenant-wan, vrf-multitenant, wireguard-mesh,
    macvlan-host-bridge, nftables-firewall, bridge-vlan-trunk,
    p2p-partition, iperf3-benchmark, healthcheck-depends-on,
    parametric-imports, ci-matrix-sweep, lab-portability,
    rust-integration-test.
  - `docs/cli/` with 29 reference pages (8 hand-crafted +
    21 auto-stubs).
  - `docs/USER_GUIDE.md` 60-minute guided walkthrough that builds
    one realistic site-to-site WAN progressively (786→1160 LOC).
  - `docs/TROUBLESHOOTING.md` expanded 193→416 LOC with apply,
    archive, scenario, library-test, and common-misconfig
    sections.
  - Doc-CI gate: `every_nll_snippet_in_docs_parses` and
    `internal_doc_links_resolve` lib tests catch drift on every
    PR.

#### From Plan 155 (round-3 polish)

- `nlink-lab spawn --wait-log <REGEX>` — block the spawn until a line
  matching REGEX appears in the spawned process's captured
  stdout/stderr, mirroring `--wait-tcp` for services that signal
  readiness via a log line rather than a port. `--wait-log-stream`
  selects which stream to watch (`stdout` / `stderr` / `both`,
  default `both`). `--wait-log` and `--wait-tcp` AND-compose: both
  must succeed before spawn returns. Library: new
  `RunningLab::wait_for_log_line(pid, regex, LogStream, timeout,
  interval)`. (Plan 155 PR E — round-3 §4.2)
- `nlink-lab ps --alive-only` flag (and library helper
  `RunningLab::process_status_alive_only`) that filters out tracked
  processes whose PID has exited. Useful for "is X still running?"
  polling loops where the default retention behaviour (exited entries
  remain in the listing with `alive: false`) is a footgun. The default
  `ps` behaviour is unchanged. (Plan 155 PR C — round-3 §3.2)
- `nlink_lab::ExecOpts` and `nlink_lab::SpawnOpts` — borrow-based
  option structs for `RunningLab::exec_with_opts`,
  `exec_attached_with_opts`, and `spawn_with_logs_with_opts`. Carry
  `workdir` and `env` (plus `log_dir` for spawn). Existing `exec`,
  `exec_in`, `exec_attached`, `exec_attached_in`, `spawn_with_logs`,
  `spawn_with_logs_in` methods are now thin wrappers over these — no
  caller break. (Plan 155 PR B)
- JSON output schemas for the four high-traffic shapes under
  `docs/json-schemas/`: `deploy`, `status` (list + scan variants),
  `spawn`, `ps`. Hand-written draft-07 schemas; the source of truth
  remains the code. Linked from `--json` `--help`. (Plan 155 PR D —
  round-3 §5.1)
- `nlink-lab exec --workdir <dir>` and `nlink-lab spawn --workdir <dir>`
  — set the working directory of the child. For namespace nodes this is
  `chdir()` on the host filesystem (namespace nodes share the host mount
  namespace); for container nodes it's passed as `-w` to the runtime.
  Library: new `exec_in`, `exec_attached_in`, `spawn_with_logs_in`
  methods on `RunningLab`; the existing zero-workdir methods delegate
  to these with `None`.
- `nlink-lab status --scan` now also reports **stale** labs — state files
  claiming namespaces that no longer exist on the host (typical after a
  reboot or WSL restart). Human output lists missing namespaces and
  suggests `destroy <lab>`; `--json` adds a `stale` array alongside
  `bridges`/`veths`/`netns`.
- `nlink-lab destroy --orphans` — reap host resources (mgmt bridges,
  veth peers, named namespaces) that match the lab naming scheme but
  have no `state.json`. Left behind by crashed deploys. Composes with
  `--all` (clean state-backed labs + orphans) or runs standalone.
- `nlink-lab status --scan` — scan the host for the same set and report
  anything unaccounted for. Prints nothing when clean; otherwise names
  each resource and suggests `destroy --orphans`. `--json` emits
  `{ labs, orphans }` instead of the labs list alone.
- NLL DSL as sole topology format (TOML removed)
- `InterfaceKind` enum replacing string-based interface types
- Shell completions for bash, zsh, fish, powershell
- `--json` global flag for machine-readable CLI output
- `--dry-run` flag on deploy command
- `Lab::build_validated()` method for early error detection
- `validate_interface_name()` helper for Linux IFNAMSIZ enforcement
- 4 new validation rules: interface-name-length, wireguard-peer-exists,
  vrf-table-unique, duplicate-link-endpoint (18 rules total)
- NLL: `burst` on rate limits, `env`/`volumes`/`runtime` for containers,
  multiple addresses on WG/dummy/vxlan, spaceless interpolation `${i+1}`
- Duplicate node/network name detection in NLL lowering
- Asymmetric impairment example
- `getrandom` for safe WireGuard key generation (no more panic)
- `time` crate for ISO 8601 timestamps
- Atomic state file writes (temp + rename)

### Changed

#### From Plans 150–154 (this session)

- **Upgraded `nlink` 0.13.0 → 0.15.1.** Mostly additive; the typed
  `*Config::parse_params` rollout, new
  `nlink::netlink::impair::PerPeerImpairer` helper (which Plan 128
  consumes), legacy `nlink::tc::builders::*` deletion (we never
  used it). MSRV bumped to 1.85 (already required by edition 2024).
- **Bridge naming**: shared L2 bridges now use
  `network_bridge_name_for(net_name) → "nb{hash8}"` (10 chars,
  always within IFNAMSIZ). Replaces the previous
  `{prefix}-{net_name}[..15]` truncation that silently collided
  whenever the lab prefix grew long enough. Mirrors the existing
  Plan-149 fix for veth peer names. Internal — no caller-visible
  change.
- **Process liveness check** now treats zombie state (`Z` in
  `/proc/<pid>/stat`) as not-alive. Previously `kill(pid, 0) == 0`
  reported zombies as alive forever, since `spawn_with_logs`
  drops its `Child` without `wait()`-ing. Affects
  `RunningLab::process_status` and `process_status_alive_only`.
- **Builder DSL**: `.port(node, |p| p.interface("eth0").address(...))`
  now auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, callers had to write both `.member(...)` and
  `.port(...)` separately — and forgetting `.member` produced
  empty members and a missing veth at deploy time. The deploy
  step's address-application pass also handles bare-`node` port
  keys now (using `port.interface` for the iface name) so both
  builder and NLL keying styles work.
- **`for` loops inside `network` blocks** use the same
  `interpolate()` engine as the lower stage (now `pub(crate)`),
  so arithmetic and nested vars work consistently across loop
  forms.

#### From Plan 155 + earlier (existing Unreleased)

- Upgraded `nlink` dependency from 0.12.2 to 0.13.0. Internal only —
  no behavioural change. `NetemConfig::rate_bps(u64)` was removed in
  favour of `rate(Rate)`; `NetemConfig::{loss,corrupt,reorder}` and
  `RateLimiter::{egress,ingress}` now take typed `Percent`/`Rate`
  wrappers instead of `f64`/`&str`. `del_qdisc`/`change_qdisc` take
  `TcHandle` (use `TcHandle::ROOT` in place of `"root"`).
- `nlink-lab logs --pid <pid> --follow` now actually follows. Previously
  `--follow` was silently dropped on the `--pid` path (container logs
  were the only case it worked for). The CLI now implements `tail -F`
  semantics: print the existing tail, then poll the log file for new
  bytes, reopening from offset 0 if truncation/rotation is detected.
  `--tail N` is honoured for the initial dump as before.
- `nlink-lab exec` (non-JSON mode) now streams stdio live. Previously it
  captured the full stdout/stderr into buffers and printed them only
  after the child exited, which made it unusable for services,
  `tail -f`, `ping`, and any other long-running command. `--json` still
  returns structured `{ exit_code, stdout, stderr, duration_ms }`.
  `RunningLab::exec_attached(node, cmd, args)` exposes the streaming
  path for library callers.

### Fixed
- `nlink-lab shell` no longer fails with
  `nsenter: neither filename nor target pid supplied for ns/net` — the
  nsenter invocation now passes `--net=<path>` as a single argv entry
  instead of two entries, which nsenter was misparsing as the bare
  `--net` flag with a stray command argument.
- Bridge-network peer names no longer collide when two networks share a
  4-char prefix (e.g. `lan_a` / `lan_b`). Previously the mgmt-side veth
  peer was named `br{net_name[..4]}p{idx}`, which collapsed both names
  to `brlan_p{idx}` and failed the second `add_link` with EEXIST. Peer
  names are now `np{hash8}{idx}`, derived from a DJB2 hash of the
  network name — deterministic, within the 15-char IFNAMSIZ budget, and
  exposed as `nlink_lab::network_peer_name_for`.
- Veth-creation errors for bridge networks now name the mgmt-side peer
  interface as well as the node-side endpoint, so an EEXIST is no
  longer misattributed to whichever name the user typed.
- Rate limiting now applies to both link endpoints (was left-only)
- Bare integer tokens rejected as node names
- Division by zero in interpolation now logs error
- Firewall: unrecognized match expressions now error instead of silently passing
- Removed no-op `replace()` call in NLL diagnostics
- All actionable compiler warnings resolved

## [0.1.0] - 2026-03-22

Initial release.

- Core lab engine: parse, validate, deploy, destroy
- NLL and TOML topology formats
- 14 validation rules
- 18-step deployment sequence
- VRF, WireGuard, bond, VLAN, VXLAN, bridge support
- `#[lab_test]` proc macro for integration testing
- 12 built-in templates via `nlink-lab init`
- Runtime impairment modification, diagnostics, packet capture
- DOT graph output, process management
- Container node support (Docker/Podman)
