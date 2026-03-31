use std::path::PathBuf;

pub(crate) fn run(
    lab: String,
    output: Option<PathBuf>,
    json: bool,
) -> nlink_lab::Result<()> {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let content = if json {
        serde_json::to_string_pretty(running.topology())?
    } else {
        toml::to_string_pretty(running.topology())
            .map_err(|e| nlink_lab::Error::invalid_topology(format!("serialize: {e}")))?
    };
    match output {
        Some(path) => {
            std::fs::write(&path, &content)?;
            eprintln!("Exported to {}", path.display());
        }
        None => print!("{content}"),
    }
    Ok(())
}
