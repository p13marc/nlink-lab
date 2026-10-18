//! The operations that change labs (#266): what the daemon's jobs run and
//! what the CLI's commands call where they would otherwise do the same
//! thing their own way — deploy's `--force` and validation, the strict
//! end of assertions, image pulls, a container node's restart, the
//! dry-run report.

use std::path::{Path, PathBuf};
use std::time::Instant;

use nlink_lab_shared::api::{self, Request, Response, SourceBundle};

use super::OpCtx;
use super::view::{DryRunReport, plan_lists};
use crate::deploy::ApplyOptions;
use crate::diff::LayeredDiff;
use crate::error::{Error, Result};
use crate::running::RunningLab;
use crate::state;
use crate::test_runner::AssertionResult;
use crate::types::{Impairment, Topology};
use crate::validator::ValidationIssue;

/// Validate `topology`: its warnings, or its errors as an error.
pub fn validate(topology: &Topology) -> Result<Vec<ValidationIssue>> {
    let result = topology.validate();
    if result.has_errors() {
        return Err(Error::ValidationErrors(result.errors().cloned().collect()));
    }
    Ok(result.warnings().cloned().collect())
}

/// Make room for a deploy of `name` (what `deploy --force` does): destroy
/// the lab of that name, or sweep what one left without a state file.
pub async fn clear_name(name: &str, prefix: &str) -> Result<()> {
    state::validate_lab_name(name)?;
    if state::exists(name) {
        RunningLab::load(name)?.destroy().await
    } else {
        crate::host::force_cleanup(name, prefix).await;
        Ok(())
    }
}

/// How a run whose assertions failed ends: an error (exit 2) when
/// `strict`, else how many failed. `left` says how the lab stays
/// (`deployed`, `as applied`).
pub fn assertions_outcome(
    results: &[AssertionResult],
    strict: bool,
    lab: &str,
    left: &str,
) -> Result<usize> {
    let failed = results.iter().filter(|a| !a.passed).count();
    if failed > 0 && strict {
        return Err(Error::Validation(format!(
            "{failed} of {} assertion(s) failed (lab {lab:?} left {left} for inspection)",
            results.len()
        )));
    }
    Ok(failed)
}

/// `desired` made the topology of `lab`: a lab deployed with a suffix
/// (`deploy --suffix`/`--unique`) gets it again; any other name is an
/// error.
pub fn target_lab(desired: &mut Topology, lab: &str) -> Result<()> {
    if desired.lab.name != lab
        && let Some(sfx) = state::source_of(lab).and_then(|s| s.suffix)
        && format!("{}-{sfx}", desired.lab.name) == lab
    {
        desired.lab.add_suffix(&sfx);
    }
    if desired.lab.name != lab {
        return Err(Error::invalid_argument(format!(
            "topology is for lab {:?}, not {lab:?}",
            desired.lab.name
        )));
    }
    Ok(())
}

/// What converging a lab on a topology would change, in the pieces of a
/// [`DryRunReport`]: the layered diff, the plan's removals and its other
/// work.
#[derive(Debug)]
pub struct Check {
    pub layered: LayeredDiff,
    pub removals: Vec<String>,
    pub ops: Vec<String>,
}

impl Check {
    /// The `apply --dry-run` / `verify` document (schema v3).
    pub fn report<'a>(&'a self, lab: &'a str) -> DryRunReport<'a> {
        DryRunReport::new(lab, &self.layered, &self.removals).with_ops(&self.ops)
    }
}

/// What `apply` of `desired` to `running` would change, read from the
/// kernel and the plan; nothing changes.
pub async fn check(running: &RunningLab, desired: &Topology, opts: &ApplyOptions) -> Result<Check> {
    let layered = crate::compute_layered_diff_with(running, desired, opts).await?;
    let (removals, ops) = plan_lists(&crate::apply_plan_with(running, desired, opts)?);
    Ok(Check {
        layered,
        removals,
        ops,
    })
}

/// Pull the images a topology's containers use, with the runtime its
/// deploy will use, reporting each to `ctx`. Returns the images.
pub async fn pull_images(topology: &Topology, ctx: &OpCtx) -> Result<Vec<String>> {
    let images: Vec<String> = topology
        .nodes
        .values()
        .filter_map(|n| n.image.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if images.is_empty() {
        return Ok(images);
    }
    ctx.set_lab(&topology.lab.name);
    ctx.started();
    let pulled = async {
        // the runtime the deploy will use: detection preferred podman over
        // a lab's `runtime "docker"`, and pulled into the wrong store (#209)
        let rt = crate::container::Runtime::new(&topology.lab.runtime.clone().unwrap_or_default())?;
        for (i, image) in images.iter().enumerate() {
            ctx.step(i + 1, images.len(), || image.clone());
            ctx.waiting(
                api::WaitKind::ImagePull {
                    image: image.clone(),
                },
                None,
                None,
            );
            // blocking, as `podman pull` is: off the runtime's threads
            let (rt, image2) = (rt.clone(), image.clone());
            tokio::task::spawn_blocking(move || rt.pull_image(&image2))
                .await
                .map_err(|e| Error::Runtime(format!("pull: {e}")))??;
            ctx.log("info", format!("pulled {image}"));
            ctx.check()?;
        }
        Ok(())
    }
    .await;
    ctx.finished(&pulled, false);
    pulled.map(|()| images)
}

/// A container node's restart.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct Restarted {
    pub node: String,
    pub old_pid: u32,
    pub pid: u32,
    /// Links put back into its new namespace.
    pub links: usize,
    pub ops: usize,
}

/// Restart a container node and put back what its new namespace lacks:
/// `docker restart` recreates it, so every veth the deployer moved in is
/// gone (#31), and so are its sysctls and `run` commands.
pub async fn restart_node(running: &mut RunningLab, node: &str, ctx: &OpCtx) -> Result<Restarted> {
    let container = running.container_for(node).cloned().ok_or_else(|| {
        if running.topology().nodes.contains_key(node) {
            Error::invalid_argument(format!(
                "node '{node}' is not a container: only container nodes restart"
            ))
        } else {
            Error::NodeNotFound {
                name: node.to_string(),
            }
        }
    })?;
    let links = crate::host::node_link_count(running.topology(), node);
    let rt = running.runtime();
    let id = container.id.clone();
    let binary = rt.binary().to_string();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(binary)
            .args(["restart", &id])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await
    .map_err(|e| Error::Runtime(format!("restart failed: {e}")))?
    .map_err(|e| Error::Runtime(format!("restart failed: {e}")))?;
    if !status.success() {
        return Err(Error::Runtime(format!(
            "container restart exited with {}",
            status.code().unwrap_or(-1)
        )));
    }
    // The persisted init PID died with the old container process; read
    // the new one (a `.State.Pid` of 0 is refused by `inspect_pid`) so
    // later `/proc/<pid>/ns/net` references stay valid.
    let pid = rt
        .inspect_pid(&container.id)
        .map_err(|e| Error::Runtime(format!("'{node}' was restarted but is not running: {e}")))?;
    running.set_container_pid(node, pid)?;
    running.save_state()?;
    let ops = crate::deploy::reattach_node_with(running, node, ctx)
        .await?
        .ops;
    Ok(Restarted {
        node: node.to_string(),
        old_pid: container.pid,
        pid,
        links,
        ops,
    })
}

/// An impairment of the API, its values checked as NLL checks them.
pub fn impairment_of(spec: &api::ImpairmentSpec) -> Result<Impairment> {
    let pairs = [
        ("delay", &spec.delay),
        ("jitter", &spec.jitter),
        ("loss", &spec.loss),
        ("rate", &spec.rate),
        ("corrupt", &spec.corrupt),
        ("reorder", &spec.reorder),
        ("duplicate", &spec.duplicate),
        ("delay-correlation", &spec.delay_correlation),
        ("loss-correlation", &spec.loss_correlation),
        ("limit", &spec.limit),
    ];
    for (key, value) in pairs {
        if let Some(value) = value {
            Impairment::validate_property(key, value)
                .map_err(|e| Error::invalid_argument(format!("{key} {value}: {e}")))?;
        }
    }
    let imp = Impairment::from(spec);
    if imp == Impairment::default() {
        return Err(Error::invalid_argument(
            "an impairment sets something (delay, loss, rate, …); clear_impairment clears one",
        ));
    }
    Ok(imp)
}

/// Write a source bundle under `dir` and return its entry file. Imports
/// are relative to the entry file; one that climbs (`../common.nll`)
/// stays inside `dir`, as the entry is put as deep as the deepest climb.
pub fn stage_source(bundle: &SourceBundle, dir: &Path) -> Result<PathBuf> {
    let mut imports = Vec::new();
    let mut depth = 0usize;
    for (rel, text) in &bundle.imports {
        let (up, rest) = normalize(rel)?;
        depth = depth.max(up);
        imports.push((up, rest, text));
    }
    let base = (0..depth).fold(dir.to_path_buf(), |p, _| p.join("_"));
    let name = Path::new(&bundle.path)
        .file_name()
        .filter(|n| !n.is_empty())
        .map_or_else(|| "topology.nll".into(), |n| n.to_os_string());
    let write = |path: &Path, text: &str| -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    };
    let entry = base.join(name);
    write(&entry, &bundle.text)?;
    for (up, rest, text) in imports {
        let at = (0..depth - up).fold(dir.to_path_buf(), |p, _| p.join("_"));
        write(&rest.iter().fold(at, |p, c| p.join(c)), text)?;
    }
    Ok(entry)
}

/// A relative path as how far it climbs and where it goes from there.
fn normalize(rel: &str) -> Result<(usize, Vec<String>)> {
    let bad =
        || Error::invalid_argument(format!("import path {rel:?} is not a relative file path"));
    let path = Path::new(rel);
    if path.is_absolute() || rel.is_empty() {
        return Err(bad());
    }
    let (mut up, mut rest) = (0usize, Vec::<String>::new());
    for c in path.components() {
        match c {
            std::path::Component::Normal(s) => rest.push(s.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if rest.pop().is_none() {
                    up += 1;
                }
            }
            _ => return Err(bad()),
        }
    }
    if rest.is_empty() {
        return Err(bad());
    }
    Ok((up, rest))
}

/// The topology a deploy, plan or pull is about: the source bundle's,
/// parsed with its params from `staging`, else the topology sent.
pub fn topology_of(
    topology: serde_json::Value,
    source: Option<&SourceBundle>,
    staging: &Path,
) -> Result<Topology> {
    match source {
        Some(bundle) => parse_bundle(&stage_source(bundle, staging)?, bundle),
        None => serde_json::from_value(topology)
            .map_err(|e| Error::invalid_argument(format!("not a topology: {e}"))),
    }
}

/// Where a bundle's containers' relative host files are: beside the
/// client's file (its absolute `path`), not the copy staged here (#285).
/// A bundle without one (an unsaved design) has them nowhere.
pub fn origin(bundle: &SourceBundle) -> Option<String> {
    let path = Path::new(&bundle.path);
    path.is_absolute()
        .then(|| path.parent())
        .flatten()
        .map(|d| d.to_string_lossy().into_owned())
}

/// The staged `entry` of `bundle`, parsed with its params, its host files
/// the client's.
fn parse_bundle(entry: &Path, bundle: &SourceBundle) -> Result<Topology> {
    let mut topo = parse_entry(entry, &bundle.params)?;
    if let Some(dir) = origin(bundle) {
        topo.source_dir = Some(dir);
    }
    Ok(topo)
}

fn parse_entry(entry: &Path, params: &[(String, String)]) -> Result<Topology> {
    if params.is_empty() {
        crate::parser::parse_file(entry)
    } else {
        crate::parser::parse_file_with_params(entry, params)
    }
}

/// `topo` as a deploy with `options` makes it: suffixed, without its
/// assertions if asked, and validated (its warnings logged).
fn adjusted(mut topo: Topology, options: &api::DeployOptions) -> Result<Topology> {
    if let Some(sfx) = &options.suffix {
        topo.lab.add_suffix(sfx);
    }
    if options.skip_assertions {
        topo.assertions.clear();
    }
    for w in validate(&topo)? {
        tracing::warn!("{}: {w}", topo.lab.name);
    }
    Ok(topo)
}

/// A scratch directory under the system's temporary directory, removed
/// when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!("nlink-lab-{what}-{}-{n}", std::process::id())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The lab a deploy will make, its topology checked: what the daemon
/// queues the deploy behind.
pub fn deploy_target(
    topology: serde_json::Value,
    options: &api::DeployOptions,
    source: Option<&SourceBundle>,
) -> Result<Topology> {
    let scratch = Scratch::new("bundle");
    adjusted(topology_of(topology, source, &scratch.0)?, options)
}

/// Run a request that changes a lab, or reads what needs a topology or
/// the kernel's state (`plan_deploy`, `check_apply`, `verify`): `None`
/// for the other requests. Its progress goes to `ctx`.
pub async fn handle(req: Request, ctx: &OpCtx) -> Option<Result<Response>> {
    Some(match req {
        Request::Deploy {
            topology,
            options,
            source,
        } => deploy(topology, options, source, ctx).await,
        Request::PlanDeploy { topology } => (|| {
            let scratch = Scratch::new("plan");
            let topo = topology_of(topology, None, &scratch.0)?;
            validate(&topo)?;
            let plan = crate::plan_for(&topo)?;
            let ops: Vec<serde_json::Value> = plan
                .ops
                .iter()
                .map(|op| serde_json::json!({ "stage": op.stage().name(), "op": op.describe() }))
                .collect();
            report("plan", &ops)
        })(),
        Request::Apply {
            lab,
            topology,
            options,
        } => apply(&lab, topology, &options, ctx).await,
        Request::CheckApply {
            lab,
            topology,
            options,
        } => {
            async {
                let running = RunningLab::load(&lab)?;
                let mut desired = desired_of(&lab, topology)?;
                crate::subnet_pool::resolve_auto_subnets(running.topology(), &mut desired)?;
                let opts = ApplyOptions {
                    reset_impairments: options.reset_impairments,
                };
                let check = check(&running, &desired, &opts).await?;
                report("dry_run", &check.report(&lab))
            }
            .await
        }
        Request::Verify { lab } => {
            async {
                let running = RunningLab::load(&lab)?;
                let desired = running.topology().clone();
                let check = check(&running, &desired, &ApplyOptions::default()).await?;
                report("dry_run", &check.report(&lab))
            }
            .await
        }
        Request::Destroy { lab } => {
            async {
                state::validate_lab_name(&lab)?;
                RunningLab::load(&lab)?.destroy_with(ctx).await?;
                Ok(done(ctx, &lab, serde_json::Value::Null))
            }
            .await
        }
        Request::Restart { lab, node } => {
            async {
                let mut running = RunningLab::load(&lab)?;
                ctx.set_lab(&lab);
                ctx.started();
                let restarted = restart_node(&mut running, &node, ctx).await;
                ctx.finished(&restarted, false);
                Ok(done(ctx, &lab, serde_json::to_value(restarted?)?))
            }
            .await
        }
        Request::Snapshot {
            lab,
            name,
            description,
        } => (|| {
            state::validate_snapshot_name(&name)?;
            if !state::exists(&lab) {
                return Err(Error::NotFound { name: lab.clone() });
            }
            // a copy of the persisted state, taken under the lab's lock
            let _lock = state::lock_blocking(&lab)?;
            let (st, topology) = state::load(&lab)?;
            let meta = state::snapshot_save(&lab, &name, description.as_deref(), &st, &topology)?;
            report("snapshot", &meta)
        })(),
        Request::DeleteSnapshot { lab, name } => {
            state::snapshot_remove(&lab, &name).map(|()| Response::Ok)
        }
        Request::Restore { lab, name, dry_run } => {
            async {
                let snapshot = state::snapshot_load(&lab, &name)?;
                let mut running = RunningLab::load(&lab)?;
                if dry_run {
                    let opts = ApplyOptions {
                        reset_impairments: true,
                    };
                    let check = check(&running, &snapshot.topology, &opts).await?;
                    return report("dry_run", &check.report(&lab));
                }
                let applied = crate::deploy::restore_with(&mut running, &snapshot, ctx).await?;
                Ok(done(ctx, &lab, serde_json::to_value(applied)?))
            }
            .await
        }
        Request::RunScenario { lab, name } => {
            async {
                let mut running = RunningLab::load(&lab)?;
                let scenario = running
                    .topology()
                    .scenarios
                    .iter()
                    .find(|s| s.name == name)
                    .cloned()
                    .ok_or_else(|| {
                        Error::invalid_argument(format!("lab {lab:?} has no scenario {name:?}"))
                    })?;
                let result =
                    crate::scenario::run_scenario_with(&mut running, &scenario, ctx).await?;
                Ok(done(ctx, &lab, serde_json::to_value(result)?))
            }
            .await
        }
        Request::RunBenchmark { lab, name } => (|| {
            let running = RunningLab::load(&lab)?;
            let benchmark = running
                .topology()
                .benchmarks
                .iter()
                .find(|b| b.name == name)
                .cloned()
                .ok_or_else(|| {
                    Error::invalid_argument(format!("lab {lab:?} has no benchmark {name:?}"))
                })?;
            let result = crate::benchmark::run_benchmark_with(&running, &benchmark, ctx)?;
            Ok(done(ctx, &lab, serde_json::to_value(result)?))
        })(),
        Request::RunAssertions { lab } => (|| {
            let running = RunningLab::load(&lab)?;
            ctx.set_lab(&lab);
            ctx.started();
            let results = crate::deploy::run_assertions(&running, running.topology());
            // each said, as after a deploy or an apply
            for r in &results {
                ctx.emit(api::OpPhase::Assertion {
                    description: r.description.clone(),
                    passed: r.passed,
                    detail: r.detail.clone(),
                });
            }
            let outcome = Ok(());
            ctx.finished(&outcome, false);
            Ok(done(ctx, &lab, serde_json::to_value(results)?))
        })(),
        Request::PullImages { topology } => {
            async {
                let scratch = Scratch::new("pull");
                let topo = topology_of(topology, None, &scratch.0)?;
                let images = pull_images(&topo, ctx).await?;
                Ok(done(
                    ctx,
                    &topo.lab.name,
                    serde_json::json!({ "images": images }),
                ))
            }
            .await
        }
        Request::ReapOrphans => {
            async {
                let cleanups = crate::host::reap_orphans(&state::list()?).await;
                Ok(done(ctx, "", serde_json::to_value(cleanups)?))
            }
            .await
        }
        Request::Impair {
            lab,
            endpoint,
            direction,
            spec,
        } => {
            async {
                let impairment = impairment_of(&spec)?;
                let mut running = RunningLab::load(&lab)?;
                let peer = running.impair(&endpoint, &impairment, direction).await?;
                report(
                    "impairment",
                    &serde_json::json!({ "endpoint": endpoint, "peer": peer }),
                )
            }
            .await
        }
        Request::ClearImpairment { lab, endpoint } => {
            async {
                RunningLab::load(&lab)?.clear_impairment(&endpoint).await?;
                Ok(Response::Ok)
            }
            .await
        }
        Request::Partition { lab, endpoint } => {
            async {
                RunningLab::load(&lab)?.partition(&endpoint).await?;
                Ok(Response::Ok)
            }
            .await
        }
        Request::Heal { lab, endpoint } => {
            async {
                RunningLab::load(&lab)?.heal(&endpoint).await?;
                Ok(Response::Ok)
            }
            .await
        }
        Request::SetPairImpairment {
            lab,
            network,
            src,
            dst,
            spec,
            rate_cap,
        } => {
            async {
                let impairment = spec.as_ref().map(impairment_of).transpose()?;
                let edit = crate::edit::Edit::SetPairImpairment {
                    network,
                    src,
                    dst,
                    impairment,
                    rate_cap,
                };
                let mut running = RunningLab::load(&lab)?;
                let edited = crate::edit::edited(running.topology(), &[edit])?;
                crate::edit::apply_edited_with(&mut running, &edited, ctx).await?;
                Ok(Response::Ok)
            }
            .await
        }
        Request::Spawn {
            lab,
            node,
            argv,
            env,
            workdir,
            wait,
        } => {
            spawn(
                &lab,
                &node,
                &argv,
                &env,
                workdir.as_deref(),
                wait.as_ref(),
                ctx,
            )
            .await
        }
        Request::Kill { lab, pid, signal } => (|| {
            let running = RunningLab::load(&lab)?;
            match signal {
                None => {
                    let killed = running.kill_process(pid)?;
                    report("kill", &serde_json::json!({ "pid": pid, "killed": killed }))
                }
                Some(name) => {
                    let signal = crate::running::parse_signal(&name)?;
                    running.signal_process(pid, signal)?;
                    report(
                        "kill",
                        &serde_json::json!({ "pid": pid, "signal": signal.name() }),
                    )
                }
            }
        })(),
        _ => return None,
    })
}

fn report(kind: &str, value: &impl serde::Serialize) -> Result<Response> {
    Ok(Response::Report {
        kind: kind.to_string(),
        report: serde_json::to_value(value)?,
    })
}

fn done(ctx: &OpCtx, lab: &str, report: serde_json::Value) -> Response {
    Response::Done {
        op: ctx.id().to_string(),
        lab: lab.to_string(),
        report,
    }
}

/// The topology an `apply` or `check_apply` sent, checked and made the
/// lab's.
fn desired_of(lab: &str, topology: serde_json::Value) -> Result<Topology> {
    let mut desired: Topology = serde_json::from_value(topology)
        .map_err(|e| Error::invalid_argument(format!("not a topology: {e}")))?;
    target_lab(&mut desired, lab)?;
    validate(&desired)?;
    Ok(desired)
}

async fn deploy(
    topology: serde_json::Value,
    options: api::DeployOptions,
    source: Option<SourceBundle>,
    ctx: &OpCtx,
) -> Result<Response> {
    let mut topo = deploy_target(topology, &options, source.as_ref())?;
    let name = topo.lab.name.clone();
    ctx.set_lab(&name);
    if options.force {
        clear_name(&name, topo.lab.prefix()).await?;
    } else if state::exists(&name) {
        // before its sources are written over the running lab's
        return Err(Error::AlreadyExists { name });
    }
    // the lab's files live with its state, and go with it: parsed again
    // from there, the topology's paths point there
    let entry = match &source {
        Some(bundle) => {
            let entry = stage_source(bundle, &state::state_dir(&name).join("source"))?;
            topo = adjusted(parse_bundle(&entry, bundle)?, &options)?;
            Some(entry)
        }
        None => None,
    };
    let started = Instant::now();
    let lab = topo.deploy_with(ctx).await?;
    if let (Some(bundle), Some(entry)) = (&source, entry) {
        let recorded = state::LabSource {
            path: Some(entry.to_string_lossy().into_owned()),
            params: bundle.params.clone(),
            suffix: options.suffix.clone(),
            dir: origin(bundle),
        };
        if let Err(e) = state::set_source(&name, &recorded) {
            tracing::warn!("could not record the source of '{name}': {e}");
        }
    }
    let failed = assertions_outcome(lab.assertion_results(), options.strict, &name, "deployed")?;
    Ok(done(
        ctx,
        &name,
        serde_json::json!({
            "nodes": topo.nodes.len(),
            "links": topo.links.len(),
            "duration_ms": started.elapsed().as_millis() as u64,
            "assertions": lab.assertion_results(),
            "assertions_failed": failed,
        }),
    ))
}

async fn apply(
    lab: &str,
    topology: serde_json::Value,
    options: &api::ApplyOptions,
    ctx: &OpCtx,
) -> Result<Response> {
    let mut desired = desired_of(lab, topology)?;
    let mut running = RunningLab::load(lab)?;
    crate::subnet_pool::resolve_auto_subnets(running.topology(), &mut desired)?;
    let opts = ApplyOptions {
        reset_impairments: options.reset_impairments,
    };
    let applied = crate::apply_with_ctx(&mut running, &desired, &opts, ctx).await?;
    let failed = assertions_outcome(
        running.assertion_results(),
        options.strict,
        lab,
        "as applied",
    )?;
    Ok(done(
        ctx,
        lab,
        serde_json::json!({
            "ops": applied.ops,
            "removed": applied.removed,
            "applied": applied.applied,
            "assertions": running.assertion_results(),
            "assertions_failed": failed,
        }),
    ))
}

#[allow(clippy::too_many_arguments)]
async fn spawn(
    lab: &str,
    node: &str,
    argv: &[String],
    env: &[(String, String)],
    workdir: Option<&str>,
    wait: Option<&api::WaitSpec>,
    ctx: &OpCtx,
) -> Result<Response> {
    if argv.is_empty() {
        return Err(Error::invalid_argument("`spawn` needs a command"));
    }
    let mut running = RunningLab::load(lab)?;
    if !running.topology().nodes.contains_key(node) {
        return Err(Error::NodeNotFound {
            name: node.to_string(),
        });
    }
    // checked before the process starts: a bad log regex used to start
    // it, fail, and a retry started a second one (#209)
    if let Some(spec) = wait {
        crate::wait::WaitCondition::from_spec(spec, Some(0))?;
    }
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let workdir = workdir.map(Path::new);
    let pid = running.spawn_with_logs_with_opts(
        node,
        &args,
        crate::running::SpawnOpts {
            log_dir: None,
            workdir,
            env: &env_refs,
        },
    )?;
    running.save_state()?;
    if let Some(spec) = wait {
        let conditions = crate::wait::WaitCondition::from_spec(spec, Some(pid))?;
        if !conditions.is_empty() {
            let timeout = std::time::Duration::from_millis(if spec.timeout_ms == 0 {
                30_000
            } else {
                spec.timeout_ms
            });
            running
                .wait_all(
                    node,
                    &conditions,
                    timeout,
                    std::time::Duration::from_millis(500),
                    ctx,
                )
                .await?;
        }
    }
    let logs = running.log_paths(pid);
    Ok(Response::Spawned {
        pid,
        stdout: logs.map(|(o, _)| o.to_string()),
        stderr: logs.map(|(_, e)| e.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_keeps_its_imports_where_the_entry_finds_them() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = SourceBundle {
            path: "/home/u/labs/main.nll".into(),
            text: "import \"lib/a.nll\"\nimport \"../common.nll\"\n".into(),
            imports: [
                ("lib/a.nll".to_string(), "A".to_string()),
                ("../common.nll".to_string(), "C".to_string()),
            ]
            .into(),
            params: vec![],
        };
        let entry = stage_source(&bundle, dir.path()).unwrap();
        let base = entry.parent().unwrap();
        assert_eq!(entry.file_name().unwrap(), "main.nll");
        assert!(entry.starts_with(dir.path()));
        assert_eq!(
            std::fs::read_to_string(base.join("lib/a.nll")).unwrap(),
            "A"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("../common.nll")).unwrap(),
            "C"
        );
        assert!(base.join("../common.nll").starts_with(dir.path()));
    }

    /// A bundle's containers' host files are the client's, beside its
    /// file, not the copy staged under the lab's state (#285).
    #[test]
    fn a_bundles_host_files_are_the_clients() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = SourceBundle {
            path: "/home/u/labs/main.nll".into(),
            text: "import \"sub/web.nll\" as s\nlab \"b\"\nnode c image \"alpine\" { env-file \"c.env\" }\n"
                .into(),
            imports: [(
                "sub/web.nll".to_string(),
                "lab \"w\"\nnode web image \"nginx\" { config \"a.conf\" \"/etc/a.conf\" }\n".to_string(),
            )]
            .into(),
            params: vec![],
        };
        assert_eq!(origin(&bundle).as_deref(), Some("/home/u/labs"));
        let topo = topology_of(serde_json::Value::Null, Some(&bundle), dir.path()).unwrap();
        assert_eq!(topo.source_dir.as_deref(), Some("/home/u/labs"));
        let plan = crate::plan_for(&topo).unwrap();
        let opts: Vec<_> = plan
            .ops
            .iter()
            .filter_map(|o| match o {
                crate::deploy::op::Op::CreateContainer { opts, .. } => Some(opts.clone()),
                _ => None,
            })
            .collect();
        assert!(
            opts.iter()
                .any(|o| o.env_file.as_deref() == Some("/home/u/labs/c.env"))
        );
        assert!(opts
            .iter()
            .any(|o| o.configs.first().map(|c| c.0.as_str()) == Some("/home/u/labs/sub/a.conf")));
        // an unsaved design has no directory: its paths stay the staged copy's
        let unsaved = SourceBundle {
            path: "design.nll".into(),
            ..bundle
        };
        assert_eq!(origin(&unsaved), None);
    }

    #[test]
    fn a_bundle_cannot_write_outside_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["/etc/passwd", "", "..", "a/../.."] {
            let bundle = SourceBundle {
                path: "main.nll".into(),
                text: String::new(),
                imports: [(bad.to_string(), "x".to_string())].into(),
                params: vec![],
            };
            assert!(stage_source(&bundle, dir.path()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_bundle_deploys_its_parsed_topology() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = SourceBundle {
            path: "main.nll".into(),
            text: "param d default 2ms\nlab \"b\"\nnode a\nnode b\n\
                   link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay ${d} }\n"
                .into(),
            imports: Default::default(),
            params: vec![("d".into(), "7ms".into())],
        };
        let topo = topology_of(serde_json::Value::Null, Some(&bundle), dir.path()).unwrap();
        assert_eq!(topo.lab.name, "b");
        assert_eq!(topo.impairments["a:eth0"].delay.as_deref(), Some("7ms"));
    }

    #[test]
    fn an_impairment_is_checked() {
        let ok = api::ImpairmentSpec {
            delay: Some("10ms".into()),
            ..Default::default()
        };
        assert_eq!(impairment_of(&ok).unwrap().delay.as_deref(), Some("10ms"));
        let bad = api::ImpairmentSpec {
            delay: Some("10".into()),
            ..Default::default()
        };
        assert!(impairment_of(&bad).is_err());
        assert!(impairment_of(&api::ImpairmentSpec::default()).is_err());
    }

    #[test]
    fn strict_assertions_fail_the_run() {
        let failed = AssertionResult {
            description: "reach a b".into(),
            passed: false,
            detail: None,
            duration_ms: 0,
        };
        assert_eq!(
            assertions_outcome(std::slice::from_ref(&failed), false, "l", "deployed").unwrap(),
            1
        );
        let err = assertions_outcome(&[failed], true, "l", "deployed").unwrap_err();
        assert_eq!(err.to_api().exit_code, 2);
    }
}
