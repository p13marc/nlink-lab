//! nlink-lab: Network lab engine for Linux.
//!
//! Create isolated, reproducible network topologies using Linux network namespaces.
//! Unlike containerlab (which focuses on container orchestration), nlink-lab is
//! **networking-first**: deep control over L2/L3/L4 topology, traffic control,
//! firewalling, and diagnostics — all through a declarative TOML topology file
//! or a Rust builder DSL.
//!
//! # Quick Start
//!
//! ```ignore
//! use nlink_lab::parser;
//!
//! // Parse a topology file
//! let topology = parser::parse_file("datacenter.toml")?;
//!
//! // Validate
//! let result = topology.validate();
//! if result.has_errors() {
//!     for issue in result.errors() {
//!         eprintln!("ERROR: {}", issue);
//!     }
//!     std::process::exit(1);
//! }
//!
//! // Deploy
//! let lab = topology.deploy().await?;
//!
//! // Interact
//! let output = lab.exec("server1", "ping", &["-c1", "10.0.0.1"]).await?;
//! println!("{}", output);
//!
//! // Teardown
//! lab.destroy().await?;
//! ```
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────┐
//! │          nlink-lab CLI           │
//! │  deploy / destroy / status      │
//! └──────────┬──────────────────────┘
//!            │
//! ┌──────────▼──────────────────────┐
//! │         Lab Engine              │
//! │  Parser → Validator → Deployer  │
//! └──────────┬──────────────────────┘
//!            │
//! ┌──────────▼──────────────────────┐
//! │           nlink                 │
//! │  Namespaces, Links, TC, nftables│
//! └─────────────────────────────────┘
//! ```

pub mod builder;
pub mod deploy;
pub mod error;
pub mod helpers;
pub mod parser;
pub mod running;
pub mod state;
pub mod types;
pub mod validator;

pub use builder::Lab;
pub use error::{Error, Result};
pub use running::{ExecOutput, ProcessInfo, RunningLab};
pub use types::{
    EndpointRef, ExecConfig, FirewallConfig, FirewallRule, Impairment, InterfaceConfig, LabConfig,
    Link, Network, Node, PortConfig, Profile, RateLimit, RouteConfig, Topology, VlanConfig,
    VrfConfig, WireguardConfig,
};
pub use validator::{Severity, ValidationIssue, ValidationResult};
