//! `nlink-lab fmt` — canonical whitespace for NLL sources (issue #54).
//!
//! This is a *token-stream* formatter, not an AST printer: the real lexer
//! decides where tokens begin and end, comments are recovered from the
//! gaps between tokens, and only layout changes — nothing is reordered,
//! re-quoted or expanded, so `lex(fmt(src)) == lex(src)` holds for every
//! input the parser accepts (the AST, by design, drops comments, blank
//! lines and quoting, which is why it cannot drive a preserving printer).
//!
//! Rules:
//! - two-space indentation per `{`/`[` nesting level, `}`/`]` dedent;
//! - tokens that touch in the source stay touching (names like
//!   `spine${i}`, endpoints, `10.0.${i}.0/24`); tokens separated by
//!   whitespace get exactly one space; no space before `,`; braces are
//!   always set off by one space (`node r { … }`);
//! - line structure is kept, runs of blank lines collapse to one, leading
//!   blank lines and blank lines next to a brace go, the file ends with
//!   exactly one newline;
//! - `#` comments keep their line (two spaces before a trailing one);
//!   `/* … */` comments are emitted verbatim.

use logos::Logos as _;

use crate::error::{Error, Result};
use crate::parser::nll::lexer::Token;

#[derive(Debug)]
enum Item<'a> {
    Tok(&'a str, bool), // text, glued-to-previous-token
    LineComment(&'a str),
    BlockComment(&'a str),
    Newlines(usize),
}

/// Split `gap` (the source between two tokens) into comments and
/// newline runs; whitespace is discarded.
fn scan_gap<'a>(gap: &'a str, out: &mut Vec<Item<'a>>) -> Result<()> {
    let b = gap.as_bytes();
    let mut i = 0;
    let mut newlines = 0usize;
    let flush = |newlines: &mut usize, out: &mut Vec<Item<'a>>| {
        if *newlines > 0 {
            out.push(Item::Newlines(*newlines));
            *newlines = 0;
        }
    };
    while i < b.len() {
        match b[i] {
            b'\n' => {
                newlines += 1;
                i += 1;
            }
            b'#' => {
                flush(&mut newlines, out);
                let end = gap[i..].find('\n').map_or(b.len(), |n| i + n);
                out.push(Item::LineComment(gap[i..end].trim_end()));
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                flush(&mut newlines, out);
                let mut depth = 0usize;
                let mut j = i;
                let end = loop {
                    if j + 1 >= b.len() {
                        return Err(Error::NllParse("unterminated block comment".into()));
                    }
                    if b[j] == b'/' && b[j + 1] == b'*' {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'*' && b[j + 1] == b'/' {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break j;
                        }
                    } else {
                        j += 1;
                    }
                };
                out.push(Item::BlockComment(&gap[i..end]));
                i = end;
            }
            _ => i += 1,
        }
    }
    flush(&mut newlines, out);
    Ok(())
}

fn items(src: &str) -> Result<Vec<Item<'_>>> {
    let mut out = Vec::new();
    let mut lexer = Token::lexer(src);
    let mut prev_end = 0usize;
    while let Some(res) = lexer.next() {
        let span = lexer.span();
        let text = &src[span.clone()];
        match res {
            Ok(Token::Newline) if text.starts_with("/*") => {
                // a multi-line block comment surfaces as a Newline token
                scan_gap(&src[prev_end..span.start], &mut out)?;
                out.push(Item::BlockComment(text));
            }
            Ok(Token::Newline) => {
                scan_gap(&src[prev_end..span.end], &mut out)?;
            }
            Ok(_) => {
                let gap = &src[prev_end..span.start];
                scan_gap(gap, &mut out)?;
                let glued = gap.is_empty() && prev_end > 0;
                out.push(Item::Tok(text, glued));
            }
            Err(_) => {
                let (line, col) = crate::parser::nll::lexer::line_col(src, span.start);
                return Err(Error::NllParseAt {
                    message: format!("unexpected character at line {line}, column {col}: {text:?}"),
                    offset: span.start,
                });
            }
        }
        prev_end = span.end;
    }
    scan_gap(&src[prev_end..], &mut out)?;
    // logos yields one Newline token per '\n'; fold runs so the blank-line
    // rule sees them.
    let mut merged: Vec<Item<'_>> = Vec::with_capacity(out.len());
    for item in out {
        match (merged.last_mut(), item) {
            (Some(Item::Newlines(n)), Item::Newlines(m)) => *n += m,
            (_, item) => merged.push(item),
        }
    }
    Ok(merged)
}

fn closes_first(content: &str) -> bool {
    content.starts_with('}') || content.starts_with(']')
}

/// Format an NLL source. Errors only on input the lexer rejects.
pub fn format(src: &str) -> Result<String> {
    let items = items(src)?;
    let mut out = String::with_capacity(src.len() + 16);
    let mut line = String::new();
    let mut depth = 0usize;
    let mut pending_blank = false;
    let mut first_line = true;
    let mut last_was_tok = false;

    let flush_line = |line: &mut String,
                      out: &mut String,
                      depth: usize,
                      pending_blank: &mut bool,
                      first_line: &mut bool| {
        let content = line.trim_end();
        if content.is_empty() {
            line.clear();
            return;
        }
        // no blank line right after `{` or right before `}`
        if *pending_blank && !*first_line && !closes_first(content) && !out.ends_with("{\n") {
            out.push('\n');
        }
        *pending_blank = false;
        *first_line = false;
        // a closing bracket at the start of a line sits at the outer level
        let closes = closes_first(content);
        let indent = depth.saturating_sub(usize::from(closes));
        for _ in 0..indent {
            out.push_str("  ");
        }
        out.push_str(content);
        out.push('\n');
        line.clear();
    };

    for item in &items {
        match item {
            Item::Newlines(n) => {
                // depth after this line = depth before + opens − closes on it
                let opens = line.matches(['{', '[']).count();
                let closes = line.matches(['}', ']']).count();
                let line_depth = depth;
                flush_line(
                    &mut line,
                    &mut out,
                    line_depth,
                    &mut pending_blank,
                    &mut first_line,
                );
                depth = (depth + opens).saturating_sub(closes);
                if *n >= 2 {
                    pending_blank = true;
                }
                last_was_tok = false;
            }
            Item::Tok(text, glued) => {
                let prev_brace = line.ends_with('{');
                let brace = matches!(*text, "{" | "}");
                let keep_glued = *glued && last_was_tok && !brace && !prev_brace;
                if !line.is_empty() && !keep_glued && *text != "," {
                    line.push(' ');
                }
                line.push_str(text);
                last_was_tok = true;
            }
            Item::LineComment(text) => {
                if !line.is_empty() {
                    line.push_str("  ");
                }
                line.push_str(text);
                last_was_tok = false;
            }
            Item::BlockComment(text) => {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(text);
                last_was_tok = false;
                if text.contains('\n') {
                    // keep the comment's own internal layout: flush as-is
                    let content = std::mem::take(&mut line);
                    if pending_blank && !first_line {
                        out.push('\n');
                    }
                    pending_blank = false;
                    first_line = false;
                    for _ in 0..depth {
                        out.push_str("  ");
                    }
                    out.push_str(content.trim_end());
                    out.push('\n');
                }
            }
        }
    }
    let line_depth = depth;
    flush_line(
        &mut line,
        &mut out,
        line_depth,
        &mut pending_blank,
        &mut first_line,
    );
    Ok(out)
}

/// Token sequence with source text, for `fmt`'s own invariant checks.
pub fn token_texts(src: &str) -> Result<Vec<String>> {
    Ok(items(src)?
        .into_iter()
        .filter_map(|i| match i {
            Item::Tok(t, _) => Some(t.to_string()),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_layout_and_keeps_comments() {
        let src = "\n\nlab   \"t\" {   # my lab\n     description  \"x\"\n\n\n\n}\n/* block\n   comment */\nnode   spine${i} : router{ lo 10.255.0.${i}/32 }   \nlink a:eth0 --  b:eth0 {  10.0.0.1/24 -- 10.0.0.2/24  delay 10ms }\n";
        let got = format(src).unwrap();
        let want = "lab \"t\" {  # my lab\n  description \"x\"\n}\n/* block\n   comment */\nnode spine${i} : router { lo 10.255.0.${i}/32 }\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 10ms }\n";
        assert_eq!(got, want);
    }

    #[test]
    fn idempotent_and_token_preserving() {
        let src = "lab \"t\"\nnode a{forward ipv4\nroute default via 10.0.0.1}\nnetwork lan {\n  members [a:eth0,\n    b:eth0]\n  impair a -- b { delay ${(i + 1) % 4}ms }\n}\n";
        let once = format(src).unwrap();
        assert_eq!(format(&once).unwrap(), once, "not idempotent:\n{once}");
        assert_eq!(token_texts(src).unwrap(), token_texts(&once).unwrap());
        assert!(once.contains("    b:eth0]"), "{once}");
        assert!(once.contains("route default via 10.0.0.1"), "{once}");
    }

    #[test]
    fn every_example_is_stable_and_parses_identically() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        let mut files = Vec::new();
        for entry in walkdir(std::path::Path::new(root)) {
            files.push(entry);
        }
        assert!(files.len() > 30);
        for path in files {
            let src = std::fs::read_to_string(&path).unwrap();
            let once = format(&src).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let twice = format(&once).unwrap();
            assert_eq!(once, twice, "{} is not idempotent", path.display());
            assert_eq!(
                token_texts(&src).unwrap(),
                token_texts(&once).unwrap(),
                "{} tokens changed",
                path.display()
            );
            if !path.to_string_lossy().contains("/imports/") {
                let a = crate::parser::nll::parse_file_with_imports(&src, &path).unwrap();
                let b = crate::parser::nll::parse_file_with_imports(&once, &path)
                    .unwrap_or_else(|e| panic!("{} no longer parses: {e}\n{once}", path.display()));
                assert_eq!(
                    serde_json::to_value(&a).unwrap(),
                    serde_json::to_value(&b).unwrap(),
                    "{} topology changed",
                    path.display()
                );
            }
        }
    }

    fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else if p.extension().is_some_and(|x| x == "nll") {
                out.push(p);
            }
        }
        out
    }
}
