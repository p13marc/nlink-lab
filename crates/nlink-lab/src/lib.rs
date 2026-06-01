#![allow(clippy::field_reassign_with_default)]
// Plan 158b — Error::Namespace and Nlink variants carry an inline
// `nlink::Error` so that `Error::ext_ack()` can walk the typed
// source chain. Boxing the source breaks thiserror's #[source]
// downcast walk; an ergonomic clippy lint about the resulting
// enum size is the lesser cost.
#![allow(clippy::result_large_err)]
//! nlink-lab: Network lab engine for Linux.
//!
//! Create isolated, reproducible network topologies using Linux network namespaces.
//! Unlike containerlab (which focuses on container orchestration), nlink-lab is
//! **networking-first**: deep control over L2/L3/L4 topology, traffic control,
//! firewalling, and diagnostics — all through the NLL topology DSL
//! or a Rust builder DSL.
//!
//! # Quick Start
//!
//! ```ignore
//! use nlink_lab::parser;
//!
//! // Parse a topology file
//! let topology = parser::parse_file("datacenter.nll")?;
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
//! let output = lab.exec("server1", "ping", &["-c1", "10.0.0.1"])?;
//! println!("{}", output.stdout);
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

pub mod benchmark;
pub mod builder;
pub mod capture;
pub mod container;
pub mod deploy;
pub mod diff;
pub mod dns;
pub mod error;
pub mod helpers;
pub mod impair_parse;
pub mod ipfunc;
pub mod parser;
pub mod portability;
pub mod proc_stat;
pub mod render;
pub mod running;
pub mod scenario;
pub mod state;
pub mod subnet_pool;
pub mod templates;
pub mod test_helpers;
pub mod test_runner;
pub mod types;
pub mod validator;
pub mod watch;
pub mod wifi;

pub use builder::Lab;
pub use deploy::{apply_diff, compute_layered_diff};
pub use diff::{LayeredDiff, TopologyDiff, diff_topologies};
pub use error::{Error, Result};
pub use proc_stat::ProcStat;
pub use running::{
    ExecOpts, ExecOutput, LogStream, NodeDiagnostic, ProcessInfo, RunningLab, SpawnOpts,
};
pub use watch::{WatchEvent, WatchEventKind, WatchFamily, WatchOpts, watch_loop};
pub use types::{
    ContainerRuntime, DnsMode, EndpointRef, ExecConfig, FirewallConfig, FirewallRule, Impairment,
    InterfaceConfig, InterfaceKind, LabConfig, Link, Network, Node, PortConfig, Profile, RateLimit,
    RouteConfig, Topology, VlanConfig, VrfConfig, WireguardConfig, mgmt_bridge_name_for,
    network_peer_name_for,
};
pub use validator::{Severity, ValidationIssue, ValidationResult};

/// Proc macro for integration testing. See [`lab_test`] for details.
pub use nlink_lab_macros::lab_test;
