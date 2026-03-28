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

/// Parse a topology from an NLL string.
pub fn parse(input: &str) -> Result<Topology> {
    nll::parse(input)
}

/// Parse a topology file.
pub fn parse_file<P: AsRef<Path>>(path: P) -> Result<Topology> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    let filename = path.display().to_string();
    nll::parse_with_source(&contents, &filename)
}
