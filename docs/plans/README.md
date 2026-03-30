# Plans

Implementation plans for nlink-lab.

## Active Plans

| Plan | Description | Effort | Status |
|------|-------------|--------|--------|
| [105](105-dns-phase2.md) | DNS Phase 2 — per-namespace isolation | Small | **Implemented** |
| [106](106-macvlan-ipvlan.md) | macvlan and ipvlan interface support | Medium | **Implemented** |
| [107](107-rich-assertions.md) | Rich validation assertions | Medium | **Implemented** |
| [108](108-scenario-dsl.md) | Timed scenario / fault injection DSL | Large | **Implemented** (core) |
| [109](109-ci-integration.md) | CI/CD integration (JUnit/TAP, `test` command) | Medium | **Implemented** |
| [110](110-integration-tests.md) | Integration test expansion (17 → 32) | Medium | **Implemented** |
| [111](111-benchmark-block.md) | Benchmark block (iperf3/ping with assertions) | Medium | **Implemented** |
| [112](112-wifi-emulation.md) | Wi-Fi emulation via mac80211_hwsim | Large | **Implemented** |

### Recommended execution order

```
110 (tests)  ─── can start immediately, no dependencies
105 (DNS P2) ─── can start immediately, small
106 (macvlan) ── can start immediately, medium
107 (assertions) ── can start immediately, unlocks 108/109
109 (CI) ──────── benefits from 107, but standalone value too
108 (scenario) ── benefits from 107, largest effort
111 (benchmark) ─ standalone, benefits from 109
```

## Completed

Plans 050 (advanced interfaces), 051 (phase 3 features), 052 (ecosystem),
060 (NLL parser), 070 (topoviewer GUI), 071 (Zenoh backend & metrics),
072 (lab templates), 080 (bug fixes & safety), 081 (code quality),
082 (NLL completeness), 083 (validator hardening), 084 (CLI UX),
085 (test coverage), 086 (feature flags), 087 (topology composition
& hot-reload), 088 (remove TOML), 090 (hardening),
091 (user documentation), 092 (structured errors),
093 (NLL v2 language & ergonomics), 094 (NLL v2 composition),
095 (container core), 096 (container lifecycle), 097 (parser hardening),
098 (NLL patterns), 099 (production readiness), 100 (validate & errors),
101 (NLL syntax cleanup), 102 (CLI quality), 103 (container CLI),
and 104 (polish) have been implemented and their plan files removed.

DNS support (Phase 1) was implemented on 2026-03-30. See [dns-support.md](dns-support.md).

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
