# Plan 093: NLL Expression Engine

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `crates/nlink-lab/src/parser/nll/`

## Summary

Upgrade the NLL interpolation system from single-operation arithmetic to a
proper expression engine with modulo, conditionals, compound expressions,
contextual auto-variables, and block comments.

## Breaking Changes

None. All changes are additive — existing topologies continue to work.

## Phase 1: Modulo Operator (hours)

### Current state

`eval_expr()` in `lower.rs:283-339` supports `+`, `-`, `*`, `/` via string
splitting on the operator character. The match arm at line 317:

```rust
match op {
    '+' => left_val + right_val,
    '-' => left_val - right_val,
    '*' => left_val * right_val,
    '/' => { /* division with zero check */ },
    _ => unreachable!(),
}
```

### Change

Add `'%'` to the operator search list (line 287) and the match arm (line 317).

The `%` character does NOT conflict with the `Percent` token (`0.1%`) because
`eval_expr()` operates on the expression string inside `${}`, not on raw tokens.
The lexer handles `Percent` separately as a typed literal.

### Files

- `lower.rs:287` — add `'%'` to `['+', '-', '*', '/']` operator list
- `lower.rs:317` — add `'%' => left_val % right_val` match arm

### Tasks

- [ ] Add `%` to operator list and match arm in `eval_expr()`
- [ ] Add tests: `${i % 2}`, `${i % 3}`, `${4 % 0}` (zero check)

## Phase 2: Compound Expressions (day 1)

### Problem

Only single binary operations work. `${a + b + c}` and `${(i - 1) * 2}` fail.
This limits spine-leaf and fat-tree topology generation.

### Change

Replace the string-splitting approach in `eval_expr()` with a recursive-descent
mini expression parser. Grammar:

```
expr       = term (('+' | '-') term)*
term       = factor (('*' | '/' | '%') factor)*
factor     = '(' expr ')' | variable | integer
variable   = [a-zA-Z_][a-zA-Z0-9_]*
integer    = [0-9]+
```

This gives standard arithmetic precedence: `*/%` bind tighter than `+-`,
parentheses override.

### Implementation

Replace `eval_expr()` with:

```rust
fn eval_expr(expr: &str, vars: &HashMap<String, String>) -> String {
    let tokens = tokenize_expr(expr.trim());
    match parse_expr(&tokens, &mut 0, vars) {
        Ok(val) => val.to_string(),
        Err(_) => format!("${{{expr}}}"),  // return original on failure
    }
}

fn parse_expr(tokens: &[ExprToken], pos: &mut usize, vars: &HashMap<String, String>) -> Result<i64> {
    let mut left = parse_term(tokens, pos, vars)?;
    while matches!(tokens.get(*pos), Some(ExprToken::Plus | ExprToken::Minus)) {
        let op = tokens[*pos]; *pos += 1;
        let right = parse_term(tokens, pos, vars)?;
        left = match op { ExprToken::Plus => left + right, _ => left - right };
    }
    Ok(left)
}
// ... parse_term, parse_factor similarly
```

### Files

- `lower.rs` — replace `eval_expr()` entirely (~50 lines)
- No lexer/parser/AST changes needed (interpolation is post-parse)

### Tasks

- [ ] Implement `tokenize_expr()` for expression tokens
- [ ] Implement recursive-descent `parse_expr()`, `parse_term()`, `parse_factor()`
- [ ] Preserve backward compatibility: `${i}`, `${i + 1}`, `${i+1}` all still work
- [ ] Add tests: `${(i - 1) * 2}`, `${a + b + c}`, `${i % 2 + 1}`

## Phase 3: Conditional Expressions (day 1-2)

### Problem

Can't produce topology variants from a single file without maintaining separate
files for different environments.

### Change

Add ternary operator to the expression grammar:

```
expr = ternary
ternary = comparison ('?' value ':' value)?
comparison = arith (('==' | '!=') arith)?
```

Syntax: `${env == "prod" ? "5ms" : "50ms"}`

String comparison uses `==` and `!=`. Both sides can be variables or literals.
The true/false branches return string values (not further expressions).

### Implementation

Extend `eval_expr()`:
1. Check for `?` in the expression
2. Split into `condition ? true_val : false_val`
3. Evaluate condition as boolean (string equality comparison)
4. Return the appropriate branch value

```rust
// In eval_expr(), before arithmetic parsing:
if let Some(q_pos) = expr.find('?') {
    let condition = &expr[..q_pos].trim();
    let branches = &expr[q_pos + 1..];
    let colon = branches.find(':').ok_or("missing : in ternary")?;
    let true_val = branches[..colon].trim();
    let false_val = branches[colon + 1..].trim();

    let result = eval_condition(condition, vars);
    return if result { true_val } else { false_val }.to_string();
}
```

### Files

- `lower.rs` — extend `eval_expr()` with ternary support

### Tasks

- [ ] Implement ternary parsing in `eval_expr()`
- [ ] Implement `eval_condition()` with `==` and `!=` operators
- [ ] Add tests: `${x == "prod" ? "5ms" : "50ms"}`, `${i != 0 ? "yes" : "no"}`
- [ ] Test interaction with variables: `let env = "dev"` then `${env == "prod" ? ...}`

## Phase 4: Contextual Auto-Variables (day 2)

### Problem

Users must manually track context inside loops. No access to lab-level
metadata from within node/link blocks without `let` boilerplate.

### Change

Inject contextual variables automatically during lowering:

| Variable | Context | Value |
|----------|---------|-------|
| `${loop.index}` | `for` body | Current iteration value |
| `${loop.first}` | `for` body | `"true"` on first iteration, `"false"` otherwise |
| `${loop.last}` | `for` body | `"true"` on last iteration, `"false"` otherwise |
| `${lab.name}` | After `lab` decl | Lab name string |
| `${lab.prefix}` | After `lab` decl | Lab prefix (or name if no prefix) |

### Implementation

In `expand_for()` (lower.rs:216-246), inject auto-variables before loop body:

```rust
for i in for_loop.start..=for_loop.end {
    vars.insert(for_loop.var.clone(), i.to_string());
    vars.insert("loop.index".into(), i.to_string());
    vars.insert("loop.first".into(), (i == for_loop.start).to_string());
    vars.insert("loop.last".into(), (i == for_loop.end).to_string());
    // ... expand body ...
}
vars.remove("loop.index");
vars.remove("loop.first");
vars.remove("loop.last");
```

For lab variables, inject in `lower()` before processing statements:

```rust
vars.insert("lab.name".into(), lab.name.clone());
vars.insert("lab.prefix".into(), lab.prefix.clone().unwrap_or(lab.name.clone()));
```

### Files

- `lower.rs` — `expand_for()` and `lower()`/`lower_with_base_dir()`

### Tasks

- [ ] Inject `loop.index`, `loop.first`, `loop.last` in `expand_for()`
- [ ] Inject `lab.name`, `lab.prefix` in the lowering entry point
- [ ] Clean up loop variables after loop completes
- [ ] Add tests for all auto-variables
- [ ] Verify `loop.*` variables work with conditionals: `${loop.last ? "closing" : ""}`

## Phase 5: Block Comments (day 2)

### Problem

Only line comments (`#`) exist. Commenting out multi-line sections (common during
debugging) requires prefixing every line.

### Change

Add `/* ... */` block comments to the lexer.

### Implementation

logos doesn't natively support multi-line skip patterns, so handle block comments
in a post-lexing filter pass:

```rust
pub fn lex(input: &str) -> Result<Vec<Spanned>> {
    // Strip block comments before lexing
    let stripped = strip_block_comments(input)?;
    let mut lexer = Token::lexer(&stripped);
    // ...
}

fn strip_block_comments(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut depth = 0; // support nested block comments
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next(); depth += 1;
        } else if c == '*' && chars.peek() == Some(&'/') && depth > 0 {
            chars.next(); depth -= 1;
            // Insert newlines to preserve line numbers for error reporting
            result.push(' ');
        } else if depth == 0 {
            result.push(c);
        } else if c == '\n' {
            result.push('\n'); // preserve line numbers
        }
    }
    if depth > 0 { return Err(Error::NllParse("unterminated block comment".into())); }
    Ok(result)
}
```

### Files

- `lexer.rs` — add `strip_block_comments()` pre-processing step

### Tasks

- [ ] Implement `strip_block_comments()` with nesting support
- [ ] Preserve line numbers (replace comment content with spaces/newlines)
- [ ] Handle unterminated block comment error
- [ ] Add tests: basic, nested, multiline, unterminated error

## Progress

### Phase 1: Modulo
- [ ] Add `%` operator
- [ ] Tests

### Phase 2: Compound Expressions
- [ ] Expression tokenizer
- [ ] Recursive-descent parser
- [ ] Backward compatibility
- [ ] Tests

### Phase 3: Conditionals
- [ ] Ternary parsing
- [ ] Condition evaluation
- [ ] Tests

### Phase 4: Auto-Variables
- [ ] Loop variables
- [ ] Lab variables
- [ ] Tests

### Phase 5: Block Comments
- [ ] strip_block_comments()
- [ ] Line number preservation
- [ ] Tests
