//! `nlink-lab stats`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub lab: String,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if containers.is_empty() {
        if ctx.json {
            println!("[]");
        } else {
            println!("No container nodes in lab '{lab}'.");
        }
        return Ok(());
    }
    let rt = running.runtime_binary().unwrap_or("docker");
    let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
    let format = if ctx.json {
        "{{json .}}"
    } else {
        "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}"
    };
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
    if ctx.json {
        // docker/podman emit one JSON object per line; wrap as an array
        let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}
