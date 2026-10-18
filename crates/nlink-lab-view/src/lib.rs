//! What the nlink-lab desktop app draws, without a renderer (#273): the
//! topology as a graph, where its nodes go, the metrics and impairments it
//! shows, the shapes of a view and their SVG. Pure: everything here is
//! data in, data out, and tested as such; `top` shares its impairment
//! logic.

pub mod graph;
pub mod impair;
pub mod layout;
pub mod metrics;
pub mod scene;
pub mod svg;

pub use graph::{EdgeKind, GraphOptions, NodeKind, TopoGraph};
pub use layout::{LayoutKind, Pos, Positions};
pub use metrics::MetricsStore;
pub use scene::{Hit, Overlays, Scene};
