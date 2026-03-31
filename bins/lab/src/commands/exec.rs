use crate::util::check_root;

pub(crate) fn run(lab: String, node: String, cmd: Vec<String>) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    // Validate node exists
    let node_names: Vec<&str> = running.node_names().collect();
    if !node_names.contains(&node.as_str()) {
        eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
        eprintln!("Available nodes: {}", node_names.join(", "));
        std::process::exit(1);
    }
    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
    let output = running.exec(&node, &cmd[0], &args)?;

    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}
