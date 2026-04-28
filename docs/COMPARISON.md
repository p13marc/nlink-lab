# nlink-lab vs containerlab

Both build Linux network labs from a config file. They target
**different jobs**.

This page is honest about both. Don't expect to be told nlink-lab
is "better" — for some workloads it isn't, and pretending otherwise
wastes your time.

## 30-second decision

Use **[containerlab](https://containerlab.dev)** if any of:

- Your lab needs to run a **vendor NOS image** (Cisco cEOS,
  Nokia SR Linux, Juniper vMX/vJunos/cRPD, Palo Alto, Fortinet,
  …). containerlab is the de facto tool here; it's not a fight
  you should pick.
- You want a **web UI** for inspecting topology and live state.
- You need **multi-host clustering** (deploy a single lab across
  multiple hosts via inventory).
- Your team has **Ansible/Terraform automation** built around
  containerlab's YAML and the lab integration with that pipeline
  is more important than DSL ergonomics.

Use **nlink-lab** if any of:

- Your topology needs **deep Linux networking primitives**:
  per-destination netem on a shared L2, custom HTB / flower /
  u32 TC chains, VRF + WireGuard composition, macvlan/ipvlan to
  host NICs, custom nftables rulesets per node.
- You want a **Rust-native testing API** —
  `#[lab_test]` deploys, runs assertions, tears down inside
  `cargo test`, with no Docker daemon.
- You can't (or don't want to) **run a Docker daemon** —
  locked-down CI, embedded host, unprivileged rootless container,
  CIs without nested virtualization.
- Your DSL needs **loops, parametric imports, glob patterns, IP
  arithmetic** without YAML's quote-and-anchor gymnastics.
- Your impairment use case includes **distance-dependent /
  per-pair / shared-segment** scenarios — modeling radio,
  satellite, or geo-distributed paths.

If neither bucket fits cleanly, you might want both — see
[when to use both](#when-to-use-both).

## Capability matrix

Where this page diverges from containerlab's own docs, it's
because the comparison is honest about both wins and losses.

| | containerlab | nlink-lab |
|---|---|---|
| Vendor NOS support | ✅ 80+ images | ❌ pure Linux only |
| Generic Linux nodes | ✅ via `kind: linux` | ✅ native, first-class |
| Per-link netem | ✅ post-deploy via `tools netem` | ✅ inline in topology |
| **Per-pair netem on shared L2** | ❌ structural — Docker bridge → one egress queue | ✅ `PerPeerImpairer` (HTB+netem+flower) |
| Asymmetric (one-direction) impairment | ⚠️ workaround | ✅ first-class `->` / `<-` syntax |
| TC depth: HTB hierarchy + flower / u32 | ⚠️ raw `exec:` blocks | ✅ first-class |
| nftables rules per node | ⚠️ raw `exec:` | ✅ first-class, written via netlink |
| WireGuard with auto-keygen | ❌ — vendor image needed | ✅ first-class |
| VRF | ❌ — vendor image needed | ✅ first-class |
| macvlan / ipvlan to host NIC | ⚠️ kind-specific | ✅ first-class |
| VXLAN | ⚠️ kind-specific | ✅ first-class |
| Bridge VLAN filtering (802.1Q) | ⚠️ kind-specific | ✅ first-class |
| Multi-host clustering | ✅ via `clab tools cert` + inventory | ❌ single-host |
| Web UI / topology graph | ✅ `clab graph` web view | ⚠️ Dot/ASCII output only |
| Save / restore live state | ✅ `clab save` | ⚠️ via NLL re-deploy; lab archive in [Plan 153](plans/153-export-import.md) |
| Reconcile (apply config diff) | ⚠️ partial | ✅ `apply` (full coverage in [Plan 152](plans/152-apply-reconcile.md)) |
| Library API | Go (undocumented) | Rust (first-class, `#[lab_test]`) |
| DSL features | static YAML + Go templates | NLL: loops, imports, parametric, glob, arithmetic |
| Deploy speed | seconds (Docker pull + boot) | sub-second |
| CI footprint | Docker daemon | namespace + `CAP_NET_ADMIN`+`CAP_SYS_ADMIN` |
| Topology validation | YAML-schema via clab | 20-rule pre-deploy validator with miette source spans |
| Packet capture | external (`tcpdump` exec) | built-in `capture` (zero-copy via netring, BPF filter) |
| Healthcheck-gated startup | ✅ Docker healthchecks | ✅ `depends-on` + healthcheck polling (containers and processes) |
| Performance benchmarks in topology | ⚠️ external | ✅ `benchmark` block with `assert avg below 50ms` |
| Wi-Fi emulation | ❌ | ✅ via mac80211_hwsim (`wifi { mode ap }`) |
| Scenario engine (timed fault injection) | ⚠️ external script | ✅ `scenario` block with `at 5s { down node:iface }` |
| Status: maturity / community | mature, large ecosystem | beta, single-maintainer |
| Bus factor | Nokia-backed; many contributors | 1 (single author) |

## Where the architectural differences come from

### Container model vs. namespace model

containerlab assumes every node is a Docker container. Pros:
single bridge for the lab network, image distribution, vendor NOS
support. Cons: every node carries a container's overhead, the
network model is constrained to what Docker exposes (point-to-point
veths, bridges with no VLAN filtering by default, no per-pair TC).

nlink-lab uses **raw Linux network namespaces by default**, with
optional Docker/Podman containers when a node needs an image. Pros:
no Docker overhead, full kernel networking surface, sub-second
deploy. Cons: no vendor NOS, no Docker network plugin ecosystem.

Concretely: this is why per-pair netem on a shared bridge works in
nlink-lab and not in containerlab. The kernel's HTB+netem+flower
combination is one of those things Docker's network model doesn't
expose, and you can't fix it with a config file.

### YAML vs. NLL

containerlab's `.clab.yml` is YAML. Generation happens via Go
templates outside the YAML, processed by `clab generate`.

nlink-lab's NLL is a parsed DSL with loops, imports, parametric
modules, IP arithmetic, conditional blocks, and miette-backed
error reporting. The cost: a learning curve unfamiliar from YAML.
The benefit: 12-node satellite mesh in 25 lines, with arithmetic
and modulo wrap (see
[satellite-mesh recipe](cookbook/satellite-mesh.md)).

YAML excels at **readability for static, hand-written topologies**.
NLL excels at **expressing repeating structure, parameterized
deployments, and computed addressing**. Both are valid.

### CLI-first vs. library-first

containerlab is a CLI; the Go library API exists but isn't part of
the public contract.

nlink-lab is a library; the CLI is a thin wrapper. `#[lab_test]`
lets you write integration tests as ordinary `cargo test` runs.
For Rust shops, this is a real win; for Go shops or
language-agnostic teams, it's neutral.

## Side-by-side example

A 3-spine 4-leaf datacenter fabric with point-to-point /30 links.

### containerlab (`.clab.yml`)

Approximately 90 lines (per-node block × 7 nodes, per-link block ×
12 links, all hand-written or generated via `clab generate
spine-leaf-fabric`):

```yaml
name: dc
topology:
  nodes:
    spine1: { kind: linux, image: nicolaka/netshoot }
    spine2: { kind: linux, image: nicolaka/netshoot }
    spine3: { kind: linux, image: nicolaka/netshoot }
    leaf1:  { kind: linux, image: nicolaka/netshoot }
    leaf2:  { kind: linux, image: nicolaka/netshoot }
    leaf3:  { kind: linux, image: nicolaka/netshoot }
    leaf4:  { kind: linux, image: nicolaka/netshoot }
  links:
    - endpoints: [spine1:eth1, leaf1:eth1]
    - endpoints: [spine1:eth2, leaf2:eth1]
    # ... 10 more, with addresses configured separately ...
```

(Addresses come from a `pre-deploy` script or per-node startup
config.)

### nlink-lab (NLL)

Approximately 25 lines:

```nll
lab "dc"
profile router { forward ipv4 }

for i in 1..3 { node spine${i} : router { lo 10.255.0.${i}/32 } }
for i in 1..4 { node leaf${i}  : router { lo 10.255.1.${i}/32 } }

pool fabric 10.0.0.0/16 /30

for s in 1..3 {
  for l in 1..4 {
    link spine${s}:eth${l} -- leaf${l}:eth${s} { pool fabric }
  }
}
```

The `for` loops and pool-based address allocation produce the same
topology in less than a third the lines. Modulo isn't needed here;
when it is, NLL has it.

## When to use both

There's no rule against having containerlab and nlink-lab in the
same project. A common pattern:

- **containerlab** runs the outer fabric — vendor NOS images,
  multi-host, Ansible-driven configuration.
- **nlink-lab** runs inside a `kind: linux` container that
  containerlab manages, providing fine-grained TC and namespace
  control for testing application behavior under realistic
  network conditions.

The boundary is clean: containerlab handles the things only it can
handle (vendor images, multi-host); nlink-lab handles everything
inside a Linux node.

## Migration

If you have a `.clab.yml` with `kind: linux` nodes and veth links,
the mechanical mapping is:

| containerlab | nlink-lab |
|---|---|
| `name: foo` | `lab "foo"` |
| `nodes.bar.kind: linux` | `node bar` |
| `nodes.bar.image: alpine` | `node bar image "alpine"` |
| `links.- endpoints: [a:e0, b:e0]` | `link a:e0 -- b:e0 { ... }` |
| node startup-config files | node `exec` blocks or `cmd` |
| `tools netem add` | `impair` (inline) |

For features containerlab doesn't have a counterpart for (per-pair
impair, VRF, WireGuard auto-keygen, parametric imports), see the
[cookbook](cookbook/).

## Honest limitations

Where nlink-lab is **worse** than containerlab today:

1. **Bus factor of 1.** Same author maintains nlink (the netlink
   library) and nlink-lab. If that author gets hit by a bus, both
   are blocked. containerlab is Nokia-backed with many
   contributors.
2. **No multi-host.** A 200-node lab on a single laptop works, but
   distributing across hosts (for actual scale) doesn't.
3. **No web UI.** `nlink-lab graph` outputs Dot or ASCII.
   No interactive view.
4. **Docs are younger.** containerlab has years of accumulated
   recipes, blog posts, conference talks. nlink-lab has the
   [cookbook](cookbook/) — comprehensive but newer.
5. **Save/restore is incomplete.** `nlink-lab apply` reconciles
   topology changes, but there's no `clab save` equivalent yet —
   see [Plan 153](plans/153-export-import.md).
6. **No vendor NOS support, ever.** This is a design decision, not
   a bug — the project doesn't compete in that space.

## TL;DR

If you're testing **vendor NOS software**, use containerlab.
If you're testing **anything else** in realistic Linux-native
network conditions, nlink-lab gives you depth containerlab
structurally cannot.

There's room for both.
