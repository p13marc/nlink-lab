//! `nlink-lab bundle`: a lab's failure bundle, on demand (#338).

use std::path::PathBuf;

use crate::ctx::{Ctx, require_root};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Where to write it: `DIR/<lab>/`. Default `$NLINK_LAB_BUNDLE_DIR`,
    /// else `./nlink-lab-bundles`.
    #[arg(short = 'o', long = "output", value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Also pack it as `<lab>.tar.gz` beside the directory.
    #[arg(long)]
    pub archive: bool,

    /// Why, for the summary (default: "on demand").
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        output,
        archive,
        reason,
    } = args;
    let lab = lab.resolve()?;
    require_root()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let root = output
        .or_else(|| std::env::var_os(nlink_lab::bundle::BUNDLE_DIR_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("nlink-lab-bundles"));
    let opts = nlink_lab::bundle::BundleOptions {
        reason: Some(reason.unwrap_or_else(|| "on demand".into())),
        replay: running
            .topology()
            .lab
            .seed
            .map(|s| format!("nlink-lab deploy --seed {s} <topology>")),
        ..Default::default()
    };
    let report = nlink_lab::bundle::collect(&running, &root.join(&lab), &opts).await?;
    let packed = if archive {
        Some(nlink_lab::bundle::archive(&report.dir)?)
    } else {
        None
    };
    if ctx.json {
        let mut value = serde_json::to_value(&report)?;
        if let Some(p) = &packed {
            value["archive"] = serde_json::Value::String(p.display().to_string());
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "bundle of lab '{lab}' in {} ({} files, {} bytes)",
            report.dir.display(),
            report.files,
            report.bytes
        );
        if let Some(p) = &packed {
            println!("packed as {}", p.display());
        }
        for c in &report.capped {
            println!("  capped: {c}");
        }
        for e in &report.errors {
            println!("  not collected: {e}");
        }
    }
    Ok(())
}
