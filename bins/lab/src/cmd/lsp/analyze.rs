//! One analysis pass over a document — [`nlink_lab::internal::analysis`] — with
//! its places as LSP positions.

use std::path::Path;

use nlink_lab::internal::analysis::{self, Severity};
use nlink_lab::parser::nll::lexer::Spanned;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use super::position::LineIndex;

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

/// Lex, parse (resolving `import`s relative to `path`), validate and lint:
/// see [`analysis::analyze`].
pub fn analyze(text: &str, path: Option<&Path>) -> Analysis {
    let a = analysis::analyze(text, path);
    let index = LineIndex::new(text);
    let diagnostics = a
        .diagnostics
        .iter()
        .map(|d| Diagnostic {
            range: index.range(d.range.clone()),
            severity: Some(match d.severity {
                Severity::Error => DiagnosticSeverity::ERROR,
                Severity::Warning => DiagnosticSeverity::WARNING,
                Severity::Hint => DiagnosticSeverity::HINT,
            }),
            code: Some(NumberOrString::String(d.code.clone())),
            source: Some(SOURCE.to_string()),
            message: match &d.hint {
                Some(hint) => format!("{}\nhint: {hint}", d.message),
                None => d.message.clone(),
            },
            ..Default::default()
        })
        .collect();
    Analysis {
        text: a.text,
        index,
        tokens: a.tokens,
        topology: a.topology,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_at(src: &str, d: &Diagnostic) -> String {
        let ix = LineIndex::new(src);
        let start = ix.offset(d.range.start);
        let end = ix.offset(d.range.end);
        src[start..end].to_string()
    }

    /// The library places in bytes; the protocol gets UTF-16 positions,
    /// its severities, the rule as the code and `nlink-lab` as the source.
    #[test]
    fn diagnostics_reach_the_protocol_as_positions() {
        let src = "lab \"é\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { loss 150% }\n";
        let a = analyze(src, None);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        let d = &a.diagnostics[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(text_at(src, d), "150%");
        assert_eq!(d.range.start.line, 3);
        assert_eq!(d.code, Some(NumberOrString::String("parse".into())));
        assert_eq!(d.source.as_deref(), Some("nlink-lab"));

        let src =
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n";
        let a = analyze(src, None);
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::HINT)
                    && d.code == Some(NumberOrString::String("no-assertions".into()))),
            "{:?}",
            a.diagnostics
        );
    }
}
