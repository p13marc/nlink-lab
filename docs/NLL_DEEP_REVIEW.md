# NLL Deep Review

A critical assessment of the NLL language — what works, what's broken,
what's inconsistent, and how to fix it. Permission to break backward
compatibility.

## 1. What NLL Gets Right

Before the critique: NLL's core design is strong.

- **`link A:eth0 -- B:eth0`** is genuinely excellent syntax. The visual `--`
  operator reads naturally, the endpoint notation is compact, and inline
  properties (addresses, impairments) keep related information together.

- **Typed tokens** (CIDR, Duration, Rate, Percent) catch errors at lex time.
  Writing `delay 10mss` fails immediately, not during deployment.

- **Declarative, non-Turing-complete**: `for` loops with finite bounds,
  `let` bindings — enough expressiveness without the "inner platform" trap.

- **miette diagnostics**: Parse errors show the source line with an arrow
  pointing to the problem. This is best-in-class for infrastructure DSLs.

- **Inline impairments**: `delay 10ms jitter 2ms` directly on links is
  unique among network lab tools. Containerlab's most-requested feature.

These are the foundation. The problems are in the details.

## 2. Inconsistencies

### 2.1 The String/Value Identity Crisis

NLL has no consistent rule for when values need quotes:

```nll
lab "my-lab"              # string: quoted
image "nginx:latest"      # string: quoted
cpu "0.5"                 # number: quoted (!)
memory "256m"             # quantity: quoted (!)
hostname "web-01"         # string: quoted
pull always               # enum: bare identifier
delay 10ms                # duration: bare typed literal
mtu 9000                  # integer: bare
forward ipv4              # keyword: bare
privileged                # flag: bare (no value at all)
```

The rule is: keywords and typed literals (Duration, Rate, Int, CIDR) are
bare; strings that could be confused with identifiers are quoted. But
`cpu "0.5"` is a string because `0.5` isn't a valid token (no float type
in the lexer). And `pull always` works because `always` lexes as an Ident.

**This is confusing.** A user writing `cpu 0.5` gets a parse error but
`mtu 9000` works fine. Why? Because one is a float and the other is an int.

**Fix: Add a float literal to the lexer.** Then `cpu 0.5` and `memory 512m`
(as a RateLit variant) work without quotes. Alternatively, make all
non-keyword values consistently quoted:

```nll
# Option A: Add float token (least disruptive)
cpu 0.5
memory 512m       # if lexer adds memory units (m, g) to RateLit
hostname "web-01" # still quoted because it's free-form text

# Option B: Everything quoted (most consistent)
cpu "0.5"
memory "512m"
hostname "web-01"
delay "10ms"      # breaking: currently bare
mtu "9000"        # breaking: currently bare
```

Option A is better. Keep typed literals bare; add float support.

### 2.2 Three Ways to Specify Addresses on Links

```nll
# 1. Explicit pair
link a:e0 -- b:e0 { 10.0.0.1/24 -- 10.0.0.2/24 }

# 2. Auto-assign from subnet
link a:e0 -- b:e0 { 10.0.0.0/30 }

# 3. No addresses
link a:e0 -- b:e0
```

The difference between 1 and 2 is the presence of `--` inside the CIDR
block. A user writing `10.0.0.1/24` (single address, with host bits set)
gets auto-assign behavior instead of an error. There's no way to assign
just one side.

**Fix**: Make the auto-assign syntax explicit:

```nll
# Explicit pair (existing)
link a:e0 -- b:e0 { 10.0.0.1/24 -- 10.0.0.2/24 }

# Auto-assign (explicit keyword)
link a:e0 -- b:e0 { subnet 10.0.0.0/30 }

# Single address is an error (currently silently auto-assigns)
```

This is a breaking change but eliminates ambiguity.

### 2.3 Impairment Specification Fragmentation

Same concept, three syntaxes, no clear guidance on when to use which:

```nll
# 1. Inline symmetric (in link block)
link a:e0 -- b:e0 { delay 10ms }

# 2. Inline asymmetric (in link block)
link a:e0 -- b:e0 { -> delay 5ms; <- delay 100ms }

# 3. Standalone statement
impair a:e0 delay 10ms
```

Inline impairments apply to both endpoints (symmetric) or one direction
(asymmetric). Standalone `impair` applies to one endpoint. But what
happens if you use both on the same endpoint?

```nll
link a:e0 -- b:e0 { delay 10ms }
impair a:e0 delay 50ms    # overrides? merges? last wins?
```

The answer: last wins during lowering. But this isn't documented.

**Fix**: Choose one model. The inline syntax is the right default. Make
standalone `impair` explicitly about runtime modification (different
semantic than deploy-time). Or remove standalone `impair` and only
support inline.

### 2.4 Container vs Namespace Node Distinction

The only way to distinguish a container node from a namespace node is
the presence of `image`:

```nll
node host                         # namespace
node host image "alpine:latest"   # container
```

But container-specific properties (cpu, memory, healthcheck, depends-on)
are accepted on ALL nodes by the parser. The validator checks some of
them (`container-requires-image`), but not all. A user writing:

```nll
node host {
    cpu "0.5"     # accepted by parser, rejected by validator
    memory "256m" # accepted by parser, rejected by validator
    privileged    # accepted by parser, NOT rejected by validator
}
```

**Fix**: Either make the parser reject container properties on non-container
nodes, or make all properties valid on all nodes (and have namespaces
ignore container-specific ones). The validator approach is correct but
incomplete.

## 3. Verbosity Pain Points

### 3.1 Route Boilerplate

The most repeated pattern across all examples:

```nll
node host1 { route default via 10.0.0.1 }
node host2 { route default via 10.0.1.1 }
node host3 { route default via 10.0.2.1 }
```

With cross-references:

```nll
node host1 { route default via ${router.eth0} }
```

This is better but still verbose. The `route default via` prefix appears
on almost every leaf node. Consider a shorthand:

```nll
node host1 { gateway ${router.eth0} }
# or even shorter:
node host1 via ${router.eth0}
```

### 3.2 Full-Mesh Link Generation

Generating full-mesh topologies requires O(n^2) link statements or nested
loops:

```nll
for i in 1..4 {
    for j in 1..4 {
        # Need i < j to avoid duplicates, but NLL has no conditionals
    }
}
```

Currently impossible without manually enumerating links. Consider:

```nll
mesh [n1, n2, n3, n4] { 10.0.0.0/16 /30 }
```

### 3.3 NodeDef Has Too Many Fields

The `NodeDef` AST struct has 20+ fields. Most are container-specific.
A namespace node uses maybe 3 of them:

```rust
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,
    pub image: Option<String>,         // container
    pub cmd: Option<Vec<String>>,      // container
    pub env: Vec<String>,              // container
    pub volumes: Vec<String>,          // container
    pub cpu: Option<String>,           // container
    pub memory: Option<String>,        // container
    pub privileged: bool,              // container
    pub cap_add: Vec<String>,          // container
    pub cap_drop: Vec<String>,         // container
    pub entrypoint: Option<String>,    // container
    pub hostname: Option<String>,      // container
    pub workdir: Option<String>,       // container
    pub labels: Vec<String>,           // container
    pub pull: Option<String>,          // container
    pub container_exec: Vec<String>,   // container
    pub healthcheck: Option<String>,   // container
    pub healthcheck_interval: Option<String>,  // container
    pub healthcheck_timeout: Option<String>,   // container
    pub startup_delay: Option<String>, // container
    pub env_file: Option<String>,      // container
    pub configs: Vec<(String, String)>,// container
    pub overlay: Option<String>,       // container
    pub depends_on: Vec<String>,       // container
    pub props: Vec<NodeProp>,          // generic (routes, firewall, etc.)
}
```

17 of 25 fields are container-only. This flat structure makes the code
harder to maintain and the syntax harder to understand.

**Fix**: Group container properties into a sub-struct:

```rust
pub struct NodeDef {
    pub name: String,
    pub profiles: Vec<String>,
    pub container: Option<ContainerDef>,  // only present if image is set
    pub props: Vec<NodeProp>,
}

pub struct ContainerDef {
    pub image: String,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    // ... etc
}
```

The NLL syntax wouldn't change — it's an internal refactor.

### 3.4 Healthcheck Keyword Hacks

The healthcheck block parser reuses `Token::Delay` for "interval" and has
a string-matching fallback:

```rust
Some(Token::Delay) => { // reuse "delay" as "interval"
    healthcheck_interval = Some(parse_value(tokens, pos)?);
}
Some(Token::Mtu) => { // reuse for timeout via ident
    healthcheck_timeout = Some(parse_value(tokens, pos)?);
}
_ => {
    let key = parse_value(tokens, pos)?;
    match key.as_str() {
        "interval" => ...,
        "timeout" => ...,
    }
}
```

This is a hack. `delay` means impairment delay, not polling interval.
`mtu` means network MTU, not health check timeout. Add proper tokens:
`Interval`, `Timeout`.

## 4. Fragile Areas

### 4.1 No Type Validation at Parse Time

The parser accepts any value for typed properties:

```nll
delay garbage     # lexes as Ident, parser accepts via parse_value()
jitter "not-a-duration"
loss badvalue
```

Validation happens during deployment when `parse_duration()` or
`parse_percent()` is called. But the error message points to the deploy
step, not the NLL source line.

**Fix**: Validate typed values at parse time. When `delay` expects a
Duration, only accept `Token::Duration`. Reject anything else immediately
with a source-located error.

### 4.2 Cross-References Are Strings, Not Symbols

`${router.eth0}` is expanded by string substitution. The parser doesn't
validate that `router` is a defined node or that `eth0` is a defined
interface. A typo produces a string `${router.eth0}` in the lowered
topology, which silently fails later.

**Fix**: After lowering, validate that all remaining `${...}` patterns
in routes and firewall rules have been resolved. Emit an error with the
source location if any remain unresolved.

### 4.3 Interpolation Depth Limitation

`${leaf${i}.eth0}` doesn't work — the lexer captures `${leaf${i}` as
one interpolation token (until the first `}`), leaving `.eth0}` dangling.
The datacenter-fabric example had to work around this:

```nll
# Can't write: route default via ${leaf${l}.eth0}
# Must write each node manually or use a different approach
```

**Fix**: Either support nested interpolation or provide a `concat()`
function: `${concat("leaf", i, ".eth0")}`. Or use a different delimiter
for cross-references: `@{leaf${i}.eth0}` where `@{}` is resolved after
`${}` expansion.

### 4.4 Defaults Scope Is Implicit

```nll
defaults link { mtu 9000 }
# ... 50 lines of topology ...
link a:e0 -- b:e0  # does this get mtu 9000?
```

Yes, because defaults are collected in the first pass and applied globally.
But the syntax reads like it should be position-dependent (like CSS
cascade). There's no way to clear defaults or scope them to a section.

**Fix**: Either make defaults explicitly global (document it clearly) or
add scoping:

```nll
# Option 1: Explicit global keyword
global defaults link { mtu 9000 }

# Option 2: Scoped defaults via block
with defaults link { mtu 9000 } {
    link a:e0 -- b:e0
    link b:e0 -- c:e0
}
```

Option 1 is simpler and sufficient.

## 5. Missing Features That Would Be Cool

### 5.1 Topology Patterns as First-Class Constructs

```nll
# Instead of manually building a spine-leaf:
spine-leaf fabric {
    spines 4
    leaves 8
    servers-per-leaf 2
    fabric-subnet 10.0.0.0/16 /31
    access-subnet 10.1.0.0/16 /24
}
```

This generates nodes, links, and addresses automatically. The user
overrides specific nodes if needed. Containerlab doesn't have this either —
it would be a genuine innovation.

### 5.2 Reachability Assertions

```nll
validate {
    reach host1 host2              # host1 can ping host2
    no-reach host1 host3           # firewall blocks host1 → host3
    path host1 host2 via router    # traffic goes through router
    latency host1 host2 < 50ms    # round-trip under 50ms
}
```

These are post-deploy checks declared in the topology. The deployer runs
them after setup and reports pass/fail. Currently this requires writing
`exec` commands in integration tests.

### 5.3 Named Subnets / Address Pools

```nll
pool fabric 10.0.0.0/16 /30
pool access 10.1.0.0/16 /24
pool mgmt 172.16.0.0/16 /24

link s1:e1 -- l1:e1 { pool fabric }   # auto-assigns from pool
link l1:e3 -- h1:e0 { pool access }
```

Eliminates manual address planning entirely. Subnets are allocated
sequentially from the pool. No duplicates possible.

### 5.4 Bidirectional Cross-References

Currently cross-references only resolve addresses:

```nll
route default via ${router.eth0}  # resolves to IP
```

Could also resolve MAC addresses, interface names, or node properties:

```nll
route default via ${router.eth0.ip}     # explicit: IP address
arp static ${router.eth0.mac}           # MAC address
sysctl "${router.hostname}"             # node hostname
```

### 5.5 Inline Topology Visualization

```nll
# Auto-generated ASCII art from the topology:
#
#   spine1 ──── leaf1 ──── server1
#     │           │
#   spine2 ──── leaf2 ──── server2
#
# (generated by `nlink-lab render --ascii`)
```

The `render` command already exists. Adding `--ascii` or `--dot` output
modes would help users visualize their topology without deploying.

### 5.6 Snapshot/Restore Points

```nll
lab "test" {
    snapshot after-deploy     # save state after initial deploy
    snapshot after-impairment # save after impairment changes
}
```

Allow restoring to a named snapshot: `nlink-lab restore test after-deploy`.

## 6. Syntax Rethink Proposals

### 6.1 Unified Property Syntax

Replace the mixed string/bare/keyword system with a consistent rule:

- **Keywords are always bare**: `forward`, `privileged`, `drop`, `accept`
- **Typed values are always bare**: `10ms`, `100mbit`, `0.1%`, `9000`, `10.0.0.1/24`
- **Free-form strings are always quoted**: `"nginx:latest"`, `"web-01"`, `"role=web"`
- **Add float literal to lexer**: `0.5`, `1.5` — currently requires quotes

This means `cpu 0.5` works (float), `memory 512m` works (if we add
memory-unit RateLit variants like `m`, `g`, `t`), and `hostname "web-01"`
stays quoted (free-form).

### 6.2 Explicit Container Block

Instead of flat container properties on NodeDef, use a dedicated block:

```nll
# Current (flat, 20+ fields on NodeDef)
node web image "nginx" {
    cpu "0.5"
    memory "256m"
    healthcheck "curl localhost"
    route default via ${router.eth0}
}

# Proposed (grouped)
node web {
    container "nginx" {
        cpu 0.5
        memory 512m
        healthcheck "curl localhost"
        cap-add [NET_ADMIN]
    }
    route default via ${router.eth0}
}
```

Benefits:
- Clear separation between network config (routes, firewall) and
  container config (cpu, memory, healthcheck)
- NodeDef stays lean
- Container properties are visually grouped
- Easy to add new container properties without polluting the node

### 6.3 Streamlined Firewall Syntax

```nll
# Current
firewall policy drop {
    accept ct established,related
    accept tcp dport 80 src 10.0.0.0/8
    accept udp dport 53
}

# Proposed (more concise, clearer structure)
firewall drop {
    allow established
    allow tcp:80 from 10.0.0.0/8
    allow udp:53
    allow icmp
    deny from 192.168.0.0/16
}
```

Changes:
- `policy` keyword dropped (redundant)
- `accept` → `allow`, `drop` → `deny` (more intuitive verbs)
- `tcp dport 80` → `tcp:80` (standard port notation)
- `ct established,related` → `established` (shorthand for common patterns)
- `src` → `from`, `dst` → `to` (more readable)
- `icmp` without a type number (allow all ICMP, not just type 8)

### 6.4 Implicit Default Route via Link Gateway

The most common pattern is a leaf node with a default route to the first
hop. This could be implicit:

```nll
# Current
node host { route default via ${router.eth0} }
link router:eth0 -- host:eth0 { 10.0.0.0/30 }

# Proposed: gateway keyword on the link
link router:eth0 -- host:eth0 {
    10.0.0.0/30
    gateway left    # host gets default route via router's IP
}
```

Or even more implicit — if a node has exactly one link and no explicit
routes, auto-add a default route via the peer:

```nll
node host    # no routes defined
link router:eth0 -- host:eth0 { 10.0.0.0/30 }
# host automatically gets: route default via 10.0.0.1
```

This would be opt-in via a `auto-gateway` flag on the lab or link.

## 7. Priority Matrix

| Proposal | Impact | Effort | Breaking |
|----------|--------|--------|----------|
| Add float literal | High | Low | No |
| Fix healthcheck keyword hacks | Medium | Low | No |
| Validate types at parse time | High | Medium | No |
| Error on unresolved cross-refs | High | Low | No |
| NodeDef container sub-struct | Low (internal) | Medium | No |
| Explicit `subnet` keyword | Medium | Low | Yes |
| Topology patterns (spine-leaf) | High | High | No |
| Named subnet pools | High | High | No |
| Reachability assertions | Medium | Medium | No |
| Unified firewall syntax | Medium | High | Yes |
| Container block syntax | Medium | High | Yes |
| Implicit default route | Low | Medium | No |
| Nested interpolation | Medium | High | No |
| Render --ascii | Medium | Medium | No |

## 8. Design Principles (Informed by Research)

After studying HCL, CUE, Dhall, Starlark, Jsonnet, and DSL design
literature, these principles should guide NLL's evolution:

1. **One obvious way to do each thing.** Three ways to specify addresses
   or impairments is one way too many. Choose the best one.

2. **Types, not strings.** `cpu 0.5` should be a float, not `cpu "0.5"`.
   `delay 10ms` is already right. Extend this to all values.

3. **Validate early, fail fast.** Parse-time validation with source-located
   errors. Don't defer to deployment.

4. **Composition over inheritance.** Profiles are right. Deep inheritance
   chains (profile inheriting from profile) should be avoided.

5. **Hermeticity.** Same topology file = same topology. No environment
   variables, no randomness, no system-dependent behavior.

6. **Non-Turing-complete by design.** `for` loops with finite bounds,
   `let` bindings, ternary expressions. No recursion, no closures, no
   general functions. This is a feature, not a limitation.

7. **Domain vocabulary.** Use networking terms: `link`, `node`, `delay`,
   `jitter`, `gateway`, `subnet`. Not programming terms: `resource`,
   `object`, `instance`, `template`.
