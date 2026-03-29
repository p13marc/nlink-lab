# Plan 091: User Documentation

**Priority:** High
**Effort:** 3-4 days
**Depends on:** None
**Target:** `docs/`

## Summary

Create user-facing documentation that takes someone from "what is this?" to
"I'm using it in CI." The README and NLL spec exist but there's no guided
path between them — no tutorials, no troubleshooting, no integration testing
guide.

## Current State

| Asset | Status |
|-------|--------|
| README.md | Good quick-start, examples table, containerlab comparison |
| docs/NLL_DSL_DESIGN.md | Complete language spec (600+ lines) |
| docs/NLINK_LAB.md | Design doc (architecture, roadmap) — developer-facing |
| CLI `--help` | Excellent — all 18 commands have rich clap descriptions |
| examples/*.nll | 16 well-commented examples covering all major features |
| CHANGELOG.md | Maintained, semantic versioning |
| rustdoc | Crate-level docs present; per-type docs sparse |
| Man page | None |
| Tutorials | None |
| Troubleshooting | None |

## Phase 1: User Guide (day 1-2)

Create `docs/USER_GUIDE.md` — the primary document for new users.

### Sections

1. **Installation** — build from source, cargo install, requirements
   (Linux, root/CAP_NET_ADMIN, kernel version)

2. **Your First Lab** — step-by-step walkthrough:
   - Write a 2-node NLL file from scratch
   - `nlink-lab validate` — catch errors before deploying
   - `sudo nlink-lab deploy` — see what happens (namespace creation, link setup)
   - `nlink-lab status` — inspect running labs
   - `sudo nlink-lab exec` — run commands inside nodes
   - `sudo nlink-lab destroy` — clean up
   - What each step does under the hood (namespaces, veths, netlink)

3. **NLL by Example** — progressive complexity:
   - Simple link with addresses
   - Adding routes and IP forwarding (profiles)
   - Loops for repetitive topologies
   - Variables and interpolation
   - Link impairments (delay, loss, rate)
   - Asymmetric impairments (`->` / `<-`)
   - Firewall rules (nftables)
   - Bridge networks with VLANs
   - WireGuard tunnels
   - VRF multi-tenancy
   - Containers (Docker/Podman nodes)
   - Imports for topology composition

4. **Runtime Operations** — what you can do with a running lab:
   - Modify impairments at runtime (`nlink-lab impair`)
   - Packet capture (`nlink-lab capture`)
   - Diagnostics (`nlink-lab diagnose`)
   - Process management (`nlink-lab ps`, `nlink-lab kill`)
   - Live topology diff and apply (`nlink-lab diff`, `nlink-lab apply`)

5. **Topology Templates** — `nlink-lab init --list` and how to use them

6. **Daemon Mode & TopoViewer** — live metrics via Zenoh, GUI visualization

### Tasks

- [ ] Write Installation section
- [ ] Write Your First Lab walkthrough
- [ ] Write NLL by Example (progressive complexity)
- [ ] Write Runtime Operations guide
- [ ] Write Templates section
- [ ] Write Daemon + TopoViewer section

## Phase 2: Integration Testing Guide (day 2)

Create `docs/TESTING_GUIDE.md` — for Rust developers using nlink-lab in tests.

### Sections

1. **The `#[lab_test]` macro** — attribute, arguments, how it works
   (deploy → run test → destroy, auto-skip without root)

2. **File-based tests** — `#[lab_test("path/to/topology.nll")]`

3. **Builder-based tests** — `#[lab_test(topology = my_fn)]` with
   programmatic topology construction

4. **Assertions** — common patterns:
   - `lab.exec()` to verify addresses, routes, firewall rules
   - Ping connectivity checks
   - Process output inspection

5. **CI setup** — running tests in GitHub Actions with `sudo`,
   required capabilities, kernel version constraints

6. **Best practices** — unique lab names, cleanup, test isolation

### Tasks

- [ ] Write `#[lab_test]` reference
- [ ] Write file-based and builder-based examples
- [ ] Write CI setup guide (GitHub Actions example)
- [ ] Write best practices section

## Phase 3: Troubleshooting Guide (day 3)

Create `docs/TROUBLESHOOTING.md` — common errors and how to fix them.

### Sections

1. **Permission errors** — "operation not permitted" → need root or
   CAP_NET_ADMIN

2. **Namespace already exists** — stale state from crashed deploy →
   `sudo nlink-lab destroy --force`

3. **NLL parse errors** — reading miette diagnostics, common syntax mistakes

4. **Deploy failures** — namespace creation, veth limits, address conflicts

5. **Container errors** — Docker/Podman not found, image pull failures,
   `--network=none` requirements

6. **State corruption** — `~/.nlink-lab/` directory, manual cleanup

7. **Kernel requirements** — minimum kernel version for features
   (WireGuard 5.6+, VXLAN, nftables)

8. **Performance** — large topologies, namespace limits, file descriptor limits

### Tasks

- [ ] Write permission and privilege section
- [ ] Write namespace cleanup section
- [ ] Write NLL parse error section
- [ ] Write deploy failure section
- [ ] Write container troubleshooting section
- [ ] Write state corruption recovery section

## Phase 4: Man Page (day 3-4)

Generate a man page from clap metadata.

### Approach

Use `clap_mangen` to generate `nlink-lab.1` from the CLI struct.
Add a build script or a `xtask` command to regenerate it.

### Tasks

- [ ] Add `clap_mangen` dependency
- [ ] Create man page generation script
- [ ] Generate `nlink-lab.1`
- [ ] Add install instructions for man page

## Progress

### Phase 1: User Guide
- [x] Installation
- [x] Your First Lab
- [x] NLL by Example
- [x] Runtime Operations
- [x] Templates
- [x] Daemon + TopoViewer

### Phase 2: Integration Testing Guide
- [x] `#[lab_test]` reference
- [x] File + builder examples
- [x] CI setup
- [x] Best practices

### Phase 3: Troubleshooting Guide
- [x] Permission errors
- [x] Namespace cleanup
- [x] NLL parse errors
- [x] Deploy failures
- [x] Container troubleshooting
- [x] State corruption

### Phase 4: Man Page
- [ ] clap_mangen integration
- [ ] Generate man page
