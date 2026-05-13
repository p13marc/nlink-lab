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
│   │   │   │       └── lower.rs   AST → Topology, imports, loops
│   │   │   ├── validator.rs      20-rule validator
│   │   │   ├── render.rs         Topology → flat NLL serializer
│   │   │   ├── deploy.rs         The 18-step deploy sequence
│   │   │   ├── running.rs        RunningLab — interact with deployed lab
│   │   │   ├── state.rs          Persistence (~/.nlink-lab/, flock)
│   │   │   ├── diff.rs           TopologyDiff — drives `apply`
│   │   │   ├── scenario.rs       Timed fault-injection engine
│   │   │   ├── benchmark.rs      ping/iperf3 + assertions
│   │   │   ├── capture.rs        Packet capture (netring backend)
│   │   │   ├── dns.rs            /etc/hosts injection / removal
│   │   │   ├── wifi.rs           hostapd/wpa_supplicant + hwsim
│   │   │   ├── container.rs      Docker / Podman wrapper
│   │   │   ├── test_runner.rs    `nlink-lab test` (CI mode)
│   │   │   ├── helpers.rs        parse_cidr, parse_duration, ...
│   │   │   ├── ipfunc.rs         subnet() / host() NLL functions
│   │   │   └── templates/        Built-in `nlink-lab init` templates
│   │   └── tests/                Integration tests (root-gated)
│   ├── nlink-lab-macros/         #[lab_test] proc macro
│   └── nlink-lab-shared/         Zenoh metrics types
├── bins/
│   ├── lab/                      `nlink-lab` CLI (clap)
│   ├── nlink-lab-backend/        Zenoh backend daemon
│   └── topoviewer/               Topology viewer (iced GUI)
├── examples/                     40 .nll files, all parse-tested
├── docs/                         User-facing docs (this dir)
└── editors/                      VS Code / Neovim / Helix / Zed
```

Source-of-truth count: **~22k LOC** across `crates/nlink-lab/src/`,
**~24k LOC** including the macros and shared crates.

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
   │  validator.rs         ──→  20 rules: CIDRs valid, endpoints exist, no cycles, ...
   ↓
ValidationResult            ──→  Errors block deploy; warnings reported but allowed
   │
   ↓                       (deploy commands only)
deploy.rs::deploy()        ──→  18-step kernel ops via nlink (netlink)
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

## The 18-step deploy sequence

`deploy.rs:deploy()` executes these in order. Each step uses
`nlink` (the netlink library) for the actual kernel operations.

```
 1. Parse topology file → Topology
 2. Validate (bail on errors)
 3. Create namespaces
 3d. Create host-reachable mgmt bridge (if `mgmt ... host-reachable`)
 4. Create bridge networks (if any)
 5. Create veth pairs spanning namespaces
 6. Create additional interfaces (vxlan, bond, vlan, wireguard)
 7. Assign interfaces to bridges/bonds
 8. Configure VLANs on bridge ports
 9. Set interface addresses
10. Bring interfaces up
11. Apply sysctls per namespace
12. Add routes per namespace
13. Apply nftables rules per namespace
14. Apply TC qdiscs/impairments per interface
14b. Apply per-pair network impairments (PerPeerImpairer)
15. Apply rate limits
15b. Inject /etc/hosts entries (if `dns hosts`)
16. Spawn background processes (topo-sorted by depends_on)
17. Run validation block (reach / no-reach / tcp-connect / ...)
18. Write state file
```

### Rollback semantics

Each step appends to a `Cleanup` struct (in `deploy.rs`). The
struct's `Drop` impl unwinds in reverse: kill spawned processes,
remove DNS injections, delete namespaces, etc. If `deploy()` panics
or returns an error mid-sequence, RAII drops the `Cleanup` and
unwinds.

The exception is **after step 17** (validation) and **before step
18** (state file write): if validation fails, the state file is
NOT written, but the kernel state is still up. The user has to
`destroy` explicitly. This is intentional — failing validation is
the kind of error a user might want to inspect before tearing
down. (`destroy --orphans` reaps it without a state file.)

### Concurrency

`state::lock(&lab_name)` uses `libc::flock()` on
`~/.nlink-lab/<name>/.lock`. Held for the duration of `deploy`,
`destroy`, and `apply`. Different labs have different lock files
and run in parallel without contention.

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
| `CLONE_NEWNS`    | sometimes | Only when `dns hosts` (or any `/etc/netns/<ns>/` overlay) is configured. The mount ns is private to the spawned process; needed for the `/etc/hosts` bind-mount to be visible to the child without polluting the host. Source: `namespace.rs:1016`. |
| `CLONE_NEWPID`   | **no**  | PIDs are shared with the host. **`host_pid == ns_pid` for every spawned process.** |
| `CLONE_NEWUTS`   | no      | Hostname/domainname inherited from host. |
| `CLONE_NEWIPC`   | no      | SysV IPC, POSIX message queues shared with host. |
| `CLONE_NEWUSER`  | no      | No UID mapping. Root in the namespace is root on the host. |

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
| Deploy fails at step N | `deploy.rs:deploy()` (steps numbered in comments) |
| `apply` reconciles wrong | `diff.rs` (diff engine) + `deploy.rs:apply_diff()` |
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
| `serde` + `toml` + `serde_json` | State serialization | TOML for state.json, JSON for `--json` output. |
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

Today: GitHub Actions (`.github/workflows/`). Gates:

- `cargo build --workspace`
- `cargo test -p nlink-lab --lib` (unit tests; no root needed)
- `cargo clippy --workspace --all-features -- --deny warnings`
- `cargo fmt --check`

Not yet wired (see **Plan 150 Phase D**):

- A doc-snippet parse test (every \`\`\`nll block in `docs/`
  parses).
- A link-check job for internal `docs/` references.

Privileged integration tests (root-gated via `#[ignore]`) need a
self-hosted runner with `CAP_NET_ADMIN`. They run via
`cargo test -- --ignored` and aren't on every PR yet.

## Fuzz harness

There's a fuzz target at `crates/nlink-lab/fuzz/` (excluded from
the workspace). It targets the NLL parser. To run:

```bash
cd crates/nlink-lab/fuzz
cargo +nightly fuzz run nll_parse
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
- **18 deploy steps are numbered in source comments**, e.g.
  `// ── Step 14b: Apply per-pair network impairments ──`.
  When adding a new step, update both the code comment and
  `CLAUDE.md`'s deploy-sequence list.
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
- **nlink-side concerns**: file in [nlink](https://github.com/p13marc/nlink)
  directly. Plan 128 has a good example of nlink-lab proposing a
  helper to nlink.
- **Build/CI/tooling**: PR welcome; small fixes don't need a plan.

## Where to NOT touch (yet)

- The Zenoh backend daemon (`bins/nlink-lab-backend/`) is a
  parallel surface for live metrics. If you're adding a new
  topology feature, the daemon is downstream — don't co-evolve.
- The topoviewer GUI (`bins/topoviewer/`) is an experimental
  iced-based viewer. Not yet on the supported-surface list.

## See also

- [USER_GUIDE.md](USER_GUIDE.md) — for end users
- [NLL_DSL_DESIGN.md](NLL_DSL_DESIGN.md) — the language itself
- [COMPARISON.md](COMPARISON.md) — vs containerlab
- [plans/](plans/) — design proposals (active and historical)
