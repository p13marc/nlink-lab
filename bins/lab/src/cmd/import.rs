//! `nlink-lab import`.

use std::path::PathBuf;

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Path to a `.nlz` archive.
    pub archive: PathBuf,

    /// Extract to this directory. Default: `./<lab-name>/`
    #[arg(short = 'd', long)]
    pub dir: Option<PathBuf>,

    /// Extract + validate only; don't deploy.
    #[arg(long)]
    pub no_deploy: bool,

    /// Use the archive's rendered.toml as-is, skip re-parsing the NLL.
    #[arg(long)]
    pub no_reparse: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        archive,
        dir,
        no_deploy,
        no_reparse,
    } = args;
    use nlink_lab::portability::import_archive;
    let report = import_archive(&archive, dir.as_deref(), no_reparse)?;
    if !ctx.quiet {
        eprintln!(
            "Extracted lab '{}' to {} (format v{}, exported by {})",
            report.manifest.lab_name,
            report.extracted_to.display(),
            report.manifest.format_version,
            report.manifest.exported_by,
        );
    }
    if no_deploy {
        if !ctx.quiet {
            eprintln!("(--no-deploy: skipping deploy)");
        }
        return Ok(());
    }
    // Deploy the imported topology. We re-read the extracted
    // topology.nll so the import path matches what `deploy`
    // would do for a regular file.
    let topology_path = report.extracted_to.join("topology.nll");
    let topo = nlink_lab::parser::parse_file(&topology_path)?;
    let lab = topo.deploy().await?;
    if !ctx.quiet {
        eprintln!("Deployed lab '{}'", lab.name());
    }
    Ok(())
}
