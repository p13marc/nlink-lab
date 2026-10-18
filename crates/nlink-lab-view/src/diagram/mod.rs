//! The topology as text diagrams, from its declaration rather than a
//! layout: what `render --diagram` prints and the desktop app exports.

mod ascii;
mod dot;
mod mermaid;

pub use ascii::topology_to_ascii;
pub use dot::topology_to_dot;
pub use mermaid::topology_to_mermaid;
