//! NLL (nlink-lab Language) parser.
//!
//! Parses `.nll` topology files into [`Topology`] structs.
//!
//! The pipeline is: Source → Lexer → Parser → AST → Lowering → Topology.

pub(crate) mod ast;
pub mod lexer;
pub(crate) mod lower;
pub(crate) mod parser;

use std::path::Path;

use crate::error::Result;
use crate::types::Topology;

/// Parse an NLL string into a topology (no import support).
pub fn parse(input: &str) -> Result<Topology> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    lower::lower(&ast)
}

/// Parse an NLL string with `param` overrides, no imports.
///
/// Useful for re-parsing a self-contained NLL (e.g. extracted from
/// a `.nlz` archive) with the same overrides used at export time.
pub fn parse_with_params(input: &str, params: &[(String, String)]) -> Result<Topology> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    lower::lower_with_params(&ast, None, params)
}

/// Parse an NLL string from a file path, with import resolution.
///
/// Imports are resolved relative to the file's parent directory.
pub fn parse_file_with_imports(input: &str, file_path: &Path) -> Result<Topology> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;

    if ast.imports.is_empty() {
        lower::lower(&ast)
    } else {
        let base_dir = file_path.parent().unwrap_or(Path::new("."));
        lower::lower_with_imports(&ast, base_dir)
    }
}

/// Parse an NLL string from a file path, with import resolution and CLI parameters.
///
/// Parameters are matched against `param` declarations in the top-level file.
pub fn parse_file_with_params(
    input: &str,
    file_path: &Path,
    params: &[(String, String)],
) -> Result<Topology> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    let base_dir = file_path.parent().unwrap_or(Path::new("."));
    lower::lower_with_params(&ast, Some(base_dir), params)
}

/// Parse an NLL string, producing rich diagnostics with source context on error.
pub fn parse_with_source(input: &str, filename: &str) -> Result<Topology> {
    parse(input).map_err(|e| attach_source(e, input, filename))
}

/// Turn a bare `NllParse` error into an `NllDiagnostic` carrying the
/// source it was raised against, so miette points at the right file —
/// an error inside an imported module is attributed to that module,
/// not to the importing file. Errors that already carry a source (or
/// are not parse errors) pass through unchanged.
pub(crate) fn attach_source(err: crate::Error, input: &str, filename: &str) -> crate::Error {
    let (message, offset) = match err {
        crate::Error::NllParseAt { message, offset } => (message, offset.min(input.len())),
        crate::Error::NllParse(message) => (message, 0),
        other => return other,
    };
    crate::Error::NllDiagnostic(Box::new(crate::error::NllDiagnostic {
        message,
        src: miette::NamedSource::new(filename, input.to_string()),
        span: (offset, if offset < input.len() { 1 } else { 0 }).into(),
        label: "here".to_string(),
        help: None,
    }))
}
