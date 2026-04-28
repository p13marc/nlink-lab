# Plan 150: Documentation Overhaul

**Date:** 2026-04-27
**Status:** Proposed
**Effort:** Large (5–7 days, splittable into 4 phases that can each ship independently)
**Priority:** P0 — the project's biggest gap is not capability, it's that nobody (including us) can explain in 30 seconds why nlink-lab exists.

---

## Problem Statement

The technical work is solid: 22k LOC, 321 tests, 40 examples, all features
real (no stubs). The DSL is genuinely expressive — `for`/`import`/pools/
glob patterns/IP arithmetic work and are tested. nlink-lab can express
topologies containerlab structurally **cannot** (per-pair impair on
shared L2, VRF + WireGuard composition, custom TC chains across
namespaces).

But:

- **README.md leads with mechanics, not the wedge.** A first-time visitor
  cannot figure out in one screen *why* they would pick nlink-lab over
  containerlab/Kathara/GNS3.
- **No CLI reference doc.** 32 commands; users must `--help` each one
  to discover flags. Zero discoverability via web search.
- **No cookbook.** `examples/` has 40 .nll files but no narrative. A
  user looking for "how do I model a satellite link" has to grep.
- **No comparison page.** Most evaluators arrive having already used
  containerlab. The lack of an honest "here's what's different and
  why you might care" page sends them back.
- **No architecture doc for contributors.** The only path into the
  codebase is reading `deploy.rs`. New contributors won't.
- **Fuzz harness exists but is undocumented** (`crates/nlink-lab/fuzz`
  excluded from the workspace; nobody knows it's there).
- **`USER_GUIDE.md` is solid but assumes reader knows Linux
  namespaces.** No on-ramp for the Rust-shop developer who knows
  `tokio` but has never run `ip netns`.

The differentiator (deep Linux API + Rust-native + expressive DSL +
no-Docker-tax) is real. The reason it's invisible is that the docs
don't claim it confidently and don't prove it with side-by-side
examples.

## Goals

1. A first-time visitor to the README understands the wedge in 30
   seconds and can copy-paste a working example in 90.
2. A user evaluating against containerlab can answer "should I switch /
   add this alongside" without leaving the docs.
3. Every CLI command has a reference page with all flags, examples,
   and exit codes.
4. A contributor can read the architecture doc and submit a parser
   change without spelunking `deploy.rs`.
5. The docs prove the DSL's expressiveness with side-by-side
   "containerlab can't do this" examples.

## Phases

This plan is intentionally split into four phases that can each ship
as a separate PR. Each phase has independent value; merging them
in order maximizes compounding clarity.

### Phase A — README rewrite (1 day, P0)

The README is the single highest-leverage doc. Today it's a feature
list. Replace with a story-driven structure:

```markdown
# nlink-lab

Reproducible Linux network labs in 100ms, scriptable from Rust,
with deeper TC/nftables/WireGuard/VRF control than any
container-based alternative.

```nll-ignore
network radio {
  members [hq, alpha, bravo]
  subnet 172.100.3.0/24
  impair hq    -- alpha { delay 15ms loss 1% }
  impair hq    -- bravo { delay 40ms loss 5% rate-cap 10mbit }
  impair alpha -- bravo { delay 60ms loss 8% }
}
```

That's a 3-node satellite mesh with distance-dependent
impairment. **There is no way to express this in a single
containerlab/Docker topology** — Docker's network model can't
attach per-destination netem on a shared bridge. nlink-lab does it
in three lines.

[60-second quickstart](#quickstart) ·
[Why not containerlab?](docs/COMPARISON.md) ·
[Cookbook](docs/cookbook/) ·
[CLI reference](docs/cli/) ·
[NLL language spec](docs/NLL_DSL_DESIGN.md)

## Quickstart

```bash
cargo install nlink-lab-cli
sudo nlink-lab deploy examples/simple.nll
sudo nlink-lab exec simple router -- ping -c 3 host
sudo nlink-lab destroy simple
```

## Why nlink-lab

- **Deep Linux networking, no Docker required.** TC, nftables,
  VRF, WireGuard, macvlan, ipvlan, VXLAN, bonds, bridges — all via
  netlink directly. Sub-second deploy. Runs in any CI runner with
  `CAP_NET_ADMIN`.
- **A real DSL, not stringly-typed YAML.** Loops, imports,
  parametric modules, glob patterns, IP arithmetic, conditional
  blocks. Errors come with miette source spans. Type-checked at
  parse time.
- **Library-first.** `use nlink_lab::Topology` and deploy from
  `#[tokio::test]`. The CLI is a thin wrapper — you don't have to
  shell out.
- **Reconcile, don't redeploy.** `nlink-lab apply` converges live
  state to NLL with zero packet loss for unchanged links.

## What it isn't

- A vendor NOS lab tool. If you need cEOS, SR Linux, or vMX, use
  [containerlab](https://containerlab.dev). nlink-lab targets pure
  Linux topologies.
- A multi-host orchestrator. Single-host only.
- A GUI. CLI + library only (a TUI is on the roadmap).

## Status

Beta. API and NLL syntax stable since 0.x; breaking changes
flagged in CHANGELOG with migration notes.

## Documentation

- [60-minute walkthrough](docs/USER_GUIDE.md)
- [Cookbook](docs/cookbook/) — 12 real-world recipes
- [CLI reference](docs/cli/) — every command, every flag
- [NLL language spec](docs/NLL_DSL_DESIGN.md)
- [vs. containerlab](docs/COMPARISON.md)
- [Architecture](docs/ARCHITECTURE.md) — for contributors

## License

MIT OR Apache-2.0
```

**Acceptance:** README leads with the wedge, has a working code
sample inside the first screen, and links to every other doc.

### Phase B — CLI reference + cookbook scaffolding (2 days, P0)

#### B.1 `docs/cli/` — one page per command

Auto-generate skeletons from `clap` `--help` output, then hand-edit
the high-traffic ones (`deploy`, `destroy`, `validate`, `exec`,
`spawn`, `apply`, `capture`, `status`).

Per-command structure:

```markdown
# `nlink-lab deploy`

Deploy a topology from an NLL file.

## Usage

```text
nlink-lab deploy <FILE> [--set KEY=VALUE]... [--unique] [--suffix STR] [--json]
```

## Description

… one paragraph explaining what it does, when you'd use it, and
what side effects it has (root required, state file written to
~/.nlink-lab/, …).

## Flags

| Flag | Description |
|------|-------------|
| `--set KEY=VALUE` | Override an NLL `param`. Repeatable. |
| `--unique` | Append a random 4-char suffix to the lab name to allow concurrent labs from the same NLL file. |
| ... | ... |

## Examples

### Basic deploy
…

### CI: parameterized deploy with JSON output
…

### Concurrent test labs
…

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Validation failed |
| 2 | Deployment failed (state may be partial — run `destroy`) |
| ... | ... |

## See also

`destroy`, `apply`, `validate`
```

Generate stubs:

```bash
for cmd in $(nlink-lab --help | grep -E '^\s+[a-z]' | awk '{print $1}'); do
    nlink-lab "$cmd" --help > docs/cli/_raw/"$cmd".txt
done
```

Then promote the 8 high-traffic commands from `_raw/` to full pages
in this phase. Defer the long tail to Phase D.

#### B.2 `docs/cookbook/README.md` — 12 recipes, written in this phase

Each recipe is one .md file under `docs/cookbook/`, paired with a
runnable `examples/cookbook/<name>.nll`. Format:

```markdown
# Recipe: Distance-dependent satellite mesh

**Use case:** Test how a P2P/mesh protocol handles realistic
satellite links where delay and loss vary with distance.

**Why nlink-lab:** Per-pair impairment on a shared L2 segment.
containerlab can't express this in one topology — its Docker
network model only supports per-link netem on point-to-point veths.

## NLL

[satellite-mesh.nll embedded]

## Run

```bash
sudo nlink-lab deploy examples/cookbook/satellite-mesh.nll
sudo nlink-lab exec satellite-mesh hq -- ping -c 5 alpha
sudo nlink-lab exec satellite-mesh hq -- ping -c 5 bravo
sudo nlink-lab destroy satellite-mesh
```

## What you'll see

Pings to alpha return ~15ms; pings to bravo return ~40ms with
occasional drops (5% loss). The HTB+netem+flower tree on hq's
radio interface ensures per-destination netem leaves are honored.

## Variations

- Add a per-pair rate cap: `rate-cap 10mbit`
- Make it asymmetric: only one of the two directional rules
- Inject a partition mid-test using a `scenario` block …

## See also

[Cookbook: P2P partition test](p2p-partition.md),
[NLL: per-pair impair syntax](../NLL_DSL_DESIGN.md#per-pair-impairment)
```

Initial 12 recipes (3 are deferred to Plan 151 as
"killer examples"; the remaining 9 ship in this phase):

1. Distance-dependent satellite mesh _(Plan 151)_
2. P2P partition mid-test via `scenario` _(Plan 151)_
3. Asymmetric WAN with one-way loss spike _(Plan 151)_
4. VRF: customer separation in a single namespace
5. WireGuard 3-node mesh with auto-key generation
6. macvlan: attach lab nodes to a host physical NIC
7. nftables: stateful firewall with conntrack zones
8. Bridge VLAN trunks: 802.1Q access/trunk port mix
9. iperf3 benchmark with assertion: "p99 latency < 50ms or test fails"
10. Healthcheck-gated process startup with `depends_on`
11. CI matrix: `--set` parameter sweeps from a shell loop
12. Parametric `import for_each` for spine-leaf fabrics

**Acceptance:** 8 high-traffic CLI pages live; 9 of 12 cookbook
recipes live (the 3 "killer" ones land in Plan 151); each cookbook
.nll file has an integration test that at minimum parses + validates.

### Phase C — Comparison page + architecture doc (2 days, P1)

#### C.1 `docs/COMPARISON.md` — honest vs containerlab

Audience: someone who already uses containerlab and is wondering if
they should add or switch. Structure:

```markdown
# nlink-lab vs containerlab

Both build Linux network labs from a config file. They target
different jobs.

## Quick decision

Use **containerlab** if:
- Your lab needs to run a vendor NOS image (cEOS, SR Linux, vMX,
  cRPD, vJunos, Palo Alto, Fortinet, …).
- You want a web UI / topology graph.
- You want multi-host clustering or K8s-mode.
- Your team is invested in YAML-based IaC and Ansible/Terraform
  integration.

Use **nlink-lab** if:
- Your topology needs deep Linux networking primitives:
  per-destination impairment on shared L2, custom HTB/flower TC
  chains, VRF + WireGuard composition, macvlan/ipvlan to host NICs.
- You want a Rust-native testing API (`#[tokio::test]` →
  deploy → assert → destroy).
- You can't run a Docker daemon (locked-down CI, embedded host,
  unprivileged container).
- Your DSL needs loops, parametric imports, glob patterns, IP
  arithmetic — without YAML's quote-and-anchor gymnastics.

## Capability matrix

| | containerlab | nlink-lab |
|---|---|---|
| Vendor NOS support | ✅ 20+ images | ❌ pure Linux only |
| Pure Linux nodes | ✅ via `linux` kind | ✅ native |
| Per-link netem | ✅ | ✅ |
| **Per-pair netem on shared bridge** | ❌ structural — Docker network model | ✅ via `PerPeerImpairer` |
| Asymmetric impair (one direction) | ⚠️ workaround | ✅ first-class |
| HTB / flower / u32 / matchall TC | ⚠️ via raw `exec:` | ✅ first-class |
| nftables per-node | ⚠️ via raw `exec:` | ✅ first-class |
| WireGuard with auto-key gen | ❌ | ✅ |
| VRF | ❌ | ✅ |
| macvlan/ipvlan to host NIC | ⚠️ kind-specific | ✅ first-class |
| Multi-host | ✅ | ❌ |
| Web UI | ✅ | ❌ |
| Save/restore lab state | ✅ | ⚠️ NLL-as-state (re-deploy) |
| Library API | ⚠️ Go, undocumented | ✅ Rust, first-class |
| Deploy speed | seconds (Docker overhead) | sub-second |
| CI footprint | Docker daemon + image pulls | namespace + CAP_NET_ADMIN |
| Reconcile / declarative apply | ✅ partial | ✅ via `apply` (Plan 152) |
| DSL: loops / imports / parametric | ❌ static YAML | ✅ |

## Side-by-side: a 3-leaf spine fabric

[containerlab YAML — 80 lines, hand-written]
[nlink-lab NLL — 22 lines using `for` loop and `${spine.eth1}` cross-ref]

## When to use both

[Section: containerlab for the NOS sled, nlink-lab inside Linux
nodes for fine-grained TC and CI, with example.]

## Migrating

If you have an existing `.clab.yml` with `kind: linux` nodes and
veth links, see [migrate-from-clab.md](cookbook/migrate-from-clab.md)
— there's a one-page mechanical mapping.
```

This page is honest about both wins and losses. Don't claim
nlink-lab beats containerlab on vendor NOS or multi-host — readers
who care about those would resent it. Win on the depth axis.

#### C.2 `docs/ARCHITECTURE.md` — for contributors

Single-page, ~600 lines. Mirrors the existing internal docs (deploy
sequence, parser/lower split, validator rules) but as a contributor
on-ramp:

- Crate layout and what each module owns
- The Topology → AST → Lower → Validate → Deploy pipeline (with a
  diagram)
- The 18-step deploy sequence as a flowchart with rollback
  semantics
- How to add a new NLL keyword end-to-end (lexer → AST → parser
  branch → lower → types → render → validator → deploy → docs).
  Use Plan 128's per-pair impair as the worked example.
- How to write a unit test for a new feature
- Where the integration tests live and how to run them (privileged
  runner only)
- nlink dependency: what it provides, version-pin policy, where to
  send upstream issues
- The fuzz harness (`crates/nlink-lab/fuzz`): targets, how to run,
  corpus location

**Acceptance:** ARCHITECTURE.md has a "I want to add a new feature
end-to-end" section that a stranger to the codebase can follow
without IRC support.

### Phase D — Long-tail polish (1–2 days, P2)

- Promote remaining 24 CLI commands from `docs/cli/_raw/` to full
  pages.
- `docs/TROUBLESHOOTING.md` expansion: 10 new entries based on real
  errors seen during dogfooding (today's TROUBLESHOOTING.md is 193
  LOC; double it).
- `docs/USER_GUIDE.md` quickstart-flow restructure: today the user
  guide is reference-style; restructure first 200 LOC as a
  60-minute walkthrough that builds one nontrivial lab from
  scratch.
- Doc-test sweep: make sure every NLL snippet in docs is also
  a file in `examples/` that the test suite parses+validates, so
  docs can't drift silently.
- `docs/fuzz.md`: how to run the fuzzer, what's covered, how to
  triage findings.

## Cross-cutting work

- **Doc-CI gate.** Add a `cargo test --doc` job + a "every NLL
  snippet in docs is also a file in examples/" linter. Run on PR.
  Prevents docs from rotting.
- **Link-check.** A daily/weekly job that walks every internal link
  in `docs/` and fails on 404s. Avoids the silent rot where
  `OAM_CONTROLLER_API_ICD.md` is referenced but doesn't exist.
- **Frontmatter convention.** Every doc starts with one-line
  description + last-reviewed date. Lets us see at-a-glance which
  docs need a refresh after a release.

## Tests

| Test | Description |
|------|-------------|
| `tests/docs_examples.rs` | Walk every fenced ```nll block in `docs/` and assert it parses. Failures point at the .md:line. |
| `tests/cookbook_examples.rs` | Each `examples/cookbook/*.nll` parses + validates. |
| CI gate `docs-link-check` | No broken internal links. |

## File Changes

| File | Change |
|------|--------|
| `README.md` | **Rewrite** — story-driven, Phase A. |
| `docs/COMPARISON.md` | **New** — Phase C.1. |
| `docs/ARCHITECTURE.md` | **New** — Phase C.2. |
| `docs/cli/*.md` | **New** — per-command pages, Phase B.1. |
| `docs/cookbook/*.md` | **New** — 9 recipes, Phase B.2. |
| `examples/cookbook/*.nll` | **New** — paired with each recipe. |
| `docs/USER_GUIDE.md` | Restructure first 200 LOC as walkthrough. |
| `docs/TROUBLESHOOTING.md` | Expand. |
| `docs/fuzz.md` | **New**. |
| `tests/docs_examples.rs` | **New** — doc-snippet parse test. |
| `.github/workflows/docs.yml` | **New** — link check + doc-test. |

## Acceptance

- A new visitor reads README.md and can run a working example in
  90 seconds.
- The `vs containerlab` page exists and is linked from the README.
- Every high-traffic CLI command has a reference page.
- 9 cookbook recipes live with paired runnable .nll files.
- `cargo test --doc` passes; CI gates docs-link-check and
  doc-snippet-parse.

## Out of scope (future plans)

- Killer examples + writeups (Plan 151)
- `apply` reconcile completion (Plan 152)
- `export`/`import` lab portability (Plan 153)
- `#[nlink_lab::test]` proc macro (Plan 154)

These are referenced from the docs as "future" until they ship,
then linked.
