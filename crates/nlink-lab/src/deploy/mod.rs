//! Lab deployment engine.
//!
//! Takes a validated [`Topology`] and creates the actual network lab using
//! nlink APIs. Follows the deployment sequence from the design document.

use nlink::netlink::namespace;
use nlink::{Connection, Route};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::running::RunningLab;
use crate::state::{self, LabState};
use crate::types::{Impairment, Topology};

mod apply;
pub mod op;
pub(crate) mod plan;
pub mod rollback;

use rollback::{Journal, Undo};

pub use op::{NsRef, Op, Plan, StackConfig, Stage};
pub(crate) use plan::network::*;
pub(crate) use plan::nftables::*;
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
    deploy_with(topology, &crate::ops::OpCtx::default()).await
}

/// [`deploy`], reporting its progress to `ctx` and stopping (rolled back)
/// when `ctx` is cancelled (#236).
pub async fn deploy_with(topology: &Topology, ctx: &crate::ops::OpCtx) -> Result<RunningLab> {
    ctx.set_lab(&topology.lab.name);
    ctx.started();
    let mut rolled_back = false;
    let result = deploy_ctx(topology, ctx, &mut rolled_back).await;
    ctx.finished(&result, rolled_back);
    result
}

/// The stages of `plan` in order, with their op counts.
fn plan_stages(plan: &Plan) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for op in &plan.ops {
        let name = op.stage().name();
        match out.last_mut() {
            Some((s, n)) if s == name => *n += 1,
            _ => out.push((name.to_string(), 1)),
        }
    }
    out
}

async fn deploy_ctx(
    topology: &Topology,
    ctx: &crate::ops::OpCtx,
    rolled_back: &mut bool,
) -> Result<RunningLab> {
    topology.validate().bail()?;
    let lock = state::lock(&topology.lab.name)?;
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
        ctx.log(
            "warn",
            format!(
                "unwinding {} journal entries left by an interrupted run",
                pending.entries().len()
            ),
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

    match deploy_inner(topology, &mut journal, ctx).await {
        Ok(running) => {
            journal.discard();
            Ok(running)
        }
        Err(e) => {
            // the caller reports the error: the log is for -v
            tracing::info!("deploy of '{lab_name}' failed: {e}; rolling back");
            ctx.emit(nlink_lab_shared::api::OpPhase::RollingBack {
                reason: e.to_string(),
                entries: journal.entries().len(),
            });
            journal.unwind().await;
            *rolled_back = true;
            // The lab never existed: its directory (the events log, an
            // archive's files) and its lock file go too (#103).
            if !state::exists(&lab_name) {
                let _ = state::remove(&lab_name);
                lock.remove_file();
            }
            Err(e)
        }
    }
}

async fn deploy_inner(
    topology: &Topology,
    journal: &mut Journal,
    ctx: &crate::ops::OpCtx,
) -> Result<RunningLab> {
    let inputs = PlanInputs::for_deploy(topology)?;
    let plan = plan::plan(topology, &inputs)?;
    tracing::info!("plan: {} op(s)", plan.ops.len());
    ctx.emit(nlink_lab_shared::api::OpPhase::Planned {
        stages: plan_stages(&plan),
        ops: plan.ops.len(),
        removals: 0,
    });
    let mut env = apply::ApplyEnv::for_deploy(topology)?;
    env.ctx = ctx.clone();
    let owned = std::mem::replace(journal, Journal::ephemeral(&topology.lab.name));
    let (res, env, owned) = apply::execute_isolated(plan.clone(), env, owned).await;
    *journal = owned;
    res?;

    // ── state file ──
    #[cfg(feature = "wireguard")]
    let wg_public_keys_b64 = {
        use nlink::netlink::genl::wireguard::PublicKey;
        let mut map = BTreeMap::new();
        if let Some(keys) = &inputs.wg_keys {
            wireguard_secrets::save(&topology.lab.name, keys)?;
            for (node, ifaces) in keys {
                let mut node_map = BTreeMap::new();
                for (iface, (_priv, pubkey)) in ifaces {
                    node_map.insert(iface.clone(), PublicKey(*pubkey).to_string());
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
    lab_state.process_tags = env.process_tags.clone();
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
    running.set_process_tags(env.process_tags);
    // What it holds is what was just written.
    running.mark_saved();

    // ── validate { … } assertions ──
    // Never fails the deploy; the structured results ride on the
    // returned lab so callers (`deploy --strict`) can decide.
    if !topology.assertions.is_empty() {
        tracing::info!("running validate assertions");
        if topology.assertion_retries.is_some_and(|r| r > 1) {
            ctx.waiting(
                nlink_lab_shared::api::WaitKind::AssertionRetries,
                Some(format!("{} assertion(s)", topology.assertions.len())),
                None,
            );
        }
        // blocking (pings, retries with sleeps): off the async thread
        let results = {
            let (lab, topo) = (running.clone(), topology.clone());
            tokio::task::spawn_blocking(move || run_assertions(&lab, &topo))
                .await
                .map_err(|e| Error::deploy_failed(format!("assertions: {e}")))?
        };
        for r in &results {
            ctx.emit(nlink_lab_shared::api::OpPhase::Assertion {
                description: r.description.clone(),
                passed: r.passed,
                detail: r.detail.clone(),
            });
        }
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

/// `desired` with its `subnet auto/N` networks given the running lab's
/// subnets ([`crate::subnet_pool::resolve_auto_subnets`]); borrowed when
/// it has none.
fn with_running_subnets<'a>(
    running: &RunningLab,
    desired: &'a Topology,
) -> Result<std::borrow::Cow<'a, Topology>> {
    let has_auto = desired.networks.values().any(|n| {
        n.subnet
            .as_deref()
            .is_some_and(|s| crate::subnet_pool::parse_auto_placeholder(s).is_some())
    });
    if !has_auto {
        return Ok(std::borrow::Cow::Borrowed(desired));
    }
    let mut owned = desired.clone();
    crate::subnet_pool::resolve_auto_subnets(running.topology(), &mut owned)?;
    Ok(std::borrow::Cow::Owned(owned))
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
    apply_plan_with(running, desired, &ApplyOptions::default())
}

/// [`apply_plan`] for an [`apply_with`] with these options: what `apply
/// --reset-impairments` and `restore` will run.
pub fn apply_plan_with(
    running: &RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<Plan> {
    let desired = &with_running_subnets(running, desired)?;
    desired.validate().bail()?;
    Ok(plan_apply(running, desired, opts)?.1)
}

/// Knobs for [`apply_with`].
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// Drop every runtime impairment (`nlink-lab impair`) and converge
    /// on the topology's `impair` declarations instead of keeping the
    /// live values (#59).
    pub reset_impairments: bool,
}

/// A running lab's topology with what runtime commands added to it: the
/// live impairments (`impair` at the CLI), and the partitioned endpoints.
/// What its traffic control really is.
pub(crate) fn live_view(running: &RunningLab) -> (Topology, BTreeSet<String>) {
    let mut topo = running.topology().clone();
    for (ep, live) in running.live_impairments() {
        if *live == Impairment::default() {
            topo.impairments.remove(ep);
        } else {
            topo.impairments.insert(ep.clone(), live.clone());
        }
    }
    (topo, running.partitions().keys().cloned().collect())
}

/// The two sides of an apply, with the runtime state carried over.
pub(crate) struct Overlay {
    /// The running lab's [`live_view`].
    pub current: Topology,
    pub current_partitions: BTreeSet<String>,
    /// The desired topology, with the live impairments it keeps.
    pub desired: Topology,
    pub desired_partitions: BTreeSet<String>,
    /// The live impairments kept (overlaid onto `desired`).
    pub kept: BTreeMap<String, Impairment>,
}

/// Runtime impairments and partitions are in neither topology. Unless the
/// topology *changed* that endpoint's declaration (or the caller asked for
/// a reset), a live impairment stays: it is overlaid onto both sides so the
/// plan sees no change there. A partition stays unless reset, as long as
/// its endpoint does: it drops traffic under whatever root qdisc the
/// endpoint gets (#168).
pub(crate) fn overlay_live_impairments(
    running: &RunningLab,
    desired_in: &Topology,
    opts: &ApplyOptions,
) -> Overlay {
    let (current, current_partitions) = live_view(running);
    let mut desired = desired_in.clone();
    let mut kept = BTreeMap::new();
    let unchanged =
        |ep: &str| running.topology().impairments.get(ep) == desired_in.impairments.get(ep);
    let endpoints: BTreeSet<String> = crate::impair_parse::topology_endpoints(desired_in)
        .into_iter()
        .collect();
    for (ep, live) in running.live_impairments() {
        if opts.reset_impairments || !unchanged(ep) || !endpoints.contains(ep) {
            // Reset, or the topology changed this endpoint: its
            // declaration wins. Or the endpoint is gone with its link or
            // node: a netem on it would be declared on a missing
            // interface, and every apply after failed (#204).
            continue;
        }
        if *live == Impairment::default() {
            desired.impairments.remove(ep);
        } else {
            desired.impairments.insert(ep.clone(), live.clone());
        }
        kept.insert(ep.clone(), live.clone());
    }
    let desired_partitions = if opts.reset_impairments {
        BTreeSet::new()
    } else {
        current_partitions
            .intersection(&endpoints)
            .cloned()
            .collect()
    };
    Overlay {
        current,
        current_partitions,
        desired,
        desired_partitions,
        kept,
    }
}

fn plan_apply(
    running: &RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<(PlanInputs, Plan)> {
    let (inputs, _, cur_plan, des_plan) = apply_plans(running, desired, opts)?;
    Ok((inputs, Plan::diff(&cur_plan, &des_plan)))
}

/// The current and desired plans of an apply, with the runtime state
/// overlaid ([`overlay_live_impairments`]) and planned with the keys of
/// the WireGuard interfaces that already exist, so `apply` does not rotate
/// every peer's key and `apply --check` compares the keys in use.
/// [`PlanInputs::for_deploy`], with the saved keys of the `key auto`
/// WireGuard interfaces that already exist, so planning a running lab does
/// not rotate every peer's key; a key the topology writes is the one it
/// gets (#204).
fn inputs_with_saved_keys(running: &RunningLab, topology: &Topology) -> Result<PlanInputs> {
    #[cfg(feature = "wireguard")]
    {
        let mut inputs = PlanInputs::for_deploy(topology)?;
        if let Some(fresh) = inputs.wg_keys.as_mut()
            && let Some(saved) = wireguard_secrets::load(running.name())
        {
            for (node, ifaces) in saved {
                if let Some(target) = fresh.get_mut(&node) {
                    for (iface, keys) in ifaces {
                        let auto = topology
                            .nodes
                            .get(&node)
                            .and_then(|n| n.wireguard.get(&iface))
                            .is_some_and(|wg| {
                                matches!(wg.private_key.as_deref(), None | Some("auto"))
                            });
                        if auto && target.contains_key(&iface) {
                            target.insert(iface, keys);
                        }
                    }
                }
            }
        }
        Ok(inputs)
    }
    #[cfg(not(feature = "wireguard"))]
    {
        let _ = running;
        PlanInputs::for_deploy(topology)
    }
}

fn apply_plans(
    running: &RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<(PlanInputs, Overlay, Plan, Plan)> {
    let overlay = overlay_live_impairments(running, desired, opts);
    let desired = &overlay.desired;

    // Each side planned with its own interfaces' keys, the saved ones where
    // they exist: the current side's too, so an interface the desired
    // topology drops is in the current plan's Stack and `Plan::diff`
    // removes it (#204).
    let inputs = inputs_with_saved_keys(running, desired)?;
    let current_inputs = inputs_with_saved_keys(running, &overlay.current)?;

    let cur_inputs = PlanInputs {
        partitions: overlay.current_partitions.clone(),
        ..current_inputs
    };
    let inputs = PlanInputs {
        partitions: overlay.desired_partitions.clone(),
        ..inputs
    };
    let cur_plan = plan::plan(&overlay.current, &cur_inputs)?;
    let des_plan = plan::plan(desired, &inputs)?;
    Ok((inputs, overlay, cur_plan, des_plan))
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
    apply_with_ctx(running, desired, opts, &crate::ops::OpCtx::default()).await
}

/// [`apply_with`], reporting its progress to `ctx` and stopping (rolled
/// back) when `ctx` is cancelled (#236).
pub async fn apply_with_ctx(
    running: &mut RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
    ctx: &crate::ops::OpCtx,
) -> Result<ApplyReport> {
    ctx.set_lab(running.name());
    ctx.started();
    let mut rolled_back = false;
    let result = apply_ctx(running, desired, opts, ctx, &mut rolled_back).await;
    ctx.finished(&result, rolled_back);
    result
}

async fn apply_ctx(
    running: &mut RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
    ctx: &crate::ops::OpCtx,
    rolled_back: &mut bool,
) -> Result<ApplyReport> {
    let desired = &with_running_subnets(running, desired)?;
    desired.validate().bail()?;
    let _lock = state::lock(running.name())?;
    let (inputs, overlay, cur_plan, des_plan) = apply_plans(running, desired, opts)?;
    let diff = Plan::diff(&cur_plan, &des_plan);
    let report = ApplyReport {
        ops: diff.ops.len(),
        removed: diff.ops.iter().filter(|o| o.is_removal()).count(),
        applied: diff.ops.iter().map(|o| o.describe()).collect(),
    };
    tracing::info!("apply: {} op(s), {} removal(s)", report.ops, report.removed);
    ctx.emit(nlink_lab_shared::api::OpPhase::Planned {
        stages: plan_stages(&diff),
        ops: report.ops,
        removals: report.removed,
    });
    preflight_recreates(running, desired).await?;

    let mut env = apply::ApplyEnv::from_running(running, desired, &cur_plan)?;
    env.ctx = ctx.clone();
    let journal = Journal::new(running.name());
    let (res, env, mut journal) = apply::execute_isolated(diff.clone(), env, journal).await;
    if let Err(e) = res {
        // the caller reports the error: the log is for -v
        tracing::info!(
            "apply to '{}' failed: {e}; rolling back this apply",
            running.name()
        );
        ctx.emit(nlink_lab_shared::api::OpPhase::RollingBack {
            reason: e.to_string(),
            entries: journal.entries().len(),
        });
        journal.unwind().await;
        *rolled_back = true;
        restore_dns(running, &cur_plan);
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

    running.set_topology(desired.clone().into_owned());
    running.set_live_impairments(overlay.kept);
    running.retain_partitions(|ep| overlay.desired_partitions.contains(ep));
    running.absorb_apply(
        env.namespace_names,
        env.containers,
        env.pids,
        env.starttimes,
        env.exec_pids,
        env.process_logs,
        env.process_tags,
        env.mgmt_peers,
        env.dns_injected,
        env.wifi_loaded,
    );
    // This handle's changes, merged into the file as it is: created_at
    // and wg_public_keys survive (#28), and so does a process spawned or
    // an impairment set since this lab was loaded.
    running.save_merged(Some(desired))?;

    // ── validate { … } assertions, exactly as after deploy (#86) ──
    // Never fails the apply; results ride on `running` for `--strict`.
    if desired.assertions.is_empty() {
        running.set_assertion_results(Vec::new());
    } else {
        tracing::info!("running validate assertions");
        let results = {
            let (lab, topo) = (running.clone(), desired.clone().into_owned());
            tokio::task::spawn_blocking(move || run_assertions(&lab, &topo))
                .await
                .map_err(|e| Error::deploy_failed(format!("assertions: {e}")))?
        };
        for r in &results {
            ctx.emit(nlink_lab_shared::api::OpPhase::Assertion {
                description: r.description.clone(),
                passed: r.passed,
                detail: r.detail.clone(),
            });
        }
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
    restore_with(running, snapshot, &crate::ops::OpCtx::default()).await
}

/// [`restore`], reporting its progress to `ctx` (#236).
pub async fn restore_with(
    running: &mut RunningLab,
    snapshot: &crate::state::Snapshot,
    ctx: &crate::ops::OpCtx,
) -> Result<ApplyReport> {
    let opts = ApplyOptions {
        reset_impairments: true,
    };
    // With reset the plan replaces every runtime impairment and every
    // partition by the snapshot topology's declarations, and the
    // bookkeeping is dropped with them.
    let report = apply_with_ctx(running, &snapshot.topology, &opts, ctx).await?;

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

/// After a failed apply's rollback: the lab's `/etc/hosts` section and
/// per-node overlays as the running topology has them. The apply rewrote
/// them for the desired topology, and what existed before it is not the
/// journal's to remove (#204).
fn restore_dns(running: &RunningLab, current: &Plan) {
    let entries = crate::dns::generate_hosts_entries(running.topology());
    for op in &current.ops {
        let res = match op {
            Op::DnsInject { lab } => crate::dns::inject_hosts(lab, &entries),
            Op::DnsNetnsEtc { ns, .. } if crate::dns::netns_hosts_path(ns).exists() => {
                crate::dns::create_netns_etc(ns, &entries)
            }
            _ => continue,
        };
        if let Err(e) = res {
            tracing::warn!("rollback: DNS entries not restored: {e}");
        }
    }
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
/// The node's bridge-network ports, sysctls, mgmt veth and `run` commands
/// lived in the old namespace as well, and are re-created the same way.
pub async fn reattach_node(running: &mut RunningLab, node: &str) -> Result<ApplyReport> {
    reattach_node_with(running, node, &crate::ops::OpCtx::default()).await
}

/// [`reattach_node`], reporting its progress to `ctx` (#236).
pub async fn reattach_node_with(
    running: &mut RunningLab,
    node: &str,
    ctx: &crate::ops::OpCtx,
) -> Result<ApplyReport> {
    ctx.set_lab(running.name());
    ctx.started();
    let result = reattach_ctx(running, node, ctx).await;
    ctx.finished(&result, result.is_err());
    result
}

async fn reattach_ctx(
    running: &mut RunningLab,
    node: &str,
    ctx: &crate::ops::OpCtx,
) -> Result<ApplyReport> {
    // The lab as it is, runtime impairments included: every node's Stack
    // is re-applied, so planning from the declared values undid an
    // `impair` anywhere in the lab, not only on the restarted node (#221).
    let (desired, all_partitions) = live_view(running);
    desired.validate().bail()?;
    let _lock = state::lock(running.name())?;
    let touches = |ep: &String| ep.split(':').next() == Some(node);
    let lost: std::collections::BTreeSet<String> = desired
        .links
        .iter()
        .filter(|l| l.endpoints.iter().any(touches))
        .flat_map(|l| l.endpoints.iter().cloned())
        .collect();
    // The lost link ends lost their netem with the veth; what `impair`
    // installed at runtime is what comes back (it is in `desired`).
    let mut current = desired.clone();
    current.links.retain(|l| !l.endpoints.iter().any(touches));
    for ep in &lost {
        current.impairments.remove(ep);
    }
    // A partition of a lost link end died with the veth.
    let partitions: BTreeSet<String> = all_partitions
        .into_iter()
        .filter(|ep| !lost.contains(ep))
        .collect();
    // The keys in use: fresh ones re-keyed every `key auto` interface of
    // the lab, and were not saved (#221).
    let inputs = PlanInputs {
        partitions,
        ..inputs_with_saved_keys(running, &desired)?
    };
    let cur_plan = plan::plan(&current, &inputs)?;
    let des_plan = plan::plan(&desired, &inputs)?;
    let mut diff = Plan::diff(&cur_plan, &des_plan);
    // Everything else that lived in the old namespace is gone too, and
    // the diff sees it as unchanged: the node's sysctls (forwarding), its
    // bridge-network ports, its mgmt veth and its `run` commands.
    for op in des_plan.ops.iter().filter(|op| op.lives_in_node(node)) {
        if !diff.ops.contains(op) {
            diff.ops.push(op.clone());
        }
    }
    diff.ops.sort_by_key(|o| o.stage());
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
    ctx.emit(nlink_lab_shared::api::OpPhase::Planned {
        stages: plan_stages(&diff),
        ops: report.ops,
        removals: report.removed,
    });
    let mut env = apply::ApplyEnv::from_running(running, &desired, &cur_plan)?;
    env.ctx = ctx.clone();
    let journal = Journal::new(running.name());
    let (res, env, mut journal) = apply::execute_isolated(diff.clone(), env, journal).await;
    if let Err(e) = res {
        tracing::warn!("reattach '{node}' failed: {e}; rolling back");
        journal.unwind().await;
        return Err(e);
    }
    journal.discard();
    #[cfg(feature = "wireguard")]
    if let Some(keys) = &inputs.wg_keys {
        wireguard_secrets::save(running.name(), keys)?;
    }
    running.retain_partitions(|ep| !lost.contains(ep));
    running.absorb_apply(
        env.namespace_names,
        env.containers,
        env.pids,
        env.starttimes,
        env.exec_pids,
        env.process_logs,
        env.process_tags,
        env.mgmt_peers,
        env.dns_injected,
        env.wifi_loaded,
    );
    // The lab lock is held: write state.json directly, as `apply` does
    // (`save_state` would take the lock again and block on itself).
    // This handle's changes, merged into the file as it is: created_at
    // and wg_public_keys survive (#28), and so does a process spawned or
    // an impairment set since this lab was loaded.
    running.save_merged(Some(running.topology()))?;
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
        use nlink::netlink::genl::wireguard::{PrivateKey, PublicKey};
        use std::os::unix::fs::OpenOptionsExt;
        let mut out: BTreeMap<String, BTreeMap<String, (String, String)>> = BTreeMap::new();
        for (node, ifaces) in keys {
            for (iface, (priv_k, pub_k)) in ifaces {
                out.entry(node.clone()).or_default().insert(
                    iface.clone(),
                    (
                        PrivateKey::from_bytes(*priv_k).to_string(),
                        PublicKey(*pub_k).to_string(),
                    ),
                );
            }
        }
        let p = path(lab);
        if let Some(dir) = p.parent() {
            crate::state::ensure_dir(dir)?;
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
        use nlink::netlink::genl::wireguard::{PrivateKey, PublicKey};
        let text = std::fs::read_to_string(path(lab)).ok()?;
        let raw: BTreeMap<String, BTreeMap<String, (String, String)>> =
            serde_json::from_str(&text).ok()?;
        let mut out: WgKeys = BTreeMap::new();
        for (node, ifaces) in raw {
            for (iface, (p, q)) in ifaces {
                let (Ok(p), Ok(q)) = (p.parse::<PrivateKey>(), q.parse::<PublicKey>()) else {
                    continue;
                };
                out.entry(node.clone())
                    .or_default()
                    .insert(iface, (*p.as_bytes(), q.0));
            }
        }
        Some(out)
    }
}

/// The options a node's network layer is planned with, in an apply and
/// in `apply --check` alike:
///
/// * purge in apply mode, also over the VRF tables the node declared
///   routes into before (`previous_vrf_tables`): a table it no longer
///   declares into is not in the config, so purge would otherwise leave
///   its routes behind;
/// * `previous`, the node's config as the running lab has it: what it
///   declared and this one no longer does is removed, declared links
///   included (#159, nlink #512);
/// * `preserve_undeclared_tc`: a link nlink recreates (a VLAN id, a VNI, a
///   bond mode changed, nlink #417) keeps nlink-lab's netem, `qdisc`,
///   rate limit and per-pair tree, which nlink snapshots before the
///   delete and replays after (nlink #645). Without it nlink refuses the
///   recreate, as that Tc is not in the config.
pub(crate) fn network_plan_options(
    purge: bool,
    previous_vrf_tables: &[u32],
    previous: Option<&std::sync::Arc<nlink::netlink::config::NetworkConfig>>,
) -> nlink::netlink::config::PlanOptions {
    let mut opts = nlink::netlink::config::PlanOptions::default()
        .purge(purge)
        .purge_tables(previous_vrf_tables.iter().copied())
        .preserve_undeclared_tc(true);
    if let Some(prev) = previous {
        opts = opts.previous(prev.clone());
    }
    opts
}

/// Each node's network config in `plan` (links, addresses, routes,
/// traffic control), as `PlanOptions::previous` for the plan that
/// replaces it: what it declared and the new one does not is removed.
pub(crate) fn stack_networks(
    plan: &Plan,
) -> BTreeMap<String, std::sync::Arc<nlink::netlink::config::NetworkConfig>> {
    plan.ops
        .iter()
        .filter_map(|op| match op {
            Op::Stack { node, cfg } => {
                Some((node.clone(), std::sync::Arc::new(cfg.network.clone())))
            }
            _ => None,
        })
        .collect()
}

/// A node's declarative layers as one nlink `Stack` (nlink #519): its
/// network config, its nftables table, and its WireGuard devices. With no
/// firewall and no NAT the table is declared absent, so one an earlier
/// apply left behind is deleted (and reported by `apply --check`).
pub(crate) fn node_stack(cfg: &StackConfig) -> Result<nlink::facade::Stack> {
    use nlink::netlink::nftables::{Family, config::NftablesConfig};
    let nftables = if cfg.firewall.is_none() && cfg.nat.is_none() {
        NftablesConfig::new().absent_table(NLINK_LAB_TABLE, Family::Inet)
    } else {
        topology_to_nftables_config(cfg.firewall.as_ref(), cfg.nat.as_ref())?
    };
    #[allow(unused_mut)]
    let mut stack = nlink::facade::Stack::new()
        .network(cfg.network.clone())
        .nftables(nftables);
    #[cfg(feature = "wireguard")]
    if let Some(wg) = &cfg.wireguard {
        stack = stack.wireguard(wg.clone());
    }
    Ok(stack)
}

/// Apply a node's stack. nlink plans every layer once and applies that
/// plan: WireGuard links first (created and up, so their addresses land on
/// existing devices), then the network config, the nftables table, and
/// WireGuard's keys and peers. What a concurrent change can cause — EBUSY,
/// EAGAIN, an interrupted dump, an nftables commit refused because the
/// ruleset moved since the plan (ERESTART) — is planned and applied again,
/// twice at most, 50 ms apart and doubling. An unchanged node is zero
/// kernel changes.
pub(crate) async fn apply_stack_for_node(
    handle: &NsRef,
    node_name: &str,
    cfg: &StackConfig,
    plan: nlink::netlink::config::PlanOptions,
) -> Result<()> {
    let stack = node_stack(cfg)?;
    let mut apply = nlink::netlink::config::ApplyOptions::default();
    apply.plan = plan;
    let retry = nlink::reconcile::ReconcileOptions::new()
        .with_max_retries(2)
        .with_backoff(std::time::Duration::from_millis(50));
    let report = stack
        .apply_reconcile_in(handle.spec(), retry, apply)
        .await
        .map_err(|e| Error::nlink(format!("stack apply on '{node_name}'"), e))?;
    tracing::info!(
        node = %node_name,
        attempts = report.attempts,
        changes = report.change_count(),
        "stack reconcile complete"
    );
    Ok(())
}

/// Converge one node's traffic control after a runtime `impair`,
/// `--clear`, `--partition` or `--heal` (#108, #168): its Tc alone,
/// declared from `after` (a [`live_view`]) and planned against what
/// `before` declared, so what the change dropped is removed. Its links,
/// addresses and routes are not part of the config, and not touched.
pub(crate) async fn reconcile_node_tc(
    handle: &NsRef,
    node: &str,
    before: (&Topology, &BTreeSet<String>),
    after: (&Topology, &BTreeSet<String>),
) -> Result<()> {
    use nlink::netlink::config::{ApplyOptions, NetworkConfig, PlanOptions};
    let declare = |(topology, partitions): (&Topology, &BTreeSet<String>)| {
        plan::tc::declare_node_tc(NetworkConfig::new(), node, topology, partitions)
    };
    let previous = std::sync::Arc::new(declare(before)?);
    let cfg = declare(after)?;
    let conn: Connection<Route> = handle
        .connection()
        .map_err(|e| Error::nlink(format!("connection for '{node}'"), e))?;
    let mut apply = ApplyOptions::default();
    apply.plan = PlanOptions::default().previous(previous);
    let retry = nlink::reconcile::ReconcileOptions::new()
        .with_max_retries(2)
        .with_backoff(std::time::Duration::from_millis(50));
    let report = cfg
        .apply_reconcile_with_options(&conn, retry, apply)
        .await
        .map_err(|e| Error::nlink(format!("traffic control on '{node}'"), e))?;
    tracing::info!(node = %node, changes = report.changes_made, "traffic control reconciled");
    Ok(())
}

/// Compute the full layered diff between a running lab's live
/// state and a desired topology. Plan 158f Phase 2.
///
/// Aggregates the lab-graph differences (nodes/links/impair/sysctls/…,
/// [`crate::diff::diff_topologies`]) with what each node's `Stack` would
/// change: the same stack `apply` builds (saved WireGuard keys included),
/// planned the way `apply` plans it (purge, removals against the running
/// topology's config, recreates that keep their Tc) with
/// `Stack::plan_in`: its network, nftables and WireGuard diffs.
///
/// Used by `nlink-lab apply --check`, `apply --dry-run` and `verify`, so
/// CI and operators see the full set of kernel changes `apply` would
/// commit — not just the lab-graph subset that `TopologyDiff` covers.
/// One dump round-trip per node per layer; ms-scale on a quiet host.
pub async fn compute_layered_diff(
    running: &RunningLab,
    desired: &Topology,
) -> Result<crate::diff::LayeredDiff> {
    compute_layered_diff_with(running, desired, &ApplyOptions::default()).await
}

/// [`compute_layered_diff`] for an [`apply_with`] with these options:
/// with `reset_impairments`, the runtime impairments and partitions it
/// would drop are part of the diff. Without it they were not, so `apply
/// --reset-impairments` on an unchanged topology found nothing to do.
pub async fn compute_layered_diff_with(
    running: &RunningLab,
    desired: &Topology,
    opts: &ApplyOptions,
) -> Result<crate::diff::LayeredDiff> {
    let desired = &with_running_subnets(running, desired)?;
    let topology = crate::diff::diff_topologies(running.topology(), desired);
    let (_, _, cur_plan, des_plan) = apply_plans(running, desired, opts)?;
    let previous = stack_networks(&cur_plan);

    let mut network = BTreeMap::new();
    let mut nftables = BTreeMap::new();
    let mut wireguard = BTreeMap::new();
    for op in &des_plan.ops {
        let Op::Stack {
            node: node_name,
            cfg,
        } = op
        else {
            continue;
        };
        // A node `desired` adds has no namespace yet: the lab-graph diff
        // (`topology.nodes_added`) reports it.
        let Ok(handle) = node_handle_for(running, node_name) else {
            continue;
        };
        // Planned the way `apply` plans it: purge, the previous config's
        // removals, the Tc a recreate keeps.
        let tables: Vec<u32> = running
            .topology()
            .nodes
            .get(node_name)
            .map(|n| n.vrfs.values().map(|v| v.table).collect())
            .unwrap_or_default();
        let opts = network_plan_options(true, &tables, previous.get(node_name));
        let plan = node_stack(cfg)?
            .plan_in(handle.spec(), opts)
            .await
            .map_err(|e| Error::nlink(format!("stack plan on '{node_name}'"), e))?;
        if let Some(d) = plan.network.filter(|d| !d.is_empty()) {
            network.insert(node_name.clone(), d);
        }
        if let Some(d) = plan.nftables.filter(|d| !d.is_empty()) {
            nftables.insert(node_name.clone(), d);
        }
        if let Some(d) = plan.wireguard.filter(|d| !d.is_empty()) {
            wireguard.insert(node_name.clone(), d);
        }
    }

    Ok(crate::diff::LayeredDiff {
        topology,
        network,
        nftables,
        wireguard,
    })
}

/// Refuse an apply before anything changes when it implies a link
/// recreate nlink would refuse.
///
/// nlink recreates a declared link whose kind parameters cannot change in
/// place (a VLAN id, a VNI, a bond mode, nlink #417), and refuses up front
/// when the link carries state the config does not declare and the
/// recreate would destroy (a permanent neighbour, a hand-added FDB entry
/// or route, FRR's routes, nlink #426). That refusal comes from the
/// `Stack` stage, after earlier stages of the same apply have run, so it
/// is checked here first. nlink's offline prediction
/// (`NetworkConfig::diff_against`) picks the nodes; only those pay for a
/// read-only kernel diff.
async fn preflight_recreates(running: &RunningLab, desired: &Topology) -> Result<()> {
    let current = running.topology();
    let cur_autos = auto_routes_for(current);
    let des_autos = auto_routes_for(desired);
    let mut refused = Vec::new();
    for (node_name, node) in &desired.nodes {
        let Some(prev) = current.nodes.get(node_name) else {
            continue;
        };
        let des_cfg =
            topology_to_network_config(node_name, node, desired, des_autos.get(node_name))?;
        let cur_cfg =
            topology_to_network_config(node_name, prev, current, cur_autos.get(node_name))?;
        if des_cfg.diff_against(&cur_cfg).is_empty() {
            continue;
        }
        let conn: Connection<Route> = node_handle_for(running, node_name)?
            .connection()
            .map_err(|e| Error::nlink(format!("NetworkConfig connection on '{node_name}'"), e))?;
        let tables: Vec<u32> = prev.vrfs.values().map(|v| v.table).collect();
        let previous = std::sync::Arc::new(cur_cfg);
        let diff = des_cfg
            .diff_with_options(&conn, network_plan_options(true, &tables, Some(&previous)))
            .await
            .map_err(|e| Error::nlink(format!("NetworkConfig::diff on '{node_name}'"), e))?;
        for r in diff.links_to_recreate.iter().filter(|r| r.is_refused()) {
            let blockers: Vec<String> = r.blocked_by.iter().map(ToString::to_string).collect();
            refused.push(format!(
                "{node_name}:{} ({}) carries {}",
                r.name,
                r.reason,
                blockers.join(", ")
            ));
        }
    }
    if refused.is_empty() {
        return Ok(());
    }
    Err(Error::deploy_failed(format!(
        "apply would recreate a link that carries state the topology does not declare, \
         and nothing has been changed: {}. Remove that state first (e.g. `ip neigh del`, \
         a hand-added route or FDB entry; for FRR, take the link out of routing), or \
         destroy and deploy the lab again.",
        refused.join("; ")
    )))
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
pub(crate) fn run_assertions(
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

    /// The delay of the netem `plan` declares on `node:iface`.
    fn declared_delay(plan: &Plan, node: &str, iface: &str) -> Option<std::time::Duration> {
        plan.ops.iter().find_map(|o| match o {
            Op::Stack { node: n, cfg } if n == node => {
                cfg.network
                    .qdiscs()
                    .iter()
                    .find_map(|q| match q.qdisc_type() {
                        nlink::netlink::tc_spec::QdiscSpec::Netem(netem) if q.dev() == iface => {
                            netem.delay
                        }
                        _ => None,
                    })
            }
            _ => None,
        })
    }

    const LINK_1MS: &str = "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 1ms }\n";
    const LINK_2MS: &str = "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 2ms }\n";
    const MS: fn(u64) -> Option<std::time::Duration> =
        |ms| Some(std::time::Duration::from_millis(ms));

    #[test]
    fn overlay_keeps_live_impairment_when_topology_unchanged() {
        let topo = crate::parser::parse(LINK_1MS).unwrap();
        let running =
            running_with_live(&topo, &[("a:eth0", Some("50ms")), ("b:eth0", Some("7ms"))]);
        let o = overlay_live_impairments(&running, &topo, &ApplyOptions::default());
        // both sides carry the live values → the stacks do not change
        assert_eq!(
            o.current.impairments["a:eth0"].delay.as_deref(),
            Some("50ms")
        );
        assert_eq!(
            o.desired.impairments["a:eth0"].delay.as_deref(),
            Some("50ms")
        );
        assert_eq!(
            o.desired.impairments["b:eth0"].delay.as_deref(),
            Some("7ms")
        );
        assert_eq!(o.kept.len(), 2);
        let (_, _, cur, des) = apply_plans(&running, &topo, &ApplyOptions::default()).unwrap();
        assert_eq!(stack_networks(&cur), stack_networks(&des));
        assert_eq!(declared_delay(&des, "a", "eth0"), MS(50));
    }

    #[test]
    fn overlay_drops_live_impairment_when_topology_changes_the_endpoint() {
        let topo = crate::parser::parse(LINK_1MS).unwrap();
        let desired = crate::parser::parse(LINK_2MS).unwrap();
        let running = running_with_live(&topo, &[("a:eth0", Some("50ms"))]);
        let o = overlay_live_impairments(&running, &desired, &ApplyOptions::default());
        assert_eq!(
            o.desired.impairments["a:eth0"].delay.as_deref(),
            Some("2ms")
        );
        assert!(o.kept.is_empty());
        let (_, _, cur, des) = apply_plans(&running, &desired, &ApplyOptions::default()).unwrap();
        assert_eq!(declared_delay(&cur, "a", "eth0"), MS(50));
        assert_eq!(declared_delay(&des, "a", "eth0"), MS(2));
    }

    #[test]
    fn overlay_reset_and_cleared_live_impairment() {
        let topo = crate::parser::parse(LINK_1MS).unwrap();
        // `impair --clear` on a declared endpoint records an empty value:
        // the overlay removes the declaration on both sides.
        let running = running_with_live(&topo, &[("a:eth0", None)]);
        let o = overlay_live_impairments(&running, &topo, &ApplyOptions::default());
        assert!(!o.current.impairments.contains_key("a:eth0"));
        assert!(!o.desired.impairments.contains_key("a:eth0"));
        assert_eq!(o.kept.len(), 1);
        // --reset-impairments: the topology wins again.
        let reset = ApplyOptions {
            reset_impairments: true,
        };
        let o = overlay_live_impairments(&running, &topo, &reset);
        assert!(
            !o.current.impairments.contains_key("a:eth0"),
            "current reflects the kernel"
        );
        assert_eq!(
            o.desired.impairments["a:eth0"].delay.as_deref(),
            Some("1ms")
        );
        assert!(o.kept.is_empty());
        let (_, _, cur, des) = apply_plans(&running, &topo, &reset).unwrap();
        assert_eq!(declared_delay(&cur, "a", "eth0"), None);
        assert_eq!(
            declared_delay(&des, "a", "eth0"),
            MS(1),
            "reset re-installs it"
        );
        // A live value different from the declaration is replaced too.
        let running = running_with_live(&topo, &[("a:eth0", Some("50ms"))]);
        let (_, _, _, des) = apply_plans(&running, &topo, &reset).unwrap();
        assert_eq!(declared_delay(&des, "a", "eth0"), MS(1));
    }

    /// A live impairment on an endpoint whose link the desired topology
    /// drops is not declared again: a netem on a missing interface failed
    /// every apply after (#204).
    #[test]
    fn overlay_drops_live_impairment_of_a_removed_link() {
        // only `impair` put a netem there: the declaration did not change
        let topo = crate::parser::parse(
            "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
        )
        .unwrap();
        let desired = crate::parser::parse("lab \"t\"\nnode a\nnode b\n").unwrap();
        let running = running_with_live(&topo, &[("a:eth0", Some("50ms"))]);
        let o = overlay_live_impairments(&running, &desired, &ApplyOptions::default());
        assert!(!o.desired.impairments.contains_key("a:eth0"));
        assert!(o.kept.is_empty());
        let (_, _, _, des) = apply_plans(&running, &desired, &ApplyOptions::default()).unwrap();
        assert_eq!(declared_delay(&des, "a", "eth0"), None);
    }

    /// The running lab's plan is made with its own WireGuard keys: planned
    /// with the desired topology's (none, once it drops its tunnels), the
    /// current Stack had no WireGuard device and the diff deleted nothing
    /// (#204).
    #[cfg(feature = "wireguard")]
    #[test]
    fn apply_deletes_the_wireguard_interfaces_the_topology_drops() {
        let wg = |addr: &str, peer: &str| {
            format!("{{ wireguard wg0 {{ key auto listen 51820 address {addr} peers [{peer}] }} }}")
        };
        let link = "link a:eth0 -- b:eth0 { 10.0.0.1/30 -- 10.0.0.2/30 }";
        let topo = crate::parser::parse(&format!(
            "lab \"wg-unit-204\"\nnode a {}\nnode b {}\n{link}\n",
            wg("192.168.255.1/32", "b"),
            wg("192.168.255.2/32", "a")
        ))
        .unwrap();
        let desired =
            crate::parser::parse(&format!("lab \"wg-unit-204\"\nnode a\nnode b\n{link}\n"))
                .unwrap();
        let running = running_with_live(&topo, &[]);
        let (_, _, cur, des) = apply_plans(&running, &desired, &ApplyOptions::default()).unwrap();
        let diff = Plan::diff(&cur, &des);
        for n in ["a", "b"] {
            assert!(
                diff.ops.iter().any(
                    |o| matches!(o, Op::DeleteLink { node, iface } if node == n && iface == "wg0")
                ),
                "{n}: {:?}",
                diff.ops
            );
        }
    }

    /// A partition is drop filters under the root (#168): it survives an
    /// apply, even one that changes the endpoint's impairment, unless
    /// reset or the endpoint is gone.
    #[test]
    fn overlay_partitions_survive_unless_reset_or_gone() {
        let topo = crate::parser::parse(LINK_1MS).unwrap();
        let mut running = running_with_live(&topo, &[]);
        running.set_partitions(BTreeMap::from([(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("1ms".into()),
                ..Default::default()
            },
        )]));
        let parts = |desired: &Topology, opts: &ApplyOptions| {
            overlay_live_impairments(&running, desired, opts).desired_partitions
        };
        let a: BTreeSet<String> = ["a:eth0".to_string()].into();
        assert_eq!(parts(&topo, &ApplyOptions::default()), a);
        let (_, _, cur, des) = apply_plans(&running, &topo, &ApplyOptions::default()).unwrap();
        assert_eq!(stack_networks(&cur), stack_networks(&des), "partition kept");
        assert_eq!(
            declared_delay(&des, "a", "eth0"),
            MS(1),
            "under it, the netem"
        );

        let changed = crate::parser::parse(LINK_2MS).unwrap();
        assert_eq!(parts(&changed, &ApplyOptions::default()), a);
        let (_, _, _, des) = apply_plans(&running, &changed, &ApplyOptions::default()).unwrap();
        assert_eq!(declared_delay(&des, "a", "eth0"), MS(2));

        let reset = ApplyOptions {
            reset_impairments: true,
        };
        assert!(parts(&topo, &reset).is_empty());
        let gone = crate::parser::parse("lab \"t\"\nnode a\nnode b\n").unwrap();
        assert!(parts(&gone, &ApplyOptions::default()).is_empty());
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

    /// A router must never get a route for a network it is already
    /// attached to. Such a route has the same destination and prefix as
    /// the kernel's connected route, so `replace_route` replaces it, and
    /// every later gateway on that segment stops resolving —
    /// `ENETUNREACH`, "Nexthop has invalid gateway" (#138).
    ///
    /// The shape that produced it: `edge` sits on two segments, and BFS
    /// reaches a node on the *second* one the long way round, through
    /// the first.
    #[test]
    fn test_auto_route_never_covers_a_connected_network() {
        let topo = crate::parser::parse(
            r#"lab "t" { routing auto }
profile router { forward ipv4 }
node edge : router
node hub : router
node far : router
network a {
  members [edge:eth0, hub:eth0]
  port edge:eth0 { 172.16.1.2/24 }
  port hub:eth0 { 172.16.1.1/24 }
}
network b {
  members [edge:eth1, hub:eth1, far:eth1]
  port edge:eth1 { 172.16.2.2/24 }
  port hub:eth1 { 172.16.2.1/24 }
  port far:eth1 { 172.16.2.3/24 }
}
"#,
        )
        .unwrap();
        let routes = auto_generate_routes(&topo);
        for connected in ["172.16.1.0/24", "172.16.2.0/24"] {
            assert!(
                !routes
                    .get("edge")
                    .is_some_and(|r| r.contains_key(connected)),
                "edge is on {connected}; routing it via a gateway replaces the \
                 connected route: {:?}",
                routes.get("edge")
            );
        }
    }

    /// The same invariant over every shipped example that uses
    /// `routing auto`. `auto_generate_routes` is pure, so this costs
    /// nothing and needs no root — and a deploy is the only other thing
    /// that would have caught #138, which CI does not do for most
    /// examples.
    #[test]
    fn examples_never_auto_route_over_a_connected_network() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let mut checked = 0;
        let mut stack = vec![root.join("examples")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read examples dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "nll") {
                    continue;
                }
                // Imported fragments are not standalone topologies.
                let Ok(topo) = crate::parser::parse_file(&path) else {
                    continue;
                };
                if topo.lab.routing != crate::types::RoutingMode::Auto {
                    continue;
                }
                checked += 1;
                let routes = auto_generate_routes(&topo);
                // Every address the node holds, as a network CIDR.
                for (node_name, node_routes) in &routes {
                    let mut connected: std::collections::BTreeSet<String> = Default::default();
                    for link in &topo.links {
                        let Some(addrs) = &link.addresses else {
                            continue;
                        };
                        for (i, ep) in link.endpoints.iter().enumerate() {
                            if crate::types::EndpointRef::parse(ep)
                                .is_some_and(|e| &e.node == node_name)
                                && let Some(cidr) = super::plan::network::network_cidr(&addrs[i])
                            {
                                connected.insert(cidr);
                            }
                        }
                    }
                    for network in topo.networks.values() {
                        for (ep, port) in &network.ports {
                            if !crate::types::EndpointRef::parse(ep)
                                .is_some_and(|e| &e.node == node_name)
                            {
                                continue;
                            }
                            for addr in &port.addresses {
                                if let Some(cidr) = super::plan::network::network_cidr(addr) {
                                    connected.insert(cidr);
                                }
                            }
                        }
                    }
                    for dest in node_routes.keys() {
                        assert!(
                            !connected.contains(dest),
                            "{}: {node_name} is already on {dest}, but routing auto \
                             emitted a route for it — that replaces the connected \
                             route and breaks every gateway on the segment (#138)",
                            path.display()
                        );
                    }
                }
            }
        }
        assert!(checked >= 2, "expected examples using `routing auto`");
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
                    ..Default::default()
                },
                crate::types::FirewallRule {
                    match_expr: Some("tcp dport 22".into()),
                    action: Some("accept".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
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
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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
                ..Default::default()
            }],
            ..Default::default()
        };
        let nat = crate::types::NatConfig {
            rules: vec![crate::types::NatRule {
                action: crate::types::NatAction::Dnat,
                src: None,
                dst: Some("203.0.113.1/32".into()),
                target: Some("10.0.0.10".into()),
                target_port: Some(8080),
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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
                ..Default::default()
            }],
            ..Default::default()
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
                    ..Default::default()
                },
                crate::types::FirewallRule {
                    match_expr: Some("ip6 daddr fd00::1".into()),
                    action: Some("accept".into()),
                    ..Default::default()
                },
                crate::types::FirewallRule {
                    match_expr: Some("ip saddr 10.0.0.1".into()),
                    action: Some("accept".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let cfg = topology_to_nftables_config(Some(&fw), None).unwrap();
        let input_rules = cfg.tables()[0]
            .rules()
            .iter()
            .filter(|r| r.chain() == "input")
            .count();
        assert_eq!(input_rules, 3);
    }

    /// #164: input and forward chains are always declared (forward accepts
    /// unless declared), output only when declared; each chain's rules get
    /// their own keys; a reject is a reject; a policy is accept or drop.
    #[test]
    fn nftables_config_chains_keys_and_strict_verdicts() {
        use nlink::netlink::nftables::types::Policy;
        let fw = crate::types::FirewallConfig {
            policy: Some("drop".into()),
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("tcp dport 22".into()),
                action: Some("reject".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cfg = topology_to_nftables_config(Some(&fw), None).unwrap();
        let t = &cfg.tables()[0];
        let chains: Vec<(&str, Option<Policy>)> =
            t.chains().iter().map(|c| (c.name(), c.policy())).collect();
        assert_eq!(
            chains,
            [
                ("input", Some(Policy::Drop)),
                ("forward", Some(Policy::Accept))
            ],
            "an undeclared forward chain accepts"
        );
        assert!(matches!(
            t.rules()[0].exprs().unwrap().last(),
            Some(nlink::netlink::nftables::Expr::Reject { .. })
        ));

        let mut fw2 = fw.clone();
        fw2.forward = Some(crate::types::FirewallChain {
            policy: Some("drop".into()),
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("iifname lan0".into()),
                action: Some("accept".into()),
                ..Default::default()
            }],
        });
        fw2.output = Some(Default::default());
        let cfg = topology_to_nftables_config(Some(&fw2), None).unwrap();
        let t = &cfg.tables()[0];
        let names: Vec<&str> = t.chains().iter().map(|c| c.name()).collect();
        assert_eq!(names, ["input", "forward", "output"]);
        let keys: Vec<&str> = t.rules().iter().filter_map(|r| r.handle_key()).collect();
        assert_eq!(keys, ["nlink-lab/fw/input/0", "nlink-lab/fw/forward/0"]);

        let mut bad = fw.clone();
        bad.policy = Some("reject".into());
        let err = topology_to_nftables_config(Some(&bad), None).unwrap_err();
        assert!(err.to_string().contains("accept or drop"), "{err}");
        let mut bad = fw;
        bad.rules[0].action = Some("dorp".into());
        let err = topology_to_nftables_config(Some(&bad), None).unwrap_err();
        assert!(err.to_string().contains("unknown firewall action"), "{err}");
    }

    /// #164 wave C: declared sets reach the config with their key, flags,
    /// size, timeout and elements; rules look them up.
    #[test]
    fn nftables_config_declares_sets_and_lookups() {
        use nlink::netlink::nftables::types::{SetFlags, SetKeyType};
        let topo = crate::parser::parse(
            r#"lab "t"
node gw {
  firewall {
    set blocked [10.0.2.66, 10.0.9.0/24]
    set pub ipv4 . port [10.0.1.10 . 22, 10.0.1.0/24 . 8000-8999]
    set q ipv4 dynamic timeout 300s size 1024
    forward policy drop {
      drop src @blocked
      drop src @q
      accept dst . tcp dport @pub
    }
  }
}
"#,
        )
        .unwrap();
        let fw = topo.effective_firewall(&topo.nodes["gw"]);
        let cfg = topology_to_nftables_config(fw.as_deref(), None).unwrap();
        let t = &cfg.tables()[0];
        let set = |n: &str| t.sets().iter().find(|s| s.name() == n).unwrap();
        assert!(matches!(set("blocked").key_type(), SetKeyType::Ipv4Addr));
        assert!(set("blocked").flags().contains(SetFlags::INTERVAL));
        assert_eq!(set("blocked").elements().len(), 2);
        assert!(matches!(set("pub").key_type(), SetKeyType::Concat(k) if k.len() == 2));
        assert!(set("pub").flags().contains(SetFlags::INTERVAL));
        assert_eq!(set("q").size(), Some(1024));
        assert_eq!(
            set("q").timeout(),
            Some(std::time::Duration::from_secs(300))
        );
        assert!(set("q").flags().contains(SetFlags::EVAL));
        let lookups = t
            .rules()
            .iter()
            .filter(|r| {
                r.exprs()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, nlink::netlink::nftables::Expr::Lookup(_)))
            })
            .count();
        assert_eq!(lookups, 3);
    }

    /// #166: statements lower in a fixed order (limit, counter, named
    /// counter, log, verdict); each named counter is declared once.
    #[test]
    fn nftables_config_statements_and_counter_objects() {
        use nlink::netlink::nftables::Expr;
        let topo = crate::parser::parse(
            r#"lab "t"
node s {
  firewall policy drop {
    drop tcp dport 22 limit 5/second counter counter name ssh log "x: "
    tcp dport 23 counter name ssh
    counter name all
  }
}
"#,
        )
        .unwrap();
        let fw = topo.effective_firewall(&topo.nodes["s"]);
        let cfg = topology_to_nftables_config(fw.as_deref(), None).unwrap();
        let t = &cfg.tables()[0];
        let objects: Vec<&str> = t.objects().iter().map(|o| o.name()).collect();
        assert_eq!(objects, ["all", "ssh"], "declared once each, sorted");
        let kinds: Vec<&'static str> = t.rules()[0]
            .exprs()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Expr::Limit(_) => Some("limit"),
                Expr::Counter => Some("counter"),
                Expr::Objref(_) => Some("objref"),
                Expr::Log(_) => Some("log"),
                Expr::Verdict(_) | Expr::Immediate { .. } => Some("verdict"),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds[..4],
            ["limit", "counter", "objref", "log"],
            "{kinds:?}"
        );
        // a verdict-less rule ends with its statement
        assert!(matches!(
            t.rules()[2].exprs().unwrap().last(),
            Some(Expr::Objref(_))
        ));
    }

    /// #167: mangle statements lower to their nlink expressions; a clamp
    /// rule matches SYNs; NAT qualifiers add their matches and nothing for
    /// a rule without them.
    #[test]
    fn nftables_config_mangle_statements_and_nat_matches() {
        use nlink::netlink::nftables::Expr;
        let topo = crate::parser::parse(
            r#"lab "t"
node gw {
  firewall forward {
    oif wan0 clamp mss pmtu
    mark set 0x10/0xff ct mark save
    oif wan0 tcp dport 22 classify 1:3
  }
  nat {
    masquerade src 10.0.1.0/24
    masquerade src 10.0.1.0/24 oif wan0
    dnat dst 10.0.2.1 tcp dport 80 to 10.0.1.10:5201
  }
}
qdisc gw:wan0 prio { bands 3 }
"#,
        )
        .unwrap();
        let fw = topo.effective_firewall(&topo.nodes["gw"]);
        let cfg =
            topology_to_nftables_config(fw.as_deref(), topo.nodes["gw"].nat.as_ref()).unwrap();
        let t = &cfg.tables()[0];
        let rule = |key: &str| {
            t.rules()
                .iter()
                .find(|r| r.handle_key() == Some(key))
                .unwrap()
                .exprs()
                .unwrap()
        };
        let clamp = rule("nlink-lab/fw/forward/0");
        assert!(
            clamp.iter().any(|e| matches!(e, Expr::Rt { .. })),
            "{clamp:?}"
        );
        assert!(
            clamp.iter().any(|e| matches!(e, Expr::ExthdrSet { .. })),
            "{clamp:?}"
        );
        let marks = rule("nlink-lab/fw/forward/1");
        assert!(marks.iter().any(|e| matches!(e, Expr::MetaSet { .. })));
        assert!(marks.iter().any(|e| matches!(e, Expr::CtSet { .. })));
        let classify = rule("nlink-lab/fw/forward/2");
        assert!(
            matches!(classify.last(), Some(Expr::MetaSet { .. })),
            "{classify:?}"
        );
        let plain = rule("nlink-lab/nat/postrouting/0/masq");
        let with_oif = rule("nlink-lab/nat/postrouting/1/masq");
        assert_eq!(
            with_oif.len(),
            plain.len() + 2,
            "oif adds a meta load and a compare"
        );
        let dnat = rule("nlink-lab/nat/prerouting/2/dnat");
        assert!(
            dnat.iter().any(|e| matches!(e, Expr::Payload { .. })),
            "the port match"
        );
    }

    #[test]
    fn nftables_config_ip_keyword_with_v6_address_errors() {
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip saddr fd00::/64".into()),
                action: Some("accept".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = topology_to_nftables_config(Some(&fw), None).unwrap_err();
        assert!(err.to_string().contains("use 'ip6 saddr'"), "{err}");
        let fw = crate::types::FirewallConfig {
            policy: None,
            rules: vec![crate::types::FirewallRule {
                match_expr: Some("ip6 daddr 10.0.0.0/8".into()),
                action: Some("accept".into()),
                ..Default::default()
            }],
            ..Default::default()
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
                    proto: None,
                    dport: None,
                    iif: None,
                    oif: None,
                    target_end: None,
                    target_port_end: None,
                    flags: Vec::new(),
                },
                crate::types::NatRule {
                    action: crate::types::NatAction::Snat,
                    src: Some("fd00:2::/64".into()),
                    dst: None,
                    target: Some("fd00:1::1".into()),
                    target_port: None,
                    proto: None,
                    dport: None,
                    iif: None,
                    oif: None,
                    target_end: None,
                    target_port_end: None,
                    flags: Vec::new(),
                },
                crate::types::NatRule {
                    action: crate::types::NatAction::Dnat,
                    src: None,
                    dst: Some("2001:db8::1/128".into()),
                    target: Some("fd00:1::2".into()),
                    target_port: Some(8080),
                    proto: None,
                    dport: None,
                    iif: None,
                    oif: None,
                    target_end: None,
                    target_port_end: None,
                    flags: Vec::new(),
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
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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
                proto: None,
                dport: None,
                iif: None,
                oif: None,
                target_end: None,
                target_port_end: None,
                flags: Vec::new(),
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

    /// Every link is declared once. A VRF or bond member that is itself a
    /// declared kind carries its master on that declaration; only members
    /// created elsewhere (veths) get a bare `.master()` entry. A second,
    /// kind-less entry is a `Physical` declaration, which nlink 0.30 skips
    /// when it recreates the link, so the link came back without its master.
    #[test]
    fn network_config_declares_each_link_once_with_its_master() {
        use nlink::netlink::config::DeclaredLinkType;
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vl0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vlan),
                parent: Some("eth0".into()),
                vni: Some(10),
                ..Default::default()
            },
        );
        node.interfaces.insert(
            "d0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Dummy),
                ..Default::default()
            },
        );
        node.interfaces.insert(
            "bond0".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Bond),
                members: vec!["eth2".into(), "d0".into()],
                ..Default::default()
            },
        );
        node.vrfs.insert(
            "red".into(),
            crate::types::VrfConfig {
                table: 10,
                interfaces: vec!["vl0".into(), "eth1".into()],
                routes: BTreeMap::new(),
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let mut names: Vec<&str> = cfg.links().iter().map(|l| l.name()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a link is declared twice: {names:?}");
        let link = |n: &str| cfg.links().iter().find(|l| l.name() == n).unwrap();
        assert!(matches!(
            link("vl0").link_type(),
            DeclaredLinkType::Vlan { .. }
        ));
        assert_eq!(link("vl0").master(), Some("red"));
        assert!(matches!(link("d0").link_type(), DeclaredLinkType::Dummy));
        assert_eq!(link("d0").master(), Some("bond0"));
        // veths from the Links stage: bare enslave entries
        assert!(matches!(
            link("eth1").link_type(),
            DeclaredLinkType::Physical
        ));
        assert_eq!(link("eth1").master(), Some("red"));
        assert_eq!(link("eth2").master(), Some("bond0"));
        // …declared after every master
        let pos = |n: &str| cfg.links().iter().position(|l| l.name() == n).unwrap();
        assert!(pos("eth1") > pos("red") && pos("eth2") > pos("bond0"));
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

    /// An IPv6 underlay is declared with its endpoints: nlink 0.30 writes
    /// them as `IFLA_VXLAN_LOCAL6`/`GROUP6` (#418), where 0.26–0.29 dropped
    /// them and this planner refused the link.
    #[test]
    fn network_config_vxlan_accepts_ipv6_underlay() {
        use nlink::netlink::config::DeclaredLinkType;
        let topo = crate::parser::parse(
            r#"lab "t"
node host
"#,
        )
        .unwrap();
        let mut node = topo.nodes["host"].clone();
        node.interfaces.insert(
            "vx6".into(),
            crate::types::InterfaceConfig {
                kind: Some(crate::types::InterfaceKind::Vxlan),
                vni: Some(42),
                local: Some("fd00:1::1".into()),
                remote: Some("fd00:1::2".into()),
                ..Default::default()
            },
        );
        let cfg = topology_to_network_config("host", &node, &topo, None).unwrap();
        let vx = cfg.links().iter().find(|l| l.name() == "vx6").unwrap();
        let DeclaredLinkType::Vxlan { local, remote, .. } = vx.link_type() else {
            panic!("vx6 should be a vxlan: {vx:?}");
        };
        assert_eq!(*local, Some("fd00:1::1".parse().unwrap()));
        assert_eq!(*remote, Some("fd00:1::2".parse().unwrap()));
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
        let b64 = nlink::netlink::genl::wireguard::PrivateKey::from_bytes(priv_key).to_string();

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
                peer_settings: Default::default(),
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
                peer_settings: Default::default(),
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
                peer_settings: Default::default(),
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
                peer_settings: Default::default(),
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
                peer_settings: Default::default(),
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
