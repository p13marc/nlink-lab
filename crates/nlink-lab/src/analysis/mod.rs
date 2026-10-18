//! One analysis pass over an NLL document: lex, parse, validate, lint —
//! what `nlink-lab lsp` publishes and the desktop app's editor shows.
//!
//! It is pure — no editor state, no I/O beyond the `import` reads the
//! parser itself does — so it is cheap to re-run on every edit. Places
//! are byte ranges into the text; a client converts them (the LSP to
//! UTF-16 positions).

pub mod locate;
pub mod symbols;

use std::ops::Range;
use std::path::Path;

use crate::parser::nll::lexer::{Spanned, Token, lex_lenient};
use locate::{Resolved, Resolver};

/// How bad a diagnostic is: a validator error or warning, or a lint's
/// hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

/// Something to say about a place in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Bytes of the text: never empty unless the text is.
    pub range: Range<usize>,
    pub severity: Severity,
    /// The validator's or the lint's rule, `parse`, or `internal`.
    pub code: String,
    pub message: String,
    /// The validator's path into the topology (`nodes.r1.routes`), when
    /// it gave one.
    pub location: Option<String>,
    /// What to do about it, when the parser knows (`did you mean …`).
    pub hint: Option<String>,
}

/// What one pass found.
pub struct Analysis {
    pub text: String,
    /// Empty when the text does not even lex.
    pub tokens: Vec<Spanned>,
    /// `None` when parsing or lowering failed — symbols and completion
    /// still work off `tokens`.
    pub topology: Option<crate::Topology>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex, parse (resolving `import`s relative to `path`), validate and lint.
///
/// `path` is the document's own path; only its parent directory is ever
/// used, as the base for `import` resolution — the file itself is never
/// read, so an unsaved buffer is analysed exactly like a saved one. Pass
/// `None` for a document with no filesystem identity; `import` then
/// reports that it needs a file-backed document.
pub fn analyze(text: &str, path: Option<&Path>) -> Analysis {
    analyze_with_params(text, path, &[])
}

/// [`analyze`] with `--set` values for the document's `param`s.
pub fn analyze_with_params(
    text: &str,
    path: Option<&Path>,
    params: &[(String, String)],
) -> Analysis {
    let tokens = lex_lenient(text);

    // A long-lived server must survive a panic in the analysis pipeline:
    // lowering, the validator and the lints index a lot of collections,
    // and killing the process takes the user's editor session with it.
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match path {
        Some(p) if params.is_empty() => crate::parser::nll::parse_file_with_imports(text, p),
        Some(p) => crate::parser::nll::parse_file_with_params(text, p, params),
        None => crate::parser::nll::parse_with_params(text, params),
    }));

    let mut diagnostics = Vec::new();
    let topology = match parsed {
        Ok(Ok(topo)) => Some(topo),
        Ok(Err(e)) => {
            diagnostics.push(error_diagnostic(&e, text, &tokens));
            None
        }
        Err(_) => {
            diagnostics.push(internal_error(text, "parsing"));
            None
        }
    };

    if let Some(topo) = &topology {
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let issues = topo.validate().issues().to_vec();
            let lints = crate::lint(topo, &[]);
            (issues, lints)
        }));
        match checked {
            Ok((issues, lints)) => {
                let resolver = Resolver::new(&tokens, Some(topo));
                for issue in issues {
                    let severity = match issue.severity {
                        crate::Severity::Error => Severity::Error,
                        crate::Severity::Warning => Severity::Warning,
                    };
                    diagnostics.push(located(
                        &resolver,
                        text,
                        severity,
                        issue.rule,
                        &issue.message,
                        issue.location.as_deref(),
                    ));
                }
                for finding in lints {
                    diagnostics.push(located(
                        &resolver,
                        text,
                        Severity::Hint,
                        finding.rule,
                        &finding.message,
                        finding.location.as_deref(),
                    ));
                }
            }
            Err(_) => diagnostics.push(internal_error(text, "validating")),
        }
    }

    Analysis {
        text: text.to_string(),
        tokens,
        topology,
        diagnostics,
    }
}

/// `span` made a place to show: inside the text, and at least one
/// character wide when the text has one there.
fn widen(text: &str, span: Range<usize>) -> Range<usize> {
    let start = span.start.min(text.len());
    let mut end = span.end.max(start).min(text.len());
    if end == start && start < text.len() {
        end = start + text[start..].chars().next().map_or(0, char::len_utf8);
    }
    start..end
}

/// The first line, where what cannot be placed goes.
pub fn first_line(text: &str) -> Range<usize> {
    widen(text, 0..text.find('\n').unwrap_or(text.len()))
}

/// A validator issue or lint finding, placed via its structural location.
fn located(
    resolver: &Resolver<'_>,
    text: &str,
    severity: Severity,
    rule: &str,
    message: &str,
    location: Option<&str>,
) -> Diagnostic {
    let (range, message) = match location {
        None => (first_line(text), message.to_string()),
        Some(loc) => match resolver.resolve(loc) {
            Resolved::Exact(span) => (widen(text, span), message.to_string()),
            // Keep the path visible whenever the range is only
            // approximate, so a reader can tell what it actually refers to.
            Resolved::Statement(span) => (widen(text, span), format!("{message} (at {loc})")),
            Resolved::Unknown => (first_line(text), format!("{message} (at {loc})")),
        },
    };
    Diagnostic {
        range,
        severity,
        code: rule.to_string(),
        message,
        location: location.map(str::to_string),
        hint: None,
    }
}

/// Map a parse/lowering error onto the buffer.
fn error_diagnostic(err: &crate::Error, text: &str, tokens: &[Spanned]) -> Diagnostic {
    let (range, message) = match err {
        // The only variant that carries a byte span into *this* buffer.
        crate::Error::NllParseAt { message, span } => (widen(text, span.clone()), message.clone()),
        // Raised while lowering an imported module: the span indexes the
        // imported file, not this buffer, so report it on the `import`
        // statement and carry the real position in the message.
        crate::Error::NllDiagnostic(diag) => {
            let name = diag.src.name().to_string();
            let (line, col) = line_col(diag.src.inner(), diag.span.offset());
            let range = import_range(tokens, &name)
                .map_or_else(|| first_line(text), |span| widen(text, span));
            (range, format!("{name}:{line}:{col}: {}", diag.message))
        }
        // Spanless: anchor on a name the message quotes, else — for the
        // import errors, which are most of this class — on the first
        // `import` statement.
        other => {
            let message = other.to_string();
            let anchor = quoted_anchor(tokens, &message).or_else(|| {
                message
                    .contains("import")
                    .then(|| import_range(tokens, ""))
                    .flatten()
            });
            let range = anchor.map_or_else(|| first_line(text), |span| widen(text, span));
            (range, message)
        }
    };
    Diagnostic {
        range,
        severity: Severity::Error,
        code: "parse".to_string(),
        message,
        location: None,
        hint: err.to_api().hint,
    }
}

fn internal_error(text: &str, phase: &str) -> Diagnostic {
    Diagnostic {
        range: first_line(text),
        severity: Severity::Error,
        code: "internal".to_string(),
        message: format!(
            "internal error while {phase} this file — please report it with the topology at \
             https://git.marcpardo.eu/marcpardo/nlink-lab/issues"
        ),
        location: None,
        hint: None,
    }
}

/// 1-based line and column (in characters) of a byte offset.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let line = text[..offset].matches('\n').count() + 1;
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    (line, text[start..offset].chars().count() + 1)
}

/// The `import "…"` string token naming `module`, so an error inside an
/// imported file lands on the statement that pulled it in.
fn import_range(tokens: &[Spanned], module: &str) -> Option<Range<usize>> {
    let mut first = None;
    for (i, t) in tokens.iter().enumerate() {
        if !matches!(t.token, Token::Import) {
            continue;
        }
        let next = tokens.get(i + 1)?;
        if let Token::String(s) = &next.token {
            if module.ends_with(s.as_str()) {
                return Some(next.span.clone());
            }
            first.get_or_insert(next.span.clone());
        }
    }
    first
}

/// A spanless error usually names the offender in quotes
/// (`required parameter 'size' not provided`). Find that word's token.
fn quoted_anchor(tokens: &[Spanned], message: &str) -> Option<Range<usize>> {
    let quoted = message
        .split('\'')
        .nth(1)
        .or_else(|| message.split('"').nth(1))?;
    if quoted.is_empty() {
        return None;
    }
    let mut named = tokens
        .iter()
        .filter(|t| symbols::word(&t.token).as_deref() == Some(quoted));
    // a duplicate is the later one: the first is the original (#218)
    let hit = if message.contains("duplicate ") {
        named.next_back()
    } else {
        named.next()
    };
    hit.map(|t| t.span.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(a: &Analysis) -> Vec<String> {
        a.diagnostics.iter().map(|d| d.code.clone()).collect()
    }

    fn text_at(src: &str, d: &Diagnostic) -> String {
        src[d.range.clone()].to_string()
    }

    /// The 0-based line a diagnostic starts on.
    fn line(src: &str, d: &Diagnostic) -> usize {
        line_col(src, d.range.start).0 - 1
    }

    /// A topology with nothing at all to report: it has a description
    /// (else the `no-description` lint fires) and assertions (else
    /// `no-assertions` does).
    const GOOD: &str = concat!(
        "lab \"t\" {\n  description \"clean\"\n}\n",
        "node a\nnode b\n",
        "link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
        "validate { reach a b }\n",
    );

    #[test]
    fn a_clean_topology_has_no_diagnostics() {
        let a = analyze(GOOD, None);
        assert!(a.topology.is_some());
        assert_eq!(a.diagnostics, vec![], "{:?}", a.diagnostics);
    }

    #[test]
    fn a_parse_error_points_at_the_offending_token() {
        let src = "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { loss 150% }\n";
        let a = analyze(src, None);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        let d = &a.diagnostics[0];
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(text_at(src, d), "150%");
        assert_eq!(line(src, d), 3);
    }

    /// A diagnostic lands on its own statement: an assertion in a second
    /// `validate` block, the second of two equal addresses, the second
    /// declaration of a name (#218).
    #[test]
    fn diagnostics_land_on_the_offender_not_the_first_lookalike() {
        let src = "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\nvalidate { reach a b }\nvalidate { reach a ghost }\n";
        let a = analyze(src, None);
        let lines: Vec<usize> = a.diagnostics.iter().map(|d| line(src, d)).collect();
        assert!(
            lines.contains(&5),
            "the ghost is on line 5: {:?}",
            a.diagnostics
        );
        assert!(!lines.contains(&4), "{:?}", a.diagnostics);

        let src =
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.1/24 }\n";
        let a = analyze(src, None);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.message.contains("duplicate address"))
            .expect("a duplicate-address warning");
        assert_eq!(line_col(src, d.range.start).1 - 1, 39, "{d:?}");

        let src = "lab \"t\"\nnode a\nnode a\n";
        let a = analyze(src, None);
        assert_eq!(line(src, &a.diagnostics[0]), 2, "{:?}", a.diagnostics);
    }

    #[test]
    fn a_lex_error_points_at_the_bad_character() {
        let src = "lab \"t\"\nnode a\n~\n";
        let a = analyze(src, None);
        let d = &a.diagnostics[0];
        assert_eq!(text_at(src, d), "~");
        // the tokens around it are kept, for completion and symbols (#218)
        assert!(
            a.tokens
                .iter()
                .any(|t| matches!(&t.token, Token::Ident(n) if n == "a")),
            "{:?}",
            a.tokens
        );
    }

    #[test]
    fn an_unterminated_block_comment_is_one_diagnostic() {
        let src = "lab \"t\"\nnode a\n/* never closed\nnode b\n";
        let a = analyze(src, None);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        assert!(
            text_at(src, &a.diagnostics[0]).starts_with("/*"),
            "the whole unterminated comment is underlined"
        );
    }

    #[test]
    fn an_unexpected_eof_lands_at_the_end_of_the_file() {
        let src = "lab \"t\"\nnode a {\n";
        let a = analyze(src, None);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        let d = &a.diagnostics[0];
        assert!(line(src, d) >= 1, "{:?}", d.range);
    }

    #[test]
    fn a_validation_error_carries_the_rule_id_as_its_code() {
        let src = "lab \"t\"\nnode a\nnode a\n";
        let a = analyze(src, None);
        assert!(
            a.diagnostics.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            a.diagnostics
        );
        assert!(!codes(&a).is_empty());
    }

    #[test]
    fn an_unreferenced_node_warning_points_at_the_node_name() {
        let src = concat!(
            "lab \"t\"\n",
            "node a\nnode b\n",
            "link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
            "node lonely\n",
        );
        let a = analyze(src, None);
        let warn = a
            .diagnostics
            .iter()
            .find(|d| d.severity == Severity::Warning)
            .expect("expected a warning");
        assert_eq!(warn.code, "unreferenced-node");
        assert_eq!(text_at(src, warn), "lonely");
        assert_eq!(warn.location.as_deref(), Some("nodes.lonely"));
    }

    #[test]
    fn lint_findings_are_hints() {
        // No `validate` block and no description: two lint rules fire.
        let src =
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n";
        let a = analyze(src, None);
        let hints: Vec<_> = a
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Hint)
            .collect();
        assert!(!hints.is_empty(), "{:?}", a.diagnostics);
        assert!(hints.iter().any(|d| d.code == "no-assertions"));
    }

    #[test]
    fn an_unresolvable_location_is_reported_at_the_top_with_the_path() {
        // The duplicate address is on a loop-generated link, so the
        // location cannot be mapped back to a source span.
        let src = concat!(
            "lab \"t\"\n",
            "for i in 1..3 {\n  node n${i}\n}\n",
            "link n1:eth0 -- n2:eth0 { 10.0.0.1/24 -- 10.0.0.1/24 }\n",
        );
        let a = analyze(src, None);
        let dup = a
            .diagnostics
            .iter()
            .find(|d| d.message.contains("(at "))
            .expect("expected an annotated message");
        assert!(dup.message.contains("(at "), "{}", dup.message);
    }

    #[test]
    fn an_import_without_a_path_is_reported_on_the_import_line() {
        let src = "import \"other.nll\" as o\nlab \"t\"\nnode a\n";
        let a = analyze(src, None);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        assert_eq!(text_at(src, &a.diagnostics[0]), "\"other.nll\"");
    }

    #[test]
    fn a_missing_import_file_is_reported_on_the_import_line() {
        let dir = tempfile::tempdir().unwrap();
        let src = "import \"nope.nll\" as o\nlab \"t\"\nnode a\n";
        let a = analyze(src, Some(&dir.path().join("buffer.nll")));
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        assert_eq!(text_at(src, &a.diagnostics[0]), "\"nope.nll\"");
    }

    #[test]
    fn an_error_inside_an_imported_file_names_that_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mod.nll"),
            "lab \"mod\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { loss 150% }\n",
        )
        .unwrap();
        let src = "import \"mod.nll\" as m\nlab \"t\"\n";
        let a = analyze(src, Some(&dir.path().join("buffer.nll")));
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        let d = &a.diagnostics[0];
        assert!(d.message.contains("mod.nll:4:"), "{}", d.message);
        // …and points at the import statement in *this* buffer.
        assert_eq!(text_at(src, d), "\"mod.nll\"");
    }

    #[test]
    fn imports_resolve_relative_to_the_documents_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.nll"), "lab \"mod\"\nnode m\n").unwrap();
        let src = "import \"mod.nll\" as m\nlab \"t\"\nnode a\n";
        // The buffer itself is never written to disk.
        let a = analyze(src, Some(&dir.path().join("unsaved.nll")));
        let topo = a
            .topology
            .expect("imports should resolve from the parent dir");
        assert!(
            topo.nodes.contains_key("m-m") || topo.nodes.contains_key("m"),
            "{:?}",
            topo.nodes.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn params_are_taken() {
        let src = "param size default 2\nlab \"t\"\nfor i in 1..${size} {\n  node n${i}\n}\n";
        let a = analyze_with_params(src, None, &[("size".into(), "4".into())]);
        assert_eq!(a.topology.unwrap().nodes.len(), 4);
        let dir = tempfile::tempdir().unwrap();
        let a = analyze_with_params(
            src,
            Some(&dir.path().join("t.nll")),
            &[("size".into(), "3".into())],
        );
        assert_eq!(a.topology.unwrap().nodes.len(), 3);
    }

    #[test]
    fn tokens_survive_a_lowering_failure_so_symbols_keep_working() {
        let src = "lab \"t\"\nnode a\nlink a:eth0 -- ghost:eth0\n";
        let a = analyze(src, None);
        assert!(a.topology.is_none() || !a.diagnostics.is_empty());
        assert!(!a.tokens.is_empty());
    }

    #[test]
    fn line_col_is_one_based() {
        assert_eq!(line_col("a\nbc", 0), (1, 1));
        assert_eq!(line_col("a\nbc", 2), (2, 1));
        assert_eq!(line_col("a\nbc", 4), (2, 3));
    }

    #[test]
    fn places_are_inside_the_text_and_never_empty() {
        assert_eq!(widen("abc", 1..1), 1..2);
        assert_eq!(widen("abc", 5..9), 3..3);
        assert_eq!(widen("é", 0..0), 0..2);
        assert_eq!(first_line("ab\ncd"), 0..2);
        assert_eq!(first_line(""), 0..0);
    }
}
