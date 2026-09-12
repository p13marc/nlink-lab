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

/// Parse a topology from an NLL string with `param` overrides, no
/// imports. Used by [`crate::portability::import_archive`] to
/// re-parse an extracted topology with bundled `--set` values.
pub fn parse_with_params(input: &str, params: &[(String, String)]) -> Result<Topology> {
    nll::parse_with_params(input, params)
}

/// Parse a topology file with import resolution.
///
/// Imports are resolved relative to the file's parent directory.
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    let filename = path.display().to_string();

    // Use import-aware parsing when loading from a file
    nll::parse_file_with_imports(&contents, path)
        .map_err(|e| nll::attach_source(e, &contents, &filename))
}

/// Parse a topology file with import resolution and CLI parameters.
///
/// Parameters are matched against `param` declarations in the file.
/// Use `--set key=value` on the CLI to pass parameters.
pub fn parse_file_with_params<P: AsRef<Path>>(
    path: P,
    params: &[(String, String)],
) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    let filename = path.display().to_string();

    nll::parse_file_with_params(&contents, path, params)
        .map_err(|e| nll::attach_source(e, &contents, &filename))
}
