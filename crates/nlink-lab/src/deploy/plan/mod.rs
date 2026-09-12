//! Planners — pure functions from the topology to declarative configs.
//!
//! Nothing in here touches the kernel; every module is unit-tested
//! without root. `super` (the deployer) and the apply path consume them.

pub(crate) mod network;
pub(crate) mod nftables;
pub(crate) mod process;
pub(crate) mod qdisc;
pub(crate) mod wireguard;
