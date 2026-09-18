# Architecture (for contributors)

This document is for someone who wants to **change the codebase**:
add an NLL keyword, fix a deploy-step bug, hook in a new
diagnostic, or sanity-check a design before opening a PR. If you
just want to use nlink-lab, the [user guide](USER_GUIDE.md) is the
right place.

## Crate layout

```
nlink-lab/                        ← workspace root
├── crates/
│   ├── nlink-lab/                ← the core library
│   │   ├── src/
│   │   │   ├── lib.rs            re-exports + module docs
│   │   │   ├── types.rs          Topology, Node, Link, Network, ...
│   │   │   ├── error.rs          Error / Result, miette diagnostics
│   │   │   ├── builder.rs        Programmatic Topology builder DSL
│   │   │   ├── parser/
│   │   │   │   └── nll/
│   │   │   │       ├── lexer.rs   logos-based, typed tokens
│   │   │   │       ├── ast.rs     untyped AST (pre-lowering)
│   │   │   │       ├── parser.rs  recursive-descent parser → AST
│   │   │   │       ├── value.rs   Val<T>: typed + spanned literals
│   │   │   │       └── lower.rs   AST → Topology, imports, loops
│   │   │   ├── validator.rs      53-rule validator (stable ids)
│   │   │   ├── lint.rs           style hints (`lint`)
│   │   │   ├── fmt.rs            NLL formatter (`fmt`)
│   │   │   ├── render.rs         Topology → flat NLL serializer
│   │   │   ├── deploy/           plan + execute (see below)
│   │   │   ├── running.rs        RunningLab — interact with deployed lab
│   │   │   ├── state.rs          Persistence (XDG state dir, flock)
│   │   │   ├── events.rs         Lifecycle event log (events.ndjson)
│   │   │   ├── diff.rs           TopologyDiff — drives `apply`
│   │   │   ├── watch.rs          nftables + RTNETLINK drift watch
│   │   │   ├── scenario.rs       Timed fault-injection engine
│   │   │   ├── benchmark.rs      ping/iperf3 + assertions
│   │   │   ├── capture.rs        Packet capture (netring backend)
│   │   │   ├── dns.rs            /etc/hosts injection / removal
│   │   │   ├── frr.rs            FRR daemons (zebra, ospfd, bgpd)
│   │   │   ├── wifi.rs           hostapd/wpa_supplicant + hwsim
│   │   │   ├── container.rs      Docker / Podman wrapper
│   │   │   ├── cgroup.rs         cpu/memory limits for container nodes
│   │   │   ├── ns_exec.rs        setns + spawn (detached, reaped)
│   │   │   ├── netns_tag.rs      Ownership tag; the orphan reaper's gate
│   │   │   ├── proc_stat.rs      /proc reads that need root
│   │   │   ├── test_runner.rs    `nlink-lab test` (CI mode)
│   │   │   ├── helpers.rs        parse_cidr, parse_duration, ...
│   │   │   ├── ipfunc.rs         subnet() / host() NLL functions
│   │   │   ├── ipmap.rs          address bookkeeping
│   │   │   ├── subnet_pool.rs    named pools, loopback pools
│   │   │   └── templates/        Built-in `nlink-lab init` templates
│   │   ├── fuzz/                 NLL parser fuzz targets (not in workspace)
│   │   └── tests/                Integration tests (root-gated)
│   ├── nlink-lab-macros/         #[lab_test] proc macro
│   └── nlink-lab-shared/         Zenoh topics + metrics types
├── bins/
│   ├── lab/                      `nlink-lab` CLI (clap; cmd/<name>.rs each)
│   ├── nlink-lab-backend/        Zenoh backend daemon + HTTP endpoints
│   └── topoviewer/               Experimental desktop viewer (iced)
├── examples/                     49 .nll files, all parse-tested
├── flatpak/                      Viewer packaging (manifest, metainfo, icon)
├── docs/                         User-facing docs (this dir)
└── editors/                      VS Code / Neovim / Helix / Zed
```

Rough size, the way `just stats` counts it: **~73k lines** across
`crates/` and `bins/`, inline `#[cfg(test)]` modules included.

## The Topology pipeline

Every command that takes an NLL file walks this pipeline:

```
.nll file
   │
   │  parser/nll/lexer.rs  ──→  Token stream (typed: Duration, RateLit, Percent, Cidr, ...)
   ↓
parser/nll/parser.rs       ──→  ast::Document  (Statements, NodeDefs, NetworkDefs, …)
   │
   │  parser/nll/lower.rs  ──→  Imports resolved, for-loops expanded, vars interpolated, addresses computed
   ↓
types::Topology            ──→  The fully-resolved, flat, immutable form
   │
   │  validator.rs         ──→  54 rules (stable ids): CIDRs valid, endpoints exist, no cycles, ...
   ↓
ValidationResult            ──→  Errors block deploy; warnings reported but allowed
   │
   ↓                       (deploy commands only)
deploy::plan(topology)     ──→  Plan: an ordered list of Ops, no kernel calls
   │
   ↓
deploy::execute(plan)      ──→  Kernel ops via nlink (netlink), journalled
   │
   ↓
running::RunningLab        ──→  Handle to a live lab; `exec`, `spawn`, `apply`, `destroy`
```

Three abstractions you'll touch most:

- **`ast::*`** — temporary, untyped. The lexer's job ends here.
- **`types::Topology`** — the canonical, fully-resolved form.
  Everything downstream operates on this (deploy, render, diff,
  reconcile).
- **`RunningLab`** — owns kernel state. Drops to `destroy`.

## Deploy: plan, then execute

Deploy used to be one long numbered function. Since Plan 161 it is
two halves, which is what makes `apply` and `--dry-run` possible:

```
 deploy(t)       = execute(plan(t))
 apply(cur, des) = execute(Plan::diff(plan(cur), plan(des)))   # with purge
```

Everything under `deploy/plan/` is **pure** — it reads a
`Topology` and returns a `Plan` (an ordered list of `Op`s) without
touching the kernel. `deploy/apply.rs` is the only module that
touches the kernel or the host, and it journals an inverse for
every op it runs.

```
crates/nlink-lab/src/deploy/
  mod.rs        deploy(), apply(), compute_layered_diff(), plan_for();
                the per-node stack appliers (network / nftables / WireGuard)
  op.rs         NsRef (Root | Named | Container), Stage, Op, Plan::diff
  plan/         PURE planners — nothing here touches the kernel:
    topology.rs   namespaces, containers, hwsim, mgmt bridge, bridge
                  networks + member veths + VLANs, p2p veths, host-side
                  macvlan/ipvlan, links-up, sysctls, DNS overlays
    network.rs    topology_to_network_config (links/addresses/routes,
                  incl. VRF-table routes via RouteBuilder::table)
    nftables.rs   topology_to_nftables_config (firewall + NAT, one table)
    wireguard.rs  key material + WireguardConfig per node
    qdisc.rs      build_netem
    process.rs    depends_on order, container create options
  apply.rs      the ONLY kernel/host-touching module
  rollback.rs   Undo + Journal (persisted as journal.json)
```

### Stages

`execute` runs ops in `Stage` order. In an `apply`, removals run
first, in *reverse* stage order.

| # | Stage | What it creates |
|---|-------|-----------------|
| 1 | `Namespaces` | network namespaces and containers |
| 2 | `Hwsim` | `mac80211_hwsim` load + PHY moves |
| 3 | `MgmtBridge` | host-reachable mgmt bridge + per-node veth peers |
| 4 | `Networks` | bridge networks, member veths, VLANs |
| 5 | `Links` | point-to-point veth pairs |
| 6 | `HostLinks` | host macvlan/ipvlan moved into namespaces |
| 7 | `LinksUp` | bring nlink-lab-created interfaces up |
| 8 | `Sysctls` | per-node sysctls |
| 9 | `Stack` | the declarative per-node stack: links, addresses, routes, nftables, WireGuard |
| 10 | `Routes` | routes in non-main (VRF) tables, owned explicitly because nlink's purge only converges the main table (#83) |
| 11 | `Tc` | netem, per-pair impairments, rate limits |
| 12 | `Dns` | `/etc/hosts` and per-namespace `/etc` overlays |
| 13 | `RoutingDaemons` | FRR (zebra + ospfd/bgpd) — after DNS, before user processes, so services see converged routes (#65) |
| 14 | `Processes` | background processes and healthchecks |
| 15 | `Wifi` | hostapd / wpa_supplicant / mesh join |

Every netlink resource commits through nlink's declarative
`NetworkConfig` / `NftablesConfig` / `WireguardConfig` reconcile
paths — zero kernel calls when nothing changed. In apply mode the
network layer runs with `ApplyOptions::with_purge(true)`.

After the stages, `deploy()` writes the state file, records a
`Deployed` lifecycle event, and *then* runs any `validate { … }`
assertions. Assertions never fail the deploy: the results ride on
the returned `RunningLab` via `assertion_results()`, and
`deploy --strict` is what turns them into a non-zero exit.

`deploy --dry-run` prints the plan and stops before `execute`.

### Rollback semantics

`apply.rs` appends an `Undo` to a `Journal` for every op it
performs, persisted next to the lab's state as `journal.json`. The
journal is unwound:

- when `execute` returns an error mid-plan,
- on the **next** deploy, if a previous run died before finishing
  (a crash leaves the journal on disk),
- by `destroy --orphans`, which reaps host resources with no state
  file behind them.

Namespaces nlink-lab created carry an ownership tag under
`/run/nlink-lab/netns/<ns>` (`netns_tag.rs`), and the orphan
reaper only ever touches tagged ones — a namespace you made by
hand is never someone else's to delete.

### Live reconcile

`apply_diff` shares the declarative builders with the
initial-deploy path, so `apply` and `deploy` cannot drift apart.
`compute_layered_diff(running, desired)` is the preview half: it
walks every node, builds the same per-namespace `NetworkConfig` /
`NftablesConfig`, and emits one `LayeredDiff` bundle for
`apply --check` / `apply --dry-run`. The JSON form is **schema
v3**; the v1 `diff` / `layered_summary` /
`layered_summary_deprecated` fields were removed in 0.7.0 after
their deprecation window.

`nlink-lab watch <lab>` subscribes to every node's nftables and
RTNETLINK multicast and prints one line per drift — the way to
catch hand-edits that bypass `apply`.

### Concurrency

`state::lock(&lab_name)` uses `libc::flock()` on a file under the
state directory's `.locks/`. It is held for the duration of
`deploy`, `destroy`, and `apply`. Different labs use different
lock files and run in parallel without contention.

**Global state caveat**: a few subsystems mutate host-global state
without per-lab locks — `dns::inject_hosts` rewrites `/etc/hosts`,
the mac80211_hwsim module load is process-global, etc. Two
parallel deploys that both touch one of these surfaces can race.
The "Process & namespace model" section below has the full
inventory.

## Process & namespace model

Quick reference for harness writers and anyone reasoning about
spawned-process visibility. Targets bare namespace nodes —
container nodes follow docker/podman's conventions instead.

### Namespaces

A process spawned by `nlink-lab spawn` (or `nlink-lab exec`) into a
bare namespace node enters exactly **one** Linux namespace via
`setns(2)`:

| Flag             | Active? | Notes |
|------------------|---------|-------|
| `CLONE_NEWNET`   | always  | Network ns — the reason nlink-lab exists. Source: `crates/nlink/src/netlink/namespace.rs:405`. |
| `CLONE_NEWNS`    | sometimes | Only when `/etc/netns/<ns>/` holds overlay files (`dns hosts`). The mount ns is private to the spawned process; needed for the `/etc/hosts` bind-mount to be visible to the child without polluting the host. Best effort: where the runtime denies `unshare`/`mount`/the sysfs remount (container job profiles), the process still runs in the network namespace with the host `/etc` (`crates/nlink-lab/src/ns_exec.rs`). |
| `CLONE_NEWPID`   | **no**  | PIDs are shared with the host. **`host_pid == ns_pid` for every spawned process.** |
| `CLONE_NEWUTS`   | no      | Hostname/domainname inherited from host. |
| `CLONE_NEWIPC`   | no      | SysV IPC, POSIX message queues shared with host. |
| `CLONE_NEWUSER`  | no      | No UID mapping. Root in the namespace is root on the host. |

### Lifetime

Background processes (`run … background` in the topology, `nlink-lab
spawn`) are started **detached**: `ns_exec::spawn_detached` double-forks
— an intermediate child calls `setsid(2)`, forks the real process and
exits at once — so nlink-lab never has a child to reap (no zombies,
whatever the deploying process does next) and the process is reparented
to init, outliving the terminal session that deployed the lab. Spawns
that need an exit code (`spawn_detached_reaped`, used by `spawn` and
`run … background`) add one level: the intermediate forks a *reaper*
before exiting, and that reaper — init's child, holding no fd of the
caller's — is the real process's parent, waits for it and writes
`<log>.rc`, which `ps` and `ProcessExited` read. The pid
handed back is the real process; it is recorded together with its
`/proc/<pid>/stat` start time, and every later signal (`destroy`,
`kill`, an `apply` that edits or removes the `run` line) verifies that
start time first. Foreground `exec`s stay attached and are waited on.

### UID

`nlink-lab` enforces root via `check_root` before any deploy / exec
/ spawn that touches netlink. Spawned processes inherit the
caller's UID — which is always root in practice. Without
`CLONE_NEWUSER`, this is *real* root: no UID mapping, full
capabilities.

### `/proc` visibility from the host

Without `CLONE_NEWPID`, the host's `/proc` shows every spawned
process. Permissions follow the standard kernel rules — they don't
change inside the namespace:

| Path                      | Readable from host non-root? | Why |
|---------------------------|------------------------------|-----|
| `/proc/<pid>/stat`        | yes (mode 0444)              | always world-readable. |
| `/proc/<pid>/status`      | yes (mode 0444)              | always world-readable. |
| `/proc/<pid>/cmdline`     | yes (mode 0444)              | always world-readable. |
| `/proc/<pid>/comm`        | yes (mode 0444)              | always world-readable. |
| `/proc/<pid>/fd/`         | **no** (mode 0700, root)     | listing requires UID match — the spawned process is root, you are not. |
| `/proc/<pid>/net/tcp`     | yes (mode 0444)              | reads the *netns*'s socket table, not the host's, when the reading process is in the same netns. From the host, this is the host's table. |

If you need to read `fd/` or other UID-restricted paths from a
non-root host shell, route the read through `nlink-lab` itself:

```bash
sudo nlink-lab proc-stat <lab> <node> <pid> --json
sudo nlink-lab exec <lab> <node> -- ls /proc/<pid>/fd
```

`proc-stat` (Plan 157 PR C) exists specifically to abstract over
the permission gymnastics; prefer it over hand-rolled `/proc`
parsing.

### Host PID vs namespace PID

Equal. **Always.** No exceptions today.

`nlink-lab spawn --json` returns `{ "pid": N, "host_pid": N, ... }`
where `pid` and `host_pid` are aliases for the same value. The
`host_pid` field exists for forward compatibility — if a future
version of nlink-lab adds `CLONE_NEWPID`, an `ns_pid` field will
appear alongside it. Until then, code that reads either is
correct.

### Globally-shared state (the parallel-deploy caveats)

Despite per-lab `flock` (see "Concurrency" above), some subsystems
touch host-global state without coordinating across labs. Two
parallel deploys that both exercise one of these can race:

- **`/etc/hosts`** — `crates/nlink-lab/src/dns.rs` rewrites the
  managed section non-atomically across labs. Only matters when
  `dns hosts` is set in the topology.
- **`mac80211_hwsim`** — kernel module load is process-global.
  Multiple Wi-Fi labs share the same hwsim radio pool.
- **Default network namespace** — `mgmt host-reachable` adds a
  bridge to the *host* network namespace; the bridge name is
  hash-derived per lab, so no name collision, but allocation of
  bridge IPs from a shared subnet is not coordinated.
- **The `nlink-lab` process pool** — `nlink-lab status --scan`
  walks `/run/netns` and `ip link show` from the host's POV; it
  doesn't take a global lock, so two `--scan` invocations can
  observe inconsistent intermediate states. Reads only — no
  mutation race.

Per-lab interface name allocation (`nl{hash8}` mgmt bridge,
`nm{hash8}<idx>` mgmt veth peers, `np{hash8}<idx>` per-network
veth peers, `nb{hash8}` bridges) is **not** a parallel-deploy
hazard: the hashes are djb2 over names which `--unique` makes
distinct. Collision probability over 100 parallel labs is ~5e-7.

## Adding a new NLL feature end-to-end

This is the contributor on-ramp. Worked example: **per-pair
impairment on shared networks** (Plan 128). The full diff is
`git show f366c0c`; this section walks through it as a tutorial.

### 1. Lexer (`parser/nll/lexer.rs`)

Question: do we need new tokens?

For per-pair impair, the answer was no — `delay`, `loss`, `rate`,
etc. are all existing keywords; `--` is `Token::DashDash` (already
used by point-to-point links); node names are just identifiers.

If you do need a new keyword, add it to the logos enum:

```rust
#[token("yourkeyword")]
YourKeyword,
```

Add to the `Display` impl and the alphabetical-help table at the
bottom of the file.

### 2. AST (`parser/nll/ast.rs`)

Add the structures the parser will produce. Per-pair impair added:

```rust
pub struct NetworkImpairDef {
    pub src: String,
    pub dst: String,
    pub props: ImpairProps,
    pub rate_cap: Option<String>,
}

// And an `impairments: Vec<NetworkImpairDef>` field on NetworkDef.
```

Keep AST nodes string-typed where the parser uses string forms
(durations like `"50ms"` are still strings here — they get parsed
to `Duration` at lower time).

### 3. Parser (`parser/nll/parser.rs`)

Hook into the existing block parser. For per-pair impair, this
went into `parse_network`:

```rust
} else if eat(tokens, pos, &Token::Impair) {
    net.impairments.push(parse_network_impair(tokens, pos)?);
}
```

Plus a new `parse_network_impair()` function that handles the
inner block.

For features that should be expanded inside `for` loops at parse
time, mirror the pattern in `parse_network_for()` (added in Plan
151): parse the body once, then expand for every loop value with
`interpolate()` substitution.

### 4. Lower (`parser/nll/lower.rs`)

Convert AST → typed runtime. For per-pair impair:

```rust
for imp in &net.impairments {
    network.impairments.push(types::NetworkImpairment {
        src: imp.src.clone(),
        dst: imp.dst.clone(),
        impairment: lower_impair_props(&imp.props),
        rate_cap: imp.rate_cap.clone(),
    });
}
```

Also extend `interpolate_network()` so top-level `let` variables
substitute into the new fields.

### 5. Types (`types.rs`)

The runtime form. Per-pair impair added:

```rust
pub struct NetworkImpairment {
    pub src: String,
    pub dst: String,
    pub impairment: Impairment,
    pub rate_cap: Option<String>,
}

// And `impairments: Vec<NetworkImpairment>` on `Network`.
```

Derive `Debug, Clone, Serialize, Deserialize`. The serialization
form is what state files persist.

### 6. Validator (`validator.rs`)

Add the rules the new feature requires. Per-pair impair added:

```rust
// Inside validate_impairment_refs():
//   - "network-impair-self-pair" — src != dst
//   - "network-impair-member"   — both src and dst are members
//   - "network-impair-needs-subnet" — network must have a subnet
```

Each rule emits a `ValidationIssue` with severity, rule name,
location, and message. Tests for these go in `validator::tests`.

### 7. Renderer (`render.rs`)

So `nlink-lab render` round-trips. Per-pair impair added:

```rust
for imp in &net.impairments {
    write!(out, "  impair {} -- {} {{", imp.src, imp.dst)?;
    // ... write props ...
    out.push_str(" }\n");
}
```

The round-trip property is: `parse(render(t)) == t` (as a
Topology). It's not currently a test invariant for every feature,
but you should hand-check it for non-trivial additions.

### 8. Deploy (`deploy.rs`)

The actual kernel op. Per-pair impair added a Step 14b:

```rust
async fn apply_network_impairments(
    topology: &Topology,
    node_handles: &HashMap<String, NodeHandle>,
) -> Result<()> {
    // Group rules by source node.
    // For each (network, src), build a PerPeerImpairer.
    // Resolve dst IPs from network's auto-assigned subnet.
    // impairer.apply(&conn).await
}
```

Use existing helper patterns (`build_netem`, `node_handle_for`,
`Connection<Route>`). The `nlink` upstream is the right place for
new TC primitives — file an issue there before adding netlink
plumbing in nlink-lab.

### 9. Tests

Three layers:

- **Unit tests** for parser, lower, validator, render. In the
  same file as the code (`#[cfg(test)] mod tests`). Should not
  require root.
- **Integration tests** in `crates/nlink-lab/tests/`. Root-gated
  via `#[ignore]`; CI flips them on with `--include-ignored` on
  privileged runners.
- **Doc-examples**: any NLL snippet in
  `docs/cookbook/*.md` should be a real file in
  `examples/cookbook/`, picked up by
  `test_all_nll_examples_parse` (in `lower.rs`).

### 10. Documentation

| File | What |
|------|------|
| `CLAUDE.md` | Type list + feature paragraph + deploy-sequence list |
| `docs/NLL_DSL_DESIGN.md` | Syntax + constraints + examples |
| `docs/cookbook/<recipe>.md` | If the feature deserves a worked recipe |
| `examples/cookbook/<recipe>.nll` | Paired runnable example |
| `docs/cli/<cmd>.md` | If a CLI flag changed |

### 11. Plan file

Per-pair impair lived as `docs/plans/128-...md` with:

- Problem statement
- Proposed approach
- Test list
- File changes table

Plan files are removed once the feature is implemented (the doc
lives on as cookbook + reference). The plan's purpose is design
review.

## Where things live

When trying to fix a bug, start here:

| Symptom | First file to read |
|---------|--------------------|
| Parse error or surprising parse | `parser/nll/parser.rs` (look for the keyword) |
| AST → Topology mismatch | `parser/nll/lower.rs` |
| Validator rejects a valid topology (or accepts an invalid one) | `validator.rs` |
| Deploy fails in stage N | `deploy/plan/` for what was planned, `deploy/apply.rs` for what ran |
| `apply` reconciles wrong | `diff.rs` (diff engine) + `deploy/mod.rs:apply_diff()` |
| A crashed deploy left resources behind | `deploy/rollback.rs` + `netns_tag.rs` |
| Render round-trip drops a field | `render.rs` |
| Container nodes misbehave | `container.rs` |
| Spawned-process bookkeeping is wrong | `running.rs` + `state.rs` |
| Scenario timing off | `scenario.rs` |
| Benchmark assertion misfires | `benchmark.rs` |
| Wi-Fi setup fails | `wifi.rs` |

## Dependencies

| Dep | Purpose | Notes |
|-----|---------|-------|
| `nlink` | netlink (link, addr, route, neigh, TC, nftables, namespace, WG, XFRM) | Single upstream maintained by the same author. Bus factor 1. |
| `netring` | Zero-copy AF_PACKET TPACKET_V3 | Powers `capture`. |
| `tokio` | Async runtime | Everything async is `#[tokio::main]` or `#[tokio::test]`. |
| `clap` | CLI parsing | `derive` form. |
| `logos` | Lexer derive macro | Produces typed tokens. |
| `miette` | Pretty error diagnostics | Source spans, color, the `--help` line in errors. |
| `serde` + `toml` + `serde_json` | State serialization | JSON for `state.json` and `--json` output; TOML for the rendered `topology.toml`. |
| `thiserror` | Error enum derive | |
| `x25519-dalek` + `getrandom` | WireGuard keypairs | Used by lower.rs when `key auto`. |

## How nlink fits in

The boundary between nlink-lab and nlink:

- **nlink** owns the netlink wire format, message types, builders
  for typed configs (`HtbQdiscConfig`, `NetemConfig`,
  `FlowerFilter`, etc.), the connection abstraction
  (`Connection<Route>`, `Connection<Netfilter>`, ...), namespace
  handling, and high-level helpers (`PerHostLimiter`,
  `PerPeerImpairer`).
- **nlink-lab** owns the topology DSL, deploy sequence, scenario
  engine, benchmark runner, container management, and CLI.

When a new TC primitive is needed (e.g. per-pair impair before
0.15.1), the right place is to file an issue / PR in nlink first.
nlink-lab's deploy logic stays declarative; the netlink plumbing
lives upstream where other consumers can also use it.

## CI

Forgejo Actions, in `.forgejo/workflows/`. Three files:

- **`ci.yml`** — every push and PR. Lanes: `fmt`, `clippy` (both
  feature edges, `-D warnings`), `test` across
  `{default, --all-features, --no-default-features}`, `stress`,
  `docs` (rustdoc with `-D warnings`, plus the docs gate below),
  `deny` (cargo-deny: advisories, licences, sources), `msrv`,
  `stable-latest`, `cli-smoke` (`scripts/cli-smoke.sh`, rootless),
  `tree-sitter` (grammar conformance against every example) and
  `fuzz`.
- **`integration.yml`** — the root-gated suite, on a self-hosted
  runner with `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`. These tests
  really create namespaces.
- **`release.yml`** — on a bare-semver tag. Asserts
  `Cargo.toml`'s version matches the tag, **refuses to release
  unless CI is green for that commit**, builds the binary tarball
  and the flatpak bundle, and attaches both plus `SHA256SUMS`.

`just ci` mirrors the rootless half locally; `just test-integration`
runs the privileged half.

The doc gate lives in `crates/nlink-lab/tests/docs_examples.rs`
and does two things: every ```` ```nll ```` block under `docs/`
must parse and validate (opt out with ```` ```nll-ignore ````, or
```` ```nll-no-validate ```` for a fragment), and every relative
markdown link in `docs/` and `README.md` must resolve. If you add
a doc, that test is what catches the typo in its links.

## Fuzz harness

There's a fuzz target at `crates/nlink-lab/fuzz/` (excluded from
the workspace). It targets the NLL parser. To run:

```bash
cd crates/nlink-lab/fuzz
cargo +nightly fuzz run fuzz_parse
```

Findings should be added as unit tests (don't just commit a
corpus entry — the bug should be reproducible from `cargo test`).

## Style

Some non-obvious choices worth knowing:

- **No `.unwrap()` on user input.** Internal asserts are fine
  (`get_link()` of a name we just created). User-facing parsers
  must `Result`.
- **`map_err` to a domain Error variant** at every nlink call
  site. The user shouldn't see a `nlink::Error::InvalidMessage`
  with no context.
- **New deploy work goes in a planner, not in `apply.rs`.**
  `deploy/plan/` must stay pure — no kernel calls, no host reads —
  so `--dry-run` and `apply --check` keep telling the truth. If a
  stage is missing, add a `Stage` variant and update the table in
  this document and in `CLAUDE.md`.
- **`#[allow(dead_code)]` is rare.** One exists for a test
  helper; everything else gets removed if unused.
- **`unsafe` is only for libc syscalls** (`flock`, `kill`, fd
  conversion). 6 blocks total. New `unsafe` should justify itself
  in a comment.

## Where to ask questions

Before opening a PR for a non-trivial change:

- **Design questions**: open a discussion / draft a plan file in
  `docs/plans/`. The recent plans (128, 150–154) are good shape
  references.
- **nlink-side concerns**: file in [nlink](https://git.marcpardo.eu/marcpardo/nlink)
  directly. Plan 128 has a good example of nlink-lab proposing a
  helper to nlink.
- **Build/CI/tooling**: PR welcome; small fixes don't need a plan.

## Where to NOT touch (yet)

- The Zenoh backend daemon (`bins/nlink-lab-backend/`) is a
  parallel surface for live metrics. If you're adding a new
  topology feature, the daemon is downstream — don't co-evolve.
- The topoviewer GUI (`bins/topoviewer/`) is an experimental
  iced-based viewer, not on the supported-surface list — see
  [GUI.md](GUI.md). It has no integration tests, so treat a change
  there as unverified until someone runs it on a display.

## See also

- [INSTALL.md](INSTALL.md) — getting a build onto a host
- [USER_GUIDE.md](USER_GUIDE.md) — for end users
- [NLL_DSL_DESIGN.md](NLL_DSL_DESIGN.md) — the language itself
- [GUI.md](GUI.md) — backend, `top`, and the desktop viewer
- [COMPARISON.md](COMPARISON.md) — vs containerlab
- [plans/](plans/) — design proposals (active and historical)
