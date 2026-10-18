//! `nlink-lab diff`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};

#[derive(clap::Args)]
pub struct Args {
    /// First topology file.
    pub a: PathBuf,

    /// Second topology file.
    pub b: PathBuf,

    /// Set a `param` value (repeatable) for both files: --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { a, b, params } = args;
    let topo_a = parse_topology(&a, &params)?;
    let topo_b = parse_topology(&b, &params)?;
    let diff = nlink_lab::diff_topologies(&topo_a, &topo_b);
    if ctx.json {
        // The whole diff, as `verify --json` has it under
        // `topology`: the summary left routes, sysctls and the rest out,
        // and gave the links as counts (#210).
        let mut doc = serde_json::to_value(&diff)?;
        doc["total_changes"] = diff.change_count().into();
        doc["schema_version"] = 1.into();
        println!("{doc}");
    } else if diff.is_empty() {
        println!("No differences.");
    } else {
        println!("Diff: {} → {}", a.display(), b.display());
        print!("{diff}");
        println!("\n{} change(s)", diff.change_count());
    }
    Ok(())
}
