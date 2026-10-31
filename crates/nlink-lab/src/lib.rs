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
//! # Layers
//!
//! - **Stable**: what these docs show. [`Topology`] and its
//!   [`TopologyBuilder`], the NLL [`parser`], the [`validator`],
//!   [`Error`] / [`Result`], [`RunningLab`] and its exec types, the
//!   [`test_helpers`], failure [`bundle`]s, [`lab_test`] and the
//!   [`prelude`]. `docs/public-api.txt` lists every item, and CI fails on
//!   a change to it that the file does not show.
//! - **Internal**: `nlink_lab::internal`, hidden from these docs. It is
//!   what nlink-lab's own CLI, daemon and desktop app share with the
//!   library. It has no semver: anything in it changes in any release.
//! - **Private**: everything else.
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

// ── The stable layer ──
pub mod builder;
pub mod bundle;
pub mod error;
pub mod lab;
pub mod parser;
pub mod running;
pub mod test_helpers;
pub mod types;
pub mod validator;

// ── The internal layer: `pub` for nlink-lab's own binaries, hidden from
// the docs and the public API (reach it through [`internal`]) ──
#[doc(hidden)]
pub mod analysis;
#[doc(hidden)]
pub mod benchmark;
#[doc(hidden)]
pub mod capture;
#[doc(hidden)]
pub mod cgroup;
#[doc(hidden)]
pub mod container;
#[doc(hidden)]
pub mod deploy;
#[doc(hidden)]
pub mod diff;
#[doc(hidden)]
pub mod edit;
#[doc(hidden)]
pub mod events;
#[doc(hidden)]
pub mod fmt;
#[doc(hidden)]
pub mod foreign;
#[doc(hidden)]
pub mod frr;
#[doc(hidden)]
pub mod helpers;
#[doc(hidden)]
pub mod host;
#[doc(hidden)]
pub mod host_registry;
#[doc(hidden)]
pub mod impair_parse;
#[doc(hidden)]
pub mod ipmap;
#[doc(hidden)]
pub mod lint;
#[doc(hidden)]
pub mod logs;
#[doc(hidden)]
pub mod metrics;
#[doc(hidden)]
pub mod netns_tag;
#[doc(hidden)]
pub mod ops;
#[doc(hidden)]
pub mod portability;
#[doc(hidden)]
pub mod privileges;
#[doc(hidden)]
pub mod proc_stat;
#[doc(hidden)]
pub mod pty;
#[doc(hidden)]
pub mod render;
#[doc(hidden)]
pub mod replay;
#[doc(hidden)]
pub mod scenario;
#[doc(hidden)]
pub mod seed;
#[doc(hidden)]
pub mod state;
#[doc(hidden)]
pub mod subnet_pool;
#[doc(hidden)]
pub mod templates;
#[doc(hidden)]
pub mod test_runner;
#[doc(hidden)]
pub mod trace;
#[doc(hidden)]
pub mod wait;
#[doc(hidden)]
pub mod watch;
#[doc(hidden)]
pub mod wifi;

// ── Private ──
pub(crate) mod conntrack;
pub(crate) mod dns;
pub(crate) mod firewall;
pub(crate) mod ipfunc;
pub(crate) mod ns_exec;
#[cfg(test)]
pub(crate) mod schema_check;
#[cfg(test)]
mod schema_check_tests;

pub use builder::TopologyBuilder;
pub use error::{Error, Result};
pub use lab::{Iface, Lab, Netem, Network, Node};
/// The daemon API's wire types: what an [`OpCtx`] reports is an
/// [`api::OpEvent`].
pub use nlink_lab_shared::api;
pub use ops::{CancelToken, OpCtx, ProgressSink};
pub use proc_stat::ProcStat;
pub use running::{
    ExecChild, ExecMode, ExecOpts, ExecOutput, LogStream, NodeDiagnostic, PreparedExec,
    ProcessInfo, RunningLab, Signal, SpawnOpts, parse_signal,
};
pub use state::LabInfo;
pub use test_runner::AssertionResult;
pub use types::{
    ContainerRuntime, DnsMode, EndpointRef, ExecConfig, FirewallConfig, FirewallRule, Impairment,
    InterfaceConfig, InterfaceKind, LabConfig, Link, PortConfig, Profile, RateLimit, RouteConfig,
    Topology, VlanConfig, VrfConfig, WireguardConfig,
};
pub use validator::{
    RuleOptions, Severity, ValidationIssue, ValidationResult, rule_ids, rule_severity,
};

// What the binaries and the root tests still name at the root; hidden,
// and in `internal` too.
#[doc(hidden)]
#[allow(deprecated)]
pub use deploy::apply_diff;
#[doc(hidden)]
pub use deploy::{
    ApplyOptions, ApplyReport, NsRef, Op, Plan, Stage, apply, apply_plan, apply_plan_with,
    apply_with, apply_with_ctx, compute_layered_diff, compute_layered_diff_with, deploy_with,
    plan_for, reattach_node, reattach_node_with, restore, restore_with,
};
#[doc(hidden)]
pub use diff::{LayeredDiff, TopologyDiff, diff_topologies};
#[doc(hidden)]
pub use events::{LifecycleEvent, LifecycleKind};
#[doc(hidden)]
pub use lint::{LINT_RULE_IDS, LintFinding, lint};
#[doc(hidden)]
pub use types::{mgmt_bridge_name_for, network_peer_name_for};
#[doc(hidden)]
pub use watch::{
    NsResolver, WatchEvent, WatchEventKind, WatchFamily, WatchOpts, WatchStream, watch_loop,
    watch_stream,
};

/// Proc macro for integration testing. See [`lab_test`] for details.
pub use nlink_lab_macros::lab_test;

/// What a lab test needs, in one import: `use nlink_lab::prelude::*;`
/// (#331). It grows the lab handles when they land (#328).
pub mod prelude {
    pub use crate::lab::{bps, gbit, kbit, mbit, ms, pct, secs, us};
    pub use crate::test_helpers::{feature, privileged};
    pub use crate::{
        Error, ExecOutput, Iface, Impairment, Lab, Netem, Network, Node, Result, RunningLab,
        Topology, TopologyBuilder, lab_test,
    };
}

/// **For nlink-lab's own binaries** (the CLI, the daemon, the desktop
/// app): what they share with the library and nothing else uses. No
/// semver: anything here changes in any release, and none of it is in the
/// docs or in `docs/public-api.txt` (#331).
#[doc(hidden)]
pub mod internal {
    #[allow(deprecated)]
    pub use crate::deploy::apply_diff;
    pub use crate::deploy::{
        ApplyOptions, ApplyReport, NsRef, Op, Plan, Stage, apply, apply_plan, apply_plan_with,
        apply_with, apply_with_ctx, compute_layered_diff, compute_layered_diff_with, deploy_with,
        plan_for, reattach_node, reattach_node_with, restore, restore_with,
    };
    pub use crate::diff::{LayeredDiff, TopologyDiff, diff_topologies};
    pub use crate::events::{LifecycleEvent, LifecycleKind};
    pub use crate::lint::{LINT_RULE_IDS, LintFinding};
    pub use crate::proc_stat::ProcStat;
    pub use crate::types::{mgmt_bridge_name_for, network_peer_name_for};
    pub use crate::watch::{
        NsResolver, WatchEvent, WatchEventKind, WatchFamily, WatchOpts, WatchStream, watch_loop,
        watch_stream,
    };
    pub use crate::{
        analysis, benchmark, capture, cgroup, container, deploy, diff, edit, events, fmt, foreign,
        frr, helpers, host, host_registry, impair_parse, ipmap, lint, logs, metrics, netns_tag,
        ops, portability, privileges, proc_stat, pty, render, replay, scenario, seed, state,
        subnet_pool, templates, test_runner, trace, wait, watch, wifi,
    };
}
