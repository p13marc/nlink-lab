use std::path::PathBuf;

pub(crate) fn run(
    template: Option<String>,
    list: bool,
    output: Option<PathBuf>,
    _format: String,
    name: Option<String>,
    force: bool,
) -> nlink_lab::Result<()> {
    if list || template.is_none() {
        println!(
            "{:<15} {:<5} {:<5} DESCRIPTION",
            "TEMPLATE", "NODES", "LINKS"
        );
        println!("{}", "─".repeat(70));
        for t in nlink_lab::templates::list() {
            println!(
                "{:<15} {:<5} {:<5} {}",
                t.name, t.node_count, t.link_count, t.description
            );
        }
        return Ok(());
    }

    let template_name = template.unwrap();
    let t = nlink_lab::templates::get(&template_name).ok_or_else(|| {
        nlink_lab::Error::invalid_topology(format!(
            "unknown template '{template_name}'. Use --list to see available templates"
        ))
    })?;

    let nll_content = nlink_lab::templates::render(t, name.as_deref());
    let out_dir = output.unwrap_or_else(|| PathBuf::from("."));
    let lab_name = name.as_deref().unwrap_or(t.name);

    let path = out_dir.join(format!("{lab_name}.nll"));
    if path.exists() && !force {
        return Err(nlink_lab::Error::AlreadyExists {
            name: format!("{} (use --force to overwrite)", path.display()),
        });
    }
    std::fs::write(&path, &nll_content)?;
    println!(
        "Created {} ({} nodes, {} links)",
        path.display(),
        t.node_count,
        t.link_count
    );

    Ok(())
}
