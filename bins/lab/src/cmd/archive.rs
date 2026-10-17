//! `nlink-lab archive`: portable `.nlz` lab archives (a manifest, the
//! topology, its params, the rendered topology, checksums) to share a
//! repro. `nlink-lab deploy FILE.nlz` deploys one.

use std::path::PathBuf;

use crate::ctx::{Ctx, LabArg};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Write an archive of a topology file, or of a running lab (`-L`).
    Create {
        /// The topology file (`.nll`); without it, the lab.
        #[arg(conflicts_with = "lab")]
        file: Option<PathBuf>,
        #[command(flatten)]
        lab: LabArg,
        /// Where to write it (default: `./<name>.nlz`).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Include the live state (PIDs, namespace names), for inspection.
        #[arg(long)]
        include_running_state: bool,
        /// Leave out the rendered topology.
        #[arg(long)]
        no_rendered: bool,
        /// NLL `param` values recorded in the archive (repeatable).
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
        params: Vec<(String, String)>,
    },
    /// Check an archive's checksums and validate it, extracting it into a
    /// directory (default `./<lab>/`).
    Extract {
        archive: PathBuf,
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Use the archive's rendered topology instead of parsing its NLL
        /// (an archive made by a newer nlink-lab).
        #[arg(long)]
        no_reparse: bool,
    },
    /// Summarize an archive without extracting it.
    Inspect { archive: PathBuf },
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    use nlink_lab::portability::{
        ArchiveSource, ExportOptions, export_archive, import_archive, inspect_archive,
    };
    match args.action {
        Action::Create {
            file,
            lab,
            output,
            include_running_state,
            no_rendered,
            params,
        } => {
            let source = match file {
                Some(path) => ArchiveSource::Nll { path },
                None => ArchiveSource::Lab {
                    name: lab.resolve()?,
                },
            };
            let out_path = output.unwrap_or_else(|| {
                let basename = match &source {
                    ArchiveSource::Lab { name } => name.clone(),
                    ArchiveSource::Nll { path } => path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("lab")
                        .to_string(),
                };
                PathBuf::from(format!("{basename}.nlz"))
            });
            export_archive(
                source,
                &out_path,
                ExportOptions {
                    include_running_state,
                    no_rendered,
                    params,
                },
            )?;
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({ "schema_version": 1, "archive": out_path })
                );
            } else if !ctx.quiet {
                eprintln!("Archive written to {}", out_path.display());
            }
            Ok(())
        }
        Action::Extract {
            archive,
            dir,
            no_reparse,
        } => {
            let report = import_archive(&archive, dir.as_deref(), no_reparse)?;
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "lab": report.manifest.lab_name,
                        "extracted_to": report.extracted_to,
                        "manifest": report.manifest,
                    })
                );
            } else if !ctx.quiet {
                eprintln!(
                    "Extracted lab '{}' to {} (format v{}, exported by {}); deploy it with `nlink-lab deploy {}`",
                    report.manifest.lab_name,
                    report.extracted_to.display(),
                    report.manifest.format_version,
                    report.manifest.exported_by,
                    archive.display(),
                );
            }
            Ok(())
        }
        Action::Inspect { archive } => {
            let summary = inspect_archive(&archive)?;
            if ctx.json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
                return Ok(());
            }
            let m = &summary.manifest;
            println!("Archive:       {}", archive.display());
            println!("Lab:           {}", m.lab_name);
            println!("Format:        v{}", m.format_version);
            println!("Exported by:   {} on {}", m.exported_by, m.exported_at);
            println!(
                "Platform:      {} {} / {}",
                m.platform.os, m.platform.kernel, m.platform.arch,
            );
            println!("State:         {:?}", m.deploy_state);
            if let Some(n) = summary.node_count {
                println!("Nodes:         {n}");
            }
            if let Some(n) = summary.link_count {
                println!("Links:         {n}");
            }
            if let Some(n) = summary.network_count {
                println!("Networks:      {n}");
            }
            println!("Files:");
            println!("  topology:    {}", m.files.topology);
            if let Some(f) = &m.files.params {
                println!("  params:      {f}");
            }
            if let Some(f) = &m.files.rendered {
                println!("  rendered:    {f}");
            }
            if let Some(f) = &m.files.state {
                println!("  state:       {f}");
            }
            Ok(())
        }
    }
}
