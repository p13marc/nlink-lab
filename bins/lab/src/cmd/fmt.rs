//! `nlink-lab fmt` — canonical whitespace for NLL files (issue #54).

use std::path::PathBuf;

use crate::ctx::Ctx;
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Files or directories (recursed for `*.nll`). `-` reads stdin.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// Exit 2 and list the files that would change; write nothing.
    #[arg(long, conflicts_with = "write")]
    pub check: bool,

    /// Rewrite files in place (default prints the result to stdout).
    #[arg(long, short = 'w')]
    pub write: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        paths,
        check,
        write,
    } = args;
    if paths.len() == 1 && paths[0].as_os_str() == "-" {
        let mut src = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut src)?;
        let out = nlink_lab::internal::fmt::format(&src)?;
        if check {
            if out != src {
                set_exit_code(EXIT_VALIDATION);
            }
        } else {
            print!("{out}");
        }
        return Ok(());
    }
    let mut files = Vec::new();
    for p in &paths {
        files.extend(crate::cmd::common::collect_nll(p, true)?);
    }
    let mut changed = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path)?;
        let out = nlink_lab::internal::fmt::format(&src)
            .map_err(|e| nlink_lab::Error::invalid_topology(format!("{}: {e}", path.display())))?;
        if !check && !write {
            if files.len() > 1 {
                println!("# {}", path.display());
            }
            print!("{out}");
        }
        if out == src {
            continue;
        }
        changed.push(path.clone());
        if write {
            std::fs::write(path, &out)?;
        }
    }
    if check {
        if ctx.json {
            crate::output::emit_json(ctx, &serde_json::json!({ "changed": changed }))?;
        } else {
            for p in &changed {
                println!("{}", p.display());
            }
        }
        if !changed.is_empty() {
            if !ctx.quiet && !ctx.json {
                eprintln!(
                    "{} of {} file(s) would be reformatted (run `nlink-lab fmt -w …`)",
                    changed.len(),
                    files.len()
                );
            }
            set_exit_code(EXIT_VALIDATION);
        }
    } else if write && !ctx.quiet {
        eprintln!("reformatted {} of {} file(s)", changed.len(), files.len());
    }
    Ok(())
}
