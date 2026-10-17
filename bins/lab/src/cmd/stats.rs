//! `nlink-lab stats`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,
}

/// One row of `stats --json`: a node's cgroup v2 usage, for namespace and
/// container nodes alike. Container rows were the runtime's own objects
/// (no `node`, sizes as strings) next to these (#223).
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct NodeStats<'a> {
    pub node: &'a str,
    pub kind: &'static str,
    #[serde(flatten)]
    pub usage: nlink_lab::cgroup::Usage,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab } = args;
    let lab = lab.resolve()?;
    let running = nlink_lab::RunningLab::load(&lab)?;

    // Namespace nodes with cpu/memory limits: cgroup usage (#66).
    let ns_rows: Vec<NodeStats> = running
        .topology()
        .nodes
        .iter()
        .filter(|(_, n)| n.image.is_none() && (n.cpu.is_some() || n.memory.is_some()))
        .filter_map(|(name, _)| {
            nlink_lab::cgroup::usage(&lab, name).map(|usage| NodeStats {
                node: name,
                kind: "namespace",
                usage,
            })
        })
        .collect();

    let containers = running.containers();
    if ctx.json {
        let mut rows: Vec<serde_json::Value> = ns_rows
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()?;
        for (name, c) in containers {
            match nlink_lab::cgroup::process_dir(c.pid)
                .and_then(|dir| nlink_lab::cgroup::usage_of(&dir))
            {
                Some(usage) => rows.push(serde_json::to_value(NodeStats {
                    node: name,
                    kind: "container",
                    usage,
                })?),
                None => eprintln!("warning: cannot read the cgroup of container node '{name}'"),
            }
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut container_table = String::new();
    if !containers.is_empty() {
        let runtime = running.runtime();
        let rt = runtime.binary();
        let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
        let format = "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}";
        let output = std::process::Command::new(rt)
            .args(["stats", "--no-stream", "--format", format])
            .args(&ids)
            .output()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("{rt} stats failed: {e}")))?;
        if !output.status.success() {
            return Err(nlink_lab::Error::deploy_failed(format!(
                "{rt} stats exited with {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        container_table = String::from_utf8_lossy(&output.stdout).into_owned();
    }

    if ns_rows.is_empty() && containers.is_empty() {
        println!("No container nodes or cpu/memory-limited namespace nodes in lab '{lab}'.");
        return Ok(());
    }
    if !ns_rows.is_empty() {
        println!(
            "{:<18} {:>10} {:>12} {:>12} {:>6}",
            "NODE", "CPU(s)", "MEM", "MEM MAX", "PIDS"
        );
        for r in &ns_rows {
            let mem_max = r
                .usage
                .memory_max
                .map(|m| format!("{:.0}M", m as f64 / 1_048_576.0))
                .unwrap_or_else(|| "max".into());
            println!(
                "{:<18} {:>10.2} {:>11.1}M {:>12} {:>6}",
                r.node,
                r.usage.cpu_usec as f64 / 1_000_000.0,
                r.usage.memory_bytes as f64 / 1_048_576.0,
                mem_max,
                r.usage.pids
            );
        }
    }
    if !container_table.is_empty() {
        if !ns_rows.is_empty() {
            println!();
        }
        print!("{container_table}");
    }
    Ok(())
}
