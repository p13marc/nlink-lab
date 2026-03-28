//! NLL (nlink-lab Language) parser.
//!
//! Parses `.nll` topology files into [`Topology`] structs.
//!
//! The pipeline is: Source → Lexer → Parser → AST → Lowering → Topology.

pub mod ast;
pub mod lexer;
pub mod lower;
pub mod parser;

use crate::error::Result;
use crate::types::Topology;

/// Parse an NLL string into a topology.
pub fn parse(input: &str) -> Result<Topology> {
    let tokens = lexer::lex(input)?;
    let ast = parser::parse_tokens(&tokens, input)?;
    lower::lower(&ast)
}

/// Parse an NLL string, producing rich diagnostics with source context on error.
pub fn parse_with_source(input: &str, filename: &str) -> Result<Topology> {
    match parse(input) {
        Ok(topo) => Ok(topo),
        Err(crate::Error::NllParse(msg)) => {
            // Try to extract line/column from the message to create a span
            let span = extract_span_from_message(&msg, input);
            // Strip internal markers from user-facing message
            let clean_msg = msg
                .split(" [at byte ")
                .next()
                .unwrap_or(&msg)
                .to_string();
            Err(crate::Error::NllDiagnostic(crate::error::NllDiagnostic {
                message: clean_msg,
                src: miette::NamedSource::new(filename, input.to_string()),
                span: span.into(),
                label: "here".to_string(),
                help: None,
            }))
        }
        Err(e) => Err(e),
    }
}

/// Extract a byte offset from an error message.
///
/// Looks for patterns like `[at byte N]` (from parser) or
/// `at line N, column M` (from lexer).
fn extract_span_from_message(msg: &str, source: &str) -> (usize, usize) {
    // Try pattern: "[at byte N]" (parser errors)
    if let Some(start) = msg.find("[at byte ") {
        let after = &msg[start + 9..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(offset) = num_str.parse::<usize>() {
            return (offset.min(source.len()), 1);
        }
    }

    // Try pattern: "at line N, column M" (lexer errors)
    if let Some(line_start) = msg.find("line ") {
        let after_line = &msg[line_start + 5..];
        if let Some(comma) = after_line.find(',') {
            let line_str = &after_line[..comma];
            if let Ok(line) = line_str.parse::<usize>() {
                if let Some(col_start) = after_line.find("column ") {
                    let after_col = &after_line[col_start + 7..];
                    let col_str: String =
                        after_col.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(col) = col_str.parse::<usize>() {
                        let mut offset = 0;
                        for (i, l) in source.lines().enumerate() {
                            if i + 1 == line {
                                offset += (col - 1).min(l.len());
                                return (offset, 1);
                            }
                            offset += l.len() + 1;
                        }
                    }
                }
            }
        }
    }

    (0, 0)
}
