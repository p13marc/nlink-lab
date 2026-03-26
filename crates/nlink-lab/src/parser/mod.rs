//! Topology parser with format dispatch.
//!
//! Supports both TOML (`.toml`) and NLL (`.nll`) topology formats.
//! Both formats produce the same [`Topology`] struct.
//!
//! # Example
//!
//! ```ignore
//! use nlink_lab::parser;
//!
//! // Auto-detect format by extension
//! let topo = parser::parse_file("datacenter.nll")?;
//!
//! // Parse TOML directly
//! let topo = parser::parse(toml_string)?;
//! ```

pub mod nll;
pub mod toml;

use std::path::Path;

use crate::error::Result;
use crate::types::Topology;

/// Parse a topology from a TOML string.
///
/// For format-agnostic parsing from files, use [`parse_file`] instead.
pub fn parse(input: &str) -> Result<Topology> {
    toml::parse(input)
}

/// Parse a topology file, selecting format by extension.
///
/// - `.nll` files are parsed with the NLL parser
/// - All other files (including `.toml`) are parsed as TOML
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;

    match path.extension().and_then(|e| e.to_str()) {
        Some("nll") => {
            let filename = path.display().to_string();
            nll::parse_with_source(&contents, &filename)
        }
        _ => toml::parse(&contents),
    }
}
