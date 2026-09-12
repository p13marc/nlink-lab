//! `nlink-lab lint` — style/portability advice, separate from `validate`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology, yellow};
use crate::output::{EXIT_VALIDATION, set_exit_code};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    #[arg(required_unless_present = "list_rules")]
    pub topology: Option<PathBuf>,

    /// Set NLL parameters (can be repeated: --set key=value).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Silence one lint rule (repeatable).
    #[arg(long, value_name = "RULE", add = crate::ctx::lint_rule_completer())]
    pub allow: Vec<String>,

    /// Exit 2 when any finding remains (CI gate).
    #[arg(long)]
    pub strict: bool,

    /// Print every lint rule and exit.
    #[arg(long, exclusive = true)]
    pub list_rules: bool,
}

/// `lint --json` envelope.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab lint --json")]
pub struct LintReport<'a> {
    pub lab: &'a str,
    pub findings: &'a [nlink_lab::LintFinding],
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        topology,
        params,
        allow,
        strict,
        list_rules,
    } = args;
    if list_rules {
        if ctx.json {
            println!(
                "{}",
                serde_json::to_string_pretty(nlink_lab::LINT_RULE_IDS)?
            );
        } else {
            for id in nlink_lab::LINT_RULE_IDS {
                println!("{id}");
            }
        }
        return Ok(());
    }
    if let Some(bad) = allow
        .iter()
        .find(|a| !nlink_lab::LINT_RULE_IDS.contains(&a.as_str()))
    {
        return Err(nlink_lab::Error::invalid_topology(format!(
            "unknown lint rule '{bad}' (see `lint --list-rules`)"
        )));
    }
    let topology = topology.expect("clap: topology is required unless --list-rules");
    let topo = parse_topology(&topology, &params)?;
    let findings = nlink_lab::lint(&topo, &allow);
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&LintReport {
                lab: &topo.lab.name,
                findings: &findings,
            })?
        );
    } else if findings.is_empty() {
        if !ctx.quiet {
            println!("Topology {:?}: no lint findings", topo.lab.name);
        }
    } else {
        for f in &findings {
            match &f.location {
                Some(loc) => println!("  {} [{}] {} at {loc}", yellow("LINT"), f.rule, f.message),
                None => println!("  {} [{}] {}", yellow("LINT"), f.rule, f.message),
            }
        }
        println!("{} finding(s)", findings.len());
    }
    if strict && !findings.is_empty() {
        set_exit_code(EXIT_VALIDATION);
    }
    Ok(())
}
