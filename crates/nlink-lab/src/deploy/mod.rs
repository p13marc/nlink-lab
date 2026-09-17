//! Lab deployment engine.
//!
//! Takes a validated [`Topology`] and creates the actual network lab using
//! nlink APIs. Follows the deployment sequence from the design document.

#[cfg(feature = "wireguard")]
use nlink::Wireguard;
use nlink::netlink::namespace;
use nlink::{Connection, Route};
use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::error::{Error, Result};
use crate::helpers::parse_rate_bps;
use crate::running::RunningLab;
use crate::state::{self, LabState};
use crate::types::{EndpointRef, Impairment, Topology};

mod apply;
pub mod op;
pub(crate) mod plan;
pub mod rollback;

use rollback::{Journal, Undo};

pub use op::{NsRef, Op, Plan, Stage};
pub(crate) use plan::network::*;
pub(crate) use plan::nftables::*;
pub(crate) use plan::qdisc::*;
#[cfg(feature = "wireguard")]
pub(crate) use plan::wireguard::WgKeys;
#[cfg(all(test, feature = "wireguard"))]
pub(crate) use plan::wireguard::*;
pub use plan::{PlanInputs, plan};

/// Deploy a topology: validate, lock, allocate pooled subnets, then
/// `execute(plan(topology))`. Every mutation is journaled; on any error
/// the journal is unwound before the error is returned. A journal left
/// behind by an interrupted earlier run of the same lab is unwound
/// first.
pub async fn deploy(topology: &Topology) -> Result<RunningLab> {
    topology.validate().bail()?;
    let _lock = state::lock(&topology.lab.name)?;
    if state::exists(&topology.lab.name) {
        return Err(Error::AlreadyExists {
            name: topology.lab.name.clone(),
        });
    }
    if let Some(mut pending) = Journal::load_pending(&topology.lab.name) {
        tracing::warn!(
            "lab '{}': unwinding {} journal entries left by an interrupted run",
            topology.lab.name,
            pending.entries().len()
        );
        pending.unwind().await;
    }

    // Resolve `auto/N` subnet placeholders against the host-wide pool
    // before any kernel state is created. Allocations are recorded
    // against the lab name so destroy/rollback can free them.
    let mut owned = topology.clone();
    let lab_name = owned.lab.name.clone();
    let allocated = crate::subnet_pool::substitute_auto_subnets(&mut owned, |prefix| {
        crate::subnet_pool::allocate(&lab_name, prefix)
    })?;
    let topology = &owned;

    let mut journal = Journal::new(&lab_name);
    if !allocated.is_empty() {
        journal.record(Undo::FreeSubnets {
            lab: lab_name.clone(),
        });
    }

    match deploy_inner(topology, &mut journal).await {
        Ok(running) => {
            journal.discard();
            Ok(running)
        }
        Err(e) => {
            tracing::warn!("deploy of '{lab_name}' failed: {e}; rolling back");
            journal.unwind().await;
            Err(e)
        }
    }
}

async fn deploy_inner(topology: &Topology, journal: &mut Journal) -> Result<RunningLab> {
    let inputs = PlanInputs::for_deploy(topology)?;
    let plan = plan::plan(topology, &inputs)?;
    tracing::info!("plan: {} op(s)", plan.ops.len());
    let mut env = apply::ApplyEnv::for_deploy(topology)?;
    apply::execute(&plan, &mut env, journal).await?;

    // ── state file ──
    #[cfg(feature = "wireguard")]
    let wg_public_keys_b64 = {
        use base64::Engine;
        let mut map = BTreeMap::new();
        if let Some(keys) = &inputs.wg_keys {
            wireguard_secrets::save(&topology.lab.name, keys)?;
            for (node, ifaces) in keys {
                let mut node_map = BTreeMap::new();
                for (iface, (_priv, pubkey)) in ifaces {
                    node_map.insert(
                        iface.clone(),
                        base64::engine::general_purpose::STANDARD.encode(pubkey),
                    );
                }
                map.insert(node.clone(), node_map);
            }
        }
        map
    };
    #[cfg(not(feature = "wireguard"))]
    let wg_public_keys_b64 = BTreeMap::new();

    tracing::info!("writing state file");
    let mut lab_state = LabState::new(topology.lab.name.clone(), now_iso8601());
    lab_state.namespaces = env.namespace_names.clone();
    lab_state.pids = env.pids.clone();
    lab_state.starttimes = env.starttimes.clone();
    lab_state.exec_pids = env.exec_pids.clone();
    lab_state.mgmt_peers = env.mgmt_peers.clone();
    lab_state.wg_public_keys = wg_public_keys_b64;
    lab_state.containers = env.containers.clone();
    lab_state.runtime = env.runtime.as_ref().map(|rt| rt.binary().to_string());
    lab_state.dns_injected = env.dns_injected;
    lab_state.wifi_loaded = env.wifi_loaded;
    lab_state.process_logs = env.process_logs.clone();
    state::save(&lab_state, topology)?;
    crate::events::record(
        &topology.lab.name,
        crate::events::LifecycleKind::Deployed {
            nodes: topology.nodes.len(),
            links: topology.links.len(),
        },
    );

    let mut running = RunningLab::new(
        topology.clone(),
        env.namespace_names,
        env.containers,
        env.runtime.as_ref().map(|rt| rt.binary().to_string()),
        env.pids,
        env.dns_injected,
        env.wifi_loaded,
    );
    running.set_starttimes(env.starttimes);
    running.set_exec_pids(env.exec_pids);
    running.set_mgmt_peers(env.mgmt_peers);
    running.set_process_logs(env.process_logs);

    // ── validate { … } assertions ──
    // Never fails the deploy; the structured results ride on the
    // returned lab so callers (`deploy --strict`) can decide.
    if !topology.assertions.is_empty() {
        tracing::info!("running validate assertions");
        let results = run_assertions(&running, topology);
        running.set_assertion_results(results);
    }

    Ok(running)
}

/// Print what a deploy would do, without touching anything.
pub fn plan_for(topology: &Topology) -> Result<Plan> {
    let inputs = PlanInputs::for_deploy(topology)?;
    plan::plan(topology, &inputs)
}

/// Outcome of [`apply`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ApplyReport {
    /// Ops executed (removals + additions/changes).
    pub ops: usize,
    /// Of which removals.
    pub removed: usize,
    /// One-line descriptions, in execution order.
    pub applied: Vec<String>,
}

/// Reconcile a running lab to `desired`: `execute(plan(desired) −
/// plan(current))`, with the declarative layers purging undeclared
/// addresses/routes. Journaled and rolled back on failure like a deploy;
/// the state file is updated read-modify-write.
/// The pure half of [`apply`]: the plan `apply` would execute for
/// `desired` on top of `running` (removals first, in reverse stage
/// order). No kernel access; used by `apply --dry-run` / `--check` to
/// show what the layered diff cannot (e.g. VRF-table route removals).
pub fn apply_plan(running: &RunningLab, desired: &Topology) -> Result<Plan> {
    desired.validate().bail()?;
    Ok(plan_apply(running, desired, &ApplyOptions::default())?.1)
}

/// Knobs for [`apply_with`].
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// Drop every runtime impairment (`nlink-lab impair`) and converge
    /// on the topology's `impair` declarations instead of keeping the
    /// live values (#59).
    pub reset_impairments: bool,
}

/// Runtime impairments (`impair` at the CLI) are not in either topology.
/// Unless the topology *changed* that endpoint's declaration (or the
/// caller asked for a reset), the live value stays: it is overlaid onto
/// both the current and the desired topology so the plan diff sees no
/// change there. Returns the overlaid pair and the live entries kept.
pub(crate) fn overlay_live_impairments(
    running: &RunningLab,
    desired_in: &Topology,
    opts: &ApplyOptions,
) -> (Topology, Topology, BTreeMap<String, Impairment>) {
    let mut current = running.topology().clone();
    let mut desired = desired_in.clone();
    let mut kept = BTreeMap::new();
    let overlay = |topo: &mut Topology, ep: &str, live: &Impairment| {
        if *live == Impairment::default() {
            topo.impairments.remove(ep);
        } else {
            topo.impairments.insert(ep.to_string(), live.clone());
        }
    };
    let unchanged =
        |ep: &str| running.topology().impairments.get(ep) == desired_in.impairments.get(ep);
    for (ep, live) in running.live_impairments() {
        // The current plan must describe what is really installed, or
        // the diff sees "declared == declared" and never touches tc.
        overlay(&mut current, ep, live);
        if opts.reset_impairments || !unchanged(ep) {
            // Reset, or the topology changed this endpoint: its
            // declaration wins.
            continue;
        }
        overlay(&mut desired, ep, live);
        kept.insert(ep.clone(), live.clone());
    }
    // A partitioned endpoint really carries 100% loss. It stays
    // partitioned across an apply unless reset was asked for or the
    // topology changed that endpoint; `apply_with` drops the partition
    // record for every endpoint the plan touched.
    let partition_imp = Impairment {
        loss: Some("100%".to_string()),
        ..Default::default()
    };
    for ep in running.partitions().keys() {
        current
            .impairments
            .insert(ep.clone(), partition_imp.clone());
        if !opts.reset_impairments && unchanged(ep) {
            desired
                .impairments
                .insert(ep.clone(), partition_imp.clone());
        }
    }
    (current, desired, kept)
}

fn plan_apply(
    running: &RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<(PlanInputs, Plan)> {
    let (current, desired, _) = overlay_live_impairments(running, desired, opts);
    let desired = &desired;

    #[cfg(feature = "wireguard")]
    let inputs = {
        let mut inputs = PlanInputs::for_deploy(desired)?;
        // keep the keys of interfaces that already exist so `apply` does
        // not rotate every peer's key
        if let Some(fresh) = inputs.wg_keys.as_mut()
            && let Some(saved) = wireguard_secrets::load(running.name())
        {
            for (node, ifaces) in saved {
                if let Some(target) = fresh.get_mut(&node) {
                    for (iface, keys) in ifaces {
                        if target.contains_key(&iface) {
                            target.insert(iface, keys);
                        }
                    }
                }
            }
        }
        inputs
    };
    #[cfg(not(feature = "wireguard"))]
    let inputs = PlanInputs::for_deploy(desired)?;

    let cur_plan = plan::plan(&current, &inputs)?;
    let des_plan = plan::plan(desired, &inputs)?;
    Ok((inputs, Plan::diff(&cur_plan, &des_plan)))
}

pub async fn apply(running: &mut RunningLab, desired: &Topology) -> Result<ApplyReport> {
    apply_with(running, desired, &ApplyOptions::default()).await
}

/// [`apply`] with [`ApplyOptions`].
pub async fn apply_with(
    running: &mut RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<ApplyReport> {
    desired.validate().bail()?;
    let _lock = state::lock(running.name())?;
    let (_, _, kept_live) = overlay_live_impairments(running, desired, opts);
    let (inputs, diff) = plan_apply(running, desired, opts)?;
    let report = ApplyReport {
        ops: diff.ops.len(),
        removed: diff.ops.iter().filter(|o| o.is_removal()).count(),
        applied: diff.ops.iter().map(|o| o.describe()).collect(),
    };
    tracing::info!("apply: {} op(s), {} removal(s)", report.ops, report.removed);

    let mut env = apply::ApplyEnv::from_running(running, desired)?;
    let mut journal = Journal::new(running.name());
    if let Err(e) = apply::execute(&diff, &mut env, &mut journal).await {
        tracing::warn!(
            "apply to '{}' failed: {e}; rolling back this apply",
            running.name()
        );
        journal.unwind().await;
        crate::events::record(
            running.name(),
            crate::events::LifecycleKind::ApplyFailed {
                error: e.to_string(),
            },
        );
        return Err(e);
    }
    journal.discard();
    crate::events::record(
        running.name(),
        crate::events::LifecycleKind::Applied {
            ops: report.ops,
            removed: report.removed,
        },
    );

    #[cfg(feature = "wireguard")]
    if let Some(keys) = &inputs.wg_keys {
        wireguard_secrets::save(running.name(), keys)?;
    }

    running.set_topology(desired.clone());
    running.set_live_impairments(kept_live);
    // Partitions whose qdisc the plan replaced or cleared are healed.
    let touched: std::collections::BTreeSet<String> = diff
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Netem { node, iface, .. }
            | Op::ClearQdisc { node, iface }
            | Op::Qdisc { node, iface, .. } => Some(format!("{node}:{iface}")),
            _ => None,
        })
        .collect();
    running.retain_partitions(|ep| !touched.contains(ep));
    running.absorb_apply(
        env.namespace_names,
        env.containers,
        env.pids,
        env.starttimes,
        env.exec_pids,
        env.process_logs,
        env.mgmt_peers,
        env.dns_injected,
        env.wifi_loaded,
    );
    // Read-modify-write: created_at / wg_public_keys survive (#28).
    let mut lab_state = match state::load(running.name()) {
        Ok((existing, _)) => existing,
        Err(_) => LabState::new(running.name().to_string(), now_iso8601()),
    };
    lab_state.schema_version = state::SCHEMA_VERSION;
    lab_state.namespaces = running.namespace_names().clone();
    lab_state.pids = running.pids().to_vec();
    lab_state.starttimes = running.starttimes().clone();
    lab_state.exec_pids = running.exec_pids().clone();
    lab_state.mgmt_peers = running.mgmt_peers().clone();
    lab_state.containers = running.containers().clone();
    lab_state.runtime = running.runtime_binary().map(|s| s.to_string());
    lab_state.dns_injected = running.dns_injected();
    lab_state.wifi_loaded = running.wifi_loaded();
    lab_state.process_logs = running.process_logs_map().clone();
    lab_state.saved_impairments = running.saved_impairments_map().clone();
    lab_state.live_impairments = running.live_impairments().clone();
    state::save(&lab_state, desired)?;

    // ── validate { … } assertions, exactly as after deploy (#86) ──
    // Never fails the apply; results ride on `running` for `--strict`.
    if desired.assertions.is_empty() {
        running.set_assertion_results(Vec::new());
    } else {
        tracing::info!("running validate assertions");
        let results = run_assertions(running, desired);
        running.set_assertion_results(results);
    }
    Ok(report)
}

/// Restore a lab to a [`Snapshot`](crate::state::Snapshot) (#59): apply
/// the snapshot's topology (dropping the current runtime impairments),
/// then put back the snapshot's runtime impairments and partitions.
///
/// Checkpoint semantics — this restores everything nlink-lab itself
/// manages (topology, `impair`, `partition`), not hand-made `ip`/`tc`
/// edits inside the namespaces.
pub async fn restore(
    running: &mut RunningLab,
    snapshot: &crate::state::Snapshot,
) -> Result<ApplyReport> {
    let opts = ApplyOptions {
        reset_impairments: true,
    };
    // With reset the plan replaces every runtime impairment and every
    // partition by the snapshot topology's declarations, and the
    // bookkeeping is dropped with them.
    let report = apply_with(running, &snapshot.topology, &opts).await?;

    // Runtime impairments, then partitions on top (a partition saves the
    // live value as what `heal` restores).
    for (ep, imp) in &snapshot.state.live_impairments {
        if *imp == Impairment::default() {
            running.clear_impairment(ep).await?;
        } else {
            running.set_impairment(ep, imp).await?;
        }
    }
    for ep in snapshot.state.saved_impairments.keys() {
        if !running.is_partitioned(ep) {
            running.partition(ep).await?;
        }
    }
    crate::events::record(
        running.name(),
        crate::events::LifecycleKind::Restored {
            name: snapshot.meta.name.clone(),
        },
    );
    Ok(report)
}

/// Re-create every link of `node` after its container was restarted.
///
/// `docker restart` gives the container a new network namespace: every
/// veth end that had been moved into it is gone, and a veth pair dies with
/// either end, so the peer's end is gone too. Planning the topology
/// *without* this node's links as "current" and the real topology as
/// "desired" yields exactly the ops a fresh deploy would have run for them
/// — veths, addresses, up, routes, netem — on both ends, through the same
/// journaled executor `apply` uses. Endpoints on those links lose their
/// runtime partition (the qdisc died with the veth); a live impairment set
/// with `impair` is re-installed.
///
/// Bridge-network ports (`network { members [...] }`) are not re-attached
/// yet; callers refuse such nodes up front.
pub async fn reattach_node(running: &mut RunningLab, node: &str) -> Result<ApplyReport> {
    let mut desired = running.topology().clone();
    desired.validate().bail()?;
    let _lock = state::lock(running.name())?;
    let touches = |ep: &String| ep.split(':').next() == Some(node);
    let lost: std::collections::BTreeSet<String> = desired
        .links
        .iter()
        .filter(|l| l.endpoints.iter().any(touches))
        .flat_map(|l| l.endpoints.iter().cloned())
        .collect();
    let mut current = desired.clone();
    current.links.retain(|l| !l.endpoints.iter().any(touches));
    for ep in &lost {
        current.impairments.remove(ep);
        // What `impair` installed at runtime is what comes back.
        if let Some(live) = running.live_impairments().get(ep) {
            desired.impairments.insert(ep.clone(), live.clone());
        }
    }
    let inputs = PlanInputs::for_deploy(&desired)?;
    let cur_plan = plan::plan(&current, &inputs)?;
    let des_plan = plan::plan(&desired, &inputs)?;
    let diff = Plan::diff(&cur_plan, &des_plan);
    let report = ApplyReport {
        ops: diff.ops.len(),
        removed: diff.ops.iter().filter(|o| o.is_removal()).count(),
        applied: diff.ops.iter().map(|o| o.describe()).collect(),
    };
    tracing::info!(
        "reattach '{node}': {} link end(s), {} op(s)",
        lost.len(),
        report.ops
    );
    let mut env = apply::ApplyEnv::from_running(running, &desired)?;
    let mut journal = Journal::new(running.name());
    if let Err(e) = apply::execute(&diff, &mut env, &mut journal).await {
        tracing::warn!("reattach '{node}' failed: {e}; rolling back");
        journal.unwind().await;
        return Err(e);
    }
    journal.discard();
    running.retain_partitions(|ep| !lost.contains(ep));
    running.absorb_apply(
        env.namespace_names,
        env.containers,
        env.pids,
        env.starttimes,
        env.exec_pids,
        env.process_logs,
        env.mgmt_peers,
        env.dns_injected,
        env.wifi_loaded,
    );
    // The lab lock is held: write state.json directly, as `apply` does
    // (`save_state` would take the lock again and block on itself).
    let mut lab_state = match state::load(running.name()) {
        Ok((existing, _)) => existing,
        Err(_) => LabState::new(running.name().to_string(), now_iso8601()),
    };
    lab_state.schema_version = state::SCHEMA_VERSION;
    lab_state.namespaces = running.namespace_names().clone();
    lab_state.pids = running.pids().to_vec();
    lab_state.starttimes = running.starttimes().clone();
    lab_state.exec_pids = running.exec_pids().clone();
    lab_state.mgmt_peers = running.mgmt_peers().clone();
    lab_state.containers = running.containers().clone();
    lab_state.runtime = running.runtime_binary().map(|s| s.to_string());
    lab_state.dns_injected = running.dns_injected();
    lab_state.wifi_loaded = running.wifi_loaded();
    lab_state.process_logs = running.process_logs_map().clone();
    lab_state.saved_impairments = running.saved_impairments_map().clone();
    lab_state.live_impairments = running.live_impairments().clone();
    state::save(&lab_state, running.topology())?;
    crate::events::record(
        running.name(),
        crate::events::LifecycleKind::Applied {
            ops: report.ops,
            removed: report.removed,
        },
    );
    Ok(report)
}

/// Superseded by [`apply`]; the `TopologyDiff` argument is ignored —
/// the kernel-level diff is computed from the plans.
#[deprecated(since = "0.9.0", note = "use `nlink_lab::apply(running, desired)`")]
pub async fn apply_diff(
    running: &mut RunningLab,
    desired: &Topology,
    _diff: &crate::diff::TopologyDiff,
) -> Result<()> {
    apply(running, desired).await.map(|_| ())
}

/// WireGuard private keys persisted (0600) so `apply` keeps them stable.
#[cfg(feature = "wireguard")]
mod wireguard_secrets {
    use super::WgKeys;
    use crate::error::Result;
    use std::collections::BTreeMap;

    fn path(lab: &str) -> std::path::PathBuf {
        crate::state::state_dir(lab).join("secrets.json")
    }

    pub fn save(lab: &str, keys: &WgKeys) -> Result<()> {
        use base64::Engine;
        use std::os::unix::fs::OpenOptionsExt;
        let enc = base64::engine::general_purpose::STANDARD;
        let mut out: BTreeMap<String, BTreeMap<String, (String, String)>> = BTreeMap::new();
        for (node, ifaces) in keys {
            for (iface, (priv_k, pub_k)) in ifaces {
                out.entry(node.clone())
                    .or_default()
                    .insert(iface.clone(), (enc.encode(priv_k), enc.encode(pub_k)));
            }
        }
        let p = path(lab);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(&p);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)?;
        use std::io::Write;
        f.write_all(serde_json::to_string_pretty(&out)?.as_bytes())?;
        Ok(())
    }

    pub fn load(lab: &str) -> Option<WgKeys> {
        use base64::Engine;
        let dec = base64::engine::general_purpose::STANDARD;
        let text = std::fs::read_to_string(path(lab)).ok()?;
        let raw: BTreeMap<String, BTreeMap<String, (String, String)>> =
            serde_json::from_str(&text).ok()?;
        let mut out: WgKeys = BTreeMap::new();
        for (node, ifaces) in raw {
            for (iface, (p, q)) in ifaces {
                let (Ok(p), Ok(q)) = (dec.decode(p), dec.decode(q)) else {
                    continue;
                };
                let (Ok(p), Ok(q)) = (<[u8; 32]>::try_from(p), <[u8; 32]>::try_from(q)) else {
                    continue;
                };
                out.entry(node.clone()).or_default().insert(iface, (p, q));
            }
        }
        Some(out)
    }
}

/// Apply the unified `nlink-lab` nftables table for a node.
/// Plan 158a.
///
/// Builds an [`NftablesConfig`] covering firewall + NAT
/// chains and rules from the desired state and commits it via
/// `NftablesDiff::apply_reconcile`. Idempotent re-apply makes
/// zero kernel calls; in-place edits replace rule bodies
/// atomically without rebuilding the chain.
async fn apply_nftables_for_node(
    node_handle: &NsRef,
    node_name: &str,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
) -> Result<()> {
    use nlink::netlink::Nftables;
    use nlink::netlink::nftables::config::ReconcileOptions;

    // Phase 0: validate user-supplied literals so the
    // declarative closures can `.expect()` cleanly.
    if let Some(nat) = nat {
        validate_nat_rule_literals(nat)?;
    }

    let cfg = topology_to_nftables_config(fw, nat)?;

    let nft_conn: Connection<Nftables> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!(
            "failed to create nftables connection for '{node_name}': {e}"
        ))
    })?;

    // Nothing declared at all: an empty `NftablesConfig` declares no
    // table, so its diff has nothing to reconcile against and a table
    // left from an earlier deploy/apply would survive. Delete it
    // outright (idempotent) so `apply` after removing a node's last
    // firewall/NAT block really clears the rules.
    if fw.is_none() && nat.is_none() {
        let removed = nft_conn
            .del_table_if_exists(NLINK_LAB_TABLE, nlink::netlink::nftables::Family::Inet)
            .await
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to remove nftables table on '{node_name}': {e}"
                ))
            })?;
        if removed {
            tracing::info!(node = %node_name, "nftables: removed table {NLINK_LAB_TABLE}");
        }
        return Ok(());
    }

    let diff = cfg.diff(&nft_conn).await.map_err(|e| {
        Error::deploy_failed(format!(
            "failed to diff nftables config on '{node_name}': {e}"
        ))
    })?;

    let report = diff
        .apply_reconcile(&nft_conn, ReconcileOptions::default())
        .await
        .map_err(|e| {
            Error::deploy_failed(format!(
                "failed to apply nftables config on '{node_name}': {e}"
            ))
        })?;

    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count,
        "nftables reconcile"
    );
    Ok(())
}

/// `NetworkConfig::apply` with a small EBUSY/EAGAIN retry budget (three
/// attempts, 50 ms doubling).
///
/// nlink 0.27 added `apply_reconcile_with_options`, which fixes the first
/// half of why this exists — `apply_reconcile` used to hardcode
/// `ApplyOptions::default()`, so purge was unreachable through it. The
/// second half stands: it returns a `ReconcileReport { attempts,
/// change_count }`, while the caller inspects `ApplyResult::errors` to
/// turn a partially-applied config into a failed deploy. Swapping to it
/// would silently drop that check, so the retry stays here until the
/// report carries the per-op errors.
///
/// nlink recomputes the diff inside every `apply`, so a retry never
/// replays a stale plan.
async fn apply_network_with_retry(
    cfg: &nlink::netlink::config::NetworkConfig,
    conn: &Connection<Route>,
    purge: bool,
) -> std::result::Result<nlink::netlink::config::ApplyResult, nlink::netlink::Error> {
    let mut backoff = std::time::Duration::from_millis(50);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let res = if purge {
            let opts = nlink::netlink::config::ApplyOptions::default().with_purge(true);
            cfg.apply_with_options(conn, opts).await
        } else {
            cfg.apply(conn).await
        };
        match res {
            Err(e) if attempt < 3 && (e.is_busy() || e.is_try_again()) => {
                tracing::debug!(attempt, "NetworkConfig::apply busy, retrying: {e}");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            other => return other,
        }
    }
}

/// Apply per-pair network impairments using `PerPeerImpairer`.
///
/// For each network with impairments, group rules by source node and
/// install one HTB+netem+flower tree per source interface.
///
/// This uses `reconcile()`, which dumps the live tree, diffs it and
/// emits only the operations needed to converge — so re-applying an
/// unchanged topology makes zero kernel calls. `apply()` would be
/// wrong here: it opens with `del_qdisc(ROOT)` and rebuilds from
/// scratch, and because `Op::NetworkImpairments` is one of the ops
/// `Plan::diff` always re-runs, that happened on *every* apply, even
/// one that changed an unrelated node. Each time, the link spent a
/// moment with no impairment at all and came back with new handles and
/// zeroed counters (#135).
///
/// `fallback_to_apply` covers the first deploy, where the live root is
/// `noqueue` rather than HTB: reconcile refuses a wrong-kind root by
/// default, and there is genuinely nothing to converge incrementally.
async fn apply_network_impairments(
    topology: &Topology,
    node_handles: &BTreeMap<String, NsRef>,
) -> Result<()> {
    use nlink::netlink::impair::{PeerImpairment, PerPeerImpairer};
    use nlink::netlink::tc_recipe::ReconcileOptions;
    use nlink::util::Rate;

    let networks_with_impair: Vec<_> = topology
        .networks
        .iter()
        .filter(|(_, n)| !n.impairments.is_empty())
        .collect();

    if networks_with_impair.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "step 14b: applying per-pair network impairments ({} network(s))",
        networks_with_impair.len()
    );

    for (net_name, network) in networks_with_impair {
        // Map node name → its interface in this network (taken from
        // the first member entry that names the node).
        let mut node_ifaces: BTreeMap<String, String> = BTreeMap::new();
        // Map node name → its IP on this network (first address from
        // the auto-assigned subnet).
        let mut node_ips: BTreeMap<String, IpAddr> = BTreeMap::new();

        for member in &network.members {
            let Some(ep) = EndpointRef::parse(member) else {
                continue;
            };
            node_ifaces
                .entry(ep.node.clone())
                .or_insert_with(|| ep.iface.clone());

            if let Some(port) = network.ports.get(member)
                && let Some(addr_with_prefix) = port.addresses.first()
                && let Some((addr_str, _)) = addr_with_prefix.split_once('/')
                && let Ok(ip) = addr_str.parse::<IpAddr>()
            {
                node_ips.entry(ep.node.clone()).or_insert(ip);
            }
        }

        // Group impairments by source node.
        let mut by_source: BTreeMap<&str, Vec<&crate::types::NetworkImpairment>> = BTreeMap::new();
        for imp in &network.impairments {
            by_source.entry(&imp.src[..]).or_default().push(imp);
        }

        for (src_node, rules) in by_source {
            let Some(src_iface) = node_ifaces.get(src_node) else {
                return Err(Error::deploy_failed(format!(
                    "network '{net_name}': src node '{src_node}' has no interface in this network"
                )));
            };
            let Some(src_handle) = node_handles.get(src_node) else {
                return Err(Error::deploy_failed(format!(
                    "network '{net_name}': src node '{src_node}' has no namespace handle"
                )));
            };

            let mut impairer = PerPeerImpairer::new(src_iface.as_str());

            for rule in rules {
                let Some(dst_ip) = node_ips.get(&rule.dst) else {
                    return Err(Error::deploy_failed(format!(
                        "network '{net_name}' impair {} -- {}: cannot resolve IP for dst node \
                         '{}' (network needs a subnet, or the dst must have an explicit address)",
                        rule.src, rule.dst, rule.dst
                    )));
                };

                let netem = build_netem(&rule.impairment)?;
                let mut peer = PeerImpairment::new(netem);
                if let Some(rc) = &rule.rate_cap {
                    let bits = parse_rate_bps(rc).map_err(|e| {
                        Error::deploy_failed(format!(
                            "network '{net_name}' impair {} -- {}: bad rate-cap '{rc}': {e}",
                            rule.src, rule.dst
                        ))
                    })?;
                    peer = peer.rate_cap(Rate::bits_per_sec(bits));
                }

                impairer = impairer.impair_dst_ip(*dst_ip, peer);
            }

            let conn: Connection<Route> = src_handle.connection().map_err(|e| {
                Error::deploy_failed(format!(
                    "network '{net_name}': connection for '{src_node}': {e}"
                ))
            })?;

            let report = impairer
                .reconcile_with_options(&conn, ReconcileOptions::new().with_fallback_to_apply(true))
                .await
                .map_err(|e| {
                    Error::deploy_failed(format!(
                        "network '{net_name}': failed to apply per-pair impairment on \
                         '{src_node}:{src_iface}': {e}"
                    ))
                })?;
            tracing::debug!(
                "network '{net_name}': {src_node}:{src_iface} reconciled, {} change(s)",
                report.changes_made
            );
        }
    }

    Ok(())
}

// NOTE: the imperative `apply_nat(...)` body (Plan 152 era)
// is deleted by Plan 158a. All NAT rule application now goes
// through `apply_nftables_for_node` which builds a single
// `NftablesConfig` covering firewall + NAT and commits it
// via `NftablesDiff::apply_reconcile`.

/// Apply the declarative [`NetworkConfig`] for one node.
/// Plan 158e Slice 1.
///
/// Wraps `cfg.diff(&conn).await?.apply(&conn, …).await?`.
/// Idempotent re-apply makes zero kernel calls for the
/// address + route layer.
async fn apply_network_config_for_node(
    node_handle: &NsRef,
    node_name: &str,
    cfg: nlink::netlink::config::NetworkConfig,
    purge: bool,
) -> Result<()> {
    // Skip the round-trip when the declared config is empty (no
    // addresses, no routes, no links, no qdiscs) — unless purging,
    // where "nothing declared" means "remove what is there".
    if !purge
        && cfg.links().is_empty()
        && cfg.addresses().is_empty()
        && cfg.routes().is_empty()
        && cfg.qdiscs().is_empty()
    {
        return Ok(());
    }

    let conn: Connection<Route> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!("NetworkConfig connection on '{node_name}': {e}"))
    })?;

    // `NetworkConfig::apply` computes the diff and applies it.
    // Idempotent — re-apply on an unchanged topology completes
    // with `changes_made == 0`.
    // apply mode: undeclared global addresses and main-table static
    // routes on managed interfaces are removed (nlink's conservative
    // purge — links and qdiscs are never touched). Retries on
    // EBUSY/EAGAIN like the nftables and WireGuard layers (#75).
    let result = apply_network_with_retry(&cfg, &conn, purge)
        .await
        .map_err(|e| Error::deploy_failed(format!("NetworkConfig::apply on '{node_name}': {e}")))?;

    tracing::info!(
        node = %node_name,
        changes = result.changes_made,
        errors = result.errors.len(),
        "NetworkConfig reconcile (addresses + routes)"
    );

    if !result.errors.is_empty() {
        let first = &result.errors[0];
        return Err(Error::deploy_failed(format!(
            "NetworkConfig::apply on '{node_name}' completed with {} error(s); \
             first: {}: {}",
            result.errors.len(),
            first.operation,
            first.error
        )));
    }
    Ok(())
}

/// Plan 159c — per-node Stack-pattern orchestrator.
///
/// Bundles the three declarative reconcile calls
/// (`apply_network_config_for_node`, `apply_nftables_for_node`,
/// `apply_wireguard_for_node`) into a single per-node call site
/// with one aggregated `tracing::info!` for the whole stack.
/// Close to upstream `facade::Stack::apply_in_with`, still not adopted —
/// but for different reasons than before nlink 0.27, which fixed three of
/// the four:
///
/// * ordering: 0.27 creates (and raises) declared WireGuard links before
///   the network layer, which is what our `ensure_devices` call does here;
/// * options: `apply_in_with(ns, ApplyOptions)` can purge now;
/// * containers: it takes a `NamespaceSpec`, which has `Pid`, so it is no
///   longer name-only.
///
/// What still keeps us on our own path: `apply_in_with` runs an
/// unconditional pre-flight `diff_in_with` across **all three layers**
/// before applying, with no way to opt out — a second full dump per node
/// per apply, which a 200-node lab pays for — and `facade::apply::
/// network_in_with` calls `apply_with_options` directly, so it has no
/// EBUSY/EAGAIN retry (see `apply_network_with_retry`). Routing through
/// `NsRef::connection<P>()` also keeps the container
/// (`connection_for_pid`) and bare-namespace cases on one code path.
#[cfg(feature = "wireguard")]
async fn apply_stack_for_node(
    node_handle: &NsRef,
    node_name: &str,
    network: nlink::netlink::config::NetworkConfig,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
    wireguard: Option<nlink::netlink::genl::wireguard::WireguardConfig>,
    purge: bool,
) -> Result<()> {
    // Plan 160 — bootstrap WG links *before* the NetworkConfig apply
    // so their tunnel addresses (declared on the network layer) land
    // on an existing interface. `ensure_devices` is idempotent, so
    // the live-reconcile path re-runs it harmlessly.
    if let Some(cfg) = &wireguard {
        ensure_wireguard_devices_for_node(node_handle, node_name, cfg).await?;
    }
    apply_network_config_for_node(node_handle, node_name, network, purge).await?;
    apply_nftables_for_node(node_handle, node_name, fw, nat).await?;
    if let Some(cfg) = wireguard {
        apply_wireguard_for_node(node_handle, node_name, cfg).await?;
    }
    tracing::info!(node = %node_name, "stack reconcile complete");
    Ok(())
}

/// Plan 159c — WG-less variant for builds without
/// `--features wireguard`.
#[cfg(not(feature = "wireguard"))]
async fn apply_stack_for_node(
    node_handle: &NsRef,
    node_name: &str,
    network: nlink::netlink::config::NetworkConfig,
    fw: Option<&crate::types::FirewallConfig>,
    nat: Option<&crate::types::NatConfig>,
    _wireguard: Option<()>,
    purge: bool,
) -> Result<()> {
    apply_network_config_for_node(node_handle, node_name, network, purge).await?;
    apply_nftables_for_node(node_handle, node_name, fw, nat).await?;
    tracing::info!(node = %node_name, "stack reconcile complete");
    Ok(())
}

/// Bootstrap a node's declared WireGuard links (Plan 160, nlink
/// 0.24 #169). `WireguardConfig::ensure_devices` creates any
/// declared-but-absent WG interface idempotently (swallowing
/// already-exists) through a same-namespace Route connection —
/// covering both the bare-namespace and container
/// (`connection_for_pid`) cases via `NsRef`, which the
/// name-only `facade::apply::wireguard*` helpers would not. Runs
/// before the `NetworkConfig` apply so the WG interfaces exist — and,
/// since nlink 0.27, are already **up** — when their tunnel addresses
/// are assigned; this retired the imperative step-6c pre-create loop.
#[cfg(feature = "wireguard")]
async fn ensure_wireguard_devices_for_node(
    node_handle: &NsRef,
    node_name: &str,
    cfg: &nlink::netlink::genl::wireguard::WireguardConfig,
) -> Result<()> {
    let route_conn: Connection<Route> = node_handle.connection().map_err(|e| {
        Error::deploy_failed(format!(
            "failed to open Route connection for WireGuard bootstrap on '{node_name}': {e}"
        ))
    })?;
    // Since nlink 0.27 `ensure_devices` raises every declared device on
    // every call, not just the ones it created — `add_link` makes a link
    // administratively down and the kernel refuses a nexthop on a down
    // device, so the loop we used to run here is now redundant (nlink
    // #330; the 0.26→0.27 migration guide names it).
    cfg.ensure_devices(&route_conn).await.map_err(|e| {
        Error::deploy_failed(format!(
            "WireguardConfig::ensure_devices on '{node_name}': {e}"
        ))
    })?;
    Ok(())
}

/// Apply a node's `WireguardConfig` via `apply_reconcile`.
/// Plan 159a Phase 2 — mirrors `apply_network_config_for_node` /
/// `apply_nftables_for_node` shape for the WG GENL layer.
/// The WG link is bootstrapped earlier (via
/// `ensure_wireguard_devices_for_node`) so `NetworkConfig` can
/// assign its addresses; this fn only configures the GENL device.
#[cfg(feature = "wireguard")]
async fn apply_wireguard_for_node(
    node_handle: &NsRef,
    node_name: &str,
    cfg: nlink::netlink::genl::wireguard::WireguardConfig,
) -> Result<()> {
    use nlink::netlink::nftables::config::ReconcileOptions;

    let wg_conn = node_handle
        .connection_async::<Wireguard>()
        .await
        .map_err(|e| {
            Error::deploy_failed(format!(
                "failed to create WireGuard connection for '{node_name}': {e}"
            ))
        })?;

    let report = cfg
        .apply_reconcile(&wg_conn, ReconcileOptions::default())
        .await
        .map_err(|e| {
            Error::deploy_failed(format!(
                "WireguardConfig::apply_reconcile on '{node_name}': {e}"
            ))
        })?;

    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count,
        "WireguardConfig reconcile complete",
    );
    Ok(())
}

/// Compute the full layered diff between a running lab's live
/// state and a desired topology. Plan 158f Phase 2.
///
/// Aggregates three views:
/// - Lab-graph differences (nodes/links/impair/sysctls/etc.) via
///   [`crate::diff::diff_topologies`].
/// - Per-namespace RTNETLINK diff (links/addresses/routes/qdiscs)
///   by building the same [`nlink::netlink::config::NetworkConfig`]
///   step 11c of the deploy uses and calling its `diff()` against
///   a per-node `Connection<Route>`.
/// - Per-namespace nftables diff by building the same
///   [`nlink::netlink::nftables::config::NftablesConfig`] step 13
///   uses and calling its `diff()` against a per-node
///   `Connection<Nftables>`.
///
/// Used by `nlink-lab apply --check` and `apply --dry-run` so
/// CI and operators see the full set of kernel changes that
/// `apply` would commit — not just the lab-graph subset that
/// `TopologyDiff` covers.
///
/// Re-computing the upstream subdiffs on every call costs one
/// dump round-trip per node per protocol family. For a 50-node
/// lab that's 100 dumps; in practice ms-scale on a quiet host.
pub async fn compute_layered_diff(
    running: &RunningLab,
    desired: &Topology,
) -> Result<crate::diff::LayeredDiff> {
    use nlink::netlink::Nftables;

    let topology = crate::diff::diff_topologies(running.topology(), desired);

    let auto_routes = auto_routes_for(desired);

    let mut network = BTreeMap::new();
    let mut nftables = BTreeMap::new();

    for (node_name, node) in &desired.nodes {
        // The handle lookup uses the running-lab state. A node
        // listed in `desired` but not in `running` (i.e. a
        // newly-added node) is captured by the lab-graph diff
        // (`topology.nodes_added`) and gets full creation work
        // during apply. We skip it here to avoid a spurious error.
        let handle = match node_handle_for(running, node_name) {
            Ok(h) => h,
            Err(_) => continue,
        };

        // RTNETLINK side.
        let cfg = topology_to_network_config(node_name, node, desired, auto_routes.get(node_name))?;
        let cfg = plan::network::with_vrf_routes(cfg, node_name, node)?;
        let cfg_is_empty = cfg.links().is_empty()
            && cfg.addresses().is_empty()
            && cfg.routes().is_empty()
            && cfg.qdiscs().is_empty();
        if !cfg_is_empty {
            let conn: Connection<Route> = handle.connection().map_err(|e| {
                Error::deploy_failed(format!("NetworkConfig connection on '{node_name}': {e}"))
            })?;
            // Purge-aware: undeclared addresses / main-table static
            // routes on managed interfaces are drift too — `apply`
            // removes them, so `apply --check` / `verify` must report
            // them (found by `verify` missing a hand-added address).
            let opts = nlink::netlink::config::DiffOptions::default().purge(true);
            let diff = cfg.diff_with_options(&conn, opts).await.map_err(|e| {
                Error::deploy_failed(format!("NetworkConfig::diff on '{node_name}': {e}"))
            })?;
            network.insert(node_name.clone(), diff);
        }

        // nftables side.
        let fw = desired.effective_firewall(node);
        let nat = node.nat.as_ref();
        if fw.is_some() || nat.is_some() {
            let cfg = topology_to_nftables_config(fw, nat)?;
            let nft_conn: Connection<Nftables> = handle.connection().map_err(|e| {
                Error::deploy_failed(format!("Nftables connection on '{node_name}': {e}"))
            })?;
            let diff = cfg.diff(&nft_conn).await.map_err(|e| {
                Error::deploy_failed(format!("NftablesConfig::diff on '{node_name}': {e}"))
            })?;
            nftables.insert(node_name.clone(), diff);
        }
    }

    Ok(crate::diff::LayeredDiff {
        topology,
        network,
        nftables,
    })
}

/// Run post-deploy `validate { … }` assertions (step 19).
///
/// Thin wrapper over [`crate::test_runner::run_assertions`] — the one
/// assertion engine shared with `nlink-lab test` and the scenario
/// engine — that additionally emits the `PASS:` / `FAIL:` log lines
/// operators grep for. Target addresses come from
/// [`crate::ipmap::build_ip_map`], so bridge-`network` topologies no
/// longer log `SKIP: no IP found` for every assertion (issue #34).
///
/// Never bails: `deploy()` succeeds regardless of the outcome. The
/// results are stored on the returned [`RunningLab`] (see
/// [`RunningLab::assertion_results`] / [`RunningLab::assertions_failed`])
/// so the CLI can implement `deploy --strict` without changing this
/// function's signature.
fn run_assertions(
    running: &RunningLab,
    topology: &Topology,
) -> Vec<crate::test_runner::AssertionResult> {
    let results = crate::test_runner::run_assertions(running, topology);
    for r in &results {
        match (r.passed, r.detail.as_deref()) {
            (true, Some(detail)) => tracing::info!("PASS: {} ({detail})", r.description),
            (true, None) => tracing::info!("PASS: {}", r.description),
            (false, Some(detail)) => tracing::warn!("FAIL: {}: {detail}", r.description),
            (false, None) => tracing::warn!("FAIL: {}", r.description),
        }
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        tracing::warn!("{failed} of {} validate assertion(s) failed", results.len());
    }
    crate::events::record(
        running.name(),
        crate::events::LifecycleKind::AssertionsRun {
            passed: results.len() - failed,
            failed,
        },
    );
    results
}

fn node_handle_for(running: &RunningLab, node_name: &str) -> Result<NsRef> {
    if let Some(ns_name) = running.namespace_names().get(node_name) {
        return Ok(NsRef::Named {
            name: ns_name.clone(),
        });
    }
    if let Some(container) = running.containers().get(node_name) {
        return Ok(NsRef::Container {
            id: container.id.clone(),
            pid: container.pid,
        });
    }
    Err(Error::NodeNotFound {
        name: node_name.to_string(),
    })
}

/// Build a NetemConfig from an Impairment.
/// Pre-create guard for a bare-namespace node (Plan 160, nlink 0.25).
///
/// Rejects only a namespace that is *live* (`is_namespace` — an nsfs
/// bind-mount), so a genuine collision still errors. A leftover marker
/// file with no live mount is a stale artifact of an unclean shutdown
/// (`exists` true but `is_namespace` false); we clear it so the deploy
/// self-heals instead of hard-failing "already exists" and forcing a
/// manual `destroy --orphans`. `create` refuses any existing marker, so
/// the clear is required before it can proceed. Under the deploy flock
/// this detect-and-clear is race-free.
fn guard_namespace_absent(ns_name: &str) -> Result<()> {
    if namespace::is_namespace(ns_name) {
        return Err(Error::AlreadyExists {
            name: format!("namespace '{ns_name}' already exists"),
        });
    }
    if namespace::exists(ns_name) {
        tracing::warn!(
            "clearing stale namespace marker '{ns_name}' (no live mount — likely an unclean prior shutdown)"
        );
        namespace::delete(ns_name).map_err(|e| Error::Namespace {
            op: "clear-stale",
            ns: ns_name.to_string(),
            source: e,
        })?;
    }
    Ok(())
}

/// Convert WiFi channel number to frequency in MHz (as string for iw).
fn freq_from_channel(channel: u32) -> String {
    let freq = match channel {
        1 => 2412,
        2 => 2417,
        3 => 2422,
        4 => 2427,
        5 => 2432,
        6 => 2437,
        7 => 2442,
        8 => 2447,
        9 => 2452,
        10 => 2457,
        11 => 2462,
        12 => 2467,
        13 => 2472,
        14 => 2484,
        // 5 GHz channels
        36 => 5180,
        40 => 5200,
        44 => 5220,
        48 => 5240,
        _ => 2412, // default to channel 1
    };
    freq.to_string()
}

pub(crate) fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_route_stub_node() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node router : router
node host
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host is a stub node → default route via router
        assert!(routes.contains_key("host"), "host should get auto-route");
        assert!(routes["host"].contains_key("default"));
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn test_auto_route_no_override() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node router : router
node host { route default via 10.0.0.99 }
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host already has a manual default route — auto shouldn't override
        let host_routes = routes.get("host");
        assert!(
            host_routes.is_none() || !host_routes.unwrap().contains_key("default"),
            "auto-route should not override manual default"
        );
    }

    #[test]
    fn test_auto_route_multi_hop() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node r1 : router
node r2 : router
node host
link r1:eth0 -- r2:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
link r2:eth1 -- host:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // host → default via r2
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.2.1"));
        // r1 has single neighbor (r2) → gets default route via r2
        assert_eq!(
            routes["r1"]["default"].via.as_deref(),
            Some("10.0.1.2"),
            "r1 should default via r2"
        );
    }

    fn running_with_live(topo: &Topology, live: &[(&str, Option<&str>)]) -> crate::RunningLab {
        let mut running = crate::RunningLab::new(
            topo.clone(),
            Default::default(),
            Default::default(),
            None,
            Vec::new(),
            false,
            false,
        );
        let mut map = BTreeMap::new();
        for (ep, delay) in live {
            map.insert(
                ep.to_string(),
                Impairment {
                    delay: delay.map(str::to_string),
                    ..Default::default()
                },
            );
        }
        running.set_live_impairments(map);
        running
    }

    #[test]
    fn overlay_keeps_live_impairment_when_topology_unchanged() {
        let topo = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }\n",
        )
        .unwrap();
        let running =
            running_with_live(&topo, &[("a:eth0", Some("50ms")), ("b:eth0", Some("7ms"))]);
        let (cur, des, kept) = overlay_live_impairments(&running, &topo, &ApplyOptions::default());
        // both sides carry the live values → no tc change in the plan
        assert_eq!(cur.impairments["a:eth0"].delay.as_deref(), Some("50ms"));
        assert_eq!(des.impairments["a:eth0"].delay.as_deref(), Some("50ms"));
        assert_eq!(des.impairments["b:eth0"].delay.as_deref(), Some("7ms"));
        assert_eq!(kept.len(), 2);
        let plan = plan_apply(&running, &topo, &ApplyOptions::default())
            .unwrap()
            .1;
        assert!(
            plan.ops
                .iter()
                .all(|o| !matches!(o, Op::Netem { .. } | Op::ClearQdisc { .. }))
        );
    }

    #[test]
    fn overlay_drops_live_impairment_when_topology_changes_the_endpoint() {
        let topo = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }\n",
        )
        .unwrap();
        let desired = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 2ms }\n",
        )
        .unwrap();
        let running = running_with_live(&topo, &[("a:eth0", Some("50ms"))]);
        let (_, des, kept) = overlay_live_impairments(&running, &desired, &ApplyOptions::default());
        assert_eq!(des.impairments["a:eth0"].delay.as_deref(), Some("2ms"));
        assert!(kept.is_empty());
        let plan = plan_apply(&running, &desired, &ApplyOptions::default())
            .unwrap()
            .1;
        assert!(plan.ops.iter().any(|o| matches!(o, Op::Netem { .. })));
    }

    #[test]
    fn overlay_reset_and_cleared_live_impairment() {
        let topo = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }\n",
        )
        .unwrap();
        // `impair --clear` on a declared endpoint records an empty value:
        // the overlay removes the declaration on both sides.
        let running = running_with_live(&topo, &[("a:eth0", None)]);
        let (cur, des, kept) = overlay_live_impairments(&running, &topo, &ApplyOptions::default());
        assert!(!cur.impairments.contains_key("a:eth0"));
        assert!(!des.impairments.contains_key("a:eth0"));
        assert_eq!(kept.len(), 1);
        // --reset-impairments: the topology wins again → a Netem op.
        let reset = ApplyOptions {
            reset_impairments: true,
        };
        let (cur, des, kept) = overlay_live_impairments(&running, &topo, &reset);
        assert!(
            !cur.impairments.contains_key("a:eth0"),
            "current reflects the kernel"
        );
        assert_eq!(des.impairments["a:eth0"].delay.as_deref(), Some("1ms"));
        assert!(kept.is_empty());
        let plan = plan_apply(&running, &topo, &reset).unwrap().1;
        assert!(
            plan.ops.iter().any(|o| matches!(o, Op::Netem { .. })),
            "reset must re-install the declaration"
        );
        // A live value different from the declaration is replaced too.
        let running = running_with_live(&topo, &[("a:eth0", Some("50ms"))]);
        let plan = plan_apply(&running, &topo, &reset).unwrap().1;
        assert!(
            plan.ops.iter().any(|o| matches!(o, Op::Netem { impairment, .. } if impairment.delay.as_deref() == Some("1ms")))
        );
    }

    #[test]
    fn overlay_partitions_survive_unless_reset_or_changed() {
        let topo = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }\n",
        )
        .unwrap();
        let mut running = running_with_live(&topo, &[]);
        running.set_partitions(BTreeMap::from([(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("1ms".into()),
                ..Default::default()
            },
        )]));
        // unchanged topology: the partition stays (no Tc op)
        let plan = plan_apply(&running, &topo, &ApplyOptions::default())
            .unwrap()
            .1;
        assert!(
            plan.ops
                .iter()
                .all(|o| !matches!(o, Op::Netem { .. } | Op::ClearQdisc { .. }))
        );
        // reset: the declaration replaces the 100% loss
        let reset = ApplyOptions {
            reset_impairments: true,
        };
        let plan = plan_apply(&running, &topo, &reset).unwrap().1;
        assert!(plan.ops.iter().any(
            |o| matches!(o, Op::Netem { impairment, .. } if impairment.delay.as_deref() == Some("1ms"))
        ));
        // topology change on that endpoint: the new declaration replaces it
        let desired = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 2ms }\n",
        )
        .unwrap();
        let plan = plan_apply(&running, &desired, &ApplyOptions::default())
            .unwrap()
            .1;
        assert!(plan.ops.iter().any(
            |o| matches!(o, Op::Netem { impairment, .. } if impairment.delay.as_deref() == Some("2ms"))
        ));
    }

    #[test]
    fn test_auto_route_shared_segment_prefers_forwarding_neighbour() {
        // On a bridge the first neighbour in name order is `other`, a
        // plain host; the default must still point at the router.
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4  forward ipv6 }
node router : router
node server
node other
node client
network lan {
  members [router:eth0, server:eth0, other:eth0]
  port router:eth0 { 10.0.1.1/24 fd00:1::1/64 }
  port server:eth0 { 10.0.1.2/24 fd00:1::2/64 }
  port other:eth0 { 10.0.1.3/24 fd00:1::3/64 }
}
link router:eth1 -- client:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        assert_eq!(routes["server"]["default"].via.as_deref(), Some("10.0.1.1"));
        assert_eq!(routes["server"]["::/0"].via.as_deref(), Some("fd00:1::1"));
        assert_eq!(routes["other"]["default"].via.as_deref(), Some("10.0.1.1"));
        assert_eq!(routes["client"]["default"].via.as_deref(), Some("10.0.2.1"));
    }

    #[test]
    fn test_auto_route_dual_stack_stub_gets_both_defaults() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4  forward ipv6 }
node router : router
node host
network lan {
  members [router:eth0, host:eth0]
  port router:eth0 { 10.0.0.1/24 fd00:1::1/64 }
  port host:eth0 { 10.0.0.2/24 fd00:1::2/64 }
}
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.0.1"));
        assert_eq!(routes["host"]["::/0"].via.as_deref(), Some("fd00:1::1"));
    }

    #[test]
    fn test_auto_route_v6_only_uses_ipv6_forwarding_sysctl() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv6 }
node r1 : router
node r2 : router
node host
link r1:eth0 -- r2:eth0 { fd00:12::1/64 -- fd00:12::2/64 }
link r2:eth1 -- host:eth0 { fd00:2::1/64 -- fd00:2::2/64 }
node far
link r1:eth1 -- far:eth0 { fd00:1::1/64 -- fd00:1::2/64 }
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        assert_eq!(routes["host"]["::/0"].via.as_deref(), Some("fd00:2::1"));
        assert!(!routes["host"].contains_key("default"));
        // r2 forwards IPv6 and has two neighbours → prefix routes
        assert_eq!(
            routes["r2"]["fd00:1::/64"].via.as_deref(),
            Some("fd00:12::1")
        );
        // r1 likewise learns host's subnet through r2
        assert_eq!(
            routes["r1"]["fd00:2::/64"].via.as_deref(),
            Some("fd00:12::2")
        );
    }

    #[test]
    fn test_auto_route_transit_prefix_routes_per_family() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4  forward ipv6 }
node r1 : router
node r2 : router
node h1
node h2
network a {
  members [r1:eth0, h1:eth0]
  port r1:eth0 { 10.0.1.1/24 fd00:1::1/64 }
  port h1:eth0 { 10.0.1.2/24 fd00:1::2/64 }
}
network core {
  members [r1:eth1, r2:eth0]
  port r1:eth1 { 10.0.9.1/24 fd00:9::1/64 }
  port r2:eth0 { 10.0.9.2/24 fd00:9::2/64 }
}
network b {
  members [r2:eth1, h2:eth0]
  port r2:eth1 { 10.0.2.1/24 fd00:2::1/64 }
  port h2:eth0 { 10.0.2.2/24 fd00:2::2/64 }
}
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        assert_eq!(routes["r1"]["10.0.2.0/24"].via.as_deref(), Some("10.0.9.2"));
        assert_eq!(
            routes["r1"]["fd00:2::/64"].via.as_deref(),
            Some("fd00:9::2")
        );
        assert_eq!(routes["r2"]["10.0.1.0/24"].via.as_deref(), Some("10.0.9.1"));
        assert_eq!(
            routes["r2"]["fd00:1::/64"].via.as_deref(),
            Some("fd00:9::1")
        );
        // never a cross-family next hop
        for node_routes in routes.values() {
            for (dest, cfg) in node_routes {
                let dest_v6 = dest.contains(':');
                let via_v6 = cfg.via.as_deref().unwrap().contains(':');
                assert_eq!(dest_v6, via_v6, "{dest} via {:?}", cfg.via);
            }
        }
    }

    #[test]
    fn test_auto_route_manual_v6_default_not_overridden() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4  forward ipv6 }
node router : router
node host { route default via fd00:1::99 }
network lan {
  members [router:eth0, host:eth0]
  port router:eth0 { 10.0.0.1/24 fd00:1::1/64 }
  port host:eth0 { 10.0.0.2/24 fd00:1::2/64 }
}
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        // `default via <v6>` counts as the IPv6 default; the v4 one is
        // still filled in.
        assert!(!routes["host"].contains_key("::/0"));
        assert_eq!(routes["host"]["default"].via.as_deref(), Some("10.0.0.1"));
    }

    /// Step 19 wrapper: returns the structured vector (not `()`), and a
    /// rootless / undeployed lab yields non-pass results with details
    /// rather than a silent pass. `parse_ping_avg` tests moved to
    /// `test_runner` alongside the single remaining implementation.
    #[test]
    fn test_run_assertions_returns_structured_results() {
        let topology = crate::parser::parse(
            r#"
lab "t"
node a
node b
network lan {
  members [a:eth0, b:eth0]
  subnet 10.0.1.0/24
}
validate {
  reach a b
  route-has a 10.0.1.0/24
}
"#,
        )
        .unwrap();
        let namespace_names = topology
            .nodes
            .keys()
            .map(|n| (n.clone(), format!("nlink-lab-test-nonexistent-{n}")))
            .collect();
        let mut running = RunningLab::new(
            topology.clone(),
            namespace_names,
            Default::default(),
            None,
            Vec::new(),
            false,
            false,
        );
        assert!(running.assertion_results().is_empty());
        assert!(!running.assertions_failed());

        let results = run_assertions(&running, &topology);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.passed));
        assert!(results.iter().all(|r| r.detail.is_some()));
        // Bridge-network address resolved (issue #34): the failure is
        // the missing namespace, not a missing IP.
        assert!(
            !results[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("no IP found"),
            "{:?}",
            results[0].detail
        );

        running.set_assertion_results(results);
        assert_eq!(running.assertion_results().len(), 2);
        assert!(running.assertions_failed());
    }

    #[test]
    fn test_apply_match_expr_tcp_dport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "tcp dport 80");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_udp_dport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "udp dport 53");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ct_state() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ct state established,related");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_tcp_sport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "tcp sport 8080");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_udp_sport() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "udp sport 5353");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_icmp_type() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "icmp type 8");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_icmpv6_type() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "icmpv6 type 128");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_mark() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "mark 42");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_saddr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr 10.0.1.0/24");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_daddr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip daddr 192.168.0.1/32");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_compound_saddr_tcp() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr 10.0.1.0/24 tcp dport 22");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_compound_daddr_udp() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip daddr 10.0.2.0/24 udp dport 53");
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_match_expr_ip_saddr_bad_cidr() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "ip saddr not-a-cidr");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("CIDR"));
    }

    #[test]
    fn test_apply_match_expr_unknown_errors() {
        let rule = nlink::netlink::nftables::types::Rule::new("test", "input")
            .family(nlink::netlink::nftables::types::Family::Inet);
        let result = apply_match_expr(rule, "unknown expression");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    // ── Plan 158a: topology_to_nftables_config tests ────────────────

    #[test]
    fn nftables_config_empty_inputs_produce_empty_config() {
        let cfg = topology_to_nftables_config(None, None).unwrap();
        assert!(
            cfg.tables().is_empty(),
            "no fw + no nat must produce zero tables"
        );
    }

    #[test]
    fn nftables_config_firewall_only_has_filter_chains() {
        let fw = crate::types::FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![
                crate::types::FirewallRule {
                    match_expr: Some("tcp dport 80".into()),
                    action: Some("accept".into()),
                },
                crate::types::FirewallRule {
                    match_expr: Some("tcp dport 22".into()),
                    action: Some("accept".into()),
                },
            ],
        };
        let cfg = topology_to_nftables_config(Some(&fw), None).unwrap();
        assert_eq!(cfg.tables().len(), 1, "exactly one table");
        let table = cfg.tables().first().unwrap();
        let chain_names: Vec<&str> = table.chains().iter().map(|c| c.name()).collect();
        assert!(
            chain_names.contains(&"input") && chain_names.contains(&"forward"),
            "expected input + forward chains, got {chain_names:?}"
        );
        assert!(
            !chain_names.contains(&"prerouting"),
            "NAT chains must not be present without NAT config"
        );
    }

    #[test]
    fn nftables_config_nat_only_has_nat_chains() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Masquerade,
                src: Some("10.0.0.0/24".into()),
                dst: None,
                target: None,
                target_port: None,
            }],
        };
        let cfg = topology_to_nftables_config(None, Some(&nat)).unwrap();
        assert_eq!(cfg.tables().len(), 1);
        let chain_names: Vec<&str> = cfg
            .tables()
            .first()
            .unwrap()
            .chains()
            .iter()
            .map(|c| c.name())
            .collect();
        assert!(
            chain_names.contains(&"prerouting") && chain_names.contains(&"postrouting"),
            "expected prerouting + postrouting, got {chain_names:?}"
        );
        assert!(
            !chain_names.contains(&"input"),
            "filter chains must not be present without firewall config"
        );
    }

    #[test]
    fn nftables_config_fw_and_nat_share_one_table() {
        let fw = crate::types::FirewallConfig {
            policy: Some("accept".into()),
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("tcp dport 22".into()),
                action: Some("accept".into()),
            }],
        };
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Dnat,
                src: None,
                dst: Some("203.0.113.1/32".into()),
                target: Some("10.0.0.10".into()),
                target_port: Some(8080),
            }],
        };
        let cfg = topology_to_nftables_config(Some(&fw), Some(&nat)).unwrap();
        assert_eq!(
            cfg.tables().len(),
            1,
            "both fw and nat must collapse into one nlink-lab table"
        );
        let table = cfg.tables().first().unwrap();
        let chain_names: Vec<&str> = table.chains().iter().map(|c| c.name()).collect();
        for expected in ["input", "forward", "prerouting", "postrouting"] {
            assert!(
                chain_names.contains(&expected),
                "expected chain '{expected}' in unified config, got {chain_names:?}"
            );
        }
    }

    #[test]
    fn nftables_config_invalid_match_expr_surfaces_early() {
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip saddr 999.999.999.999/24".into()),
                action: Some("accept".into()),
            }],
        };
        let err = topology_to_nftables_config(Some(&fw), None).unwrap_err();
        assert!(
            err.to_string().contains("invalid IPv4 CIDR"),
            "want validation error from up-front match_expr check, got: {err}"
        );
    }

    #[test]
    fn nftables_config_ip6_saddr_lowers() {
        let fw = crate::types::FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![
                crate::types::FirewallRule {
                    match_expr: Some("ip6 saddr fd00::/64 tcp dport 22".into()),
                    action: Some("accept".into()),
                },
                crate::types::FirewallRule {
                    match_expr: Some("ip6 daddr fd00::1".into()),
                    action: Some("accept".into()),
                },
                crate::types::FirewallRule {
                    match_expr: Some("ip saddr 10.0.0.1".into()),
                    action: Some("accept".into()),
                },
            ],
        };
        let cfg = topology_to_nftables_config(Some(&fw), None).unwrap();
        let input_rules = cfg.tables()[0]
            .rules()
            .iter()
            .filter(|r| r.chain() == "input")
            .count();
        assert_eq!(input_rules, 3);
    }

    #[test]
    fn nftables_config_ip_keyword_with_v6_address_errors() {
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip saddr fd00::/64".into()),
                action: Some("accept".into()),
            }],
        };
        let err = topology_to_nftables_config(Some(&fw), None).unwrap_err();
        assert!(err.to_string().contains("use 'ip6 saddr'"), "{err}");
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip6 daddr 10.0.0.0/8".into()),
                action: Some("accept".into()),
            }],
        };
        let err = topology_to_nftables_config(Some(&fw), None).unwrap_err();
        assert!(err.to_string().contains("use 'ip daddr'"), "{err}");
    }

    #[test]
    fn nftables_config_nat66_masquerade_snat_dnat() {
        let nat = crate::types::NatConfig {
            rules: vec![
                crate::types::NatRule {
                    action: crate::types::NatAction::Masquerade,
                    src: Some("fd00:2::/64".into()),
                    dst: None,
                    target: None,
                    target_port: None,
                },
                crate::types::NatRule {
                    action: crate::types::NatAction::Snat,
                    src: Some("fd00:2::/64".into()),
                    dst: None,
                    target: Some("fd00:1::1".into()),
                    target_port: None,
                },
                crate::types::NatRule {
                    action: crate::types::NatAction::Dnat,
                    src: None,
                    dst: Some("2001:db8::1/128".into()),
                    target: Some("fd00:1::2".into()),
                    target_port: Some(8080),
                },
            ],
        };
        validate_nat_rule_literals(&nat).unwrap();
        let cfg = topology_to_nftables_config(None, Some(&nat)).unwrap();
        let table = &cfg.tables()[0];
        let count = |chain: &str| table.rules().iter().filter(|r| r.chain() == chain).count();
        assert_eq!(count("postrouting"), 2);
        assert_eq!(count("prerouting"), 1);
    }

    #[test]
    fn nftables_config_nat_family_mismatch_surfaces_via_validate() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Snat,
                src: Some("fd00:2::/64".into()),
                dst: None,
                target: Some("10.0.0.1".into()),
                target_port: None,
            }],
        };
        let err = validate_nat_rule_literals(&nat).unwrap_err();
        assert!(
            err.to_string().contains("different address families"),
            "{err}"
        );
    }

    #[test]
    fn nftables_config_bare_ip_in_match_is_host_prefix() {
        // Used to pass `validate` (bare IP accepted) and fail at deploy
        // (planner demanded a `/`).
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Masquerade,
                src: Some("10.0.0.7".into()),
                dst: None,
                target: None,
                target_port: None,
            }],
        };
        validate_nat_rule_literals(&nat).unwrap();
        topology_to_nftables_config(None, Some(&nat)).unwrap();
    }

    #[test]
    fn nftables_config_invalid_nat_cidr_surfaces_via_validate() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Masquerade,
                src: Some("not-a-cidr".into()),
                dst: None,
                target: None,
                target_port: None,
            }],
        };
        let err = validate_nat_rule_literals(&nat).unwrap_err();
        assert!(
            err.to_string().contains("invalid src CIDR"),
            "want CIDR error from validate, got: {err}"
        );
    }

    #[test]
    fn nftables_config_invalid_nat_target_surfaces_via_validate() {
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Snat,
                src: Some("10.0.0.0/24".into()),
                dst: None,
                target: Some("not-an-ip".into()),
                target_port: None,
            }],
        };
        let err = validate_nat_rule_literals(&nat).unwrap_err();
        assert!(
            err.to_string().contains("invalid NAT target"),
            "want target error from validate, got: {err}"
        );
    }

    // ── Plan 158e Slice 1: topology_to_network_config tests ─────────

    #[test]
    fn network_config_empty_node_produces_empty_config() {
        let topo = crate::parser::parse(
            r#"lab "t"
node alone
"#,
        )
        .unwrap();
        let cfg = topology_to_network_config("alone", &topo.nodes["alone"], &topo, None).unwrap();
        assert!(cfg.addresses().is_empty(), "no addresses expected");
        assert!(cfg.routes().is_empty(), "no routes expected");
    }

    #[test]
    fn network_config_link_addresses_appear_per_node() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let cfg_a = topology_to_network_config("a", &topo.nodes["a"], &topo, None).unwrap();
        let cfg_b = topology_to_network_config("b", &topo.nodes["b"], &topo, None).unwrap();
        assert_eq!(cfg_a.addresses().len(), 1, "a gets one address");
        assert_eq!(cfg_b.addresses().len(), 1, "b gets one address");
    }

    #[test]
    fn network_config_default_route_translates_to_zero_cidr() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host { route default via 10.0.0.1 }
node router
link router:eth0 -- host:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let cfg = topology_to_network_config("host", &topo.nodes["host"], &topo, None).unwrap();
        let routes = cfg.routes();
        assert_eq!(routes.len(), 1, "exactly one route");
        let dst = routes[0].destination();
        let prefix = routes[0].prefix_len();
        assert_eq!(prefix, 0, "default route has /0 prefix");
        assert!(dst.is_ipv4(), "v4 default via v4 gateway");
    }

    #[test]
    fn network_config_auto_route_merges_with_manual() {
        // Manual route should win on conflict; auto-routes fill gaps.
        let topo = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        let mut node = topo.nodes["a"].clone();
        node.routes.insert(
            "default".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.1".to_string()),
                dev: None,
                metric: None,
            },
        );
        let mut autos = BTreeMap::new();
        autos.insert(
            "default".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.99".to_string()),
                dev: None,
                metric: None,
            },
        );
        autos.insert(
            "10.99.0.0/16".to_string(),
            crate::types::RouteConfig {
                via: Some("10.0.0.50".to_string()),
                dev: None,
                metric: None,
            },
        );
        let cfg = topology_to_network_config("a", &node, &topo, Some(&autos)).unwrap();
        assert_eq!(
            cfg.routes().len(),
            2,
            "manual default + auto 10.99.0.0/16 (auto default suppressed)"
        );
    }

    #[test]
    fn network_config_dummy_iface_appears_as_link() {
        // Build the topology programmatically — NLL surfaces `dummy
        // NAME { ... }` as a top-level node property, not the
        // generic `interface { kind dummy }` shape.
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "lo0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Dummy),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"lo0"),
            "expected 'lo0' in declared links, got {names:?}"
        );
    }

    #[test]
    fn network_config_bond_with_members_emits_master_links() {
        // The bond iface declares one link; each member declares
        // another link with `.master(bond_name)` set.
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "bond0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Bond),
                members: vec!["eth0".into(), "eth1".into()],
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(names.contains(&"bond0"), "expected 'bond0', got {names:?}");
        assert!(
            names.contains(&"eth0") && names.contains(&"eth1"),
            "expected bond members 'eth0' + 'eth1' declared for master assignment, got {names:?}"
        );
    }

    #[test]
    fn network_config_vlan_iface_declares_parent_and_vid() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "eth0.42".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: Some("eth0".into()),
                vni: Some(42),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"eth0.42"),
            "expected 'eth0.42' vlan link, got {names:?}"
        );
    }

    #[test]
    fn network_config_vlan_parent_dummy_declared_first_regardless_of_hashmap_order() {
        // Plan 158e polish — `node.interfaces` is a BTreeMap, so the
        // raw iteration order can put the VLAN child before the
        // parent Dummy. nlink's apply iterates links_to_add in
        // declaration order, so a VLAN declared before its parent
        // would fail with ENODEV at the kernel.
        //
        // The two-pass shape inside `topology_to_network_config`
        // guarantees the Dummy is declared in pass 1 and the VLAN in
        // pass 2. Verify that order shows up in
        // `cfg.links().iter()` for the worst-case hashing — try
        // multiple parent/vlan name pairs to defeat any single
        // hash-seed alignment.
        for (parent_name, vlan_name) in &[
            ("eth0", "eth0.42"),
            ("aaa", "zzz.7"),
            ("zzz", "aaa.99"),
            ("p", "v"),
        ] {
            let topo = crate::parser::parse(
                r#"lab "t"
node host
"#,
            )
            .unwrap();
            let mut node = topo.nodes["host"].clone();
            node.interfaces.insert(
                (*parent_name).into(),
                crate::types::InterfaceConfig {
                    kind: Some(crate::types::InterfaceKind::Dummy),
                    ..Default::default()
                },
            );
            node.interfaces.insert(
                (*vlan_name).into(),
                crate::types::InterfaceConfig {
                    kind: Some(crate::types::InterfaceKind::Vlan),
                    parent: Some((*parent_name).into()),
                    vni: Some(42),
                    ..Default::default()
                },
            );
            let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
            let positions: Vec<usize> = cfg
                .links()
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    if l.name() == *parent_name || l.name() == *vlan_name {
                        Some((i, l.name()))
                    } else {
                        None
                    }
                })
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                positions.len(),
                2,
                "expected both '{parent_name}' and '{vlan_name}' in cfg.links() for case ({parent_name}, {vlan_name}), got {} link(s)",
                positions.len()
            );
            let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
            let parent_idx = names.iter().position(|n| *n == *parent_name).unwrap();
            let vlan_idx = names.iter().position(|n| *n == *vlan_name).unwrap();
            assert!(
                parent_idx < vlan_idx,
                "VLAN '{vlan_name}' must come after parent '{parent_name}' in links order, \
                 got parent@{parent_idx} vlan@{vlan_idx}: {names:?}"
            );
        }
    }

    #[test]
    fn network_config_vlan_missing_parent_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "v0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: None,
                vni: Some(10),
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing parent"),
            "want 'missing parent' error, got: {err}"
        );
    }

    #[test]
    fn network_config_vlan_missing_vid_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "v0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: Some("eth0".into()),
                vni: None,
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing vni"),
            "want 'missing vni' error, got: {err}"
        );
    }

    /// Plan 159a Slice 4 — VRF declared at the link level with
    /// `LinkBuilder::vrf(table)` (upstream Plan 190 §2.3).
    #[test]
    fn network_config_vrf_declares_link_with_table() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.vrfs.insert(
            "vrf-blue".into(),
            crate::types::VrfConfig {
                table: 100,
                interfaces: vec![],
                routes: BTreeMap::new(),
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"vrf-blue"),
            "expected 'vrf-blue' in declared links, got {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VRF enslave runs in pass 3, so the
    /// declared `links_to_add` lists the VRF strictly before any
    /// enslave entries referencing it. Defeats BTreeMap iteration
    /// order over `node.vrfs.interfaces`.
    #[test]
    fn network_config_vrf_master_enslave_after_vrf_link() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.vrfs.insert(
            "vrf-blue".into(),
            crate::types::VrfConfig {
                table: 100,
                interfaces: vec!["eth0".into(), "eth1".into()],
                routes: BTreeMap::new(),
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        let vrf_pos = names.iter().position(|n| *n == "vrf-blue").unwrap();
        let eth0_pos = names.iter().position(|n| *n == "eth0").unwrap();
        let eth1_pos = names.iter().position(|n| *n == "eth1").unwrap();
        assert!(
            vrf_pos < eth0_pos,
            "VRF link must be declared before enslaved 'eth0'; \
             got order {names:?}"
        );
        assert!(
            vrf_pos < eth1_pos,
            "VRF link must be declared before enslaved 'eth1'; \
             got order {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN declared via
    /// `LinkBuilder::vxlan` + `vxlan_local` + `vxlan_remote` +
    /// `vxlan_port` (upstream Plan 190 §2.1).
    #[test]
    fn network_config_vxlan_declares_with_local_remote_port() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx100".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: Some(100),
                local: Some("10.0.0.1".into()),
                remote: Some("10.0.0.2".into()),
                port: Some(4789),
                mtu: Some(1450),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        assert!(
            names.contains(&"vx100"),
            "expected 'vx100' VXLAN link declared, got {names:?}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN missing VNI produces an
    /// `InvalidTopology` error before any kernel call.
    #[test]
    fn network_config_vxlan_missing_vni_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: None,
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("missing vni"),
            "want 'missing vni' error, got: {err}"
        );
    }

    /// Plan 159a Slice 4 — VXLAN with a bad IPv4 literal in
    /// `local` errors out at config-build time, not deploy time.
    #[test]
    fn network_config_vxlan_bad_local_addr_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: Some(42),
                local: Some("not-an-ip".into()),
                ..Default::default()
            },
        );
        let err = topology_to_network_config("host", &node, &topo, None).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("bad vxlan local address"),
            "want 'bad vxlan local address' error, got: {rendered}"
        );
    }

    /// Plan 159a Phase 2 — `build_wg_public_key_map` decodes
    /// the explicit base64 private key and returns the
    /// deterministic public key.
    #[cfg(feature = "wireguard")]
    #[test]
    fn build_wg_public_key_map_decodes_explicit_key() {
        // Generate a known key once.
        let priv_key = generate_wg_private_key().unwrap();
        let pub_key = derive_wg_public_key(&priv_key);
        let b64 = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(priv_key)
        };

        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: Some(b64),
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.0.0.1/24".into()],
                peers: vec![],
            },
        );
        let mut topo = topo;
        topo.nodes.insert("host".into(), node);

        let map = build_wg_public_key_map(&topo).unwrap();
        let (got_priv, got_pub) = map["host"]["wg0"];
        assert_eq!(got_priv, priv_key);
        assert_eq!(got_pub, pub_key);
    }

    /// Plan 159a Phase 2 — bad base64 in `private_key` surfaces
    /// as `InvalidTopology` before any kernel call.
    #[cfg(feature = "wireguard")]
    #[test]
    fn build_wg_public_key_map_bad_key_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: Some("not-base64!@#".into()),
                listen_port: None,
                fwmark: None,
                addresses: vec![],
                peers: vec![],
            },
        );
        let mut topo = topo;
        topo.nodes.insert("host".into(), node);

        let err = build_wg_public_key_map(&topo).unwrap_err();
        assert!(matches!(err, Error::InvalidTopology(_)));
    }

    /// Plan 159a Phase 2 — `topology_to_wireguard_config`
    /// declares one device per WG iface and resolves peer
    /// cross-references to the right public key.
    #[cfg(feature = "wireguard")]
    #[test]
    fn topology_to_wireguard_config_declares_devices_and_peers() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
"#,
        )
        .unwrap();
        let mut topo = topo;

        let mut a = topo.nodes["a"].clone();
        a.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.99.0.1/24".into()],
                peers: vec!["b".into()],
            },
        );
        topo.nodes.insert("a".into(), a);

        let mut b = topo.nodes["b"].clone();
        b.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51821),
                fwmark: None,
                addresses: vec!["10.99.0.2/24".into()],
                peers: vec!["a".into()],
            },
        );
        topo.nodes.insert("b".into(), b);

        let keys = build_wg_public_key_map(&topo).unwrap();
        let cfg = topology_to_wireguard_config("a", &topo.nodes["a"], &topo, &keys).unwrap();
        assert_eq!(cfg.devices().len(), 1, "expected 1 WG device for node 'a'");
        let device = &cfg.devices()[0];
        assert_eq!(device.ifname, "wg0");
        // Peer should reference 'b'.wg0's public key.
        let expected_peer_pub = keys["b"]["wg0"].1;
        let has_expected_peer = device
            .peers
            .iter()
            .any(|p| p.public_key == expected_peer_pub);
        assert!(
            has_expected_peer,
            "expected peer with b.wg0's public key in cfg",
        );
    }

    /// Plan 159a Phase 2 — peer reference to a node with no WG
    /// surfaces as `InvalidTopology`.
    #[cfg(feature = "wireguard")]
    #[test]
    fn topology_to_wireguard_config_unknown_peer_node_errors() {
        let topo = crate::parser::parse(
            r#"lab "t"
node a
node b
"#,
        )
        .unwrap();
        let mut topo = topo;

        let mut a = topo.nodes["a"].clone();
        a.wireguard.insert(
            "wg0".into(),
            crate::types::WireguardConfig {
                private_key: None,
                listen_port: Some(51820),
                fwmark: None,
                addresses: vec!["10.99.0.1/24".into()],
                peers: vec!["b".into()],
            },
        );
        topo.nodes.insert("a".into(), a);
        // b has no WG config — peer-from-a references it.

        let keys = build_wg_public_key_map(&topo).unwrap();
        let err = topology_to_wireguard_config("a", &topo.nodes["a"], &topo, &keys).unwrap_err();
        assert!(matches!(err, Error::InvalidTopology(_)));
    }

    #[test]
    fn network_config_invalid_address_surfaces() {
        let mut topo = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        // Inject an invalid address bypassing the parser, mimicking
        // what would happen if a future feature flowed bad data.
        let mut node = topo.nodes["a"].clone();
        node.interfaces.insert(
            "eth0".to_string(),
            crate::types::InterfaceConfig {
                addresses: vec!["not-a-cidr".to_string()],
                ..Default::default()
            },
        );
        topo.nodes.insert("a".to_string(), node);
        let err = topology_to_network_config("a", &topo.nodes["a"], &topo, None).unwrap_err();
        assert!(
            err.to_string().contains("invalid address"),
            "want address validation error, got: {err}"
        );
    }
}
