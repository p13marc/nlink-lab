# Plan 130: Editor/IDE Support for NLL

**Date:** 2026-04-02
**Status:** Done (Phase 1-2 complete: tree-sitter grammar + VS Code extension)
**Effort:** Large (5-7 days)
**Priority:** P2 — users write NLL by hand; syntax highlighting is essential UX

---

## Problem Statement

NLL files are written entirely by hand in text editors. Without syntax highlighting,
code completion hints, or bracket matching, the editing experience is poor. Errors
in topology files are only caught at `nlink-lab validate` time.

Modern DSLs (HCL/Terraform, Dockerfile, TOML, Nix) all ship editor support.
NLL should too.

## Scope

This plan delivers:

1. **tree-sitter-nll** — a tree-sitter grammar for NLL (canonical parse definition)
2. **VS Code extension** — syntax highlighting, bracket matching, comment toggling
3. **Neovim/Helix config** — tree-sitter queries for highlighting and folding

Out of scope (future work):
- LSP server (code completion, go-to-definition, diagnostics)
- Semantic highlighting (requires LSP)
- VS Code Marketplace publishing (manual for now)

## Architecture Decision: Tree-sitter + TextMate

| Approach | Editors | Structural Features | Effort |
|----------|---------|---------------------|--------|
| **TextMate only** | VS Code | Regex highlighting (no structure) | 1-2 days |
| **Tree-sitter only** | Neovim, Helix, Zed | Full parse tree | 3-5 days |
| **Both** | All editors | Best of both | 5-7 days |

**Decision: Both.** Tree-sitter is the canonical grammar (real parser). A TextMate
grammar derived from it provides VS Code highlighting without WASM dependencies.

**Why not TextMate only?** TextMate grammars are regex-based and cannot handle:
- Context-sensitive keywords (`forward` is a keyword only inside profiles)
- Nested block comments (`/* outer /* inner */ still outer */`)
- Structural code folding (which blocks to fold)

**Why not tree-sitter only for VS Code?** Tree-sitter in VS Code requires loading
a WASM binary via `vscode-anycode` or a custom extension. TextMate is simpler, works
everywhere (including vscode.dev, Codespaces, remote SSH), and covers 90% of the value.

## NLL Grammar Analysis

### Token Categories

From the lexer (`lexer.rs`), NLL has these token types:

| Category | Examples | tree-sitter Handling |
|----------|----------|---------------------|
| **Structural keywords** (always keywords) | `lab`, `node`, `link`, `network`, `profile`, `for`, `in`, `let`, `import`, `if`, `site`, `param` | `keyword()` in grammar rules |
| **Context-sensitive keywords** (~100) | `forward`, `delay`, `jitter`, `via`, `masquerade`, `dnat`, `route`, `firewall`, `nat`, `vrf`, `dns`, etc. | String literals in grammar rules where they are valid |
| **Typed literals** | `10.0.0.1/24`, `fd00::1/64`, `10ms`, `100mbit`, `0.1%` | `token()` with regex patterns |
| **Interpolation** | `${expr}`, `${i * 2 + 1}` | External scanner or opaque token |
| **Operators** | `--`, `->`, `<-`, `..`, `==`, `!=`, `&&`, `||` | Literal strings |
| **Delimiters** | `{ } [ ] ( ) :` | Literal strings |
| **Strings** | `"hello"` | Standard string rule |
| **Comments** | `# line`, `/* block */` | External scanner for nested blocks |

### Grammar Complexity Estimate

| Component | Estimated Rules |
|-----------|----------------|
| Top-level statements | ~15 (lab, node, link, network, profile, for, let, import, if, site, defaults, pool, validate, scenario, benchmark) |
| Node properties | ~15 (forward, sysctl, lo, route, firewall, nat, vrf, wireguard, vxlan, macvlan, ipvlan, wifi, run, container props) |
| Link/network sub-rules | ~10 (endpoint, address pair, impairment, rate, subnet, members, vlan, port) |
| Expressions/literals | ~10 (identifier, string, integer, cidr, ipv6, duration, rate, percent, interpolation, function call) |
| **Total** | **~50-60 rules** |

This is comparable to tree-sitter-hcl (~70 rules) and simpler than tree-sitter-javascript (~200).

### NLL-Specific Challenges

#### 1. Context-Sensitive Keywords

NLL's biggest grammar challenge. `forward`, `route`, `delay`, `firewall`, etc. are
identifiers in most contexts but keywords in specific blocks.

**Tree-sitter solution:** Define them as string literals inside the grammar rules where
they are valid. Tree-sitter matches by structure, not just tokens:

```javascript
// "forward" is only a keyword inside a profile/node body
forward_prop: $ => seq('forward', choice('ipv4', 'ipv6')),

// "delay" is only a keyword inside an impairment context
impairment: $ => seq(
  optional(seq('delay', $.duration)),
  optional(seq('jitter', $.duration)),
  optional(seq('loss', $.percent)),
  // ...
),
```

In other positions, the same text matches as a plain `$.identifier`.

**TextMate workaround:** Use begin/end patterns to establish block context, then
highlight keywords only within those blocks. This is fragile but covers common cases.

#### 2. Nested Block Comments

`/* outer /* inner */ still comment */` requires a counter. Tree-sitter's built-in
tokenizer is regex-based and cannot track nesting depth.

**Solution:** External scanner (`src/scanner.c`, ~40 lines). This is a well-known
tree-sitter pattern used by tree-sitter-rust and tree-sitter-swift.

```c
// Pseudocode for external scanner
bool scan_block_comment(TSLexer *lexer) {
    int depth = 1;
    while (depth > 0) {
        if (lexer->lookahead == '/' && peek_next == '*') { depth++; advance(2); }
        else if (lexer->lookahead == '*' && peek_next == '/') { depth--; advance(2); }
        else advance(1);
    }
    return true;
}
```

#### 3. Interpolation Adjacency

`spine${i}:eth0` is three tokens: `Ident("spine")`, `Interp("${i}")`, `Ident("eth0")`.
They fuse into a compound name based on byte-level span adjacency.

**Tree-sitter solution:** Define an `interpolated_name` rule:

```javascript
interpolated_name: $ => prec.right(repeat1(choice(
  $.identifier_fragment,
  $.interpolation,
))),
```

The external scanner can also handle this by emitting `identifier_fragment` tokens
that stop at `${` boundaries.

#### 4. The `--` Operator

`--` is both a link connector (`node:eth0 -- node:eth1`) and an address separator
(`10.0.0.1/24 -- 10.0.0.2/24`). Tree-sitter resolves this by structural position
(different grammar rules), so it is not problematic.

#### 5. CIDR/IP Ambiguity

`10.0.0.1/24` contains `.` and `/`. Tree-sitter needs these as atomic tokens:

```javascript
cidr: $ => token(/[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+/),
```

The `token()` wrapper ensures tree-sitter treats the regex as indivisible.
Precedence (longer match wins) handles disambiguation with identifiers and integers.

## Implementation

### Phase 1: tree-sitter-nll Grammar (3-4 days)

Create a new repository/directory `tree-sitter-nll/` (can live inside the nlink-lab
repo under `editors/tree-sitter-nll/` or as a separate repo).

#### Directory Structure

```
editors/tree-sitter-nll/
  grammar.js                  # Grammar definition
  package.json                # npm metadata
  src/
    scanner.c                 # External scanner (nested comments, interpolation)
    grammar.json              # Generated
    parser.c                  # Generated
    tree_sitter/parser.h      # Generated
  bindings/
    rust/                     # Rust bindings (generated)
      lib.rs
      build.rs
      Cargo.toml
  queries/
    highlights.scm            # Syntax highlighting
    folds.scm                 # Code folding
    indents.scm               # Auto-indentation
    locals.scm                # Variable scoping
  test/
    corpus/
      basics.txt              # Lab declarations, nodes, links
      loops.txt               # For loops, let variables
      imports.txt             # Import statements
      networking.txt          # Routes, firewall, NAT, VRF
      containers.txt          # Container properties
      advanced.txt            # Scenarios, benchmarks, Wi-Fi
```

#### grammar.js Skeleton

```javascript
module.exports = grammar({
  name: 'nll',

  externals: $ => [
    $.block_comment,
    $.identifier_fragment,
  ],

  extras: $ => [
    /\s/,
    $.line_comment,
    $.block_comment,
  ],

  word: $ => $.identifier_fragment,

  rules: {
    source_file: $ => repeat($._statement),

    _statement: $ => choice(
      $.lab_declaration,
      $.import_statement,
      $.profile_definition,
      $.node_definition,
      $.link_definition,
      $.network_definition,
      $.defaults_definition,
      $.pool_definition,
      $.let_binding,
      $.for_loop,
      $.if_block,
      $.site_block,
      $.validate_block,
      $.scenario_block,
      $.benchmark_block,
      $.pattern_definition,
    ),

    // Top-level: lab "name" { ... }
    lab_declaration: $ => seq('lab', $.string, optional($.lab_body)),

    // Top-level: node name : profile { ... }
    node_definition: $ => seq(
      'node', $.name,
      optional(seq(':', $.profile_list)),
      optional($.node_body),
    ),

    // Top-level: link ep -- ep { ... }
    link_definition: $ => seq(
      'link', $.endpoint, '--', $.endpoint,
      optional(seq(':', $.identifier)),  // link profile
      optional($.link_body),
    ),

    // ... ~50 more rules

    // Literals
    cidr: $ => token(/[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+/),
    ipv6_cidr: $ => token(/[0-9a-fA-F:]+\/[0-9]+/),
    duration: $ => token(/[0-9]+(ms|s|m|h)/),
    rate: $ => token(/[0-9]+(kbit|mbit|gbit|kbps|mbps|gbps)/),
    percent: $ => token(/[0-9]+(\.[0-9]+)?%/),
    integer: $ => /[0-9]+/,
    string: $ => /"[^"]*"/,
    interpolation: $ => /\$\{[^}]+\}/,

    // Comments
    line_comment: $ => /#.*/,
    // block_comment handled by external scanner
  },
});
```

#### External Scanner (`src/scanner.c`)

~80 lines handling:
1. **Nested block comments** — track `/*` depth counter
2. **Identifier fragments** — emit identifier characters, stopping at `${` or non-ident chars

#### Highlight Queries (`queries/highlights.scm`)

```scheme
; Structural keywords
["lab" "node" "link" "network" "profile" "for" "in" "let" "import"
 "as" "if" "site" "param" "defaults" "pool" "validate" "scenario"
 "benchmark" "mesh" "ring" "star"] @keyword

; Context-sensitive keywords (matched by parent node type)
(forward_prop "forward" @keyword)
(route_prop "route" @keyword "via" @keyword)
(firewall_block "firewall" @keyword "policy" @keyword)
(nat_block "nat" @keyword)
(impairment "delay" @keyword)
(impairment "jitter" @keyword)
(impairment "loss" @keyword)
(impairment "rate" @keyword)

; Actions
["accept" "drop" "masquerade" "dnat" "snat" "translate"] @keyword.operator

; Operators
["--" "->" "<-" "=" "==" "!=" ".." "<" ">" "<=" ">=" "&&" "||"] @operator

; Punctuation
["{" "}" "[" "]" "(" ")" ":"] @punctuation.bracket

; Literals
(string) @string
(integer) @number
(cidr) @number
(ipv6_cidr) @number
(duration) @number
(rate) @number
(percent) @number

; Interpolation
(interpolation) @string.special

; Names
(node_definition name: (_) @type)
(profile_definition name: (_) @type.definition)
(network_definition name: (_) @type)
(site_block name: (_) @type)
(let_binding name: (_) @variable)
(for_loop variable: (_) @variable)
(param_definition name: (_) @variable)

; Function calls
(function_call name: (_) @function)

; Comments
(line_comment) @comment
(block_comment) @comment

; Endpoints
(endpoint node: (_) @variable interface: (_) @property)
```

#### Fold Queries (`queries/folds.scm`)

```scheme
(node_body) @fold
(link_body) @fold
(network_body) @fold
(profile_body) @fold
(for_loop) @fold
(if_block) @fold
(site_block) @fold
(firewall_block) @fold
(nat_block) @fold
(scenario_block) @fold
(benchmark_block) @fold
```

#### Test Corpus

One test file per feature area, using tree-sitter's test format:

```
================
Simple lab declaration
================
lab "simple"
node router
node host
---
(source_file
  (lab_declaration (string))
  (node_definition name: (identifier))
  (node_definition name: (identifier)))
```

~20-30 test cases covering all statement types and edge cases.

### Phase 2: VS Code Extension (1-2 days)

#### Directory Structure

```
editors/vscode-nll/
  package.json                    # Extension manifest
  syntaxes/
    nll.tmLanguage.json           # TextMate grammar
  language-configuration.json     # Brackets, comments, auto-closing
  CHANGELOG.md
  README.md
  .vscodeignore
```

#### `package.json`

```json
{
  "name": "nll",
  "displayName": "NLL — nlink-lab Language",
  "description": "Syntax highlighting for NLL network topology files",
  "version": "0.1.0",
  "publisher": "nlink-lab",
  "engines": { "vscode": "^1.85.0" },
  "categories": ["Programming Languages"],
  "contributes": {
    "languages": [{
      "id": "nll",
      "aliases": ["NLL", "nlink-lab"],
      "extensions": [".nll"],
      "configuration": "./language-configuration.json"
    }],
    "grammars": [{
      "language": "nll",
      "scopeName": "source.nll",
      "path": "./syntaxes/nll.tmLanguage.json"
    }]
  }
}
```

#### `language-configuration.json`

```json
{
  "comments": {
    "lineComment": "#",
    "blockComment": ["/*", "*/"]
  },
  "brackets": [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"]
  ],
  "autoClosingPairs": [
    { "open": "{", "close": "}" },
    { "open": "[", "close": "]" },
    { "open": "(", "close": ")" },
    { "open": "\"", "close": "\"" }
  ],
  "surroundingPairs": [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"],
    ["\"", "\""]
  ],
  "indentationRules": {
    "increaseIndentPattern": "\\{\\s*$",
    "decreaseIndentPattern": "^\\s*\\}"
  },
  "folding": {
    "markers": {
      "start": "\\{",
      "end": "\\}"
    }
  }
}
```

#### TextMate Grammar (`nll.tmLanguage.json`)

The TextMate grammar uses regex patterns organized by scope. Key sections:

```json
{
  "scopeName": "source.nll",
  "patterns": [
    { "include": "#comments" },
    { "include": "#strings" },
    { "include": "#interpolation" },
    { "include": "#keywords" },
    { "include": "#literals" },
    { "include": "#operators" }
  ],
  "repository": {
    "comments": {
      "patterns": [
        { "name": "comment.line.number-sign.nll", "match": "#.*$" },
        { "name": "comment.block.nll", "begin": "/\\*", "end": "\\*/" }
      ]
    },
    "keywords": {
      "patterns": [
        {
          "name": "keyword.control.nll",
          "match": "\\b(lab|node|link|network|profile|for|in|let|import|as|if|site|param|defaults|pool|validate|scenario|benchmark)\\b"
        },
        {
          "name": "keyword.other.nll",
          "match": "\\b(forward|ipv4|ipv6|route|via|dev|default|firewall|policy|accept|drop|nat|masquerade|dnat|snat|translate|delay|jitter|loss|rate|corrupt|reorder|src|dst|to|vrf|table|wireguard|vxlan|macvlan|ipvlan|wifi|mode|ssid|channel|run|background|image|cmd|cpu|memory|privileged|dns|hosts|routing|auto|mesh|ring|star|members|subnet|lo|sysctl|mtu|container|egress|ingress)\\b"
        },
        {
          "name": "keyword.control.loop.nll",
          "match": "\\b(for_each|with)\\b"
        }
      ]
    },
    "strings": {
      "name": "string.quoted.double.nll",
      "begin": "\"",
      "end": "\"",
      "patterns": [{ "include": "#interpolation" }]
    },
    "interpolation": {
      "name": "string.interpolated.nll",
      "match": "\\$\\{[^}]+\\}"
    },
    "literals": {
      "patterns": [
        { "name": "constant.numeric.cidr.nll", "match": "\\b[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+/[0-9]+\\b" },
        { "name": "constant.numeric.ip.nll", "match": "\\b[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+\\b" },
        { "name": "constant.numeric.duration.nll", "match": "\\b[0-9]+(ms|s|m|h)\\b" },
        { "name": "constant.numeric.rate.nll", "match": "\\b[0-9]+(kbit|mbit|gbit|kbps|mbps|gbps)\\b" },
        { "name": "constant.numeric.percent.nll", "match": "\\b[0-9]+(\\.[0-9]+)?%\\b" },
        { "name": "constant.numeric.nll", "match": "\\b[0-9]+\\b" }
      ]
    },
    "operators": {
      "name": "keyword.operator.nll",
      "match": "(--|->|<-|\\.\\.|==|!=|<=|>=|&&|\\|\\||[<>=])"
    }
  }
}
```

**Known limitation:** TextMate cannot distinguish context-sensitive keywords from
identifiers. `forward`, `route`, `delay`, etc. will always highlight as keywords
even when used as node/variable names. This is acceptable — these names are unusual
for topology elements, and the false-positive rate is low.

### Phase 3: Neovim/Helix Integration (0.5-1 day)

#### Neovim

Users add to their nvim-treesitter config:

```lua
local parser_config = require("nvim-treesitter.parsers").get_parser_configs()
parser_config.nll = {
  install_info = {
    url = "https://github.com/yourorg/tree-sitter-nll",
    files = { "src/parser.c", "src/scanner.c" },
    branch = "main",
  },
  filetype = "nll",
}

vim.filetype.add({ extension = { nll = "nll" } })
```

Then `TSInstall nll` compiles and installs the parser.

The `queries/highlights.scm` from the tree-sitter-nll repo is automatically used.

#### Helix

Users add to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "nll"
scope = "source.nll"
injection-regex = "nll"
file-types = ["nll"]
comment-token = "#"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 2, unit = "  " }
auto-format = false

[language.auto-pairs]
'{' = '}'
'[' = ']'
'(' = ')'
'"' = '"'

[[grammar]]
name = "nll"
source = { git = "https://github.com/yourorg/tree-sitter-nll", rev = "main" }
```

Then `hx --grammar fetch && hx --grammar build` compiles the parser.
Query files go in `~/.config/helix/runtime/queries/nll/`.

#### Zed

Zed uses tree-sitter natively. A `languages/nll/` directory in a Zed extension with
`config.toml`, `highlights.scm`, and a reference to the grammar provides full support.

## Testing Strategy

### Tree-sitter Tests

The tree-sitter test corpus validates that every NLL construct parses to the expected
S-expression tree. Minimum 20 test cases:

| Test File | Coverage |
|-----------|----------|
| `basics.txt` | Lab declarations, nodes, links, profiles |
| `addressing.txt` | CIDRs, IPv6, subnet/host functions, pools |
| `loops.txt` | For loops (range, list), let bindings, interpolation |
| `imports.txt` | Import, import-as, for_each fleet |
| `networking.txt` | Routes, route groups, VRF, WireGuard, VXLAN |
| `security.txt` | Firewall rules, NAT (masquerade, dnat, snat, translate) |
| `impairments.txt` | Delay, jitter, loss, rate, asymmetric, inline |
| `containers.txt` | Image, cmd, cpu, memory, healthcheck, depends-on |
| `networks.txt` | Bridge networks, VLAN, members, glob patterns |
| `advanced.txt` | Scenarios, benchmarks, Wi-Fi, sites, conditionals |
| `comments.txt` | Line comments, nested block comments |
| `edge_cases.txt` | Interpolation adjacency, context-sensitive keywords as names |

### Manual Testing

Parse all 33 example `.nll` files with `tree-sitter parse` and verify no errors.

### VS Code Testing

Install the extension locally (`code --install-extension .`) and verify highlighting
on example files.

## Reference Grammars

These existing tree-sitter grammars are the most relevant models for NLL:

| Grammar | Relevance |
|---------|-----------|
| [tree-sitter-hcl](https://github.com/tree-sitter-grammars/tree-sitter-hcl) | Block syntax, interpolation, `for_each`, typed attributes |
| [tree-sitter-just](https://github.com/IndianBoy42/tree-sitter-just) | Context-sensitive keywords, interpolation |
| [tree-sitter-toml](https://github.com/tree-sitter-grammars/tree-sitter-toml) | Simple block structure, typed literals |
| [tree-sitter-nginx](https://github.com/opa-oz/tree-sitter-nginx) | Directive-based DSL with context-sensitive keywords |

## Deliverables

| Deliverable | Location | Description |
|-------------|----------|-------------|
| tree-sitter-nll | `editors/tree-sitter-nll/` | Grammar, external scanner, queries, tests |
| vscode-nll | `editors/vscode-nll/` | VS Code extension (TextMate + language config) |
| Editor docs | `README.md` | Section on editor setup (Neovim, Helix, VS Code) |

## File Changes Summary

| Component | Files | Estimated Lines |
|-----------|-------|----------------|
| `grammar.js` | 1 | ~300-400 |
| `src/scanner.c` | 1 | ~80 |
| `queries/*.scm` | 4 | ~120 |
| `test/corpus/*.txt` | 10-12 | ~400 |
| `package.json` (tree-sitter) | 1 | ~30 |
| VS Code `package.json` | 1 | ~40 |
| `nll.tmLanguage.json` | 1 | ~150 |
| `language-configuration.json` | 1 | ~40 |
| Documentation | 1-2 | ~50 |
| **Total** | ~22 | ~1,200-1,300 |

## Future Work (Not in This Plan)

- **LSP server** — `nlink-lab lsp` command providing diagnostics, completion, hover.
  Would reuse the existing parser and validator. Medium effort (3-5 days).
- **Semantic highlighting** — tree-sitter-based highlighting with scope awareness
  (distinguish node names from profile names from variable names). Requires LSP or
  custom VS Code extension logic.
- **VS Code Marketplace / Open VSX publishing** — automated CI release pipeline.
- **Snippet support** — VS Code snippets for common topology patterns (node, link, for loop).
