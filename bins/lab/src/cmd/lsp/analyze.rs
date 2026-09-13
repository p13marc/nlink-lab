//! One analysis pass over a document: lex, parse, validate, lint.
//!
//! This is the whole diagnostic pipeline and it is pure — no LSP state,
//! no I/O beyond the `import` reads the parser itself does — so it is
//! unit-testable and cheap to re-run on every keystroke.

use std::path::Path;

use nlink_lab::parser::nll::lexer::{Spanned, lex};
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use super::locate::{Resolved, Resolver};
use super::position::LineIndex;
use super::symbols;

/// `source` on every diagnostic we publish.
const SOURCE: &str = "nlink-lab";

/// Everything the server keeps for one open document.
pub struct Analysis {
    pub text: String,
    pub index: LineIndex,
    /// Empty when the document does not even lex.
    pub tokens: Vec<Spanned>,
    /// `None` when parsing or lowering failed — symbols and completion
    /// still work off `tokens`.
    pub topology: Option<nlink_lab::Topology>,
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
    let index = LineIndex::new(text);
    let tokens = lex(text).unwrap_or_default();

    // A long-lived server must survive a panic in the analysis pipeline:
    // lowering, the validator and the lints index a lot of collections,
    // and killing the process takes the user's editor session with it.
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match path {
        Some(p) => nlink_lab::parser::nll::parse_file_with_imports(text, p),
        None => nlink_lab::parser::nll::parse(text),
    }));

    let mut diagnostics = Vec::new();
    let topology = match parsed {
        Ok(Ok(topo)) => Some(topo),
        Ok(Err(e)) => {
            diagnostics.push(error_diagnostic(&e, &index, &tokens));
            None
        }
        Err(_) => {
            diagnostics.push(internal_error(&index, "parsing"));
            None
        }
    };

    if let Some(topo) = &topology {
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let issues = topo.validate().issues().to_vec();
            let lints = nlink_lab::lint(topo, &[]);
            (issues, lints)
        }));
        match checked {
            Ok((issues, lints)) => {
                let resolver = Resolver::new(&tokens, Some(topo));
                for issue in issues {
                    let severity = match issue.severity {
                        nlink_lab::Severity::Error => DiagnosticSeverity::ERROR,
                        nlink_lab::Severity::Warning => DiagnosticSeverity::WARNING,
                    };
                    diagnostics.push(located(
                        &resolver,
                        &index,
                        severity,
                        issue.rule,
                        &issue.message,
                        issue.location.as_deref(),
                    ));
                }
                for finding in lints {
                    diagnostics.push(located(
                        &resolver,
                        &index,
                        DiagnosticSeverity::HINT,
                        finding.rule,
                        &finding.message,
                        finding.location.as_deref(),
                    ));
                }
            }
            Err(_) => diagnostics.push(internal_error(&index, "validating")),
        }
    }

    Analysis {
        text: text.to_string(),
        index,
        tokens,
        topology,
        diagnostics,
    }
}

/// A validator issue or lint finding, placed via its structural location.
fn located(
    resolver: &Resolver<'_>,
    index: &LineIndex,
    severity: DiagnosticSeverity,
    rule: &str,
    message: &str,
    location: Option<&str>,
) -> Diagnostic {
    let (range, message) = match location {
        None => (index.first_line(), message.to_string()),
        Some(loc) => match resolver.resolve(loc) {
            Resolved::Exact(span) => (index.range(span), message.to_string()),
            // Keep the path visible whenever the range is only
            // approximate, so a reader can tell what it actually refers to.
            Resolved::Statement(span) => (index.range(span), format!("{message} (at {loc})")),
            Resolved::Unknown => (index.first_line(), format!("{message} (at {loc})")),
        },
    };
    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(rule.to_string())),
        source: Some(SOURCE.to_string()),
        message,
        ..Default::default()
    }
}

/// Map a parse/lowering error onto the buffer.
fn error_diagnostic(err: &nlink_lab::Error, index: &LineIndex, tokens: &[Spanned]) -> Diagnostic {
    let (range, message) = match err {
        // The only variant that carries a byte span into *this* buffer.
        nlink_lab::Error::NllParseAt { message, span } => {
            (index.range(span.clone()), message.clone())
        }
        // Raised while lowering an imported module: the span indexes the
        // imported file, not this buffer, so report it on the `import`
        // statement and carry the real position in the message.
        nlink_lab::Error::NllDiagnostic(diag) => {
            let name = diag.src.name().to_string();
            let (line, col) = line_col(diag.src.inner(), diag.span.offset());
            let range = import_range(tokens, &name)
                .map_or_else(|| index.first_line(), |span| index.range(span));
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
            let range = anchor.map_or_else(|| index.first_line(), |span| index.range(span));
            (range, message)
        }
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("parse".to_string())),
        source: Some(SOURCE.to_string()),
        message,
        ..Default::default()
    }
}

fn internal_error(index: &LineIndex, phase: &str) -> Diagnostic {
    Diagnostic {
        range: index.first_line(),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("internal".to_string())),
        source: Some(SOURCE.to_string()),
        message: format!(
            "internal error while {phase} this file — please report it with the topology at \
             https://git.marcpardo.eu/marcpardo/nlink-lab/issues"
        ),
        ..Default::default()
    }
}

/// 1-based line and column (in characters) of a byte offset.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let line = text[..offset].matches('\n').count() + 1;
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    (line, text[start..offset].chars().count() + 1)
}

/// The `import "…"` string token naming `module`, so an error inside an
/// imported file lands on the statement that pulled it in.
fn import_range(tokens: &[Spanned], module: &str) -> Option<std::ops::Range<usize>> {
    let mut first = None;
    for (i, t) in tokens.iter().enumerate() {
        if !matches!(t.token, nlink_lab::parser::nll::lexer::Token::Import) {
            continue;
        }
        let next = tokens.get(i + 1)?;
        if let nlink_lab::parser::nll::lexer::Token::String(s) = &next.token {
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
fn quoted_anchor(tokens: &[Spanned], message: &str) -> Option<std::ops::Range<usize>> {
    let quoted = message
        .split('\'')
        .nth(1)
        .or_else(|| message.split('"').nth(1))?;
    if quoted.is_empty() {
        return None;
    }
    tokens
        .iter()
        .find(|t| symbols::word(&t.token).as_deref() == Some(quoted))
        .map(|t| t.span.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(a: &Analysis) -> Vec<String> {
        a.diagnostics
            .iter()
            .map(|d| match &d.code {
                Some(NumberOrString::String(s)) => s.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn text_at(src: &str, d: &Diagnostic) -> String {
        let ix = LineIndex::new(src);
        let start = ix.offset(d.range.start);
        let end = ix.offset(d.range.end);
        src[start..end].to_string()
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
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(text_at(src, d), "150%");
        assert_eq!(d.range.start.line, 3);
    }

    #[test]
    fn a_lex_error_points_at_the_bad_character() {
        let src = "lab \"t\"\nnode a\n@\n";
        let a = analyze(src, None);
        let d = &a.diagnostics[0];
        assert_eq!(text_at(src, d), "@");
        assert!(
            a.tokens.is_empty(),
            "a file that does not lex has no tokens"
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
        assert!(d.range.start.line >= 1, "{:?}", d.range);
    }

    #[test]
    fn a_validation_error_carries_the_rule_id_as_its_code() {
        let src = "lab \"t\"\nnode a\nnode a\n";
        let a = analyze(src, None);
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
            "{:?}",
            a.diagnostics
        );
        assert!(
            a.diagnostics
                .iter()
                .all(|d| d.source.as_deref() == Some("nlink-lab")),
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
            .find(|d| d.severity == Some(DiagnosticSeverity::WARNING))
            .expect("expected a warning");
        assert_eq!(
            warn.code,
            Some(NumberOrString::String("unreferenced-node".into()))
        );
        assert_eq!(text_at(src, warn), "lonely");
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
            .filter(|d| d.severity == Some(DiagnosticSeverity::HINT))
            .collect();
        assert!(!hints.is_empty(), "{:?}", a.diagnostics);
        assert!(
            hints
                .iter()
                .any(|d| d.code == Some(NumberOrString::String("no-assertions".into())))
        );
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
}
