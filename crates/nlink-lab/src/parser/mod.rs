//! Topology parser.
//!
//! Parses NLL (`.nll`) topology files into [`Topology`] structs.
//!
//! # Example
//!
//! ```ignore
//! use nlink_lab::parser;
//!
//! let topo = parser::parse_file("datacenter.nll")?;
//! ```

pub mod nll;

use std::path::Path;

use crate::error::Result;
use crate::types::Topology;

/// Parse a topology from an NLL string (no import support).
pub fn parse(input: &str) -> Result<Topology> {
    nll::parse(input)
}

/// Parse a topology file with import resolution.
///
/// Imports are resolved relative to the file's parent directory.
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    let filename = path.display().to_string();

    // Use import-aware parsing when loading from a file
    match nll::parse_file_with_imports(&contents, path) {
        Ok(topo) => Ok(topo),
        Err(crate::Error::NllParse(msg)) => {
            let span = nll::extract_span(&msg, &contents);
            Err(crate::Error::NllDiagnostic(crate::error::NllDiagnostic {
                message: msg.split(" [at byte ").next().unwrap_or(&msg).to_string(),
                src: miette::NamedSource::new(&filename, contents),
                span: span.into(),
                label: "here".to_string(),
                help: None,
            }))
        }
        Err(e) => Err(e),
    }
}
