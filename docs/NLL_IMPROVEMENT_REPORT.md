# NLL DSL Improvement Report

A detailed analysis of the NLL language — what works, what's missing, what other
tools do better, and concrete proposals for improvement.

## 1. Competitive Landscape

### How other tools define topologies

| Tool | Format | Loops | Variables | Impairments | Composition |
|------|--------|-------|-----------|-------------|-------------|
| **NLL** | Custom DSL | `for` ranges | `let` + `${}` | Inline, asymmetric | `import ... as` |
| **Containerlab** | YAML + Go templates | Go `range` | Go `{{ .Var }}` | None (post-deploy) | None |
| **Mininet** | Python API | Full Python | Full Python | `TCLink` kwargs | Python imports |
| **Kathara** | INI (`lab.conf`) | None | None | None (startup scripts) | None |
| **GNS3** | JSON (GUI-generated) | None | None | None | None |
| **Terraform/HCL** | HCL blocks | `for_each`, `count` | `var`, `local` | N/A | Modules with inputs/outputs |
| **NixOS** | Nix expressions | `map`, `genAttrs` | `let ... in` | N/A | Module merging |

### NLL's unique advantages

1. **Inline impairments** — no other tool supports `delay 10ms jitter 2ms` directly
   on link definitions. Containerlab's most-requested feature (#1398).
2. **Asymmetric impairments** — `-> delay 5ms` / `<- delay 100ms` on a single link.
   No equivalent in any tool surveyed.
3. **Typed literals** — `10ms`, `100mbit`, `0.1%` are first-class tokens, not strings.
   Catches typos at parse time.
4. **No runtime dependency** — pure namespaces, millisecond startup. Containerlab
   requires Docker/Podman + image pulls.

### What competitors do better

| Feature | Best implementation | Gap in NLL |
|---------|-------------------|------------|
| Multi-tier defaults | Containerlab: `defaults → kinds → groups → nodes` | NLL has only single-level profile inheritance |
| Parametric composition | Terraform modules: `module "vpc" { source = "./vpc"; cidr = "10.0.0.0/16" }` | NLL imports are static — no parameter passing |
| Cross-references | HCL: `route via ${aws_instance.gw.private_ip}` | NLL can't reference another node's address |
| For-expressions | HCL: `[for s in var.list : upper(s)]` | NLL `for` is statement-only, not value-producing |
| Auto-addressing | None (gap in all tools) | Opportunity to be first |
| Magic variables | Containerlab: `__clabNodeName__` | NLL has no auto-available contextual variables |
| Conditional expressions | HCL: `condition ? val_a : val_b` | NLL has no conditionals |
| Expanded topology preview | Containerlab users request "save rendered template" (#3013) | NLL has no `render` command |

## 2. Current NLL Internals

### Parser architecture

- **Lexer**: logos-based, 45 keywords, 8 operators, 11 literal types
- **Parser**: Recursive descent, LL(1), position-based (`&mut usize`)
- **AST**: 27 node types (6 statement variants, 10 node-property variants)
- **Lowering**: Three-phase — collect profiles/vars, expand loops, lower to Topology
- **Interpolation**: `${expr}` with single binary operation (add, sub, mul, div)

### What the Topology type supports but NLL doesn't expose

| Capability | Type field | NLL status |
|-----------|-----------|------------|
| Bond interfaces | `InterfaceKind::Bond` | Not exposed |
| VLAN sub-interfaces | `InterfaceConfig.parent` | Token exists, no grammar |
| Network subnet auto-assign | `Network.subnet` | Field exists, always None |
| Interface-level addresses | `InterfaceConfig.addresses` | Only via dummy/vxlan/lo |
| Bond members | `InterfaceConfig.members` | Not exposed |

### Interpolation limitations

- Only single binary operations: `${i + 1}`, `${i * 2}`
- No modulo (`%`), no bitwise, no chaining (`${a + b + c}`)
- No string functions (upper, lower, join, replace)
- No conditional/ternary
- Variable scope is flat — inner loops shadow, don't nest
- Left/right operands must be variable names or integer literals

### Parser constraints affecting new features

- No multi-token lookahead (would need refactor for complex expressions)
- Firewall match expressions are whitelisted — adding `src`/`dst` requires
  updating `parse_match_expr()`
- For-loop ranges are integers only — no list iteration
- String interpolation in addresses needs careful regex priority ordering

## 3. Proposals

### Tier 1: High impact, low-medium effort

These are concrete, implementable improvements that would meaningfully reduce
boilerplate and improve expressiveness.

#### 3.1 Subnet auto-assignment on links

**Problem**: Every point-to-point link requires manually specifying both addresses.
For /30 subnets (standard in datacenter fabrics), this is pure boilerplate.

```nll
# Current — 4 fabric links, each with redundant addressing
link spine1:eth1 -- leaf1:eth1 { 10.0.11.1/30 -- 10.0.11.2/30  mtu 9000 }
link spine1:eth2 -- leaf2:eth1 { 10.0.12.1/30 -- 10.0.12.2/30  mtu 9000 }
link spine2:eth1 -- leaf1:eth2 { 10.0.21.1/30 -- 10.0.21.2/30  mtu 9000 }
link spine2:eth2 -- leaf2:eth2 { 10.0.22.1/30 -- 10.0.22.2/30  mtu 9000 }
```

**Proposal**: Allow a single subnet on a link — auto-assign .1 to left, .2 to right.

```nll
# Proposed
link spine1:eth1 -- leaf1:eth1 { 10.0.11.0/30  mtu 9000 }
link spine1:eth2 -- leaf2:eth1 { 10.0.12.0/30  mtu 9000 }
```

**Implementation**: In the parser, if only one CIDR token appears (no `--`), treat
it as a subnet and derive the two endpoint addresses from the network address.
For /30: `.1/30` and `.2/30`. For /31: `.0/31` and `.1/31` (RFC 3021).
For /24+: `.1/prefix` and `.2/prefix`.

**Effort**: Low — parser change (detect single vs. paired CIDR), lowering change
(split subnet into two addresses).

#### 3.2 Modulo operator in interpolation

**Problem**: Spine-leaf fabrics and fat-tree topologies need modulo for mapping
nodes to tiers. Currently impossible without manual expansion.

```nll
# Can't express: connect leaf[i] to spine[i % num_spines]
for i in 0..7 {
    link leaf${i}:uplink -- spine${i % 2}:eth${i} { ... }
}
```

**Proposal**: Add `%` operator to interpolation expressions.

**Implementation**: Add `'%' => left % right` to `eval_expr()` in lower.rs.
Single line of code.

**Effort**: Trivial — one match arm in `eval_expr()`.

#### 3.3 List iteration in for-loops

**Problem**: For-loops only support integer ranges. Can't iterate over a list of
names, which forces awkward patterns with integer-indexed naming.

```nll
# Current — awkward integer-indexed names
for i in 1..3 { node server${i} }

# Desired — iterate over meaningful names
for role in [web, api, db] {
    node ${role} { route default via 10.0.0.1 }
}
```

**Proposal**: Extend `for` syntax to accept bracketed string lists.

```
for_stmt = "for" IDENT "in" ( INT ".." INT | "[" value ("," value)* "]" ) block
```

**Implementation**:
- Lexer: No change (brackets and commas already tokenized)
- AST: Add `ForLoop::List(Vec<String>)` variant alongside `Range(i64, i64)`
- Parser: Check for `[` after `in` to dispatch to list parsing
- Lower: Expand list iteration like range iteration but using string values

**Effort**: Medium — AST change, parser addition, lowering addition.

#### 3.4 Contextual auto-variables

**Problem**: Inside for-loops and node blocks, users often need the current node
name or loop index. Currently must manually track with `let` variables.

```nll
# Current — must manually compute
for i in 1..4 {
    node host${i} {
        route default via 10.${i}.0.1
    }
}

# With auto-variables
for i in 1..4 {
    node host${i} {
        route default via 10.${loop.index}.0.1
    }
}
```

**Proposal**: Inject these variables automatically:

| Variable | Available in | Value |
|----------|-------------|-------|
| `${loop.index}` | `for` body | Current iteration value (same as loop var) |
| `${loop.first}` | `for` body | `true` on first iteration |
| `${loop.last}` | `for` body | `true` on last iteration |
| `${lab.name}` | Everywhere after `lab` | Lab name string |
| `${lab.prefix}` | Everywhere after `lab` | Lab prefix string |

**Implementation**: Inject into variable map at lowering time. `loop.index` is
already available as the loop variable, but `loop.first`/`loop.last` require
checking `i == start` and `i == end`.

**Effort**: Low — a few lines in `lower_for_loop()`.

#### 3.5 Firewall `src`/`dst` matching

**Problem**: The firewall match expression parser only accepts `ct`, `tcp`/`udp`
`dport`/`sport`, `icmp`, and `mark`. Can't filter by source or destination IP,
which is basic firewall functionality.

```nll
# Current — no way to restrict by source
firewall policy drop {
    accept tcp dport 80
}

# Proposed
firewall policy drop {
    accept tcp dport 80 src 10.0.0.0/8
    drop src 192.168.0.0/16
    accept dst 10.0.0.1 tcp dport 443
}
```

**Implementation**: Add `src` and `dst` keywords to lexer, extend
`parse_match_expr()` to accept them with CIDR/IP arguments. Generate
nftables `ip saddr`/`ip daddr` expressions.

**Effort**: Low-medium — lexer tokens + parser extension.

### Tier 2: High impact, higher effort

These require more significant design decisions but would fundamentally
improve NLL's expressiveness.

#### 3.6 Parametric imports (modules)

**Problem**: NLL's `import` is static — you can alias a file but can't pass
parameters. This limits reusability for patterns like "spine-leaf with N spines."

```nll
# Current — imported file has hardcoded values
import "spine-leaf.nll" as dc

# Proposed — pass parameters to the imported module
import "spine-leaf.nll" as dc(spines=4, leaves=8, base_subnet="10.0.0.0/16")
```

**Inspiration**: Terraform modules accept input variables and expose outputs.
This is the most-requested pattern in infrastructure-as-code.

**Implementation**:
- AST: Add `ImportDef.params: Vec<(String, String)>`
- Parser: Parse `(key=value, ...)` after alias
- Lower: Inject params as variables before expanding the imported file
- Validation: Imported files could declare expected params with `param name default`

**Effort**: Medium-high — requires careful scoping of imported variables.

#### 3.7 Cross-references between nodes

**Problem**: Addresses are duplicated across link definitions and route targets.
Change one and you must find-and-replace all references.

```nll
# Current — 10.0.0.1 appears in 3 places
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via 10.0.0.1 }
```

**Proposal**: Allow referencing a node's interface address.

```nll
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
node host { route default via ${router.eth0.address} }
```

**Implementation**: This requires a two-pass lowering:
1. First pass: collect all address assignments from links
2. Second pass: resolve `${node.iface.address}` references

The parser would treat these as normal interpolation expressions. The lowering
phase would need a post-link-processing resolution step.

**Effort**: High — requires two-pass lowering and a reference resolution system.
The variable system currently only handles simple string substitution.

#### 3.8 Default blocks

**Problem**: Common properties like MTU must be repeated on every link. Profile
inheritance helps for node properties but there's no equivalent for links.

```nll
# Current — mtu 9000 on every fabric link
link s1:e1 -- l1:e1 { 10.0.1.0/30 mtu 9000 }
link s1:e2 -- l2:e1 { 10.0.2.0/30 mtu 9000 }
link s2:e1 -- l1:e2 { 10.0.3.0/30 mtu 9000 }
link s2:e2 -- l2:e2 { 10.0.4.0/30 mtu 9000 }

# Proposed
defaults link { mtu 9000 }

link s1:e1 -- l1:e1 { 10.0.1.0/30 }
link s1:e2 -- l2:e1 { 10.0.2.0/30 }
```

**Implementation**:
- AST: Add `DefaultsDef { kind: "link"|"node"|"impair", props: ... }`
- Lowering: Apply defaults before per-statement properties (merge with override)

**Effort**: Medium — clean design, straightforward implementation.

#### 3.9 Conditional expressions

**Problem**: Can't produce topology variants from a single file. Must maintain
separate files for dev/staging/prod.

```nll
# Proposed — ternary in interpolation
let env = "dev"
impair router:wan0 delay ${env == "prod" ? "5ms" : "50ms"}
```

**Implementation**: Extend `eval_expr()` to parse ternary expressions.
Syntax: `${cond ? true_val : false_val}` where `cond` is `var == "literal"`.

**Effort**: Medium — expression parser extension, but the current single-operation
limitation makes this harder. Would need a mini expression parser.

#### 3.10 Render/expand command

**Problem**: When using loops, variables, and imports, it's hard to verify what
the expanded topology looks like. Containerlab users have requested this same
feature (#3013).

```bash
# Proposed
nlink-lab render examples/spine-leaf.nll

# Output: fully expanded NLL with all loops unrolled, variables substituted,
# imports inlined — a flat topology you can read and verify
```

**Implementation**:
- Parse and lower to Topology
- Serialize Topology back to NLL (new serializer)
- Or: pretty-print the intermediate expanded AST

**Effort**: Medium — the parse→lower pipeline exists, need a Topology→NLL printer.

### Tier 3: Nice-to-have

These improve the language but aren't critical for most users.

#### 3.11 Multi-profile inheritance

```nll
profile router { forward ipv4 }
profile monitored { sysctl "net.core.rmem_max" "16777216" }
node r1 : router, monitored  # merge both profiles
```

NixOS proves this composition model works well for infrastructure configuration.
Would require defining merge semantics (last-wins for sysctls, append for routes).

#### 3.12 Block comments

```nll
/* This entire section is disabled
link r1:wan0 -- r2:wan0 { ... }
impair r1:wan0 delay 50ms
*/
```

Currently only `#` line comments exist. Block comments make it easy to
temporarily disable sections during debugging.

#### 3.13 For-expressions (value-producing loops)

```nll
network mgmt {
    members [for i in 1..4 : router${i}:mgmt0]
}
```

HCL's for-expressions produce list values. This would eliminate the pattern of
manually writing out member lists.

#### 3.14 Subnet pools with auto-allocation

**Problem**: In large topologies, manually assigning non-overlapping subnets is
tedious and error-prone.

```nll
# Proposed
pool fabric 10.0.0.0/16 /30    # allocates /30 subnets from this pool
pool access 10.1.0.0/16 /24

link s1:e1 -- l1:e1 { auto fabric }    # gets 10.0.0.0/30 → .1 and .2
link s1:e2 -- l2:e1 { auto fabric }    # gets 10.0.0.4/30 → .5 and .6
link l1:e3 -- h1:e0 { auto access }    # gets 10.1.0.0/24 → .1 and .2
```

No tool surveyed offers this. Would be a genuine innovation for NLL.

#### 3.15 Validation assertions

```nll
validate {
    ping host1 host2        # host1 can reach host2
    no-ping host1 host3     # host1 cannot reach host3 (firewall blocks)
    reachable server:eth0   # interface is up and has addresses
}
```

NixOS's test infrastructure validates VM behavior declaratively. NLL could do
the same for network topologies, replacing ad-hoc `exec ping` patterns.

#### 3.16 Lab metadata

```nll
lab "datacenter" {
    description "Production datacenter simulation"
    version "2.1.0"
    author "Network Team"
    tags ["datacenter", "spine-leaf", "bgp"]
}
```

Kathara supports `LAB_AUTHOR`, `LAB_VERSION` etc. Useful for topology
sharing and catalog systems.

## 4. Prioritized Roadmap

| Priority | Proposal | Effort | Impact | Dependencies |
|----------|----------|--------|--------|--------------|
| **P0** | 3.2 Modulo operator | Trivial | Medium | None |
| **P0** | 3.1 Subnet auto-assignment | Low | High | None |
| **P0** | 3.5 Firewall src/dst | Low | Medium | None |
| **P1** | 3.4 Contextual auto-variables | Low | Medium | None |
| **P1** | 3.3 List iteration | Medium | High | None |
| **P1** | 3.8 Default blocks | Medium | High | None |
| **P1** | 3.10 Render command | Medium | Medium | None |
| **P2** | 3.12 Block comments | Low | Low | None |
| **P2** | 3.6 Parametric imports | Medium-high | High | None |
| **P2** | 3.9 Conditional expressions | Medium | Medium | None |
| **P2** | 3.11 Multi-profile inheritance | Medium | Medium | None |
| **P2** | 3.13 For-expressions | Medium | Medium | 3.3 |
| **P3** | 3.7 Cross-references | High | High | Two-pass lowering |
| **P3** | 3.14 Subnet pools | Medium | High | 3.1 |
| **P3** | 3.15 Validation assertions | Medium | Medium | None |
| **P3** | 3.16 Lab metadata | Low | Low | None |

### Suggested implementation plan

**Phase A** (1-2 days): P0 items — modulo, subnet auto-assign, firewall src/dst.
These are small parser/lowering changes with immediate payoff.

**Phase B** (2-3 days): P1 items — auto-variables, list iteration, defaults,
render command. These are medium parser changes that remove the most boilerplate.

**Phase C** (3-5 days): P2 items — block comments, parametric imports, conditionals,
multi-profile, for-expressions. Larger features that improve composition.

**Phase D** (future): P3 items — cross-references, subnet pools, validation
assertions. These require architectural changes (two-pass lowering, allocator).

## 5. Design Principles

Any NLL improvement should follow these principles (consistent with existing design):

1. **Declarative over imperative** — describe the desired state, don't script steps
2. **Typed literals** — durations, rates, percentages, CIDRs are first-class
3. **Inline over separate** — impairments on links, firewall in nodes, not separate config
4. **Sensible defaults** — auto-assign where possible, require explicit only when ambiguous
5. **Parse-time validation** — catch errors before deployment, not during
6. **Composable** — profiles, imports, loops enable reuse without copy-paste
7. **No magic** — every expansion is predictable and inspectable (hence the render command)
