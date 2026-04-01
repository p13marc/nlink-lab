# Parser & AST Analysis — Should We Switch Crates?

**Date:** 2026-04-01
**Current stack:** logos (lexer) + hand-written recursive descent parser
**Alternatives evaluated:** pest, winnow, chumsky, lalrpop, tree-sitter

---

## Current Parser Stats

| Component | Lines | Role |
|-----------|-------|------|
| `lexer.rs` | ~720 | logos-based tokenizer, ~20 reserved + typed literals |
| `parser.rs` | ~3,500 | Recursive descent, ~30 `parse_*` functions |
| `ast.rs` | ~530 | AST types (statements, node props, etc.) |
| `lower.rs` | ~4,100 | AST → Topology (loops, imports, interpolation, IP functions) |

## NLL's Hard Parsing Problems

Two features make NLL unusual compared to typical DSLs:

### 1. Context-Sensitive Keywords (~100 words)

`delay`, `jitter`, `via`, `mode`, `ssid`, `masquerade`, `dnat`, etc. are
identifiers in most contexts but keywords in specific blocks. Plan 113
moved 97 tokens from hard keywords to context-sensitive idents. The current
approach: lex as `Token::Ident`, match with `eat_kw("delay")` in context.

### 2. Interpolation Adjacency

`spine${i}:eth0` must fuse `Ident("spine")` + `Interp("${i}")` + `Colon` +
`Ident("eth0")` into a compound name. `parse_name()` uses byte-level span
adjacency (`tokens[pos].span.start != prev_end`) to decide whether to fuse.
This is impossible in grammar-based tools without a pre-processing pass.

---

## Evaluation Summary

| Criterion | logos+RD | pest | winnow | chumsky | lalrpop | tree-sitter |
|---|---|---|---|---|---|---|
| Context-sensitive keywords | **Trivial** | Awkward | Trivial | Trivial | Very hard | Awkward |
| Interpolation adjacency | **Works** | Very hard | Same | Possible | Pre-pass needed | C externals needed |
| Error message quality | Basic | Medium | **Good** | **Excellent** | Medium | Good (editors) |
| Error recovery | None | None | Manual | **Automatic** | None | Automatic |
| Add new block | 50-150 lines | Grammar + walk | 30-100 lines | 30-100 lines | Grammar + action | Grammar + C |
| Compile time | ~1-2s | +3-8s | +1-2s | **+15-30s** | +5-10s | +5-10s (C) |
| Binary size | ~50-100 KB | ~200-400 KB | ~30-50 KB | ~200-400 KB | ~200-300 KB | ~500 KB+ |
| Already in deps | logos: yes | No | **Yes** | No | No | No |

## Recommendation: Stay, Improve Errors

**Keep logos + hand-written recursive descent.** NLL's two hardest problems
(context-sensitive keywords and interpolation adjacency) are easiest in a
hand-written parser. Every grammar-based tool makes them harder.

### What to Improve (without switching)

**Phase 1 — Better error messages (low effort, high value):**
- Convert `NllParse(String)` errors to `NllDiagnostic` with source spans
- Get miette-powered error rendering: source context, underlines, colors

**Phase 2 — Error recovery (medium effort):**
- In the main parse loop, catch errors and skip to next top-level keyword
- Report multiple errors per file instead of aborting at the first

**Phase 3 — Optional winnow migration (if maintenance grows):**
- winnow is already in Cargo.toml (used by nlink, not by nlink-lab parser)
- Operates on the same logos token stream
- Handles context-sensitive keywords identically
- Gives `ContextError` for better messages with less manual work
- Can migrate one `parse_*` function at a time (incremental)

### What NOT to do

- **Don't adopt pest or lalrpop** — grammar-file approaches fight NLL's
  context-sensitivity. You end up doing the same string matching plus
  an extra tree-walking layer.
- **Don't adopt chumsky** — compile time (+15-30s) is disproportionate,
  API still unstable (1.0 alpha). Its error recovery can be approximated
  with 50 lines of manual recovery code.
- **Don't adopt tree-sitter for runtime** — it's for editor integration.
  Consider it separately for a VS Code/Neovim extension.

## Why Not winnow Today?

winnow would be a lateral move, not an upgrade:
- Same logos token stream, same context-sensitive matching
- Slightly better error messages (ContextError)
- Slightly more concise (~10-20% fewer lines)
- But: migration cost for 3,500 lines of working parser code
- The improvement doesn't justify the effort

If the parser grows significantly (new complex features, deeper nesting),
winnow migration becomes more attractive. For now, the hand-written
parser is the right tool.
