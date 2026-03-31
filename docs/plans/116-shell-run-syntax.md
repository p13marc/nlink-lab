# Plan 116: Shell-Style `run` Syntax

**Date:** 2026-03-31
**Status:** Implemented (2026-03-31)
**Effort:** Small (half day)
**Priority:** P2 — usability improvement

---

## Problem Statement

The current `run` syntax requires a bracket list of strings:

```nll
run ["iptables", "-t", "nat", "-A", "POSTROUTING", "-s", "10.2.0.0/16", "-j", "MASQUERADE"]
```

This is verbose and hard to read. For one-liner commands, a shell-style syntax
would be much cleaner:

```nll
run "iptables -t nat -A POSTROUTING -s 10.2.0.0/16 -j MASQUERADE"
```

## NLL Syntax

### Current (keep as-is)

```nll
run ["cmd", "arg1", "arg2"]              # list of strings
run background ["cmd", "arg1", "arg2"]   # background process
```

### New (add shell-style alternative)

```nll
run "cmd arg1 arg2"                      # single string → split by whitespace
run background "cmd arg1 arg2"           # background variant
```

When the parser sees `run` followed by a string (not `[`), it treats the
string as a shell command and wraps it as `["sh", "-c", "command"]`.

This also enables commands with pipes and redirects:
```nll
run "echo hello > /tmp/test.txt"
run "iperf3 -s &>/dev/null &"
```

## Implementation

### Parser change

In `parse_run_def()`:

```rust
fn parse_run_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RunDef> {
    let background = eat(tokens, pos, &Token::Background);

    // New: check if next token is a string (shell-style) or [ (list-style)
    let cmd = if matches!(at(tokens, *pos), Some(Token::String(_))) {
        let shell_cmd = expect_string(tokens, pos)?;
        vec!["sh".to_string(), "-c".to_string(), shell_cmd]
    } else {
        parse_string_list(tokens, pos)?
    };

    Ok(ast::RunDef { cmd, background })
}
```

That's the entire change — ~5 lines.

### Backward compatibility

The bracket list syntax continues to work unchanged. The new syntax is purely
additive.

## Tests

| Test | Description |
|------|-------------|
| `test_parse_run_shell_style` | Parser: `run "echo hello"` → `["sh", "-c", "echo hello"]` |
| `test_parse_run_shell_background` | Parser: `run background "iperf3 -s"` |
| `test_parse_run_list_still_works` | Parser: `run ["echo", "hello"]` unchanged |

## File Changes

| File | Change |
|------|--------|
| `parser.rs` | ~5 lines in `parse_run_def()` |
