//! `nlink-lab deploy`.

use anstream::println;
use std::path::PathBuf;
use std::time::Instant;

use crate::cmd::common;
use crate::ctx::{Ctx, green, parse_topology, require_root};
use crate::host_scan::force_cleanup;
use crate::render::{print_deploy_summary, print_topology_summary};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Validate only, don't actually deploy.
    #[arg(long)]
    pub dry_run: bool,

    /// Destroy existing lab with same name before deploying.
    #[arg(long)]
    pub force: bool,

    /// Start the Zenoh backend daemon after deploying.
    #[arg(long)]
    pub daemon: bool,

    /// Skip validate block assertions after deploy.
    #[arg(long)]
    pub skip_validate: bool,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value)]
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
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        dry_run,
        force,
        daemon,
        skip_validate,
        params,
        strict,
        suffix,
        unique,
    } = args;
    let mut topo = parse_topology(&topology, &params)?;
    if unique {
        topo.lab.add_suffix(&std::process::id().to_string());
    } else if let Some(ref sfx) = suffix {
        topo.lab.add_suffix(sfx);
    }
    if skip_validate {
        topo.assertions.clear();
    }
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

    let start = Instant::now();
    let lab = topo.deploy().await?;
    let elapsed = start.elapsed();
    // what it came from, for a later `apply` (#234)
    let source = nlink_lab::state::LabSource {
        path: std::path::absolute(&topology)
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        params: params.clone(),
        suffix: if unique {
            Some(std::process::id().to_string())
        } else {
            suffix.clone()
        },
    };
    if let Err(e) = nlink_lab::state::set_source(lab.name(), &source) {
        tracing::warn!("could not record the source of '{}': {e}", lab.name());
    }
    let assertions_failed = lab.assertions_failed();

    if ctx.json {
        let mut report = serde_json::json!({
            "name": topo.lab.name,
            "nodes": topo.nodes.len(),
            "links": topo.links.len(),
            "deploy_time_ms": elapsed.as_millis() as u64,
        });
        if !lab.assertion_results().is_empty() {
            report["assertions"] = serde_json::to_value(lab.assertion_results())?;
            report["assertions_failed"] = serde_json::Value::Bool(assertions_failed);
        }
        println!("{report}");
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
                "  nlink-lab exec {} {} -- ip addr",
                topo.lab.name, first_node
            );
            println!(
                "  nlink-lab shell {} {}        # interactive shell",
                topo.lab.name, first_node
            );
            println!("  nlink-lab destroy {}         # tear down", topo.lab.name);
        }
        common::print_assertions(lab.assertion_results());
    }

    common::assertions_outcome(lab.assertion_results(), strict, &topo.lab.name, "deployed")?;

    if daemon {
        nlink_lab_backend::run(lab, nlink_lab_backend::BackendOpts::default()).await?;
    }
    Ok(())
}
