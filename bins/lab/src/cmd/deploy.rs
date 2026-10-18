//! `nlink-lab deploy`.

use anstream::println;
use std::path::PathBuf;
use std::time::Instant;

use crate::cmd::common;
use crate::ctx::{Ctx, green, parse_topology, require_root};
use crate::host_scan::force_cleanup;
use crate::render::{print_deploy_summary, print_topology_summary};

/// `deploy --json`: the lab that was deployed (its name after
/// `--suffix`/`--unique`).
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct DeployReport {
    pub lab: String,
    pub nodes: usize,
    pub links: usize,
    pub duration_ms: u64,
    /// The `validate { … }` results, when there are assertions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<nlink_lab::test_runner::AssertionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions_failed: Option<bool>,
}

#[derive(clap::Args)]
pub struct Args {
    /// The topology file (`.nll`), or a lab archive (`.nlz`: `archive
    /// create` makes one), deployed with the params in it.
    pub topology: PathBuf,

    /// Validate only, don't actually deploy.
    #[arg(long)]
    pub dry_run: bool,

    /// Destroy existing lab with same name before deploying.
    #[arg(long)]
    pub force: bool,

    /// Skip validate block assertions after deploy.
    #[arg(long)]
    pub skip_validate: bool,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,

    /// Append suffix to lab name (for parallel test safety).
    #[arg(long)]
    pub suffix: Option<String>,

    /// Fail (exit 2) when any `validate { … }` assertion fails after
    /// deploy. The lab stays deployed for inspection.
    #[arg(long)]
    pub strict: bool,

    /// Auto-generate unique lab name suffix (appends PID).
    #[arg(long)]
    pub unique: bool,

    /// With an archive: deploy its rendered topology instead of parsing
    /// its NLL (an archive made by a newer nlink-lab).
    #[arg(long)]
    pub no_reparse: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        dry_run,
        force,
        skip_validate,
        params,
        strict,
        suffix,
        unique,
        no_reparse,
    } = args;
    let suffix = if unique {
        Some(std::process::id().to_string())
    } else {
        suffix
    };
    let archive = common::is_archive(&topology);
    if archive && !params.is_empty() {
        return Err(nlink_lab::Error::invalid_argument(
            "--set does not apply to an archive: its params are in it",
        ));
    }
    let adjust = |mut topo: nlink_lab::Topology| {
        if let Some(sfx) = &suffix {
            topo.lab.add_suffix(sfx);
        }
        if skip_validate {
            topo.assertions.clear();
        }
        topo
    };
    let mut topo = if archive {
        // a scratch copy to validate: `--force` must not destroy a lab
        // for an archive that does not deploy (#221)
        let scratch = std::env::temp_dir().join(format!("nlink-lab-nlz-{}", std::process::id()));
        let topo = common::archive_topology(&topology, &scratch, no_reparse);
        let _ = std::fs::remove_dir_all(&scratch);
        adjust(topo?)
    } else {
        adjust(parse_topology(&topology, &params)?)
    };
    common::check_topology(&topo)?;

    if dry_run {
        println!("Topology {:?} is valid", topo.lab.name);
        print_topology_summary(&topo);
        // The plan is pure: this is exactly what `deploy` would do.
        let plan = nlink_lab::plan_for(&topo)?;
        if ctx.json {
            let ops: Vec<serde_json::Value> = plan
                .ops
                .iter()
                .map(|op| {
                    serde_json::json!({ "stage": format!("{:?}", op.stage()), "op": op.describe() })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&ops)?);
        } else if !ctx.quiet {
            println!();
            common::print_plan(&plan);
        }
        return Ok(());
    }

    // before `--force`: it destroyed the lab, then refused to deploy (#221)
    require_root()?;

    // Handle --force: destroy existing lab first
    if force {
        if nlink_lab::state::exists(&topo.lab.name) {
            let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
            lab.destroy().await?;
        } else {
            // Best-effort cleanup of orphaned resources (no state file)
            force_cleanup(&topo.lab.name, topo.lab.prefix()).await;
        }
    }

    if archive {
        // the lab's files live with its state, and go with it
        let dir = nlink_lab::state::state_dir(&topo.lab.name).join("source");
        topo = adjust(common::archive_topology(&topology, &dir, no_reparse)?);
    }

    let start = Instant::now();
    let progress = crate::progress::Progress::new(ctx);
    let op = progress.op("deploy", &topo.lab.name);
    let deployed = topo.deploy_with(&op).await;
    progress.finish();
    let lab = deployed?;
    let elapsed = start.elapsed();
    // what it came from, for a later `apply` (#234)
    let source = nlink_lab::state::LabSource {
        path: std::path::absolute(&topology)
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        params: params.clone(),
        suffix: suffix.clone(),
        dir: None,
    };
    if let Err(e) = nlink_lab::state::set_source(lab.name(), &source) {
        tracing::warn!("could not record the source of '{}': {e}", lab.name());
    }
    let assertions_failed = lab.assertions_failed();

    if ctx.json {
        let report = DeployReport {
            lab: topo.lab.name.clone(),
            nodes: topo.nodes.len(),
            links: topo.links.len(),
            duration_ms: elapsed.as_millis() as u64,
            assertions_failed: (!lab.assertion_results().is_empty()).then_some(assertions_failed),
            assertions: lab.assertion_results().to_vec(),
        };
        crate::output::emit_json(ctx, &report)?;
    } else {
        println!(
            "{} Lab {:?} deployed in {:.0?}",
            green("OK"),
            topo.lab.name,
            elapsed
        );
        print_deploy_summary(&topo);

        if !ctx.quiet {
            let first_node = topo
                .nodes
                .keys()
                .next()
                .map(|s| s.as_str())
                .unwrap_or("node");
            println!();
            println!("Next steps:");
            println!(
                "  nlink-lab status {}          # inspect lab",
                topo.lab.name
            );
            println!(
                "  nlink-lab exec -L {} {} -- ip addr",
                topo.lab.name, first_node
            );
            println!(
                "  nlink-lab shell -L {} {}     # interactive shell",
                topo.lab.name, first_node
            );
            println!("  nlink-lab destroy {}         # tear down", topo.lab.name);
        }
        common::print_assertions(lab.assertion_results());
    }

    common::assertions_outcome(lab.assertion_results(), strict, &topo.lab.name, "deployed")?;
    Ok(())
}
