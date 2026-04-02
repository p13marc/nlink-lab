use crate::util::check_root;

pub(crate) fn run(lab: String, node: String, shell: String) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Validate node exists
    let node_names: Vec<&str> = running.node_names().collect();
    if !node_names.contains(&node.as_str()) {
        eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
        eprintln!("Available nodes: {}", node_names.join(", "));
        std::process::exit(1);
    }
    if let Some(container) = running.container_for(&node) {
        let rt = running.runtime_binary().unwrap_or("docker");
        let status = std::process::Command::new(rt)
            .args(["exec", "-it", &container.id, &shell])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("exec failed: {e}")))?;
        std::process::exit(status.code().unwrap_or(1));
    } else {
        let ns = running.namespace_for(&node)?;
        let ns_path = format!("/var/run/netns/{ns}");
        let status = std::process::Command::new("nsenter")
            .args(["--net", &ns_path, "--", &shell])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| nlink_lab::Error::deploy_failed(format!("nsenter failed: {e}")))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}
