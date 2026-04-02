use crate::util::check_root;
use std::path::PathBuf;

pub(crate) fn run_list(lab: String, json: bool) -> nlink_lab::Result<()> {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if containers.is_empty() {
        println!("No container nodes in lab '{lab}'.");
    } else if json {
        let data: Vec<serde_json::Value> = containers
            .iter()
            .map(|(name, state)| {
                serde_json::json!({ "node": name, "image": state.image, "id": state.id, "pid": state.pid })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        println!(
            "  {:<16} {:<20} {:<14} PID",
            "NODE", "IMAGE", "CONTAINER ID"
        );
        let mut entries: Vec<_> = containers.iter().collect();
        entries.sort_by_key(|(name, _)| (*name).clone());
        for (name, state) in entries {
            let short_id = if state.id.len() > 12 {
                &state.id[..12]
            } else {
                &state.id
            };
            println!(
                "  {:<16} {:<20} {:<14} {}",
                name, state.image, short_id, state.pid
            );
        }
    }
    Ok(())
}

pub(crate) fn run_logs(
    lab: String,
    node: String,
    follow: bool,
    tail: Option<u32>,
) -> nlink_lab::Result<()> {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Logs are only available for container nodes."
        ))
    })?;
    let rt = running.runtime_binary().unwrap_or("docker");
    let mut args = vec!["logs".to_string()];
    if follow {
        args.push("--follow".to_string());
    }
    if let Some(n) = tail {
        args.push("--tail".to_string());
        args.push(n.to_string());
    }
    args.push(container.id.clone());
    let status = std::process::Command::new(rt)
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("logs failed: {e}")))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

pub(crate) fn run_pull(topology: PathBuf) -> nlink_lab::Result<()> {
    let topo = nlink_lab::parser::parse_file(&topology)?;
    let images: std::collections::BTreeSet<&str> = topo
        .nodes
        .values()
        .filter_map(|n| n.image.as_deref())
        .collect();
    if images.is_empty() {
        println!("No container images in topology.");
    } else {
        let rt = nlink_lab::container::Runtime::detect()?;
        for image in &images {
            eprint!("Pulling {image}...");
            rt.pull_image(image)?;
            eprintln!(" done");
        }
        println!("{} image(s) pulled", images.len());
    }
    Ok(())
}

pub(crate) fn run_stats(lab: String) -> nlink_lab::Result<()> {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if containers.is_empty() {
        println!("No container nodes in lab '{lab}'.");
    } else {
        let rt = running.runtime_binary().unwrap_or("docker");
        let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
        let output = std::process::Command::new(rt)
            .args([
                "stats",
                "--no-stream",
                "--format",
                "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}",
            ])
            .args(&ids)
            .output()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("stats failed: {e}")))?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

pub(crate) fn run_restart(lab: String, node: String) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    let container = running.container_for(&node).ok_or_else(|| {
        nlink_lab::Error::deploy_failed(format!(
            "node '{node}' is not a container. Restart is only available for container nodes."
        ))
    })?;
    let rt = running.runtime_binary().unwrap_or("docker");
    eprint!("Restarting '{node}'...");
    let status = std::process::Command::new(rt)
        .args(["restart", &container.id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| nlink_lab::Error::deploy_failed(format!("restart failed: {e}")))?;
    if status.success() {
        eprintln!(" done");
    } else {
        eprintln!(" failed");
        std::process::exit(1);
    }
    Ok(())
}
