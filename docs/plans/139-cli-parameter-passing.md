# Plan 139: CLI Parameter Passing to NLL

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (half day)
**Priority:** P2 — reuse one topology across test scenarios

---

## Problem Statement

NLL `param` declarations are only usable in imported files with `for_each`. There's no
way to pass parameters from the CLI to a top-level NLL file. This forces users to
maintain separate NLL files for each test scenario (LAN, WAN, satellite, stress) that
differ only in a few values.

## Proposed CLI

```bash
nlink-lab deploy topology.nll --set wan_latency=50ms --set wan_loss=0.1%
nlink-lab render topology.nll --set wan_latency=50ms
nlink-lab validate topology.nll --set wan_latency=50ms
```

```nll
param wan_latency default 10ms
param wan_loss default 0%

link router:wan0 -- peer:wan0 {
    10.0.0.1/24 -- 10.0.0.2/24
    delay ${wan_latency} loss ${wan_loss}
}
```

## Design Decisions

### Reuse existing `param` mechanism

The infrastructure already exists: `ParamDef` in the AST, `resolve_import_params()` in
the lowerer. For CLI parameters, we do the same thing: convert `--set key=value` pairs
into variable bindings (`let key = value`) prepended to the AST before lowering.

### Params without defaults

If a `param` has no default and no `--set` value is provided, error at parse time with
a clear message listing the missing parameters.

### Params without `param` declarations

If `--set foo=bar` is provided but the NLL file has no `param foo`, warn (not error).
The variable will still be available as a `let` binding. This allows gradual adoption.

### Scope

`--set` applies to the top-level file only. Imported files still use their own `param`
declarations resolved from `import ... (key=value)` syntax.

## Implementation

### Step 1: CLI flag (`bins/lab/src/main.rs`)

Add `--set` to `Deploy`, `Render`, `Validate`, and `Test` commands:

```rust
/// Set NLL parameters (can be repeated: --set key=value).
#[arg(long = "set", value_name = "KEY=VALUE")]
params: Vec<String>,
```

### Step 2: Parse `--set` values

Helper function:

```rust
fn parse_set_params(params: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for p in params {
        let (key, value) = p.split_once('=')
            .ok_or_else(|| format!("invalid --set format: '{p}' (expected KEY=VALUE)"))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}
```

### Step 3: Parser API change (`parser/nll/mod.rs`)

Add an optional parameters argument to the parse entry points:

```rust
/// Parse NLL source with external parameter bindings.
pub fn parse_with_params(source: &str, filename: &str, params: &HashMap<String, String>) -> Result<Topology> {
    let ast = parser::parse_nll(source, filename)?;
    lower::lower_with_params(ast, filename, params)
}

pub fn parse_file_with_params(path: &str, params: &HashMap<String, String>) -> Result<Topology> {
    let source = std::fs::read_to_string(path)?;
    parse_with_params(&source, path, params)
}
```

Keep the existing `parse()` and `parse_file()` as wrappers with empty params.

### Step 4: Lowerer change (`lower.rs`)

In `lower_with_params()`, before processing statements:

1. Collect all `param` declarations from the AST
2. For each param:
   - If `--set` provides a value → use it
   - Else if param has a default → use default
   - Else → error: "missing required parameter '{name}'"
3. Convert resolved params to `let` bindings in the variable map
4. Remove `param` statements from the AST
5. Proceed with normal lowering

```rust
pub fn lower_with_params(
    mut ast: NllFile,
    filename: &str,
    cli_params: &HashMap<String, String>,
) -> Result<Topology> {
    // Extract param declarations
    let params: Vec<ParamDef> = ast.statements.iter()
        .filter_map(|s| match s {
            Statement::Param(p) => Some(p.clone()),
            _ => None,
        })
        .collect();

    // Resolve values
    let mut vars: HashMap<String, String> = HashMap::new();
    for param in &params {
        if let Some(value) = cli_params.get(&param.name) {
            vars.insert(param.name.clone(), value.clone());
        } else if let Some(default) = &param.default {
            vars.insert(param.name.clone(), default.clone());
        } else {
            return Err(Error::parse(format!(
                "missing required parameter '{}' (use --set {}=<value>)",
                param.name, param.name
            )));
        }
    }

    // Remove param statements
    ast.statements.retain(|s| !matches!(s, Statement::Param(_)));

    // Proceed with normal lowering, vars seeded with CLI params
    lower_with_vars(ast, filename, vars)
}
```

### Step 5: Wire CLI to parser

In the deploy/render/validate handlers, pass parsed `--set` params:

```rust
Commands::Deploy { topology, params, .. } => {
    let cli_params = parse_set_params(&params)?;
    let topo = if cli_params.is_empty() {
        nlink_lab::parser::parse_file(&topology)?
    } else {
        nlink_lab::parser::parse_file_with_params(&topology, &cli_params)?
    };
    // ...
}
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_parse_param_with_cli_value` | lower.rs | CLI value overrides default |
| `test_parse_param_uses_default` | lower.rs | No CLI value → uses default |
| `test_parse_param_missing_no_default` | lower.rs | Missing required param → error |
| `test_parse_param_unused_set_warns` | lower.rs | `--set` for non-declared param → warning |
| `test_render_with_params` | render.rs | Params are expanded in rendered output |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `main.rs` | +25 | `--set` flag on 4 commands + helper |
| `parser/nll/mod.rs` | +15 | `parse_with_params` / `parse_file_with_params` |
| `lower.rs` | +35 | `lower_with_params` + param resolution |
| Tests | +40 | 5 test functions |
| **Total** | ~115 | |
