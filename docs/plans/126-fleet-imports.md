# Plan 126: Fleet `for_each` Imports

**Date:** 2026-04-01
**Status:** Implemented (2026-04-01)
**Effort:** Small (half day)
**Priority:** P1 — scales fleet management to N instances

---

## Problem Statement

Importing the same template multiple times requires one line per instance:

```nll
import "imports/a18.nll" as a18(id=18)
import "imports/a18.nll" as a19(id=19)
```

For large fleets (50+ drones), this becomes verbose and error-prone.

## Proposed Syntax

```nll
import "imports/a18.nll" for_each {
  a18(id=18)
  a19(id=19)
}

import "imports/a9.nll" for_each {
  a9(id=9)
  a10(id=10)
}
```

Each line inside the `for_each` block is an alias + parameters, following
the existing `import ... as alias(params)` parameter syntax.

## Implementation

### Parser

After parsing `import STRING`, check for `for_each` keyword. If present,
parse a block of `alias(param=value, ...)` entries. Expand into multiple
`ImportDef` entries in the AST.

```rust
// In parse_import():
if eat_kw(tokens, pos, "for_each") {
    expect(tokens, pos, &Token::LBrace)?;
    let mut imports = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) { break; }
        let alias = expect_ident(tokens, pos)?;
        let params = parse_import_params(tokens, pos)?;
        imports.push(ImportDef { path: path.clone(), alias, params });
    }
    return Ok(imports);
}
```

### Lowerer

No changes — the expanded `ImportDef` entries are processed identically
to individual import statements.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_fleet_import` | `import "x.nll" for_each { a(x=1); b(x=2) }` → 2 imports |
| `test_fleet_import_lowering` | Verify nodes get correct prefixes and params |

## Documentation Updates

| File | Change |
|------|--------|
| **README.md** | Update "Parametric Imports" section with `for_each` example |
| **CLAUDE.md** | Mention `for_each` in NLL features list |
| **NLL_DSL_DESIGN.md** | Add `for_each` to import grammar |
| **examples/infra-c2-a18-a9.nll** | Convert 4 imports to 2 `for_each` blocks |

## File Changes

| File | Change |
|------|--------|
| `parser.rs` | Add `for_each` handling in import parsing |
| `ast.rs` | No change (expands to existing `ImportDef` list) |
