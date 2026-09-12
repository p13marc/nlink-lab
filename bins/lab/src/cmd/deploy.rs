//! `nlink-lab deploy`.

use std::path::PathBuf;
use std::time::Instant;

use crate::ctx::{Ctx, green, parse_topology, red, require_root, validation_failed, yellow};
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
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

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
        topo.lab.name = format!("{}-{}", topo.lab.name, std::process::id());
    } else if let Some(ref sfx) = suffix {
        topo.lab.name = format!("{}-{sfx}", topo.lab.name);
    }
    if skip_validate {
        topo.assertions.clear();
    }
    let result = topo.validate();

    // Print warnings
    for w in result.warnings() {
        eprintln!("  {} {w}", yellow("WARN"));
    }

    if result.has_errors() {
        return Err(validation_failed(&topo.lab.name, &result));
    }

    if dry_run {
        println!("Topology {:?} is valid", topo.lab.name);
        print_topology_summary(&topo);
        return Ok(());
    }

    // Handle --force: destroy existing lab first
    if force {
        if nlink_lab::state::exists(&topo.lab.name) {
            let lab = nlink_lab::RunningLab::load(&topo.lab.name)?;
            lab.destroy().await?;
        } else {
            // Best-effort cleanup of orphaned resources (no state file)
            force_cleanup(&topo.lab.name).await;
        }
    }

    require_root()?;

    let start = Instant::now();
    let lab = topo.deploy().await?;
    let elapsed = start.elapsed();
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
        for a in lab.assertion_results() {
            if !a.passed {
                eprintln!(
                    "  {} {}{}",
                    red("FAIL"),
                    a.description,
                    a.detail
                        .as_ref()
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default()
                );
            }
        }
    }

    if assertions_failed {
        if strict {
            return Err(nlink_lab::Error::Validation(format!(
                "{} of {} assertion(s) failed (lab {:?} left deployed for inspection)",
                lab.assertion_results().iter().filter(|a| !a.passed).count(),
                lab.assertion_results().len(),
                topo.lab.name
            )));
        }
        eprintln!(
            "  {} assertions failed; pass --strict to make this fatal",
            yellow("WARN")
        );
    }

    if daemon {
        nlink_lab_backend::run(lab, nlink_lab_backend::BackendOpts::default()).await?;
    }
    Ok(())
}
