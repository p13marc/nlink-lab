//! NLL (nlink-lab Language) parser.
//!
//! Parses `.nll` topology files into [`Topology`] structs.
//!
//! The pipeline is: Source → Lexer → Parser → AST → Lowering → Topology.

pub(crate) mod ast;
pub mod lexer;
pub(crate) mod lower;
pub(crate) mod parser;
pub(crate) mod value;

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

/// The names of the `param`s an NLL source declares, in order.
pub fn declared_params(input: &str) -> Result<Vec<String>> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    Ok(ast
        .statements
        .iter()
        .filter_map(|s| match s {
            ast::Statement::Param(p) => Some(p.name.clone()),
            _ => None,
        })
        .collect())
}

/// The `param`s an NLL source declares, with their defaults, in order.
/// A param without a default must be given (`--set`).
pub fn declared_param_defs(input: &str) -> Result<Vec<(String, Option<String>)>> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    Ok(ast
        .statements
        .iter()
        .filter_map(|s| match s {
            ast::Statement::Param(p) => Some((p.name.clone(), p.default.clone())),
            _ => None,
        })
        .collect())
}

/// The files an NLL file imports, all the way down: each one's text,
/// keyed by its path from the entry's directory (`sub/site.nll`,
/// `../common.nll`) — what a deploy through the daemon carries along with
/// the entry (`SourceBundle::imports`), so the daemon never reads a
/// client's files.
pub fn import_closure(entry: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    import_closure_of(&std::fs::read_to_string(entry)?, entry)
}

/// [`import_closure`] of `text`, the entry's as an editor holds it (saved
/// or not): its imports are read from beside `entry`.
pub fn import_closure_of(
    text: &str,
    entry: &Path,
) -> Result<std::collections::BTreeMap<String, String>> {
    let base = entry.parent().unwrap_or(Path::new("."));
    let mut out = std::collections::BTreeMap::new();
    // (the importing file's directory as a key prefix, its text)
    let mut queue = vec![(String::new(), text.to_string())];
    while let Some((dir, text)) = queue.pop() {
        let tokens = lexer::lex(&text)?;
        let ast = parser::parse_tokens(&tokens, &text)?;
        for imp in &ast.imports {
            let key = lexical_join(&dir, &imp.path).ok_or_else(|| {
                crate::Error::NllParse(format!(
                    "import path {:?} is not a relative file path",
                    imp.path
                ))
            })?;
            if out.contains_key(&key) {
                continue;
            }
            let text = std::fs::read_to_string(base.join(&key)).map_err(|e| {
                crate::Error::NllParse(format!("cannot read import '{}': {e}", imp.path))
            })?;
            let key_dir = match key.rsplit_once('/') {
                Some((d, _)) => d.to_string(),
                None => String::new(),
            };
            out.insert(key, text.clone());
            queue.push((key_dir, text));
        }
    }
    Ok(out)
}

/// `rel` from `dir`, both relative and `/`-separated, without touching the
/// filesystem: `("sub", "../a.nll")` is `a.nll`, `("", "../a.nll")` stays.
fn lexical_join(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&"..") | None => parts.push(".."),
                Some(_) => {
                    parts.pop();
                }
            },
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Parse an NLL string, producing rich diagnostics with source context on error.
pub fn parse_with_source(input: &str, filename: &str) -> Result<Topology> {
    parse(input).map_err(|e| attach_source(e, input, filename))
}

/// [`parse_with_params`], with diagnostics located in `name`: a source
/// that is no file, like the desktop app's unsaved templates.
pub fn parse_named_with_params(
    input: &str,
    name: &str,
    params: &[(String, String)],
) -> Result<Topology> {
    parse_with_params(input, params).map_err(|e| attach_source(e, input, name))
}

/// Turn a bare `NllParse` error into an `NllDiagnostic` carrying the
/// source it was raised against, so miette points at the right file —
/// an error inside an imported module is attributed to that module,
/// not to the importing file. Errors that already carry a source (or
/// are not parse errors) pass through unchanged.
pub(crate) fn attach_source(err: crate::Error, input: &str, filename: &str) -> crate::Error {
    let (message, offset, len) = match err {
        crate::Error::NllParseAt { message, span } => {
            let offset = span.start.min(input.len());
            let end = span.end.clamp(offset, input.len());
            // A zero-width span at end of input stays empty; anything
            // else covers the whole token (at least one byte).
            let len = if end > offset {
                end - offset
            } else if offset < input.len() {
                1
            } else {
                0
            };
            (message, offset, len)
        }
        crate::Error::NllParse(message) => (message, 0, if input.is_empty() { 0 } else { 1 }),
        other => return other,
    };
    crate::Error::NllDiagnostic(Box::new(crate::error::NllDiagnostic {
        message,
        src: miette::NamedSource::new(filename, input.to_string()),
        span: (offset, len).into(),
        label: "here".to_string(),
        help: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_import_closure_is_keyed_from_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("lab/sub")).unwrap();
        std::fs::write(root.join("common.nll"), "lab \"common\"\nnode c\n").unwrap();
        std::fs::write(
            root.join("lab/sub/site.nll"),
            "import \"../../common.nll\" as c\nlab \"site\"\nnode s\n",
        )
        .unwrap();
        std::fs::write(
            root.join("lab/main.nll"),
            "import \"sub/site.nll\" as s\nimport \"../common.nll\" as c2\nlab \"main\"\nnode m\n",
        )
        .unwrap();
        let got = import_closure(&root.join("lab/main.nll")).unwrap();
        assert_eq!(
            got.keys().cloned().collect::<Vec<_>>(),
            ["../common.nll", "sub/site.nll"]
        );
        assert!(got["sub/site.nll"].contains("node s"));
        assert_eq!(lexical_join("sub", "../a.nll").as_deref(), Some("a.nll"));
        assert_eq!(lexical_join("", "../a.nll").as_deref(), Some("../a.nll"));
        assert_eq!(lexical_join("", "/etc/x"), None);
        // and a bundle staged from it parses as the files do
        let bundle = crate::api::SourceBundle {
            path: root.join("lab/main.nll").display().to_string(),
            text: std::fs::read_to_string(root.join("lab/main.nll")).unwrap(),
            imports: got,
            params: Vec::new(),
        };
        let staged = tempfile::tempdir().unwrap();
        let entry = crate::ops::mutate::stage_source(&bundle, staged.path()).unwrap();
        let t = crate::parser::parse_file(&entry).unwrap();
        assert!(
            t.nodes.contains_key("m"),
            "{:?}",
            t.nodes.keys().collect::<Vec<_>>()
        );
    }
}
