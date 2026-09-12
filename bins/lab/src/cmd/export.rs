//! `nlink-lab export`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_set_params};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name (or path to an .nll file with --archive).
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Output file (default: stdout for plain export, `./<lab>.nlz` with `--archive`).
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Produce a portable `.nlz` archive instead of plain TOML/JSON.
    #[arg(long)]
    pub archive: bool,

    /// (with --archive) Include live state (PIDs, ns names) for inspection.
    #[arg(long, requires = "archive")]
    pub include_running_state: bool,

    /// (with --archive) Skip the rendered.toml snapshot.
    #[arg(long, requires = "archive")]
    pub no_rendered: bool,

    /// (with --archive) NLL `param` overrides recorded in the archive.
    #[arg(long = "set", value_name = "KEY=VALUE", requires = "archive")]
    pub set_params: Vec<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        output,
        archive,
        include_running_state,
        no_rendered,
        set_params,
    } = args;
    if archive {
        use nlink_lab::portability::{ArchiveSource, ExportOptions, export_archive};
        let lab_path = std::path::Path::new(&lab);
        let source =
            if lab_path.extension().and_then(|s| s.to_str()) == Some("nll") || lab_path.exists() {
                ArchiveSource::Nll {
                    path: lab_path.into(),
                }
            } else {
                ArchiveSource::Lab { name: lab.clone() }
            };

        let params = parse_set_params(&set_params)?;

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

        let opts = ExportOptions {
            include_running_state,
            no_rendered,
            params,
        };
        export_archive(source, &out_path, opts)?;
        if !ctx.quiet {
            eprintln!("Archive written to {}", out_path.display());
        }
    } else {
        let running = nlink_lab::RunningLab::load(&lab)?;
        let content = if ctx.json {
            serde_json::to_string_pretty(running.topology())?
        } else {
            toml::to_string_pretty(running.topology())
                .map_err(|e| nlink_lab::Error::invalid_topology(format!("serialize: {e}")))?
        };
        match output {
            Some(path) => {
                std::fs::write(&path, &content)?;
                if !ctx.quiet {
                    eprintln!("Exported to {}", path.display());
                }
            }
            None => print!("{content}"),
        }
    }
    Ok(())
}
